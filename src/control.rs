//! The control socket: how `kb ctl` and the MCP server reach a running kb.
//!
//! A Unix socket in kb's data directory, readable only by the user. Each
//! line sent is a JSON request, `{"command": ..., "args": {...}}`, and each
//! gets one line back: `{"ok": true, "text": ...}`, `{"ok": true, "image":
//! <base64 PNG>}` or `{"ok": false, "error": ...}`.

use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// How long a client waits for the app to answer. Longer than the longest
/// `wait` the app allows.
const REPLY_TIMEOUT: Duration = Duration::from_secs(70);

pub const NOT_RUNNING: &str = "kb isn't running. Start it in a terminal pane first (kb [URL]), then retry.";

pub enum Output {
    Text(String),
    /// A base64 PNG.
    Image(String),
}

pub type Reply = mpsc::Sender<Result<Output, String>>;

/// One command from a client, with the channel its answer goes back on.
pub struct Request {
    pub command: String,
    pub args: Value,
    pub reply: Reply,
}

/// The most kbs that can run at once.
const MAX_INSTANCES: u32 = 16;

/// The socket of instance `n`: kb.sock for the first, kb-N.sock after it.
fn instance_socket(n: u32) -> Result<PathBuf, String> {
    let name = if n == 1 { "kb.sock".to_owned() } else { format!("kb-{n}.sock") };
    Ok(crate::data_dir()?.join(name))
}

/// The socket of the kb started with `--name NAME`.
fn named_socket(name: &str) -> Result<PathBuf, String> {
    Ok(crate::data_dir()?.join(format!("kb-{name}.sock")))
}

/// Whether `name` can name a kb: it becomes part of a file name, and must
/// not look like a number or an id, which start with a digit.
pub fn check_name(name: &str) -> Result<(), String> {
    let fits = name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    match fits {
        true => Ok(()),
        false => Err(format!(
            "{name:?} can't name a kb: use up to 32 lower-case letters, digits, - and _, starting with a letter"
        )),
    }
}

/// Which kb a client means.
#[derive(Debug, PartialEq)]
enum Chosen {
    /// A number, and an id (`3-a7f2`) if given, which names one run of that
    /// kb only.
    Number(u32, Option<String>),
    /// A kb started with `--name`.
    Name(String),
}

/// The kb asked for with `kb ctl -i N|ID|NAME` or KB_INSTANCE, if any.
fn chosen_instance() -> Result<Option<Chosen>, String> {
    let Ok(text) = std::env::var("KB_INSTANCE") else { return Ok(None) };
    choose(&text).map(Some)
}

fn choose(text: &str) -> Result<Chosen, String> {
    if check_name(text).is_ok() {
        return Ok(Chosen::Name(text.to_owned()));
    }
    let (number, id) = match text.split_once('-') {
        Some((number, _)) => (number, Some(text.to_owned())),
        None => (text, None),
    };
    match number.parse::<u32>() {
        Ok(n) if (1..=MAX_INSTANCES).contains(&n) => Ok(Chosen::Number(n, id)),
        _ => Err(format!("{text:?} isn't a kb's number, id or name; kb list shows the ones running")),
    }
}

/// The longest path a Unix socket can have, less its terminating zero.
fn longest_socket_path() -> usize {
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_path.len() - 1
}

/// The socket a kb answers on. It is removed as the kb quits, before
/// Chrome has closed, so that a kb started meanwhile can take its place;
/// and then only while it is still this kb's.
#[derive(Clone, Debug)]
pub struct Socket {
    path: PathBuf,
    inode: u64,
}

