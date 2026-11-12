//! `kb skill`: install the agent skill that teaches coding agents to open kb
//! in a pane beside them and test their work in it.
//!
//! The skill lives in skill/SKILL.md and is built into the binary, so the
//! installed copy matches the kb that installed it. Install again after
//! upgrading kb.

use std::path::PathBuf;

const SKILL: &str = include_str!("../skill/SKILL.md");

const USAGE: &str = "\
usage: kb skill <install|remove|status|show>

  install  put the skill where Claude Code finds it (~/.claude/skills/kb)
  remove   take it out again
  status   say whether it is installed, and whether it is this kb's version
  show     print the skill";

fn path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".claude/skills/kb/SKILL.md"))
}

/// Run `kb skill` with the words after it. Returns the exit status.
pub fn run(words: &[String]) -> i32 {
    let result = match words.first().map(String::as_str) {
        Some("install") => path().and_then(|path| {
            let before = std::fs::read_to_string(&path).ok();
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
            }
            std::fs::write(&path, SKILL).map_err(|e| format!("writing {}: {e}", path.display()))?;
            let did = match before.as_deref() {
                None => "installed",
                Some(text) if text == SKILL => "already up to date",
                Some(_) => "updated",
            };
            println!("kb skill {did}: {}\nAgents started from now on can use it.", path.display());
            Ok(())
        }),
        Some("remove") => path().and_then(|path| {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    if let Some(folder) = path.parent() {
                        let _ = std::fs::remove_dir(folder);
                    }
                    println!("kb skill removed.");
                }
                Err(_) => println!("kb skill isn't installed."),
            }
            Ok(())
        }),
        Some("status") => path().map(|path| match std::fs::read_to_string(&path) {
            Ok(text) if text == SKILL => println!("kb skill is installed: {}", path.display()),
            Ok(_) => println!("kb skill is installed but out of date: kb skill install updates it"),
            Err(_) => println!("kb skill isn't installed: kb skill install"),
        }),
        Some("show") => {
            print!("{SKILL}");
            Ok(())
        }
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown skill command {other:?}\n{USAGE}")),
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("kb skill: {error}");
            1
        }
    }
}
