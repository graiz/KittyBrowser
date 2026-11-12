//! The commands an agent can give a running kb. One table drives the `kb ctl`
//! argument parser, its help text, and the MCP tool list, so the two
//! interfaces can't drift apart.

use serde_json::{json, Map, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Given by position. The last one takes the rest of the command line.
    Positional,
    /// `--name value`
    Option,
    /// `--name`
    Flag,
}

pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// For an option, what its value is called in the help: `--timeout SECONDS`.
    pub value: &'static str,
    pub help: &'static str,
}

pub struct Command {
    pub name: &'static str,
    pub help: &'static str,
    pub params: &'static [Param],
}

const fn required(name: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Positional, required: true, value: "", help }
}

const fn optional(name: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Positional, required: false, value: "", help }
}

const fn option(name: &'static str, value: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Option, required: false, value, help }
}

const fn flag(name: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Flag, required: false, value: "", help }
}

const SELECTOR: &str = "CSS selector (first match), text=Words, @N from links, or :focus";

pub const COMMANDS: &[Command] = &[
    Command {
        name: "status",
        help: "Show the page's URL and title, whether it is loading, the viewport size, and counts of console errors and failed requests.",
        params: &[],
    },
    Command {
        name: "open",
        help: "Go to a URL or a local HTML file, wait for it to load and go quiet, and report console errors and failed requests so far. Errors that come later (a timer, a slow fetch) show in status. Local pages (files, localhost, private addresses) are never served from the cache.",
        params: &[required("url", "URL, host name, or path to a local file")],
    },
    Command {
        name: "reload",
        help: "Reload the page from the network, ignoring the cache, wait for it to load and go quiet, and report console errors and failed requests so far. The scroll position is kept.",
        params: &[],
    },
    Command {
        name: "check",
        help: "Test that the page is healthy, for scripts: waits for the page to go quiet, then succeeds if there are no console errors, no failed requests and the page itself wasn't an error response. Otherwise lists what is wrong and fails (exit status 1).",
        params: &[],
    },
    Command {
        name: "viewport",
        help: "Lay the page out at a given size instead of the pane's, to test other widths and media queries, or to get a site's desktop layout in a small pane. If it is bigger than the pane it is shown shrunk to fit. Stays until kb ctl viewport reset.",
        params: &[required("size", "WIDTHxHEIGHT in CSS pixels (such as 1280x800), a width alone, or reset")],
    },
    Command { name: "back", help: "Go back one page in history.", params: &[] },
    Command { name: "forward", help: "Go forward one page in history.", params: &[] },
    Command {
        name: "screenshot",
        help: "Take a PNG screenshot of what the pane shows, of one element, or of the whole page. The image is in device pixels: the viewport size times the scale that status reports.",
        params: &[
            option("selector", "CSS", "capture only this element"),
            flag("full", "capture the whole page, not just the visible part"),
            option(OUT, "FILE", "where to save the PNG (default: a new file in the temp directory); its path is printed"),
        ],
    },
    Command {
        name: "console",
        help: "Show the current page's console messages and uncaught exceptions, with the file and line each came from, and any alert, confirm or prompt dialogs with how they were answered.",
        params: &[
            flag("errors", "only errors"),
            flag("all", "also show messages from pages visited earlier, under a line naming each page"),
        ],
    },
    Command {
        name: "network",
        help: "List the requests the page has made since it loaded, with status codes; ones answered from the cache are marked. The browser's own request for /favicon.ico is listed but never counted as failed.",
        params: &[flag("failed", "only requests that failed or returned 400 and up")],
    },
    Command {
        name: "eval",
        help: "Evaluate JavaScript in the page and print the result: strings as they are, other values as JSON, elements as <tag#id>. A promise is awaited and its value printed; top-level await works. It runs as at the console, so a declaration can clash with the window's own names (top, name, status): wrap code in (() => { ... })() to avoid that.",
        params: &[required("expression", "JavaScript expression")],
    },
    Command {
        name: "text",
        help: "Print the text of the page's main content (its <main> or <article>, if it has one), or of one element, as rendered, including text in shadow DOM. Elements hidden with CSS are left out, but text that is off screen or the colour of its background is not. For a text field or select, prints its value. On a long page, find or outline are usually better first steps.",
        params: &[
            optional("selector", SELECTOR),
            option("section", "HEADING", "print only what is under the heading with this text (in the element, if one is given)"),
            flag("all", "print every element the selector matches, separated by ---"),
        ],
    },
    Command {
        name: "html",
        help: "Print the HTML of the page as it is now, or of one element.",
        params: &[optional("selector", SELECTOR)],
    },
    Command {
        name: "outline",
        help: "Show the page's title and its headings, indented by level: a table of contents to decide what to read with text --section. Headings outside the main content are marked with where they are.",
        params: &[],
    },
    Command {
        name: "links",
        help: "List the visible links, buttons, tabs and menu items in the main content (or in the elements a selector matches), numbered, with their text and where they go. Click one with kb ctl click @N. Each is marked if it sits in a nav, aside, header or footer; links that go to the same place are listed once; a search engine's redirect is shown with where it really goes. The numbers last until the next links command, and stop working rather than pointing elsewhere if the page is replaced.",
        params: &[
            optional("selector", "CSS selector of the parts of the page to list; body for all of it"),
            option("match", "TEXT", "list only those whose text or address contains this"),
            option("limit", "N", "list at most this many (default 60)"),
        ],
    },
    Command {
        name: "tables",
        help: "List the page's tables with their size, the heading above each, and their first row; or, given a number, print that table with its cells separated by tabs.",
        params: &[optional("number", "which table to print, from the list"), option("limit", "N", "print at most this many rows (default 200)")],
    },
    Command {
        name: "images",
        help: "List the visible images in the main content (or in an element): alt text, shown and file size, caption, source and where each links to. To see one, use screenshot --selector.",
        params: &[optional("selector", "CSS selector of the part of the page to look in; body for all of it")],
    },
    Command {
        name: "find",
        help: "Find visible text on the page: lists each place it appears with the words around it and the heading it falls under, and scrolls the window to the first. Not case sensitive.",
        params: &[required("text", "the words to look for"), option("limit", "N", "show at most this many places (default 10)")],
    },
    Command {
        name: "style",
        help: "Print an element's computed CSS: the property named, or the common ones (layout, box, colors, font). For where the element actually ended up, see box.",
        params: &[required("selector", SELECTOR), optional("property", "CSS property name, such as color or margin-top")],
    },
    Command {
        name: "box",
        help: "Print where an element is and whether it can be seen: its size, its position on the page and in the window, whether it is rendered, whether it is within the window, and what covers it if something does.",
        params: &[required("selector", SELECTOR)],
    },
    Command {
        name: "scroll",
        help: "Scroll the page: to top, to bottom, to a number of pixels from the top, or until an element is in the middle of the window. Waits up to a second and says if the page grew, as pages that load more as you scroll do.",
        params: &[required("to", "top, bottom, a number of pixels, or a CSS selector")],
    },
    Command {
        name: "click",
        help: "Click the middle of an element with the mouse, scrolling it into view first. Fails if the element is off screen or something else covers it. Answers once the page has settled: reports any console errors and dialogs the click caused and a change of address, and if it led to another page, waits for that page to load. The mouse stays there, so :hover styles apply until it moves.",
        params: &[required("selector", SELECTOR)],
    },
    Command {
        name: "hover",
        help: "Move the mouse over the middle of an element, scrolling it into view first, so that :hover styles and mouseover handlers apply. It stays there until another hover or click.",
        params: &[required("selector", SELECTOR)],
    },
    Command {
        name: "dialog",
        help: "Choose how alert, confirm and prompt dialogs are answered from now on. They are answered at once, since an open dialog freezes the page: by default accepted, with a prompt's default text. Each one is recorded in console and reported by the command that caused it.",
        params: &[
            required("answer", "accept or dismiss"),
            option("text", "TEXT", "what to type into prompts when accepting (default: the prompt's own default)"),
        ],
    },
    Command {
        name: "type",
        help: "Focus a text field and type into it (with :focus as the selector, type into whatever has focus). The text is added to what is already there unless --clear is given. Fails if the field is off screen or covered, since a user couldn't click into it. Reports any console errors the typing causes. For a <select> use select; for a checkbox or radio button use click.",
        params: &[
            required("selector", SELECTOR),
            required("text", "text to type"),
            flag("clear", "replace the field's contents rather than adding to them"),
        ],
    },
    Command {
        name: "select",
        help: "Choose an option of a <select>, by its value or its label, as a user would: the page gets input and change events. Reports any console errors it causes.",
        params: &[required("selector", SELECTOR), required("option", "the option's value or label")],
    },
    Command {
        name: "press",
        help: "Press a key. It goes to whatever has focus (click or type into an element first). Keys: Enter, Tab, Escape, Backspace, Delete, Space, ArrowLeft, ArrowRight, ArrowUp, ArrowDown, Home, End, PageUp, PageDown, or one character. Modifiers: Ctrl+, Alt+, Shift+, as in Ctrl+a or Shift+Tab. There is no Cmd or Meta; Ctrl+a selects all. PageDown, PageUp, Home and End scroll the page when no field has focus; arrow keys don't change a <select> (use select).",
        params: &[required("key", "key name")],
    },
    Command {
        name: "wait",
        help: "Wait until an element matching a selector exists, or until one of the matching elements contains some text, or until none is left. Keeps waiting across page loads.",
        params: &[
            required("selector", "CSS selector; any match counts"),
            option("text", "TEXT", "wait until a matching element's text (or a field's value) contains this"),
            flag("gone", "wait until nothing matches the selector"),
            option("count", "N", "wait until at least N elements match"),
            option("timeout", "SECONDS", "how long to wait before failing (default 10, at most 60)"),
        ],
    },
];

