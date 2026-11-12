//! DevTools protocol transport over Chrome's `--remote-debugging-pipe`:
//! JSON messages, each terminated by a NUL byte.

use serde_json::{json, Value};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

pub struct Sender {
    pipe: File,
    next_id: u64,
}

impl Sender {
    pub fn new(pipe: File) -> Self {
        Sender { pipe, next_id: 0 }
    }

    /// Send a command and return its id. `session` targets an attached page;
    /// without it the command goes to the browser itself.
    pub fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> io::Result<u64> {
        self.next_id += 1;
        let mut message = json!({"id": self.next_id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&message)?;
        bytes.push(0);
        self.pipe.write_all(&bytes)?;
        Ok(self.next_id)
    }
}

pub struct Receiver {
    pipe: BufReader<File>,
    buf: Vec<u8>,
}

impl Receiver {
    pub fn new(pipe: File) -> Self {
        Receiver { pipe: BufReader::with_capacity(1 << 16, pipe), buf: Vec::new() }
    }

    /// The next message, or `None` once Chrome has closed the pipe.
    pub fn recv(&mut self) -> io::Result<Option<Value>> {
        self.buf.clear();
        if self.pipe.read_until(0, &mut self.buf)? == 0 {
            return Ok(None);
        }
        if self.buf.last() == Some(&0) {
            self.buf.pop();
        }
        Ok(Some(serde_json::from_slice(&self.buf)?))
    }
}
