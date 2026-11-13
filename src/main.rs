//! kb: a web browser for terminal panes, drawn with the kitty graphics protocol.
//!
//! See README.md for what it does and docs/ARCHITECTURE.md for the design of
//! each module.

mod app;
mod base64;
mod bench;
mod cli;
mod commands;
mod control;
mod devtools;
mod doctor;
mod engine;
mod history;
mod keys;
mod mcp;
mod skill;
mod splash;
mod term;
mod ui;

const USAGE: &str = "\
usage: kb [--scale N] [--dark | --light] [--devtools] [URL]
       kb list             the kbs running now, numbered
       kb doctor           check this terminal and machine can run kb, and what to fix
       kb keys             the key bindings, and how to change them
       kb ctl <command>    control a running kb; see kb ctl --help
       kb mcp <command>    the MCP server for agents; see kb mcp --help
       kb skill install    teach coding agents to preview their work in kb
       kb bench [URL]      time each way of getting frames to this terminal

  --scale N        device pixels per CSS pixel (default 2 on macOS, 1 elsewhere)
  --dark, --light  tell pages which colour scheme you prefer (default: follow
                   the terminal's background)
  --devtools       serve Chrome DevTools on a local port, so F12 can open it;
                   any program on this machine can then reach the browser

keys (kb keys lists them all; Ctrl+/ in kb shows them):
  Cmd+L or Ctrl+L   edit the address
  Cmd+F or Ctrl+F   find on the page
  Cmd+C or Ctrl+C   copy the selection
  Ctrl+[ / Ctrl+]   back / forward (also Alt+Left / Alt+Right, the mouse's buttons)
  Cmd+R or Ctrl+R   reload
  Ctrl+Shift+C      inspect an element
  Ctrl+Q            quit (the ✕ at the right of the bar asks first)
Everything else, and the mouse, goes to the page.";

fn usage_error(message: &str) -> ! {
    eprintln!("kb: {message}\n{USAGE}");
    std::process::exit(2);
}

/// Where kb keeps its profile, history, and control socket.
pub fn data_dir() -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let dir = std::path::PathBuf::from(home).join(".kittybrowser");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    Ok(dir)
}

fn main() {
    let words: Vec<String> = std::env::args().skip(1).collect();
    // The subcommands print and exit, so a reader that stops early (kb list
    // | head) should end them quietly, as it would any Unix tool. Not the
    // browser: it writes to Chrome's pipe, and must outlive Chrome closing it.
    if matches!(words.first().map(String::as_str), Some("ctl" | "list" | "keys" | "skill" | "mcp" | "doctor")) {
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    match words.first().map(String::as_str) {
        Some("ctl") => std::process::exit(cli::run(&words[1..])),
        Some("mcp") => std::process::exit(mcp::run(&words[1..])),
        Some("list") => std::process::exit(cli::list()),
        Some("doctor") => std::process::exit(doctor::run()),
        Some("bench") => std::process::exit(bench::run(&words[1..])),
        Some("skill") => std::process::exit(skill::run(&words[1..])),
        Some("keys") => {
            keys::print(data_dir().ok().map(|dir| dir.join("keys")));
            return;
        }
        _ => {}
    }

    let mut options = app::Options {
        url: None,
        scale: if cfg!(target_os = "macos") { 2.0 } else { 1.0 },
        devtools: false,
        dark: None,
    };
    let mut address = Vec::new();
    let mut args = words.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("kb {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--devtools" => options.devtools = true,
            "--dark" => options.dark = Some(true),
            "--light" => options.dark = Some(false),
            "--scale" => {
                let scale = args.next().and_then(|s| s.parse::<f64>().ok());
                match scale.filter(|s| s.is_finite() && *s > 0.0) {
                    Some(scale) => options.scale = scale,
                    None => usage_error("--scale needs a positive number"),
                }
            }
            flag if flag.starts_with('-') => usage_error(&format!("unknown option {flag}")),
            _ => address.push(arg),
        }
    }
    // Several words are one search, so it needn't be quoted.
    if !address.is_empty() {
        options.url = Some(address.join(" "));
    }
    if let Err(err) = app::run(options) {
        eprintln!("kb: {err}");
        std::process::exit(1);
    }
}
