//! Everything that touches the terminal.

pub mod graphics;
pub mod input;
pub mod size;

use std::io::{self, Write};
use std::sync::OnceLock;

static ORIGINAL: OnceLock<libc::termios> = OnceLock::new();

/// Alternate screen, hidden cursor, mouse reporting (any motion, SGR
/// encoding, pixel coordinates if offered), focus reports and bracketed
/// paste, and the kitty keyboard protocol's first level, which is what lets
/// Command and Esc through unambiguously. The two
/// trailing queries ask whether pixel coordinates took, and for the cell size.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[?25l\
\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1016h\x1b[?1004h\x1b[?2004h\x1b[>1u\
\x1b[?1016$p\x1b[16t";

const LEAVE: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\\
\x1b[<u\x1b[?2004l\x1b[?1004l\x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\
\x1b[0 q\x1b[?25h\x1b[?1049l";

pub fn write(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
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

/// Undo `enter`. Safe to call more than once, or without `enter`.
pub fn leave() {
    if let Some(termios) = ORIGINAL.get() {
        write(LEAVE);
        // Replies to our queries may still be on their way, when kb exits
        // soon after starting. Discard them, or the shell reads them as
        // typed text.
        std::thread::sleep(std::time::Duration::from_millis(40));
        unsafe {
            libc::tcflush(0, libc::TCIFLUSH);
            libc::tcsetattr(0, libc::TCSANOW, termios);
        }
    }
}
