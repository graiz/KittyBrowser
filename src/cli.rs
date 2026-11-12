//! `kb ctl`: give one command to the running kb and print its answer.

use crate::commands::{self, COMMANDS};
use crate::control::{self, Output};
use serde_json::json;
use std::path::PathBuf;

fn usage() -> String {
    let mut text = String::from(
        "usage: kb ctl <command> [arguments]\n\n\
         Controls the kb browser running in a terminal pane: what you do here\n\
         happens on the page the user is looking at. With several kbs running,\n\
         kb ctl -i N <command> picks one (kb list numbers them); the default is\n\
         the lowest-numbered.\n\n",
    );
    for command in COMMANDS {
        text.push_str(&format!("  {}\n      {}\n", command.synopsis(), command.help));
    }
    text.push_str(
        "\nA <selector> is a CSS selector (the first match is used; it also finds\n\
         elements inside shadow DOM, and a >>> b picks b inside a's shadow root),\n\
         or text=Some words for the element showing that text, or @N for link N\n\
         from kb ctl links, or :focus for whatever has focus.\n\
         kb ctl <command> --help describes one command and its arguments.\n\
         Exit status: 0 on success; 1 if the command failed, with the reason on\n\
         stderr; 2 if the command line was wrong. A command that worked but\n\
         found errors on the page (open, reload, click) still exits 0 and says\n\
         so in its output; in a script, follow it with kb ctl check.",
    );
    text
}

/// Whether an address that isn't an existing file was still meant as one,
/// rather than as a host name or a search.
fn looks_like_a_file(address: &str) -> bool {
    let lower = address.to_ascii_lowercase();
    if address.contains("://") {
        return false;
    }
    if address.starts_with(['.', '/', '~']) {
        return true;
    }
    // "site.com/page.html" and "localhost:8000/page.html" are a host and a
    // path; "pages/about.html" and "about.html" are files.
    let host_like = match address.split_once('/') {
        Some((first, _)) => first.contains(['.', ':']) || first == "localhost",
        None => false,
    };
    !host_like && [".html", ".htm", ".xhtml", ".svg", ".pdf"].iter().any(|ending| lower.ends_with(ending))
}

/// Run `kb ctl` with the words after it. Returns the exit status.
pub fn run(mut words: &[String]) -> i32 {
    // Which kb, when several are running.
    if let [flag, number, rest @ ..] = words {
        if flag == "-i" || flag == "--instance" {
            std::env::set_var("KB_INSTANCE", number);
            words = rest;
        }
    }
    let Some(name) = words.first().filter(|word| !matches!(word.as_str(), "-h" | "--help" | "help")) else {
        println!("{}", usage());
        return 0;
    };
    let Some(command) = commands::find(name) else {
        let names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        eprintln!("kb ctl: unknown command {name:?}\ncommands: {}\nsee: kb ctl --help", names.join(", "));
        return 2;
    };

    let words = &words[1..];
    if matches!(words, [word] if word == "--help" || word == "-h") {
        println!("{}", command.describe());
        return 0;
    }
    let mut args = match command.parse(words) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("kb ctl: {error}");
            return 2;
        }
    };
    // Where to save an image is ours to deal with, not the browser's.
    let out = args.as_object_mut().and_then(|args| args.remove(commands::OUT));
    let out = out.as_ref().and_then(|out| out.as_str()).map(PathBuf::from);
    // kb's working directory isn't ours, so a relative file path has to be
    // made absolute here.
    if let Some(url) = args["url"].as_str() {
        match std::fs::canonicalize(url) {
            Ok(path) => args["url"] = json!(format!("file://{}", path.display())),
            Err(_) if looks_like_a_file(url) => {
                let path = std::env::current_dir().unwrap_or_default().join(url);
                eprintln!("kb ctl: no such file: {}", path.display());
                return 1;
            }
            Err(_) => {}
        }
    }

    match control::request(command.name, args) {
        Ok(Output::Text(text)) => {
            println!("{text}");
            0
        }
        Ok(Output::Image(png)) => {
            let path = out.unwrap_or_else(|| {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_millis());
                std::env::temp_dir().join(format!("kb-screenshot-{stamp}.png"))
            });
            let saved = crate::base64::decode(&png)
                .ok_or("kb sent a damaged image".to_owned())
                .and_then(|bytes| {
                    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
                        let _ = std::fs::create_dir_all(folder);
                    }
                    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
                });
            match saved {
                Ok(()) => {
                    println!("{}", path.display());
                    0
                }
                Err(error) => {
                    eprintln!("kb ctl: {error}");
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("kb ctl: {error}");
            1
        }
    }
}

/// `kb list`: the kbs running now, one per line, numbered. Returns the
/// exit status.
pub fn list() -> i32 {
    let running = control::running();
    if running.is_empty() {
        eprintln!("kb: {}", control::NOT_RUNNING);
        return 1;
    }
    for instance in running {
        std::env::set_var("KB_INSTANCE", instance.to_string());
        let (page, pane) = match control::request("status", json!({})) {
            Ok(Output::Text(status)) => {
                let field = |name: &str| status.lines().find_map(|line| line.strip_prefix(name)).unwrap_or("").to_owned();
                let url = field("url: ");
                (if url.is_empty() { "about:blank".to_owned() } else { url }, field("pane: "))
            }
            _ => ("(not answering)".to_owned(), String::new()),
        };
        // The pane tells kbs apart: an agent knows the one it split.
        println!("{instance}\t{page}\t{pane}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_and_hosts() {
        assert!(looks_like_a_file("nothere.html"));
        assert!(looks_like_a_file("./site"));
        assert!(looks_like_a_file("/tmp/x"));
        assert!(looks_like_a_file("pages/about.html"));
        assert!(!looks_like_a_file("127.0.0.1:49844/index.html"));
        assert!(!looks_like_a_file("example.com/index.html"));
        assert!(!looks_like_a_file("example.com"));
        assert!(!looks_like_a_file("localhost:3000"));
        assert!(!looks_like_a_file("https://example.com/index.html"));
    }
}
