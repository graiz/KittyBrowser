//! kb: a web browser for terminal panes, drawn with the kitty graphics protocol.
//!
//! See README.md for what it does and docs/ARCHITECTURE.md for the design of
//! each module.

mod app;
mod base64;
mod cli;
mod commands;
mod control;
mod devtools;
mod engine;
mod history;
mod mcp;
mod term;
mod ui;

const USAGE: &str = "\
usage: kb [--scale N] [URL]
       kb ctl <command>    control the running kb; see kb ctl --help
       kb mcp <command>    the MCP server for agents; see kb mcp --help

  --scale N   device pixels per CSS pixel (default 2 on macOS, 1 elsewhere)

keys:
  Ctrl+L          edit the address
  Enter / Esc     go to the address / cancel the edit
  Right / End     accept the suggested completion (Backspace rejects it)
  Alt+Left/Right  back / forward
  Ctrl+R          reload
  Ctrl+Q          quit
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
    match words.first().map(String::as_str) {
        Some("ctl") => std::process::exit(cli::run(&words[1..])),
        Some("mcp") => std::process::exit(mcp::run(&words[1..])),
        _ => {}
    }

    let mut options = app::Options {
        url: None,
        scale: if cfg!(target_os = "macos") { 2.0 } else { 1.0 },
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
