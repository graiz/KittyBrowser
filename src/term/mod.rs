//! Everything that touches the terminal.

pub mod ascii;
pub mod cursor;
pub mod graphics;
pub mod input;
pub mod layers;
pub mod page;
pub mod size;

use std::io::{self, Write};
use std::sync::OnceLock;

static ORIGINAL: OnceLock<libc::termios> = OnceLock::new();

/// Alternate screen, hidden cursor, mouse reporting (any motion, SGR
/// encoding, pixel coordinates if offered), focus reports and bracketed
/// paste, and the kitty keyboard protocol's first level, which is what lets
/// Command and Esc through unambiguously. It saves the mouse pointer's shape
/// to restore on leaving. The trailing queries ask whether pixel coordinates
/// took, for the cell size, and for the terminal's text and background
/// colours.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[?25l\
\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1016h\x1b[?1004h\x1b[?2004h\x1b[>1u\
\x1b]22;>default\x1b\\\
\x1b[?1016$p\x1b[16t\x1b]10;?\x1b\\\x1b]11;?\x1b\\";

const LEAVE: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b]22;<\x1b\\\
\x1b[<u\x1b[?2004l\x1b[?1004l\x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\
\x1b[0 q\x1b[?25h\x1b[?1049l";

/// Set the mouse pointer's shape over the pane, by its CSS name (pointer,
/// text, default, ...). Terminals that don't know OSC 22 ignore it.
pub fn pointer(shape: &str) {
    write(format!("\x1b]22;{shape}\x1b\\").as_bytes());
}

/// Put `text` on the system clipboard, through the terminal (OSC 52).
pub fn copy(text: &str) {
    write(format!("\x1b]52;c;{}\x1b\\", crate::base64::encode(text.as_bytes())).as_bytes());
}

/// `text` made safe to print to a terminal: newlines and tabs kept, every
/// other control character (C1 too) shown as �. Page text, titles, console
/// messages and errors carry whatever a page put in them, and printed raw,
/// an escape sequence in them could retitle the terminal, write its
/// clipboard, or worse. A carriage return just before a newline is kept:
/// it can't overwrite anything there, and text from forms and the network
/// often ends its lines so.
pub fn printable(text: &str) -> String {
    let mut chars = text.chars().peekable();
    let mut shown = String::with_capacity(text.len());
    while let Some(c) = chars.next() {
        let kept = !c.is_control() || c == '\n' || c == '\t' || (c == '\r' && chars.peek() == Some(&'\n'));
        shown.push(if kept { c } else { '\u{fffd}' });
    }
    shown
}

/// `text` to draw in the pane, with its control characters dropped. Page
/// text there (an address, an inspected element, a note quoting the page)
/// is laid out cell by cell, and a control character in it would be sent
/// to the terminal as one.
pub fn drawable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Set while another program (an editor) has the terminal: kb goes on
/// working, and what it would draw is dropped.
static LENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Lend the terminal to another program, or take it back.
pub fn lend(lent: bool) {
    if lent {
        drain(std::time::Duration::from_secs(1));
    }
    LENT.store(lent, std::sync::atomic::Ordering::SeqCst);
}

pub fn write(bytes: &[u8]) {
    if LENT.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(writer) = WRITER.get() {
        writer.push(bytes);
        return;
    }
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
    sent(bytes.len());
}

/// Count bytes handed to the terminal, for the trace: the total lets a
/// trace taken at the far end of an ssh link be matched with this one.
static SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn sent(count: usize) {
    let total = SENT.fetch_add(count as u64, std::sync::atomic::Ordering::SeqCst) + count as u64;
    crate::trace::event("flushed", format_args!("{count} bytes\t{total}"));
}

/// What waits to be written, and how many bytes have been queued and
/// written in all, counting from kb's first write.
struct Writer {
    queue: std::sync::Mutex<Queue>,
    ready: std::sync::Condvar,
    /// Called once everything up to `Queue::wake_at` has been written.
    wake: Box<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct Queue {
    bytes: Vec<u8>,
    queued: u64,
    written: u64,
    wake_at: Option<u64>,
    /// How many times `drain` has given up and dropped what was queued.
    drops: u64,
}

/// The writer hands the terminal this much at a time, and between pieces
/// checks whether `drain` gave up: what it held then is dropped too, so
/// that at most a piece of it is left to land on an editor's screen once
/// the terminal reads again, not a whole frame.
const PIECE: usize = 16 * 1024;

impl Writer {
    fn new(written: u64, wake: impl Fn() + Send + Sync + 'static) -> Writer {
        let queue = Queue { queued: written, written, ..Default::default() };
        Writer { queue: std::sync::Mutex::new(queue), ready: Default::default(), wake: Box::new(wake) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push(&self, bytes: &[u8]) {
        let mut queue = self.lock();
        queue.bytes.extend_from_slice(bytes);
        queue.queued += bytes.len() as u64;
        self.ready.notify_all();
    }

    /// Write what is queued to `sink`, for ever, as it comes.
    fn run(&self, sink: &mut dyn Write) {
        loop {
            let (bytes, drops) = {
                let mut queue = self.lock();
                while queue.bytes.is_empty() {
                    queue = self.ready.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                (std::mem::take(&mut queue.bytes), queue.drops)
            };
            for piece in bytes.chunks(PIECE) {
                if self.lock().drops != drops {
                    break;
                }
                let _ = sink.write_all(piece);
            }
            let _ = sink.flush();
            sent(bytes.len());
            let woken = {
                let mut queue = self.lock();
                queue.written += bytes.len() as u64;
                let written = queue.written;
                queue.wake_at.take_if(|mark| *mark <= written).is_some()
            };
            self.ready.notify_all();
            if woken {
                (self.wake)();
            }
        }
    }

    fn mark(&self) -> u64 {
        self.lock().queued
    }

    fn reached(&self, mark: u64) -> bool {
        let mut queue = self.lock();
        let reached = queue.written >= mark;
        if !reached {
            queue.wake_at = Some(mark);
        }
        reached
    }

    /// Wait until everything queued is written, or `limit` has passed;
    /// then drop what is still queued, and what the writer holds past the
    /// piece under way, so that it can't land on an editor's screen or the
    /// shell's afterwards. Returns whether all was written.
    fn drain(&self, limit: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        let mut queue = self.lock();
        while queue.written < queue.queued {
            let Some(left) = limit.checked_sub(start.elapsed()) else {
                // Counted as written, so marks still come due; what is
                // being written now counts itself when it is.
                queue.written += queue.bytes.len() as u64;
                queue.bytes.clear();
                queue.drops += 1;
                return false;
            };
            queue = match self.ready.wait_timeout(queue, left) {
                Ok((queue, _)) => queue,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        true
    }
}

static WRITER: OnceLock<Writer> = OnceLock::new();

/// Write to the terminal from a thread of its own from now on. A terminal
/// that is slow to read (busy drawing, or another client of it is behind)
/// fills the pty, and a write then waits; on the app thread that would hold
/// up scrolls and keys on their way to Chrome. `wake` is called when a
/// mark `reached` said wasn't yet has been.
pub fn write_in_background(wake: impl Fn() + Send + Sync + 'static) {
    // Count on from what was written before, so marks are totals.
    let writer = Writer::new(SENT.load(std::sync::atomic::Ordering::SeqCst), wake);
    if WRITER.set(writer).is_ok() {
        std::thread::spawn(|| WRITER.get().unwrap().run(&mut io::stdout()));
    }
}

/// A mark for everything written so far, to ask `reached` about.
pub fn mark() -> u64 {
    WRITER.get().map_or(SENT.load(std::sync::atomic::Ordering::SeqCst), Writer::mark)
}

/// Whether everything written up to `mark` has reached the terminal. If
/// not, the writer's `wake` is called once it has.
pub fn reached(mark: u64) -> bool {
    WRITER.get().is_none_or(|writer| writer.reached(mark))
}

/// Wait until everything written has reached the terminal, or `limit`
/// has passed: a terminal that has stopped reading mustn't keep kb from
/// leaving. Returns whether it all did.
fn drain(limit: std::time::Duration) -> bool {
    WRITER.get().is_none_or(|writer| writer.drain(limit))
}

/// Write `bytes` to the terminal now, past the writer and its lock on
/// stdout, giving up after a moment if the terminal isn't reading.
fn write_past_the_writer(bytes: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt;
    // The terminal opened afresh, not stdout made non-blocking: stdout
    // shares its open file with stdin and with the shell, which would find
    // their reads failing with EAGAIN while it was.
    let Ok(tty) =
        std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY).open("/dev/tty")
    else {
        return;
    };
    // The writer may be stuck partway through an image's escape; ST ends
    // it, so that what follows is read as escapes and not as its pixels.
    let bytes = [b"\x1b\\".as_slice(), bytes].concat();
    let (mut left, start) = (bytes.as_slice(), std::time::Instant::now());
    while !left.is_empty() && start.elapsed() < std::time::Duration::from_millis(200) {
        match (&tty).write(left) {
            Ok(n) => left = &left[n..],
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
}

/// Ask the terminal for its cell size in pixels; the answer arrives as an
/// `input::Event::CellSize`.
pub fn query_cell_size() {
    write(b"\x1b[16t");
}

/// Put the terminal in raw mode and take over the screen.
pub fn enter() -> io::Result<()> {
    unsafe {
        if libc::isatty(0) == 0 || libc::isatty(1) == 0 {
            return Err(io::Error::other("needs to run in a terminal"));
        }
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) < 0 {
            return Err(io::Error::last_os_error());
        }
        let _ = ORIGINAL.set(termios);
        libc::cfmakeraw(&mut termios);
        if libc::tcsetattr(0, libc::TCSANOW, &termios) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    write(ENTER);
    Ok(())
}

/// Undo `enter`, and remove the frames left for the terminal. Safe to call
/// more than once, or without `enter`.
pub fn leave() {
    if let Some(termios) = ORIGINAL.get() {
        // The way back out goes even when the terminal stopped reading what
        // was queued, which is then dropped.
        let second = std::time::Duration::from_secs(1);
        if !drain(second) || {
            write(LEAVE);
            !drain(second)
        } {
            write_past_the_writer(LEAVE);
        }
        // Replies to our queries may still be on their way, when kb exits
        // soon after starting. Discard them, or the shell reads them as
        // typed text.
        std::thread::sleep(std::time::Duration::from_millis(40));
        unsafe {
            libc::tcflush(0, libc::TCIFLUSH);
            libc::tcsetattr(0, libc::TCSANOW, termios);
        }
    }
    // After the writer has sent what names them.
    graphics::remove_frames();
}

/// Whether the terminal draws kitty graphics: `Some(false)` when it answered
/// without them, `None` when it didn't answer within `limit` (a slow link,
/// or a terminal that ignores the question), so the caller can go ahead. It
/// asks about a one-pixel image and then for the terminal's attributes
/// (DA1), which every terminal answers, in that order: a reply to the first
/// that hasn't come by the second never will. Run it before `enter`, so the
/// replies it reads aren't ones the app is waiting for.
///
/// Replies that come after `limit` wait for whatever reads the terminal
/// next. In kb that is its own input reader, which ignores them, as long
/// as nothing fails between this and `enter`; otherwise it is the shell,
/// which reads them as typed text.
pub fn draws_graphics(limit: std::time::Duration) -> Option<bool> {
    const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";
    let mut answer = Vec::new();
    unsafe {
        if libc::isatty(0) == 0 || libc::isatty(1) == 0 {
            return None;
        }
        let mut original: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut original) < 0 {
            return None;
        }
        let mut raw = original;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(0, libc::TCSANOW, &raw);
        write(QUERY);
        let deadline = std::time::Instant::now() + limit;
        while graphics_answer(&answer).is_none() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let mut poll = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
            if left.is_zero() || libc::poll(&mut poll, 1, left.as_millis() as i32) <= 0 {
                break;
            }
            let mut buf = [0u8; 256];
            let n = libc::read(0, buf.as_mut_ptr().cast(), buf.len());
            if n <= 0 {
                break;
            }
            answer.extend_from_slice(&buf[..n as usize]);
        }
        libc::tcsetattr(0, libc::TCSANOW, &original);
    }
    let draws = graphics_answer(&answer);
    if draws == Some(false) {
        // Such a terminal shows the question as text: wipe its line.
        write(b"\r\x1b[2K");
    }
    draws
}

/// What the replies to `draws_graphics`'s questions say, once the answer to
/// the second is in: an `ESC [ ?` whose digits and semicolons end in `c`.
/// Other reports, such as a mode's state still queued from before, also
/// start `ESC [ ?` but end otherwise.
fn graphics_answer(replies: &[u8]) -> Option<bool> {
    let attributes = (0..replies.len()).any(|start| {
        let Some(rest) = replies[start..].strip_prefix(b"\x1b[?") else {
            return false;
        };
        let parameters = rest.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
        rest.get(parameters) == Some(&b'c')
    });
    attributes.then(|| replies.windows(8).any(|w| w == b"\x1b_Gi=31;"))
}

#[cfg(test)]
mod tests {
    use super::{graphics_answer, io, printable, Write, Writer};

    #[test]
    fn page_text_cant_drive_the_terminal() {
        assert_eq!(printable("line one\n\tline two"), "line one\n\tline two");
        assert_eq!(printable("a\x1b[2Jb"), "a\u{fffd}[2Jb");
        assert_eq!(printable("over\rwrite"), "over\u{fffd}write");
        assert_eq!(printable("crlf\r\nlines\r\n\r"), "crlf\r\nlines\r\n\u{fffd}");
        assert_eq!(printable("next\u{85}line, \u{9b}31m"), "next\u{fffd}line, \u{fffd}31m");
        assert_eq!(printable("bell\x07 del\x7f"), "bell\u{fffd} del\u{fffd}");
        // Writing the clipboard: nothing of the sequence's controls survives.
        let osc52 = "copied \x1b]52;c;cm0gLXJmIH4=\x07 or \x1b]52;c;cm0gLXJmIH4=\x1b\\";
        let shown = printable(osc52);
        assert!(!shown.contains(|c: char| c.is_control()), "{shown:?}");
        assert_eq!(shown, "copied \u{fffd}]52;c;cm0gLXJmIH4=\u{fffd} or \u{fffd}]52;c;cm0gLXJmIH4=\u{fffd}\\");
        // Other scripts and symbols are left as they are.
        assert_eq!(printable("café ✕ 日本"), "café ✕ 日本");
    }

    #[test]
    fn reads_whether_the_terminal_draws_graphics() {
        assert_eq!(graphics_answer(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22;52c"), Some(true));
        // Apple's Terminal: only the attributes.
        assert_eq!(graphics_answer(b"\x1b[?1;2c"), Some(false));
        // Not finished yet, or nothing.
        assert_eq!(graphics_answer(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;2"), None);
        assert_eq!(graphics_answer(b""), None);
        // An error about the image still means the protocol is there.
        assert_eq!(graphics_answer(b"\x1b_Gi=31;EINVAL:bad\x1b\\\x1b[?62c"), Some(true));
        // A report still queued from before isn't the attributes.
        assert_eq!(graphics_answer(b"\x1b[?2026;2$y\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"), Some(true));
        assert_eq!(graphics_answer(b"\x1b[?2026;2$y\x1b[?1;2c"), Some(false));
        assert_eq!(graphics_answer(b"\x1b[?2026;2$y\x1b_Gi=31;OK\x1b\\"), None);
        // A key typed meanwhile, c included, doesn't end it either.
        assert_eq!(graphics_answer(b"\x1b[?62;2$yc"), None);
    }

    /// A terminal that reads only when told to.
    struct Slow(std::sync::mpsc::Receiver<()>, std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Slow {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            self.1.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A writer at work on a `Slow` terminal: `read` lets the terminal read
    /// once, `screen` is what it has read, and `woken` how often `wake` came.
    struct Test {
        writer: std::sync::Arc<Writer>,
        read: std::sync::mpsc::Sender<()>,
        screen: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        woken: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    fn slow_writer() -> Test {
        let (read, reads) = std::sync::mpsc::channel();
        let screen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = woken.clone();
        let writer = std::sync::Arc::new(Writer::new(0, move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let (runs, mut sink) = (writer.clone(), Slow(reads, screen.clone()));
        std::thread::spawn(move || runs.run(&mut sink));
        Test { writer, read, screen, woken }
    }

    fn eventually(what: impl Fn() -> bool) -> bool {
        let start = std::time::Instant::now();
        while !what() {
            if start.elapsed() > std::time::Duration::from_secs(2) {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }

    #[test]
    fn a_mark_comes_due_once_written_and_wakes_once() {
        let Test { writer, read, screen, woken } = slow_writer();
        writer.push(b"frame");
        let mark = writer.mark();
        assert!(!writer.reached(mark));
        read.send(()).unwrap();
        assert!(eventually(|| writer.reached(mark)));
        assert!(eventually(|| woken.load(std::sync::atomic::Ordering::SeqCst) == 1));
        assert_eq!(&*screen.lock().unwrap(), b"frame");
        // Nothing waited for, nothing to wake.
        writer.push(b"more");
        read.send(()).unwrap();
        assert!(eventually(|| screen.lock().unwrap().len() == 9));
        assert!(eventually(|| writer.lock().written == 9));
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_drain_that_times_out_drops_the_queue_and_keeps_count() {
        let Test { writer, read, screen, .. } = slow_writer();
        // One write taken by the writer and stuck, one waiting behind it.
        writer.push(b"stuck");
        assert!(eventually(|| writer.lock().bytes.is_empty()));
        writer.push(b"queued");
        assert!(!writer.drain(std::time::Duration::from_millis(20)));
        // The stuck write goes through; the queued one is gone, and the
        // count of written bytes is the count of queued ones, not more.
        read.send(()).unwrap();
        assert!(eventually(|| {
            let queue = writer.lock();
            queue.written == queue.queued
        }));
        assert_eq!(&*screen.lock().unwrap(), b"stuck");
        assert_eq!(writer.lock().queued, 11);
        writer.push(b"next");
        assert!(!writer.reached(writer.mark()));
        read.send(()).unwrap();
        assert!(eventually(|| writer.reached(writer.mark())));
    }

    #[test]
    fn a_drain_that_times_out_stops_the_write_under_way() {
        let Test { writer, read, screen, .. } = slow_writer();
        // A frame of three pieces, taken by the writer, the first piece
        // stuck: once the terminal reads again, only that piece lands.
        writer.push(&vec![b'x'; super::PIECE * 3]);
        assert!(eventually(|| writer.lock().bytes.is_empty()));
        assert!(!writer.drain(std::time::Duration::from_millis(20)));
        read.send(()).unwrap();
        assert!(eventually(|| {
            let queue = writer.lock();
            queue.written == queue.queued
        }));
        assert_eq!(screen.lock().unwrap().len(), super::PIECE);
        // And what comes after goes whole.
        writer.push(b"next");
        read.send(()).unwrap();
        assert!(eventually(|| screen.lock().unwrap().ends_with(b"next")));
    }
}
