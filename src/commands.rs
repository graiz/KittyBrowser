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

/// What an argument's value is, for the MCP tool schema. On the command
/// line everything is a word; a list is the last positional, taking the
/// words that are left.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Text,
    Number,
    List,
}

pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub shape: Shape,
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
    Param { name, kind: Kind::Positional, shape: Shape::Text, required: true, value: "", help }
}

const fn optional(name: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Positional, shape: Shape::Text, required: false, value: "", help }
}

/// An optional positional that is a number.
const fn numbered(name: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Positional, shape: Shape::Number, required: false, value: "", help }
}

/// A required positional that takes the rest of the command line as a list.
const fn list(name: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Positional, shape: Shape::List, required: true, value: "", help }
}

const fn option(name: &'static str, value: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Option, shape: Shape::Text, required: false, value, help }
}

/// An option whose value is a number.
const fn number(name: &'static str, value: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Option, shape: Shape::Number, required: false, value, help }
}

const fn flag(name: &'static str, help: &'static str) -> Param {
    Param { name, kind: Kind::Flag, shape: Shape::Text, required: false, value: "", help }
}

const SELECTOR: &str =
    "CSS selector (first match; iframe >>> sel looks inside a frame), text=Words, @N from links, or :focus";

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
        params: &[option("ignore", "HOSTS", "comma-separated hosts (or parts of addresses) whose errors and failed requests don't count, such as third-party trackers")],
    },
    Command {
        name: "viewport",
        help: "Lay the page out at a given size instead of the pane's, to test other widths and media queries, or to get a site's desktop layout in a small pane. If it is bigger than the pane it is shown shrunk to fit. Stays until kb ctl viewport reset.",
        params: &[required(
                "size",
                "WIDTHxHEIGHT in CSS pixels (such as 1280x800), a width alone (as tall as fills the pane), or reset",
            )],
    },
    Command {
        name: "back",
        help: "Go back one page in history. In a window the page opened (a sign-in popup, a new tab) that has no earlier page, close it instead, showing the page that opened it: the way to dismiss a popup.",
        params: &[],
    },
    Command { name: "forward", help: "Go forward one page in history.", params: &[] },
    Command {
        name: "window",
        help: "List the windows open in this kb (the page, and any popup or new tab it opened), numbered, with their addresses and which one is shown; or, given a number, show that window in place of the one shown, closing none. Commands act on the window shown. To close a popup, use back on its first page.",
        params: &[numbered("number", "the window to show, from the list")],
    },
    Command {
        name: "quit",
        help: "Quit this kb and its browser. In a pane that ran only kb, the pane closes; a headless kb (kb --headless) stops. Cookies and sign-ins are kept in its profile.",
        params: &[],
    },
    Command {
        name: "screenshot",
        help: "Take a PNG screenshot of what the window shows, of one element, or of the whole page. By default one image pixel is one CSS pixel (a 1280x800 viewport gives 1280x800): sharp enough to read, and a quarter of the size of a retina capture. A whole page taller than 8000 pixels is scaled down to fit.",
        params: &[
            option("selector", "CSS", "capture only this element"),
            flag("full", "capture the whole page, not just the visible part"),
            option("size", "SIZE", "small (half size, for a quick look), normal (the default), sharp (device pixels, to check fine detail), or a width in pixels such as 800"),
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
        help: "Print the text of the page's main content (its <main> or <article>, if it has one), or of one element, as rendered, including text in shadow DOM. Inside an element, parts hidden with CSS are left out; an element that is itself hidden prints its text after a (hidden: …) note. Text that is off screen or the colour of its background is not detected. For a text field, prints its value; for a select, the chosen option's label. On a long page, find or outline are usually better first steps.",
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
            number("limit", "N", "list at most this many (default 60)"),
        ],
    },
    Command {
        name: "tables",
        help: "List the page's tables with their size, the heading above each, and their first row; or, given a number, print that table with its cells separated by tabs.",
        params: &[numbered("number", "which table to print, from the list"), number("limit", "N", "print at most this many rows (default 200)")],
    },
    Command {
        name: "images",
        help: "List the visible images in the main content (or in an element): alt text, shown and file size, caption, source and where each links to. To see one, use screenshot --selector.",
        params: &[optional("selector", "CSS selector of the part of the page to look in; body for all of it")],
    },
    Command {
        name: "find",
        help: "Find visible text on the page: lists each place it appears with the words around it and the heading it falls under, and scrolls the window to the first. Not case sensitive.",
        params: &[required("text", "the words to look for"), number("limit", "N", "show at most this many places (default 10)")],
    },
    Command {
        name: "scheme",
        help: "Set the colour scheme pages are told the user prefers (prefers-color-scheme), to test a dark mode: dark, light, or auto to follow the terminal's background.",
        params: &[required("scheme", "dark, light or auto")],
    },
    Command {
        name: "throttle",
        help: "Slow the network to test loading states, spinners and timeouts: slow (400 ms round trips, 400 kbit/s), fast (150 ms, 1.6 Mbit/s), offline, or off. Stays until turned off; status shows it.",
        params: &[required("speed", "off, slow, fast or offline")],
    },
    Command {
        name: "listeners",
        help: "List the event listeners attached to an element (click, input, submit…), with their first line of code, to see why an action does nothing. Listeners on ancestors, which handle events by delegation, are not included: ask about the ancestor.",
        params: &[required("selector", SELECTOR)],
    },
    Command {
        name: "storage",
        help: "Show the page's localStorage and sessionStorage and the names of its cookies; with --clear, clear cookies and all storage for the site, to test a first visit.",
        params: &[flag("clear", "clear cookies, localStorage, sessionStorage, IndexedDB and caches for this site")],
    },
    Command {
        name: "style",
        help: "Print an element's computed CSS: the property named, or the common ones (layout, box, colors, font). For where the element actually ended up, see box.",
        params: &[
            required("selector", SELECTOR),
            optional("property", "CSS property name, such as color or margin-top"),
            flag("why", "with a property: list the rules that set it, the winning one first, with their file and line"),
        ],
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
        help: "Click (or with --double, double-click) the middle of an element with the mouse, scrolling it into view first. Fails if the element is off screen or something else covers it. Answers once the page has settled: reports any console errors and dialogs the click caused and a change of address, and if it led to another page, waits for that page to load. Fails if the page never received the click. The mouse stays there, so :hover styles apply until it moves.",
        params: &[
            required("selector", SELECTOR),
            flag("double", "double-click, as for editing a todo item"),
            option("at", "X,Y", "click this point in CSS pixels from the element's top left, rather than its middle (a canvas, a map, a backdrop)"),
        ],
    },
    Command {
        name: "hover",
        help: "Move the mouse over the middle of an element, scrolling it into view first, so that :hover styles and mouseover handlers apply. It stays there until another hover or click.",
        params: &[required("selector", SELECTOR)],
    },
    Command {
        name: "dialog",
        help: "Choose how alert, confirm, prompt and \"leave site?\" dialogs are answered. In a pane, kb asks the user, as Chrome would, when the user set the dialog off; one an agent's command sets off is answered at once, accepted by default with a prompt's default text, since an open dialog freezes the page. While a dialog waits for the user, commands that need its page fail and say so; status and window name it, and the window it is in. accept or dismiss answers every dialog that way from now on, including one waiting in the pane; ask goes back to asking the user (not in a headless kb, which has no one to ask). Each dialog is recorded in console, with how it was answered and by whom, and reported by the command that caused it.",
        params: &[
            required("answer", "accept, dismiss or ask"),
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
        name: "upload",
        help: "Choose files for a file field, as a user would in the file chooser. Given the <input type=file> itself (it may be hidden), sets its files; given anything else, such as a \"Select video\" button or a drop zone, clicks it and answers the file chooser it opens. The page gets input and change events, as when a user chooses. Answers with each file's name and size as the page sees them, and any console errors it caused. Fails if a file doesn't exist, if several are given for a field that takes one, or if the click opens no file chooser. The files are read from this computer and sent to the page, so choose only files meant for it. Files that look like keys, passwords or sign-ins (in ~/.ssh or ~/.aws, .env files, private keys, browser cookies) are refused: only the user can send those, by choosing them in the page's file chooser, which agents can't see or drive.",
        params: &[
            required("selector", SELECTOR),
            list("files", "paths of the files to choose; relative ones are from the current directory"),
        ],
    },
    Command {
        name: "press",
        help: "Press a key. It goes to whatever has focus (click or type into an element first). Keys: Enter, Tab, Escape, Backspace, Delete, Space, ArrowLeft, ArrowRight, ArrowUp, ArrowDown, Home, End, PageUp, PageDown, F1 to F12, or one character. Modifiers: Ctrl+, Alt+, Shift+, as in Ctrl+a or Shift+Tab. There is no Cmd or Meta; Ctrl+a selects all. PageDown, PageUp, Home and End scroll the page when no field has focus; arrow keys don't change a <select> (use select).",
        params: &[required("key", "key name")],
    },
    Command {
        name: "wait",
        help: "Wait until an element matching a selector exists, or until one of the matching elements contains some text, or until none is left. Keeps waiting across page loads.",
        params: &[
            required("selector", "CSS selector; any match counts"),
            option("text", "TEXT", "wait until a matching element's text (or a field's value) contains this"),
            flag("gone", "wait until nothing matches the selector"),
            number("count", "N", "wait until at least N elements match"),
            flag("visible", "wait until a matching element is shown, not merely present"),
            number("timeout", "SECONDS", "how long to wait before failing (default 10, at most 60; to wait longer, wait again)"),
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
                Kind::Positional if param.shape == Shape::List => format!("<{}>...", param.name),
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
                Kind::Positional if param.shape == Shape::List => format!("<{}>...", param.name),
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
                Some(words @ [_, ..]) if param.shape == Shape::List => {
                    args.insert(param.name.to_owned(), json!(words));
                    continue;
                }
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

    /// Make the paths in the command's file lists absolute, and check each
    /// is a file: kb's working directory isn't the caller's. Over MCP a
    /// single path may come as a string rather than a list.
    pub fn resolve_files(&self, args: &mut Value) -> Result<(), String> {
        for param in self.params.iter().filter(|param| param.shape == Shape::List) {
            let paths: Vec<String> = match args[param.name].take() {
                Value::Null => continue,
                Value::String(path) => vec![path],
                Value::Array(paths) => paths
                    .into_iter()
                    .map(|path| path.as_str().map(str::to_owned))
                    .collect::<Option<_>>()
                    .ok_or(format!("{} takes a list of paths, as strings", param.name))?,
                _ => return Err(format!("{} takes a list of paths", param.name)),
            };
            if paths.is_empty() {
                return Err(format!("{} needs at least one path", param.name));
            }
            let mut absolute = Vec::new();
            for path in paths {
                let full = std::fs::canonicalize(&path).map_err(|_| {
                    format!("no such file: {}", std::env::current_dir().unwrap_or_default().join(&path).display())
                })?;
                if !full.is_file() {
                    return Err(format!("{} is a folder, not a file", full.display()));
                }
                absolute.push(full.display().to_string());
            }
            args[param.name] = json!(absolute);
        }
        Ok(())
    }

    /// The command as an MCP tool definition.
    pub fn tool(&self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for param in self.params.iter().filter(|param| param.name != OUT) {
            let schema = match (param.kind, param.shape) {
                (Kind::Flag, _) => json!({"type": "boolean"}),
                (_, Shape::Number) => json!({"type": "number"}),
                (_, Shape::List) => json!({"type": "array", "items": {"type": "string"}}),
                (_, Shape::Text) => json!({"type": "string"}),
            };
            let mut schema = schema;
            schema["description"] = json!(param.help);
            properties.insert(param.name.to_owned(), schema);
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
        assert_eq!(parse("wait #done --timeout 3").unwrap(), json!({"selector": "#done", "timeout": "3"}));
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
        assert!(parse("upload #file").unwrap_err().contains("needs <files>"));
        assert!(parse("type #name").unwrap_err().contains("needs <text>"));
        assert!(parse("wait #x --timeout").unwrap_err().contains("needs a value"));
        assert!(parse("reload now").unwrap_err().contains("takes no arguments"));
        assert!(parse("screenshot --bogus").unwrap_err().contains("has no option --bogus"));
        assert_eq!(parse("text").unwrap(), json!({}));
    }

    #[test]
    fn synopsis_and_tool() {
        let wait = find("wait").unwrap();
        assert_eq!(
            wait.synopsis(),
            "wait <selector> [--text TEXT] [--gone] [--count N] [--visible] [--timeout SECONDS]"
        );
        assert!(wait.describe().contains("--timeout SECONDS    how long to wait"));
        // Saving to a file is the command line's business only.
        let shot = find("screenshot").unwrap();
        assert!(shot.synopsis().contains("[--out FILE]"));
        assert!(shot.tool()["inputSchema"]["properties"].get("out").is_none());
        let tool = wait.tool();
        assert_eq!(tool["inputSchema"]["required"], json!(["selector"]));
        assert_eq!(tool["inputSchema"]["properties"]["timeout"]["type"], "number");
        assert_eq!(tool["inputSchema"]["properties"]["text"]["type"], "string");
        assert_eq!(find("console").unwrap().tool()["inputSchema"]["properties"]["errors"]["type"], "boolean");
    }

    #[test]
    fn files_are_a_list() {
        assert_eq!(parse("upload #f a.mp4 b c").unwrap(), json!({"selector": "#f", "files": ["a.mp4", "b", "c"]}));
        let upload = find("upload").unwrap().tool();
        assert_eq!(upload["inputSchema"]["properties"]["files"]["type"], "array");
        assert_eq!(upload["inputSchema"]["required"], json!(["selector", "files"]));
    }

    #[test]
    fn file_paths_are_made_absolute_and_checked() {
        let upload = find("upload").unwrap();
        let here = std::env::current_dir().unwrap();
        let mut args = json!({"selector": "#f", "files": "Cargo.toml"});
        upload.resolve_files(&mut args).unwrap();
        assert_eq!(args["files"], json!([here.join("Cargo.toml").canonicalize().unwrap().display().to_string()]));
        let mut missing = json!({"selector": "#f", "files": ["Cargo.toml", "nothere.mp4"]});
        assert!(upload.resolve_files(&mut missing).unwrap_err().starts_with("no such file: "));
        let mut folder = json!({"selector": "#f", "files": ["src"]});
        assert!(upload.resolve_files(&mut folder).unwrap_err().contains("is a folder"));
        let mut none = json!({"selector": "#f", "files": []});
        assert!(upload.resolve_files(&mut none).is_err());
    }
}
