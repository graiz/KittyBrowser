//! kb: a web browser for terminal panes, drawn with the kitty graphics protocol.
//!
//! Outline only. See README.md for the plan and docs/ARCHITECTURE.md for the
//! design of each module.

mod app;
mod engine;
mod term;
mod ui;

fn main() {
    let url = match std::env::args().nth(1) {
        Some(url) => url,
        None => {
            eprintln!("usage: kb URL");
            std::process::exit(2);
        }
    };
    if let Err(err) = app::run(&url) {
        eprintln!("kb: {err}");
        std::process::exit(1);
    }
}