/// The one parameter that `kb ctl` handles itself and never sends on:
/// over MCP a screenshot comes back as an image, not a file.
pub const OUT: &str = "out";

pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

impl Command {
    /// How the command is written on the command line, e.g.
    /// `wait <selector> [--timeout N]`.
    pub fn synopsis(&self) -> String {
        let mut text = self.name.to_owned();
        for param in self.params {
            let shown = match param.kind {
                Kind::Positional => format!("<{}>", param.name),
                Kind::Option => format!("--{} {}", param.name, param.value),
                Kind::Flag => format!("--{}", param.name),
            };
            text.push_str(&if param.required { format!(" {shown}") } else { format!(" [{shown}]") });
        }
        text
    }

    /// The command's full help: synopsis, description, and each argument.
    pub fn describe(&self) -> String {
        let mut text = format!("usage: kb ctl {}\n\n{}\n", self.synopsis(), self.help);
        for param in self.params {
            let shown = match param.kind {
                Kind::Positional => format!("<{}>", param.name),
                Kind::Option => format!("--{} {}", param.name, param.value),
                Kind::Flag => format!("--{}", param.name),
            };
            text.push_str(&format!("\n  {shown:<20} {}", param.help));
        }
        text
    }

    /// Turn command-line words into the command's arguments.
    pub fn parse(&self, words: &[String]) -> Result<Value, String> {
        let mut args = Map::new();
        let mut positional = Vec::new();
        let mut words = words.iter();
        while let Some(word) = words.next() {
            let (name, inline) = match word.strip_prefix("--") {
                Some(rest) => match rest.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (rest, None),
                },
                None => ("", None),
            };
            // Only names this command knows are options; anything else,
            // dashes and all, is taken as given.
            match self.params.iter().find(|p| p.kind != Kind::Positional && p.name == name) {
                Some(param) if param.kind == Kind::Flag => {
                    args.insert(param.name.to_owned(), json!(true));
                }
                Some(param) => {
                    let value = match inline {
                        Some(value) => value.to_owned(),
                        None => words.next().ok_or(format!("--{} needs a value", param.name))?.clone(),
                    };
                    args.insert(param.name.to_owned(), json!(value));
                }
                None => positional.push(word.as_str()),
            }
        }
        let slots: Vec<&Param> = self.params.iter().filter(|p| p.kind == Kind::Positional).collect();
        for (index, param) in slots.iter().enumerate() {
            let last = index + 1 == slots.len();
            let value = match positional.get(index..) {
                Some([_, rest @ ..]) if last && !rest.is_empty() => positional[index..].join(" "),
                Some([first, ..]) => (*first).to_owned(),
                _ if param.required => {
                    return Err(format!("{} needs <{}>\nusage: kb ctl {}", self.name, param.name, self.synopsis()))
                }
                _ => continue,
            };
            args.insert(param.name.to_owned(), json!(value));
        }
        if let (Some(extra), true) = (positional.first(), slots.is_empty()) {
            let problem = match extra.starts_with("--") {
                true => format!("{} has no option {extra}", self.name),
                false => format!("{} takes no arguments", self.name),
            };
            return Err(format!("{problem}\nusage: kb ctl {}", self.synopsis()));
        }
        Ok(Value::Object(args))
    }

    /// The command as an MCP tool definition.
    pub fn tool(&self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for param in self.params.iter().filter(|param| param.name != OUT) {
            let kind = if param.kind == Kind::Flag { "boolean" } else { "string" };
            properties.insert(param.name.to_owned(), json!({"type": kind, "description": param.help}));
            if param.required {
                required.push(param.name);
            }
        }
        json!({
            "name": self.name,
            "description": self.help,
            "inputSchema": {"type": "object", "properties": properties, "required": required},
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Value, String> {
        let words: Vec<String> = line.split(' ').map(str::to_owned).collect();
        find(&words[0]).unwrap().parse(&words[1..])
    }

    #[test]
    fn positionals_options_and_flags() {
        assert_eq!(parse("status").unwrap(), json!({}));
        assert_eq!(parse("open example.com").unwrap(), json!({"url": "example.com"}));
        assert_eq!(parse("console --errors").unwrap(), json!({"errors": true}));
        assert_eq!(
            parse("wait #done --timeout 3").unwrap(),
            json!({"selector": "#done", "timeout": "3"})
        );
        assert_eq!(parse("wait --timeout=3 #done").unwrap(), json!({"selector": "#done", "timeout": "3"}));
        assert_eq!(parse("screenshot --selector h1 --full").unwrap(), json!({"selector": "h1", "full": true}));
    }

    #[test]
    fn the_last_positional_takes_the_rest() {
        assert_eq!(parse("eval 1 + 1").unwrap(), json!({"expression": "1 + 1"}));
        assert_eq!(parse("type #name Ada Lovelace").unwrap(), json!({"selector": "#name", "text": "Ada Lovelace"}));
        // Unknown dashed words belong to the expression.
        assert_eq!(parse("eval x --y").unwrap(), json!({"expression": "x --y"}));
    }

    #[test]
    fn missing_and_extra_arguments() {
        assert!(parse("open").unwrap_err().contains("needs <url>"));
        assert!(parse("type #name").unwrap_err().contains("needs <text>"));
        assert!(parse("wait #x --timeout").unwrap_err().contains("needs a value"));
        assert!(parse("reload now").unwrap_err().contains("takes no arguments"));
        assert!(parse("screenshot --bogus").unwrap_err().contains("has no option --bogus"));
        assert_eq!(parse("text").unwrap(), json!({}));
    }

    #[test]
    fn synopsis_and_tool() {
        let wait = find("wait").unwrap();
        assert_eq!(wait.synopsis(), "wait <selector> [--text TEXT] [--gone] [--count N] [--timeout SECONDS]");
        assert!(wait.describe().contains("--timeout SECONDS    how long to wait"));
        // Saving to a file is the command line's business only.
        let shot = find("screenshot").unwrap();
        assert!(shot.synopsis().contains("[--out FILE]"));
        assert!(shot.tool()["inputSchema"]["properties"].get("out").is_none());
        let tool = wait.tool();
        assert_eq!(tool["inputSchema"]["required"], json!(["selector"]));
        assert_eq!(tool["inputSchema"]["properties"]["timeout"]["type"], "string");
        assert_eq!(find("console").unwrap().tool()["inputSchema"]["properties"]["errors"]["type"], "boolean");
    }
}
