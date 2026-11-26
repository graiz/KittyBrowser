//! A timeline of scrolling, for measuring it: each wheel tick as the
//! terminal sent it, each scroll sent to Chrome, each frame Chrome sent
//! back and when it was drawn. Written to the file named by KB_TRACE, one
//! line per event: milliseconds since the Unix epoch, the event, details.

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |since| since.as_secs_f64() * 1000.0)
}

/// The trace file, if KB_TRACE names one that could be opened.
fn file() -> Option<&'static Mutex<File>> {
    static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    FILE.get_or_init(|| {
        let path = std::env::var_os("KB_TRACE")?;
        File::options().create(true).append(true).open(path).ok().map(Mutex::new)
    })
    .as_ref()
}

/// Note `what` happened, with `details`, if tracing. The line is made
/// first and written at once, so that writing it takes one system call,
/// not one for each piece, in the middle of what is being timed.
pub fn event(what: &str, details: std::fmt::Arguments) {
    if let Some(file) = file() {
        let line = format!("{:.1}\t{what}\t{details}\n", now_ms());
        let _ = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).write_all(line.as_bytes());
    }
}
