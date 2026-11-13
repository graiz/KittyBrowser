//! `kb doctor`: check that this machine and this terminal can run kb, and
//! say what to fix when they can't.
//!
//! Most first runs that go wrong go wrong the same few ways: a terminal
//! without the kitty graphics protocol, tmux, or no Chrome. kb itself can
//! only show a blank pane or stray escape codes then, so this names the
//! cause before the user has to guess.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the environment says about the terminal kb is running in.
#[derive(Default)]
pub struct Env {
    pub term: Option<String>,
    pub term_program: Option<String>,
    pub tmux: bool,
    pub screen: bool,
    pub kitty: bool,
    pub wezterm: bool,
    pub rex: bool,
}

impl Env {
    pub fn read() -> Env {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Env {
            term: var("TERM"),
            term_program: var("TERM_PROGRAM"),
            tmux: var("TMUX").is_some(),
            screen: var("STY").is_some(),
            kitty: var("KITTY_WINDOW_ID").is_some(),
            wezterm: var("WEZTERM_EXECUTABLE").is_some() || var("WEZTERM_PANE").is_some(),
            rex: var("REX_SESSION").is_some(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Terminal {
    /// One kb is known to draw in.
    Supported(&'static str),
    /// One kb can't draw in, and why, with what to do instead.
    Unsupported(String),
    /// One kb doesn't recognise; it may work.
    Unknown(String),
}

const USE_INSTEAD: &str = "use Ghostty, kitty, WezTerm or Rex";

/// Decide from the environment alone, without asking the terminal.
pub fn terminal(env: &Env) -> Terminal {
    // tmux and screen sit between kb and the terminal and drop its images,
    // whatever the terminal outside them supports.
    if env.tmux {
        return Terminal::Unsupported(format!(
            "kb is running inside tmux, which doesn't pass images through; run kb in a plain pane ({USE_INSTEAD})"
        ));
    }
    if env.screen || env.term.as_deref().is_some_and(|term| term.starts_with("screen")) {
        return Terminal::Unsupported(format!(
            "kb is running inside GNU screen, which doesn't pass images through; run kb in a plain pane ({USE_INSTEAD})"
        ));
    }
    if env.kitty || env.term.as_deref() == Some("xterm-kitty") {
        return Terminal::Supported("kitty");
    }
    if env.rex {
        return Terminal::Supported("Rex");
    }
    if env.wezterm {
        return Terminal::Supported("WezTerm");
    }
    match env.term_program.as_deref() {
        Some("ghostty") => Terminal::Supported("Ghostty"),
        Some("WezTerm") => Terminal::Supported("WezTerm"),
        Some("Apple_Terminal") => {
            Terminal::Unsupported(format!("Terminal.app can't draw kitty graphics; {USE_INSTEAD}"))
        }
        Some("iTerm.app") => Terminal::Unsupported(format!("iTerm2 can't draw kitty graphics; {USE_INSTEAD}")),
        Some(other) => Terminal::Unknown(other.to_owned()),
        None if env.term.as_deref() == Some("xterm-ghostty") => Terminal::Supported("Ghostty"),
        None => Terminal::Unknown(env.term.clone().unwrap_or_else(|| "unknown".to_owned())),
    }
}

/// Stop before taking over the screen when the terminal can't show kb at
/// all, so the user reads why rather than a blank pane. KB_FORCE=1 skips it,
/// for a terminal that has gained support since.
pub fn refuse_unsupported_terminal() -> Result<(), String> {
    if std::env::var_os("KB_FORCE").is_some() {
        return Ok(());
    }
    match terminal(&Env::read()) {
        Terminal::Unsupported(why) => {
            Err(format!("{why}.\nkb doctor checks your setup; KB_FORCE=1 kb ... tries anyway."))
        }
        _ => Ok(()),
    }
}

/// Ask the terminal itself whether it draws kitty graphics: send a query
/// image and then a device attributes request, which every terminal
/// answers. A graphics reply before the attributes means yes. None when
/// stdin isn't a terminal or nothing answered.
fn probe_graphics() -> Option<bool> {
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
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
        let _ = out.flush();
        let reply = read_until_attributes(Duration::from_millis(800));
        libc::tcflush(0, libc::TCIFLUSH);
        libc::tcsetattr(0, libc::TCSANOW, &original);
        reply.map(|bytes| answered_graphics(&bytes))
    }
}

/// Read stdin until the device attributes reply (ESC [ ? ... c) or the
/// timeout.
fn read_until_attributes(timeout: Duration) -> Option<Vec<u8>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return if bytes.is_empty() { None } else { Some(bytes) };
        }
        let mut poll = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut poll, 1, left.as_millis() as i32) } <= 0 {
            continue;
        }
        let mut buf = [0u8; 256];
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..n as usize]);
        if has_attributes(&bytes) {
            return Some(bytes);
        }
    }
}

fn has_attributes(bytes: &[u8]) -> bool {
    bytes.windows(3).enumerate().any(|(start, window)| window == b"\x1b[?" && bytes[start..].contains(&b'c'))
}

fn answered_graphics(bytes: &[u8]) -> bool {
    bytes.windows(7).any(|window| window == b"\x1b_Gi=31")
}

/// Where the browser kb would start is. KB_CHROME may be a bare name, which
/// Chrome is started through PATH like any command, so look it up there.
fn resolve(path: &Path) -> Result<PathBuf, String> {
    if path.components().count() > 1 {
        return if path.is_file() { Ok(path.to_owned()) } else { Err(path.display().to_string()) };
    }
    std::env::var_os("PATH")
        .and_then(|dirs| std::env::split_paths(&dirs).map(|dir| dir.join(path)).find(|path| path.is_file()))
        .ok_or_else(|| path.display().to_string())
}

