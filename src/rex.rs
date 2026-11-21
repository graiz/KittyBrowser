//! Rex, the terminal: where a caller of kb ctl or kb mcp is, and which
//! window each pane of its session is in, so the kb beside it is the one
//! commands go to. Rex is asked through its own command, `rex`; kb needs
//! nothing else from it, and works as before where it can't be asked.

use crate::control::{Caller, Windows};
use serde_json::Value;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long to wait for Rex before going on without it. Asked on every
/// command, so short: Rex answers this in a few milliseconds.
const PATIENCE: Duration = Duration::from_secs(2);

/// The caller, if it runs in Rex: Rex gives each pane's program
/// REX_SESSION and REX_BLOCK.
pub fn caller() -> Option<Caller> {
    let session = std::env::var("REX_SESSION").ok().filter(|session| !session.is_empty())?;
    Some(Caller { session, block: std::env::var("REX_BLOCK").ok().filter(|block| !block.is_empty()) })
}

/// The blocks of `session` and the window each is in, or `None` if Rex
/// can't be asked: not installed where kb can find it, not answering, or
/// slower than `PATIENCE`.
pub fn windows(session: &str) -> Option<Windows> {
    let child = Command::new(program())
        // Never start a Rex server just to ask it.
        .args(["block", "ls", "--json", "--autostart=false", "--timeout", "2s", "-s", session])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A group of its own, so all of it can be stopped if it hangs.
        .process_group(0)
        .spawn()
        .ok()?;
    let pid = child.id() as libc::pid_t;
    let (done, output) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(child.wait_with_output());
    });
    match output.recv_timeout(PATIENCE) {
        Ok(Ok(output)) if output.status.success() => parse_blocks(&String::from_utf8_lossy(&output.stdout)),
        Ok(_) => None,
        Err(_) => {
            // The thread reaps it once it has gone.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            None
        }
    }
}

/// Rex puts its own directory in the panes' REX_BIN_DIR, which works when
/// rex isn't on PATH.
fn program() -> PathBuf {
    std::env::var_os("REX_BIN_DIR")
        .map(|dir| PathBuf::from(dir).join("rex"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("rex"))
}

/// Each block in `rex block ls --json` output, and its window.
fn parse_blocks(json: &str) -> Option<Windows> {
    let listing: Value = serde_json::from_str(json).ok()?;
    let blocks = listing["blocks"].as_array()?;
    Some(
        blocks
            .iter()
            .filter_map(|block| Some((block["block_id"].as_str()?.to_owned(), block["window_id"].as_str()?.to_owned())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_their_windows() {
        let json = r#"{"blocks": [
            {"block_id": "block:a", "label": "shell", "placement": "placed", "window_id": "window:w"},
            {"block_id": "block:b", "label": "kb", "placement": "placed", "window_id": "window:v"},
            {"block_id": "block:c", "placement": "hidden"}
        ]}"#;
        let windows = parse_blocks(json).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows["block:a"], "window:w");
        assert_eq!(windows["block:b"], "window:v");
        assert!(parse_blocks("Error: no such session").is_none());
        assert!(parse_blocks(r#"{"sessions": []}"#).is_none());
    }
}
