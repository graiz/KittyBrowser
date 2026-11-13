//! `kb doctor`: check that this machine and this terminal can run kb, and
//! say what to fix when they can't.
//!
//! Most first runs that go wrong go wrong the same few ways: a terminal
//! without the kitty graphics protocol, tmux, or no Chrome. kb itself can
//! only show a blank pane or stray escape codes then, so this names the
//! cause before the user has to guess.

use std::time::Duration;

/// The terminals kb is known to draw in, and where to get them.
pub const TERMINALS: &[(&str, &str)] = &[
    ("Ghostty", "https://ghostty.org"),
    ("kitty", "https://sw.kovidgoyal.net/kitty"),
    ("WezTerm", "https://wezterm.org"),
    ("Rex", "https://www.superlogical.com"),
];

/// How long kb waits for the terminal to say whether it draws kitty
/// graphics before going ahead anyway. kb doctor isn't in a hurry, so it
/// waits longer, which gets an answer over slower links.
pub const ASK_KB: Duration = Duration::from_millis(1500);
const ASK_DOCTOR: Duration = Duration::from_secs(5);

/// The multiplexer kb is running inside, if any: these sit between kb and
/// the terminal and don't pass its images on, whatever the terminal outside
/// them supports. TMUX and ZELLIJ are set inside them; reached over ssh,
/// only TERM is left, and tmux names itself screen there by default.
fn multiplexer(tmux: bool, zellij: bool, term: &str) -> Option<&'static str> {
    if tmux || term.starts_with("tmux") {
        Some("tmux")
    } else if zellij {
        Some("Zellij")
    } else if term.starts_with("screen") {
        Some("tmux or screen")
    } else {
        None
    }
}

fn multiplexer_here() -> Option<&'static str> {
    multiplexer(
        std::env::var_os("TMUX").is_some(),
        std::env::var_os("ZELLIJ").is_some(),
        &std::env::var("TERM").unwrap_or_default(),
    )
}

/// The terminal's name, to show; what it can do is asked of it directly.
/// It comes from variables that multiplexers and ssh pass on, so it can
/// name the terminal outside them.
fn terminal_name(program: Option<&str>, term: &str, rex: bool) -> Option<String> {
    Some(match program {
        Some("Apple_Terminal") => "Terminal".to_owned(),
        Some("iTerm.app") => "iTerm2".to_owned(),
        Some("vscode") => "VS Code's terminal".to_owned(),
        Some("ghostty") => "Ghostty".to_owned(),
        Some("rex") => "Rex".to_owned(),
        Some(name) if !name.is_empty() => name.to_owned(),
        _ if term == "xterm-kitty" => "kitty".to_owned(),
        _ if term == "xterm-ghostty" => "Ghostty".to_owned(),
        _ if rex => "Rex".to_owned(),
        _ => return None,
    })
}

fn terminal_name_here() -> Option<String> {
    terminal_name(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        &std::env::var("TERM").unwrap_or_default(),
        std::env::var_os("REX_SESSION").is_some(),
    )
}