impl Socket {
    pub fn remove(&self) {
        if std::fs::metadata(&self.path).is_ok_and(|file| file.ino() == self.inode) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Listen on `socket`, readable only by the user.
fn bind(socket: &std::path::Path) -> Result<(UnixListener, Socket), String> {
    let length = socket.as_os_str().len();
    if length > longest_socket_path() {
        return Err(format!(
            "kb's socket would be {}, which at {length} bytes is longer than a Unix socket's path can be ({}); \
             run kb with a shorter HOME",
            socket.display(),
            longest_socket_path()
        ));
    }
    // Left behind by a kb that didn't exit cleanly.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(|e| format!("listening on {}: {e}", socket.display()))?;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    let inode = std::fs::metadata(socket).map_err(|e| format!("listening on {}: {e}", socket.display()))?.ino();
    Ok((listener, Socket { path: socket.to_owned(), inode }))
}

/// A name for one run of a kb, which unlike its number the next kb to start
/// doesn't take: the number, a dash and four hex digits.
fn new_id(instance: u32) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{instance}-{:04x}", (nanos ^ std::process::id().wrapping_mul(2_654_435_761)) & 0xffff)
}

/// This kb's id, once it has claimed its number.
pub fn id() -> Option<String> {
    std::env::var("KB_ID").ok()
}

/// A running kb's place among the others: its number or name, and the
/// socket it answers on.
pub struct Claim {
    /// The number of a kb started without a name: 1 for the first kb
    /// running, 2 for one started beside it, and so on.
    instance: Option<u32>,
    name: Option<String>,
    pub socket: Socket,
    listener: UnixListener,
}

/// Take the socket of the kb named `name`, or without one, the lowest
/// instance number no running kb holds.
pub fn claim(name: Option<&str>) -> Result<Claim, String> {
    // Held until this returns, so two kbs starting together can't both
    // take the same number or name.
    let lock = std::fs::File::create(crate::data_dir()?.join("kb.lock")).map_err(|e| format!("locking: {e}"))?;
    unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) };

    if let Some(name) = name {
        check_name(name)?;
        let socket = named_socket(name)?;
        if UnixStream::connect(&socket).is_ok() {
            return Err(format!("a kb named {name} is already running; kb ctl -i {name} quit stops it"));
        }
        let (listener, socket) = bind(&socket)?;
        // Read back by status, from this process only. A name is its own
        // id: no other kb can take it while this one runs.
        std::env::set_var("KB_ID", name);
        return Ok(Claim { instance: None, name: Some(name.to_owned()), socket, listener });
    }
    for instance in 1..=MAX_INSTANCES {
        let socket = instance_socket(instance)?;
        if UnixStream::connect(&socket).is_ok() {
            continue;
        }
        let (listener, socket) = bind(&socket)?;
        std::env::set_var("KB_ID", new_id(instance));
        return Ok(Claim { instance: Some(instance), name: None, socket, listener });
    }
    Err(format!("{MAX_INSTANCES} kbs are already running; quit one first"))
}

impl Claim {
    /// The profile this kb keeps cookies and site data in. Chrome won't
    /// share a profile between processes, so each kb running beside the
    /// first has its own, and a named kb always has the same one. It holds
    /// live sign-ins, so only the user can read it.
    pub fn profile(&self) -> Result<PathBuf, String> {
        let dir = crate::data_dir()?;
        let profile = match (&self.name, self.instance) {
            (Some(name), _) => dir.join("profiles").join(name),
            (None, Some(1) | None) => dir.join("profile"),
            (None, Some(n)) => dir.join(format!("profile-{n}")),
        };
        std::fs::create_dir_all(&profile).map_err(|e| format!("creating {}: {e}", profile.display()))?;
        let _ = std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700));
        Ok(profile)
    }

    /// Start accepting clients. Each request is handed to `deliver`, which
    /// returns false once the app has gone.
    pub fn listen(self, deliver: impl Fn(Request) -> bool + Send + Clone + 'static) {
        std::thread::spawn(move || {
            for stream in self.listener.incoming().flatten() {
                let deliver = deliver.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, deliver);
                });
            }
        });
    }
}

/// Connect to the kb that commands are meant for: the one KB_INSTANCE
/// names, else the lowest-numbered one running.
fn connect() -> Result<UnixStream, String> {
    let gone = |name: &str| format!("kb {name} isn't running; kb list shows the ones that are");
    if let Some(Chosen::Name(name)) = chosen_instance()? {
        return UnixStream::connect(named_socket(&name)?).map_err(|_| {
            format!("{}; start it with: kb --headless --name {name} [URL], or kb --name {name} in a pane", gone(&name))
        });
    }
    if let Some(Chosen::Number(n, id)) = chosen_instance()? {
        let socket = instance_socket(n)?;
        if let Some(id) = id {
            // The number may have passed to a kb started since: only the
            // one with this id will do.
            let status = UnixStream::connect(&socket)
                .map_err(|_| gone(&id))
                .and_then(|stream| exchange(stream, "status", json!({"listing": true})).map_err(|_| gone(&id)))?;
            let mine = matches!(&status, Output::Text(text) if text.lines().any(|line| line == format!("id: {id}")));
            if !mine {
                return Err(gone(&id));
            }
        }
        return UnixStream::connect(socket).map_err(|_| gone(&n.to_string()));
    }
    for n in 1..=MAX_INSTANCES {
        if let Ok(stream) = UnixStream::connect(instance_socket(n)?) {
            return Ok(stream);
        }
    }
    Err(NOT_RUNNING.to_owned())
}

