//! `kb mcp`: the same commands as `kb ctl`, offered to agents as a Model
//! Context Protocol server over stdin and stdout.
//!
//! The server is off until `kb mcp on`: registered or not, it offers no
//! tools while off, so nothing can drive the browser this way unasked.
//! `kb mcp serve -i NAME` drives that kb only, never the user's own, and
//! `--read-only` and `--tools` narrow which tools it offers.

use crate::commands::{self, Command, Effect, COMMANDS};
use crate::control::{self, Output};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const USAGE: &str = "\
usage: kb mcp <on|off|status|serve [-i NAME] [--read-only] [--tools A,B,...]>

  on      let the MCP server offer its tools, and show how to register it
  off     stop offering tools (the default)
  status  say whether it is on
  serve   run the server on stdin/stdout; this is what an MCP client starts.
          With -i NAME (or a number or id) every tool drives that kb only,
          such as one started with kb --headless --name NAME; without it,
          the kb in the caller's Rex session, else the lowest-numbered kb,
          usually the one in the user's pane.
          --read-only offers only the tools that read (status, text,
          screenshot, storage...) and refuses the rest, even if named in
          --tools. --tools offers only the tools named, and fails to start
          if one isn't a tool (see: kb ctl --help). They limit the MCP
          tools only: they are no sandbox for an agent that can also run
          kb ctl";

const OFF: &str = "kb's MCP server is off. Turn it on with: kb mcp on";

/// Its existence is the switch.
fn switch() -> Result<PathBuf, String> {
    Ok(crate::data_dir()?.join("mcp-enabled"))
}

pub fn enabled() -> bool {
    switch().is_ok_and(|path| path.exists())
}

/// Run `kb mcp` with the words after it. Returns the exit status.
pub fn run(words: &[String]) -> i32 {
    let result = match words.first().map(String::as_str) {
        Some("on") => switch().and_then(|path| {
            std::fs::write(&path, "").map_err(|e| format!("writing {}: {e}", path.display()))?;
            let kb = std::env::current_exe().map_or("kb".to_owned(), |path| path.display().to_string());
            println!(
                "kb's MCP server is on.\nTo add it to Claude Code:\n  claude mcp add kb -- {kb} mcp serve\n\
                 To have it drive one kb only, started with kb --headless --name NAME:\n  \
                 claude mcp add kb -- {kb} mcp serve -i NAME\n\
                 To let it only read pages, never click, type or clear anything, add --read-only:\n  \
                 claude mcp add kb -- {kb} mcp serve -i NAME --read-only\n\
                 or name the tools it may use with --tools, such as --tools open,text,screenshot"
            );
            Ok(())
        }),
        Some("off") => switch().map(|path| {
            let _ = std::fs::remove_file(path);
            println!("kb's MCP server is off.");
        }),
        Some("status") => {
            println!("kb's MCP server is {}.", if enabled() { "on" } else { "off" });
            Ok(())
        }
        Some("serve") => {
            Offer::parse(&words[1..]).map_err(|error| format!("{error}\n{USAGE}")).and_then(|(instance, offer)| {
                if let Some(which) = instance {
                    std::env::set_var("KB_INSTANCE", which);
                }
                serve(&offer)
            })
        }
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown mcp command {other:?}\n{USAGE}")),
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("kb mcp: {error}");
            1
        }
    }
}

/// Which tools `kb mcp serve` offers. A client filters tools by name, but an
/// unattended agent on a signed-in profile is safer with a server that
/// can't do the rest at all.
#[derive(Default, Debug, PartialEq)]
struct Offer {
    read_only: bool,
    /// From --tools: only these, when given.
    tools: Option<Vec<&'static str>>,
}

impl Offer {
    /// The words after `serve`, in any order: which kb, and what to offer.
    fn parse(words: &[String]) -> Result<(Option<String>, Offer), String> {
        let mut instance = None;
        let mut offer = Offer::default();
        let mut words = words.iter();
        while let Some(word) = words.next() {
            let (flag, inline) = match word.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
                _ => (word.as_str(), None),
            };
            let mut value =
                |what: &str| inline.clone().or_else(|| words.next().cloned()).ok_or(format!("{flag} needs {what}"));
            match flag {
                "-i" | "--instance" => instance = Some(value("a kb's name, number or id")?),
                "--read-only" if inline.is_none() => offer.read_only = true,
                "--tools" => {
                    let mut tools = Vec::new();
                    for name in value("a list of tools, such as open,text")?.split(',').map(str::trim) {
                        match commands::find(name) {
                            Some(command) => tools.push(command.name),
                            None if name.is_empty() => {}
                            None => {
                                let names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
                                return Err(format!("no tool is called {name:?}; the tools are {}", names.join(", ")));
                            }
                        }
                    }
                    if tools.is_empty() {
                        return Err("--tools needs at least one tool, such as --tools open,text".to_owned());
                    }
                    offer.tools = Some(tools);
                }
                other => return Err(format!("serve takes -i NAME, --read-only and --tools, not {other:?}")),
            }
        }
        Ok((instance, offer))
    }

    /// Why this server won't run a command, if it won't. --read-only wins
    /// over --tools.
    fn refuses(&self, command: &Command) -> Option<String> {
        if self.read_only && command.effect != Effect::Reads {
            return Some(format!(
                "{} isn't offered: this kb MCP server was started with --read-only, which offers only the tools \
                 that read the page",
                command.name
            ));
        }
        match &self.tools {
            Some(tools) if !tools.contains(&command.name) => Some(format!(
                "{} isn't offered: this kb MCP server was started with --tools {}",
                command.name,
                tools.join(",")
            )),
            _ => None,
        }
    }

    fn tools(&self) -> Vec<Value> {
        COMMANDS.iter().filter(|command| self.refuses(command).is_none()).map(Command::tool).collect()
    }
}

