//! `kb mcp`: the same commands as `kb ctl`, offered to agents as a Model
//! Context Protocol server over stdin and stdout.
//!
//! The server is off until `kb mcp on`: registered or not, it offers no
//! tools while off, so nothing can drive the browser this way unasked.
//! `kb mcp serve -i NAME` drives that kb only, never the user's own.

use crate::commands::{self, COMMANDS};
use crate::control::{self, Output};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const USAGE: &str = "\
usage: kb mcp <on|off|status|serve [-i NAME]>

  on      let the MCP server offer its tools, and show how to register it
  off     stop offering tools (the default)
  status  say whether it is on
  serve   run the server on stdin/stdout; this is what an MCP client starts.
          With -i NAME (or a number or id) every tool drives that kb only,
          such as one started with kb --headless --name NAME; without it,
          the lowest-numbered kb, usually the one in the user's pane";

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
                 claude mcp add kb -- {kb} mcp serve -i NAME"
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
        Some("serve") => match &words[1..] {
            [] => serve(),
            [flag, which] if flag == "-i" || flag == "--instance" => {
                std::env::set_var("KB_INSTANCE", which);
                serve()
            }
            _ => Err(format!("serve takes -i NAME, or nothing\n{USAGE}")),
        },
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

fn serve() -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => answer(&message),
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
fn answer(message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let result = match message["method"].as_str().unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kb", "version": env!("CARGO_PKG_VERSION")},
            "instructions": instructions(),
        })),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = if enabled() { COMMANDS.iter().map(|c| c.tool()).collect() } else { Vec::new() };
            Ok(json!({"tools": tools}))
        }
        "tools/call" => Ok(call(&message["params"])),
        other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

/// What the model is told the tools act on: the kb this server is bound to,
/// or the user's.
fn instructions() -> String {
    if !enabled() {
        return OFF.to_owned();
    }
    match std::env::var("KB_INSTANCE") {
        Ok(which) => format!(
            "Controls the kb browser called {which}: a real Chrome with its own profile, so sites it is signed in to \
             stay signed in. It may be headless, with nobody watching. Every tool acts on that kb only. It must \
             already be running (kb --headless --name {which} starts it)."
        ),
        Err(_) => "Controls the kb browser running in the user's terminal pane. What these tools do happens on \
                   the page the user is looking at. kb must already be running."
            .to_owned(),
    }
}

/// Run a tool. Failures are results the model can read, not protocol errors.
fn call(params: &Value) -> Value {
    let failed = |text: &str| json!({"content": [{"type": "text", "text": text}], "isError": true});
    if !enabled() {
        return failed(OFF);
    }
    let name = params["name"].as_str().unwrap_or("");
    let Some(command) = commands::find(name) else {
        return failed(&format!("unknown tool {name:?}"));
    };
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