/// The kbs running now, as `kb ctl -i` takes them: the numbered ones in
/// order, then the named ones by name.
pub fn running() -> Vec<String> {
    let mut found: Vec<String> = (1..=MAX_INSTANCES)
        .filter(|n| instance_socket(*n).is_ok_and(|socket| UnixStream::connect(socket).is_ok()))
        .map(|n| n.to_string())
        .collect();
    let mut names: Vec<String> = crate::data_dir()
        .ok()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let name = file.strip_prefix("kb-")?.strip_suffix(".sock")?.to_owned();
            check_name(&name).ok()?;
            UnixStream::connect(entry.path()).is_ok().then_some(name)
        })
        .collect();
    names.sort();
    found.append(&mut names);
    found
}

const STALLED: &str = "kb dropped the command unanswered: Chrome didn't answer within 30 s, or kb was quitting. \
     The page may be stuck in a script (try kb ctl reload), or Chrome itself stalled; if every command does this, \
     quit kb and start it again, and run kb doctor";

/// A request's arguments as the commands read them: numbers as the words
/// `kb ctl` would send. MCP clients send `{"timeout": 30}`.
fn words(mut args: Value) -> Value {
    if let Some(args) = args.as_object_mut() {
        for value in args.values_mut() {
            if let Value::Number(number) = value {
                *value = json!(number.to_string());
            }
        }
    }
    args
}

fn serve(mut stream: UnixStream, deliver: impl Fn(Request) -> bool) -> io::Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    for line in reader.lines() {
        let response = match serde_json::from_str::<Value>(&line?) {
            Ok(mut request) => {
                let (reply, answer) = mpsc::channel();
                let command = request["command"].as_str().unwrap_or("").to_owned();
                let args = words(request["args"].take());
                if !deliver(Request { command, args, reply }) {
                    return Ok(());
                }
                match answer.recv_timeout(REPLY_TIMEOUT) {
                    Ok(Ok(Output::Text(text))) => json!({"ok": true, "text": text}),
                    Ok(Ok(Output::Image(png))) => json!({"ok": true, "image": png}),
                    Ok(Err(error)) => json!({"ok": false, "error": error}),
                    // The command was dropped unanswered: Chrome never replied.
                    Err(mpsc::RecvTimeoutError::Disconnected) => json!({"ok": false, "error": STALLED}),
                    Err(mpsc::RecvTimeoutError::Timeout) => json!({"ok": false, "error": "kb did not answer in time"}),
                }
            }
            Err(error) => json!({"ok": false, "error": format!("bad request: {error}")}),
        };
        writeln!(stream, "{response}")?;
    }
    Ok(())
}

/// Send one command to the running kb and wait for its answer.
pub fn request(command: &str, args: Value) -> Result<Output, String> {
    exchange(connect()?, command, args)
}

fn exchange(mut stream: UnixStream, command: &str, args: Value) -> Result<Output, String> {
    let lost = |e: io::Error| format!("lost the connection to kb: {e}");
    writeln!(stream, "{}", json!({"command": command, "args": args})).map_err(lost)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(lost)?;
    let mut response: Value = serde_json::from_str(&line).map_err(|_| "kb closed the connection".to_owned())?;
    match (response["text"].take(), response["image"].take()) {
        (Value::String(text), _) => Ok(Output::Text(text)),
        (_, Value::String(png)) => Ok(Output::Image(png)),
        _ => Err(response["error"].as_str().unwrap_or("kb sent an empty answer").to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_numbers_and_ids() {
        assert_eq!(choose("2"), Ok(Chosen::Number(2, None)));
        assert_eq!(choose("3-a7f2"), Ok(Chosen::Number(3, Some("3-a7f2".to_owned()))));
        assert_eq!(choose("posting"), Ok(Chosen::Name("posting".to_owned())));
        assert_eq!(choose("tik-tok_2"), Ok(Chosen::Name("tik-tok_2".to_owned())));
        assert!(choose("0").is_err());
        assert!(choose("99").is_err());
        assert!(choose("Posting").is_err());
        assert!(choose("").is_err());
        assert!(check_name("2fast").is_err());
        assert!(check_name("has space").is_err());
        assert!(check_name("../up").is_err());
        assert!(check_name(&"a".repeat(33)).is_err());
    }

    #[test]
    fn numbers_become_words() {
        let args = words(json!({"timeout": 2, "count": 1.5, "text": "done", "full": true}));
        assert_eq!(args, json!({"timeout": "2", "count": "1.5", "text": "done", "full": true}));
    }

    #[test]
    fn long_socket_paths_are_refused_plainly() {
        let long = std::env::temp_dir().join("x".repeat(120)).join("kb.sock");
        assert!(bind(&long).unwrap_err().contains("longer than a Unix socket's path can be"));
    }
}