fn serve(offer: &Offer) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => answer(&message, offer),
            Err(error) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": format!("parse error: {error}")},
            })),
        };
        if let Some(response) = response {
            writeln!(out, "{response}").and_then(|()| out.flush()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The response to one JSON-RPC message, or `None` for a notification.
fn answer(message: &Value, offer: &Offer) -> Option<Value> {
    let id = message.get("id")?.clone();
    let result = match message["method"].as_str().unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kb", "version": env!("CARGO_PKG_VERSION")},
            "instructions": instructions(offer),
        })),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = if enabled() { offer.tools() } else { Vec::new() };
            Ok(json!({"tools": tools}))
        }
        "tools/call" => Ok(call(&message["params"], offer)),
        other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

/// What the model is told the tools act on: the kb this server is bound to,
/// or the user's.
fn instructions(offer: &Offer) -> String {
    if !enabled() {
        return OFF.to_owned();
    }
    let bound = match std::env::var("KB_INSTANCE") {
        Ok(which) => format!(
            "Controls the kb browser called {which}: a real Chrome with its own profile, so sites it is signed in to \
             stay signed in. It may be headless, with nobody watching. Every tool acts on that kb only. It must \
             already be running (kb --headless --name {which} starts it)."
        ),
        Err(_) => "Controls the kb browser running in the user's terminal pane: in Rex, the one in this Rex \
                   session. What these tools do happens on the page the user is looking at. kb must already be \
                   running."
            .to_owned(),
    };
    match offer.read_only {
        true => format!(
            "{bound} This server offers only the tools that read the page: it can't open pages, click, type or \
             clear anything."
        ),
        false => bound,
    }
}

/// Run a tool. Failures are results the model can read, not protocol errors.
fn call(params: &Value, offer: &Offer) -> Value {
    let failed = |text: &str| json!({"content": [{"type": "text", "text": text}], "isError": true});
    if !enabled() {
        return failed(OFF);
    }
    let name = params["name"].as_str().unwrap_or("");
    let Some(command) = commands::find(name) else {
        return failed(&format!("unknown tool {name:?}"));
    };
    if let Some(refusal) = offer.refuses(command) {
        return failed(&refusal);
    }
    let mut args = match &params["arguments"] {
        Value::Null => json!({}),
        args => args.clone(),
    };
    // Relative paths are from where the client started this server.
    if let Err(error) = command.resolve_files(&mut args) {
        return failed(&error);
    }
    match control::request(command.name, args) {
        Ok(Output::Text(text)) => json!({"content": [{"type": "text", "text": text}]}),
        Ok(Output::Image(png)) => json!({"content": [{"type": "image", "data": png, "mimeType": "image/png"}]}),
        Err(error) => failed(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<(Option<String>, Offer), String> {
        let words: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        Offer::parse(&words)
    }

    fn names(offer: &Offer) -> Vec<String> {
        offer.tools().iter().map(|tool| tool["name"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn serve_takes_its_flags_in_any_order() {
        assert_eq!(parse("").unwrap(), (None, Offer::default()));
        let read_only = Offer { read_only: true, tools: None };
        assert_eq!(parse("-i bot --read-only").unwrap(), (Some("bot".to_owned()), read_only));
        let (instance, offer) = parse("--tools open,text --instance=bot --read-only").unwrap();
        assert_eq!(instance.as_deref(), Some("bot"));
        assert_eq!(offer, Offer { read_only: true, tools: Some(vec!["open", "text"]) });
        assert_eq!(parse("--tools=status,,box").unwrap().1.tools, Some(vec!["status", "box"]));
    }

    #[test]
    fn serve_refuses_what_it_doesnt_know() {
        assert!(parse("--tools open,nope").unwrap_err().contains("no tool is called \"nope\""));
        assert!(parse("--tools").unwrap_err().contains("--tools needs"));
        assert!(parse("--tools ,").unwrap_err().contains("at least one tool"));
        assert!(parse("-i").unwrap_err().contains("-i needs"));
        assert!(parse("--read-only=yes").is_err());
        assert!(parse("bot").unwrap_err().contains("not \"bot\""));
    }

    #[test]
    fn read_only_offers_only_reading_and_wins_over_tools() {
        let everything = names(&Offer::default());
        assert_eq!(everything.len(), COMMANDS.len());
        let read_only = Offer { read_only: true, tools: None };
        let offered = names(&read_only);
        assert!(offered.contains(&"storage".to_owned()) && offered.contains(&"text".to_owned()));
        for name in ["clear-storage", "quit", "eval", "click", "type", "open", "upload", "dialog"] {
            assert!(!offered.contains(&name.to_owned()), "{name}");
            assert!(read_only.refuses(commands::find(name).unwrap()).unwrap().contains("--read-only"));
        }
        assert!(read_only.tools().iter().all(|tool| tool["annotations"]["readOnlyHint"] == true));
        let both = Offer { read_only: true, tools: Some(vec!["text", "click"]) };
        assert_eq!(names(&both), ["text"]);
        assert!(both.refuses(commands::find("click").unwrap()).unwrap().contains("--read-only"));
        assert!(both.refuses(commands::find("status").unwrap()).unwrap().contains("--tools text,click"));
    }
}
