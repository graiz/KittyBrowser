//! The control socket: how `kb ctl` and the MCP server reach a running kb.
//!
//! A Unix socket in kb's data directory, readable only by the user. Each
//! line sent is a JSON request, `{"command": ..., "args": {...}}`, and each
//! gets one line back: `{"ok": true, "text": ...}`, `{"ok": true, "image":
//! <base64 PNG>}` or `{"ok": false, "error": ...}`.

use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
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

/// The instance asked for with `kb ctl -i N` or KB_INSTANCE, if any: a
/// number, or an id (`3-a7f2`), which names one run of that kb only.
fn chosen_instance() -> Result<Option<(u32, Option<String>)>, String> {
    let Ok(text) = std::env::var("KB_INSTANCE") else { return Ok(None) };
    let (number, id) = match text.split_once('-') {
        Some((number, _)) => (number, Some(text.clone())),
        None => (text.as_str(), None),
    };
    match number.parse::<u32>() {
        Ok(n) if (1..=MAX_INSTANCES).contains(&n) => Ok(Some((n, id))),
        _ => Err(format!("{text:?} isn't a kb's number or id; kb list shows the ones running")),
    }
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

/// A running kb's place among the others: its number, and the socket it
/// answers on.
pub struct Claim {
    /// 1 for the first kb running, 2 for one started beside it, and so on.
    pub instance: u32,
    pub socket: PathBuf,
    listener: UnixListener,
}

/// Take the lowest instance number no running kb holds, and its socket.
pub fn claim() -> Result<Claim, String> {
    // Held until this returns, so two kbs starting together can't both
    // take the same number.
    let lock = std::fs::File::create(crate::data_dir()?.join("kb.lock")).map_err(|e| format!("locking: {e}"))?;
    unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) };

    let fixed = std::env::var_os("KB_SOCKET").map(PathBuf::from);
    let candidates: Vec<(u32, PathBuf)> = match fixed {
        Some(path) => vec![(1, path)],
        None => (1..=MAX_INSTANCES).map(|n| Ok((n, instance_socket(n)?))).collect::<Result<_, String>>()?,
    };
    for (instance, socket) in candidates {
        if UnixStream::connect(&socket).is_ok() {
            continue;
        }
        // Left behind by a kb that didn't exit cleanly.
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).map_err(|e| format!("listening on {}: {e}", socket.display()))?;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
        // Read back by status, from this process only.
        std::env::set_var("KB_ID", new_id(instance));
        return Ok(Claim { instance, socket, listener });
    }
    Err(match std::env::var_os("KB_SOCKET") {
        Some(_) => "another kb is already using the socket named by KB_SOCKET".to_owned(),
        None => format!("{MAX_INSTANCES} kbs are already running; quit one first"),
    })
}

impl Claim {
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

/// Connect to the kb that commands are meant for: the one named by
/// KB_SOCKET or KB_INSTANCE, else the lowest-numbered one running.
fn connect() -> Result<UnixStream, String> {
    if let Some(path) = std::env::var_os("KB_SOCKET") {
        return UnixStream::connect(path).map_err(|_| NOT_RUNNING.to_owned());
    }
    if let Some((n, id)) = chosen_instance()? {
        let gone = |name: &str| format!("kb {name} isn't running; kb list shows the ones that are");
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

/// The instance numbers of the kbs running now.
pub fn running() -> Vec<u32> {
    (1..=MAX_INSTANCES)
        .filter(|n| instance_socket(*n).is_ok_and(|socket| UnixStream::connect(socket).is_ok()))
        .collect()
}

fn serve(mut stream: UnixStream, deliver: impl Fn(Request) -> bool) -> io::Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    for line in reader.lines() {
        let response = match serde_json::from_str::<Value>(&line?) {
            Ok(mut request) => {
                let (reply, answer) = mpsc::channel();
                let command = request["command"].as_str().unwrap_or("").to_owned();
                if !deliver(Request { command, args: request["args"].take(), reply }) {
                    return Ok(());
                }
                match answer.recv_timeout(REPLY_TIMEOUT) {
                    Ok(Ok(Output::Text(text))) => json!({"ok": true, "text": text}),
                    Ok(Ok(Output::Image(png))) => json!({"ok": true, "image": png}),
                    Ok(Err(error)) => json!({"ok": false, "error": error}),
                    Err(_) => json!({"ok": false, "error": "kb did not answer in time"}),
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
