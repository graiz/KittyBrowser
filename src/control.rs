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
use std::path::{Path, PathBuf};
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

pub fn socket_path() -> Result<PathBuf, String> {
    match std::env::var_os("KB_SOCKET") {
        Some(path) => Ok(path.into()),
        None => Ok(crate::data_dir()?.join("kb.sock")),
    }
}

/// Start accepting clients on `path`. Each request is handed to `deliver`,
/// which returns false once the app has gone.
pub fn listen(path: &Path, deliver: impl Fn(Request) -> bool + Send + Clone + 'static) -> Result<(), String> {
    if UnixStream::connect(path).is_ok() {
        return Err("kb is already running; only one can run at a time".into());
    }
    // Left behind by a kb that didn't exit cleanly.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| format!("listening on {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let deliver = deliver.clone();
            std::thread::spawn(move || {
                let _ = serve(stream, deliver);
            });
        }
    });
    Ok(())
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
    let mut stream = UnixStream::connect(socket_path()?).map_err(|_| NOT_RUNNING)?;
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