/// "Ghostty, kitty, WezTerm or Rex".
fn terminal_list() -> String {
    let names: Vec<&str> = TERMINALS.iter().map(|(name, _)| *name).collect();
    match names.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// Why the terminal can't draw. A name from `TERMINALS` was passed on
/// through something kb didn't recognise, so that terminal isn't blamed.
fn why_no_graphics(multiplexer: Option<&str>, terminal: Option<&str>) -> String {
    if let Some(name) = multiplexer {
        return format!(
            "kb is running inside {name}, which doesn't pass the kitty graphics\n\
             protocol on to your terminal. Run kb outside {name}, in one of these:"
        );
    }
    let this = terminal.filter(|name| !TERMINALS.iter().any(|(known, _)| known == name)).unwrap_or("this terminal");
    format!(
        "kb draws real web pages in your terminal, pixel for pixel, using the\n\
         kitty graphics protocol, and {this} doesn't support it.\n\
         Run kb in a terminal that does:"
    )
}

/// Why kb can't run here, for a terminal that answered without kitty
/// graphics: the banner from the help, what's missing, and where kb runs.
pub fn no_graphics() -> String {
    let why = why_no_graphics(multiplexer_here(), terminal_name_here().as_deref());
    let mut text = String::from("\n");
    for line in crate::ui::ART {
        text.push_str(&format!("  \x1b[38;5;141m{line}\x1b[0m\n"));
    }
    text.push_str(&format!("\n{why}\n\n"));
    for (name, url) in TERMINALS {
        text.push_str(&format!("    {name:<9} {url}\n"));
    }
    text.push_str(
        "\nkb doctor checks your setup. If you're sure the terminal can draw\n\
         kitty graphics, KB_FORCE=1 kb starts it anyway.\n\n",
    );
    text
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

    // The name is only to show: whether it can draw is asked of it next.
    let name = terminal_name_here().unwrap_or_else(|| "not one kb knows by name".to_owned());
    match multiplexer_here() {
        Some(multiplexer) => report.note("terminal", &format!("{name}, through {multiplexer}")),
        None => report.note("terminal", &name),
    }
    let terminal = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    let draws = crate::term::draws_graphics(ASK_DOCTOR);
    match (draws, multiplexer_here()) {
        (Some(true), _) => report.ok("graphics", "the terminal draws kitty graphics"),
        (Some(false), Some(name)) => {
            report.fail("graphics", &format!("{name} doesn't pass kitty graphics on; run kb outside it"))
        }
        (Some(false), None) => {
            report.fail("graphics", &format!("the terminal doesn't draw kitty graphics; use {}", terminal_list()))
        }
        (None, _) if !terminal => {
            report.note("graphics", "not asked: this isn't a terminal; run kb doctor in the pane kb will use")
        }
        (None, _) => report
            .note("graphics", &format!("the terminal didn't answer in {} s (a very slow link?)", ASK_DOCTOR.as_secs())),
    }

    match crate::engine::chrome::find_binary() {
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

    report.note("agent skill", &crate::skill::status().unwrap_or_else(|error| error));
    report.note(
        "mcp",
        if crate::mcp::enabled() { "on (kb mcp off turns it off)" } else { "off (kb mcp on turns it on)" },
    );

    println!();
    if report.problems == 0 && draws.is_none() {
        println!("Nothing is missing, but the terminal couldn't be asked about graphics.");
        0
    } else if report.problems == 0 {
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

    #[test]
    fn multiplexers() {
        assert_eq!(multiplexer(true, false, "xterm-kitty"), Some("tmux"));
        assert_eq!(multiplexer(false, false, "tmux-256color"), Some("tmux"));
        assert_eq!(multiplexer(false, true, "xterm-ghostty"), Some("Zellij"));
        assert_eq!(multiplexer(false, false, "screen-256color"), Some("tmux or screen"));
        assert_eq!(multiplexer(false, false, "xterm-ghostty"), None);
    }

    #[test]
    fn terminal_names() {
        assert_eq!(terminal_name(Some("Apple_Terminal"), "xterm-256color", false).as_deref(), Some("Terminal"));
        assert_eq!(terminal_name(Some("ghostty"), "xterm-ghostty", false).as_deref(), Some("Ghostty"));
        assert_eq!(terminal_name(None, "xterm-ghostty", true).as_deref(), Some("Ghostty"));
        assert_eq!(terminal_name(None, "xterm-256color", true).as_deref(), Some("Rex"));
        assert_eq!(terminal_name(None, "xterm-256color", false), None);
    }

    #[test]
    fn says_why_without_blaming_a_terminal_that_works() {
        assert!(why_no_graphics(None, Some("Terminal")).contains("and Terminal doesn't support it"));
        assert!(why_no_graphics(None, None).contains("and this terminal doesn't"));
        // Ghostty's name, passed on through something kb didn't recognise.
        assert!(why_no_graphics(None, Some("Ghostty")).contains("and this terminal doesn't"));
        assert!(why_no_graphics(Some("Zellij"), Some("Ghostty")).contains("inside Zellij,"));
        assert_eq!(terminal_list(), "Ghostty, kitty, WezTerm or Rex");
    }
}