/// The browser kb would start, or why there is none and what to do.
pub fn browser() -> Result<PathBuf, String> {
    match crate::engine::chrome::find_binary() {
        Some(path) => resolve(&path).map_err(|path| format!("KB_CHROME is {path}, which isn't a program kb can find")),
        None => {
            Err("no Chrome, Chromium, Edge or Brave found; install one, or set KB_CHROME=/path/to/browser".to_owned())
        }
    }
}

struct Report {
    problems: usize,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        println!("  ok    {what}: {detail}");
    }
    fn note(&self, what: &str, detail: &str) {
        println!("  note  {what}: {detail}");
    }
    fn fail(&mut self, what: &str, detail: &str) {
        self.problems += 1;
        println!("  FAIL  {what}: {detail}");
    }
}

/// Run `kb doctor`. Returns the exit status: 1 when something would stop kb
/// from working.
pub fn run() -> i32 {
    let mut report = Report { problems: 0 };
    println!("kb {} on {}\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS);

    let verdict = terminal(&Env::read());
    let known_bad = matches!(verdict, Terminal::Unsupported(_));
    match verdict {
        Terminal::Supported(name) => report.ok("terminal", name),
        Terminal::Unsupported(why) => report.fail("terminal", &why),
        Terminal::Unknown(name) => report.note("terminal", &format!("{name} isn't one kb knows; checking it directly")),
    }
    // Asking a terminal already known not to work would only report the
    // same problem twice.
    if !known_bad {
        match probe_graphics() {
            Some(true) => report.ok("graphics", "the terminal answers kitty graphics queries"),
            Some(false) => report.fail("graphics", &format!("the terminal doesn't draw kitty graphics; {USE_INSTEAD}")),
            None => report.note("graphics", "couldn't ask the terminal (not a terminal, or it didn't answer)"),
        }
    }
    if let Some(window) = crate::term::size::window() {
        let pixels = if window.width_px > 0 {
            format!(", {}x{} pixels", window.width_px, window.height_px)
        } else {
            String::new()
        };
        report.ok("pane", &format!("{}x{} cells{pixels}", window.cols, window.rows));
    }

    match browser() {
        Ok(path) => report.ok("browser", &path.display().to_string()),
        Err(why) => report.fail("browser", &why),
    }

    match crate::data_dir() {
        Ok(dir) => {
            let probe = dir.join(".doctor");
            match std::fs::write(&probe, "") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                    report.ok("data", &dir.display().to_string());
                }
                Err(error) => report.fail("data", &format!("can't write in {}: {error}", dir.display())),
            }
        }
        Err(error) => report.fail("data", &error),
    }

    let running = crate::control::running();
    match running.len() {
        0 => report.note("running", "no kb is open now"),
        n => report.ok("running", &format!("{n} kb{} open (kb list)", if n == 1 { "" } else { "s" })),
    }
    report.note("agent skill", &crate::skill::status().unwrap_or_else(|error| error));
    report.note(
        "mcp",
        match crate::mcp::status() {
            "on" => "on (kb mcp off turns it off)",
            _ => "off (kb mcp on turns it on)",
        },
    );

    println!();
    if report.problems == 0 {
        println!("Everything kb needs is here. Try: kb example.com");
        0
    } else {
        println!(
            "{} problem{} to fix before kb will work.",
            report.problems,
            if report.problems == 1 { "" } else { "s" }
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(program: Option<&str>, term: Option<&str>) -> Env {
        Env { term_program: program.map(str::to_owned), term: term.map(str::to_owned), ..Env::default() }
    }

    #[test]
    fn known_terminals() {
        assert_eq!(terminal(&with(Some("ghostty"), None)), Terminal::Supported("Ghostty"));
        assert_eq!(terminal(&with(None, Some("xterm-ghostty"))), Terminal::Supported("Ghostty"));
        assert_eq!(terminal(&with(None, Some("xterm-kitty"))), Terminal::Supported("kitty"));
        assert_eq!(terminal(&with(Some("WezTerm"), None)), Terminal::Supported("WezTerm"));
        assert!(matches!(terminal(&with(Some("Apple_Terminal"), None)), Terminal::Unsupported(_)));
        assert!(matches!(terminal(&with(Some("iTerm.app"), None)), Terminal::Unsupported(_)));
        assert!(matches!(terminal(&with(Some("vscode"), None)), Terminal::Unknown(_)));
    }

    #[test]
    fn multiplexers_win() {
        let tmux = Env { tmux: true, kitty: true, ..Env::default() };
        assert!(matches!(terminal(&tmux), Terminal::Unsupported(why) if why.contains("tmux")));
        assert!(matches!(terminal(&with(Some("ghostty"), Some("screen-256color"))), Terminal::Unsupported(_)));
    }

    #[test]
    fn browser_paths() {
        assert!(resolve(Path::new("sh")).is_ok_and(|path| path.is_absolute()));
        assert!(resolve(Path::new("/bin/sh")).is_ok());
        assert!(resolve(Path::new("/no/such/browser")).is_err());
        assert!(resolve(Path::new("no-such-browser-kb")).is_err());
    }

    #[test]
    fn replies() {
        assert!(answered_graphics(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"));
        assert!(!answered_graphics(b"\x1b[?62;22c"));
        assert!(has_attributes(b"\x1b[?1;2c"));
        assert!(!has_attributes(b"\x1b_Gi=31;OK\x1b\\"));
    }
}
