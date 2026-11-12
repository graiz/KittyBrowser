//! What an agent can ask of the page: carries out the commands in
//! `commands.rs` and keeps the console and network logs they report on.
//!
//! The app's loop is single threaded and never blocks on Chrome, so a
//! command that needs an answer from Chrome parks its reply channel in
//! `pending` under the id of the DevTools request, and finishes when the
//! reply with that id comes past in `observe`.

use crate::control::{Output, Reply, Request};
use crate::engine::chrome::Chrome;
use crate::term::input::{Key, KeyCode};
use crate::ui;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write;
use std::time::{Duration, Instant};

/// Console messages kept, across pages.
const MAX_CONSOLE: usize = 2000;
/// Requests kept for the current page.
const MAX_REQUESTS: usize = 2000;
/// Longest text answer; more than this is cut, and says so.
const MAX_OUTPUT: usize = 30_000;
/// How long `open` and `reload` wait for the page to finish loading.
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// How long Chrome gets to answer a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `back` and `forward` wait for the page to change.
const MOVE_TIMEOUT: Duration = Duration::from_secs(10);
/// A loaded page is reported on once nothing has been requested or logged
/// for this long, so that errors from its first fetches are counted.
const QUIET: Duration = Duration::from_millis(400);
/// But a page that never goes quiet is reported on after this long.
const QUIET_LIMIT: Duration = Duration::from_secs(4);
/// How long after an action to collect the errors it caused.
const SETTLE: Duration = Duration::from_millis(100);
const DEFAULT_WAIT: f64 = 10.0;
/// What `style` prints when no property is named, as a JavaScript array.
const COMMON_STYLES: &str = "['display', 'position', 'width', 'height', 'margin', 'padding', 'border', \
     'left', 'top', 'z-index', 'visibility', 'opacity', 'color', 'background-color', 'font-family', 'font-size', \
     'font-weight', 'line-height', 'text-align']";

/// Script fragment: how an element is named in answers, `<div#id>`.
const NAME_OF: &str = "(e => '<' + e.localName + (e.id ? '#' + e.id : '') + \
     (e.classList.length ? '.' + [...e.classList].join('.') : '') + '>')";

/// For `locate`, before a click: refuse if the click would land elsewhere.
const CLICKABLE: &str = "{ const b = el.getBoundingClientRect(); \
     const hit = b.width > 0 && b.height > 0 ? document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2) : el; \
     if (!hit) throw new Error('the element is off screen, at ' + Math.round(b.left) + ',' + Math.round(b.top) + \
       ' in the window, and can\\'t be scrolled into view, so it can\\'t be clicked; see: kb ctl box'); \
     const labels = el.labels ? [...el.labels] : []; \
     if (hit !== el && !el.contains(hit) && !hit.contains(el) && !labels.some(label => label.contains(hit))) \
       throw new Error('the element is covered by ' + NAME(hit) + ', which would get the click instead; see: kb ctl box'); \
     if (hit !== el && el.contains(hit)) landed = NAME(hit); }";

/// Where an element is on the page, without moving the page, for a screenshot.
const SHOT_RECT: &str = "(sel => { const el = document.querySelector(sel); \
     if (!el) throw new Error('no element matches ' + sel); \
     const r = el.getBoundingClientRect(), x = r.left + scrollX, y = r.top + scrollY; \
     if (r.width <= 0 || r.height <= 0) throw new Error('the element has no size, so there is nothing to capture'); \
     if (x + r.width <= 0 || y + r.height <= 0) throw new Error('the element is outside the page, at ' + \
       Math.round(x) + ',' + Math.round(y) + ', so there is nothing to capture; see: kb ctl box'); \
     return {x, y, width: r.width, height: r.height}; })";

const BOX: &str = "(sel => { const el = document.querySelector(sel); \
     if (!el) throw new Error('no element matches ' + sel); \
     const r = el.getBoundingClientRect(), style = getComputedStyle(el), n = Math.round; \
     const hidden = style.display === 'none' ? 'display: none' \
       : style.visibility !== 'visible' ? 'visibility: ' + style.visibility \
       : +style.opacity === 0 ? 'opacity: 0' \
       : r.width === 0 || r.height === 0 ? 'it has no size' : ''; \
     const x = r.left + scrollX, y = r.top + scrollY; \
     const outside = x + r.width <= 0 || y + r.height <= 0 ? 'outside the page, so it can\\'t be scrolled to' \
       : r.bottom <= 0 ? 'above the window' : r.top >= innerHeight ? 'below the window' \
       : r.right <= 0 ? 'left of the window' : r.left >= innerWidth ? 'right of the window' : ''; \
     const lines = ['size: ' + n(r.width) + 'x' + n(r.height), \
       'position: ' + n(x) + ',' + n(y) + ' on the page; ' + n(r.left) + ',' + n(r.top) + ' in the window', \
       'rendered: ' + (hidden ? 'no (' + hidden + ')' : 'yes'), \
       'in the window: ' + (outside ? 'no (' + outside + ')' : 'yes')]; \
     if (!hidden && !outside) { \
       const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); \
       const covered = hit && hit !== el && !el.contains(hit) && !hit.contains(el); \
       lines.push('covered: ' + (covered ? 'yes, by ' + NAME(hit) + ' at its centre' : 'no')); } \
     return lines.join('\\n'); })";

const SCROLL: &str = "(to => { \
     if (to === 'top') scrollTo(0, 0); \
     else if (to === 'bottom') scrollTo(0, document.documentElement.scrollHeight); \
     else if (/^\\d+$/.test(to)) scrollTo(0, +to); \
     else { const el = document.querySelector(to); \
       if (!el) throw new Error('no element matches ' + to + '; scroll takes top, bottom, a number of pixels, or a selector'); \
       el.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'}); } \
     const most = Math.max(0, document.documentElement.scrollHeight - innerHeight); \
     return 'scrolled to ' + Math.min(most, Math.round(scrollY)) + ' of ' + most + ' pixels'; })";

const SELECT: &str = "((sel, want) => { const el = document.querySelector(sel); \
     if (!el) throw new Error('no element matches ' + sel); \
     if (el.localName !== 'select') throw new Error('that element is a <' + el.localName + '>, not a <select>'); \
     const options = [...el.options], label = o => o.label.trim(); \
     const option = options.find(o => o.value === want) || options.find(o => label(o) === want) \
       || options.find(o => label(o).toLowerCase() === want.toLowerCase()); \
     if (!option) throw new Error('there is no option ' + JSON.stringify(want) + '; the options are: ' + options.map(label).join(', ')); \
     el.value = option.value; \
     el.dispatchEvent(new Event('input', {bubbles: true})); \
     el.dispatchEvent(new Event('change', {bubbles: true})); \
     return 'selected ' + label(option) + ' in ' + sel; })";

/// How `eval` turns an object into something printable: elements, and
/// lists of them, by name.
const CONTENTS: &str = "function() { \
     const show = v => v instanceof Element ? NAME(v) : v; \
     if (this instanceof NodeList || this instanceof HTMLCollection || Array.isArray(this)) return [...this].map(show); \
     return this; }";

/// A script from one of the templates above, with `NAME` filled in.
fn script(template: &str) -> String {
    template.replace("NAME", NAME_OF)
}
const MAX_WAIT: f64 = 60.0;

struct Message {
    /// Position in the stream of all messages, to tell pages apart.
    seq: u64,
    level: String,
    text: String,
    /// Where it came from, as `file:line`, when known.
    source: String,
}

struct NetRequest {
    id: String,
    method: String,
    url: String,
    kind: String,
    status: Option<u64>,
    /// Why it failed, if it did.
    error: Option<String>,
    finished: bool,
    cached: bool,
    /// The document load it belongs to.
    loader: String,
    /// Whether it is the page itself.
    page: bool,
}

impl Message {
    fn line(&self) -> String {
        match (self.level.as_str(), self.source.as_str()) {
            (PAGE, _) => format!("--- page: {} ---", self.text),
            (level, "") => format!("[{level}] {}", self.text),
            (level, source) => format!("[{level}] {} ({source})", self.text),
        }
    }
}

/// The level of the marker logged where a new page begins.
const PAGE: &str = "page";
/// The level of the record of a dialog and how it was answered.
const DIALOG: &str = "dialog";

impl NetRequest {
    fn line(&self) -> String {
        let state = match (&self.error, self.status) {
            (Some(error), _) => format!("FAILED ({error})"),
            (None, Some(status)) if self.cached => format!("{status} (cached)"),
            (None, Some(status)) if status >= 400 && is_favicon(&self.url) => format!("{status} (not counted)"),
            (None, Some(status)) => status.to_string(),
            (None, None) if self.finished => "done".to_owned(),
            (None, None) => "pending".to_owned(),
        };
        format!("{state} {} {} ({})", self.method, self.url, self.kind)
    }

    fn failed(&self) -> bool {
        !is_favicon(&self.url) && (self.error.is_some() || self.status.is_some_and(|status| status >= 400))
    }
}

/// What to do with an element once its position is known.
enum Then {
    Click(String),
    Hover(String),
    Type(String, String),
    Screenshot,
}

/// How things stood before an action, to tell what it caused.
#[derive(Clone)]
struct Baseline {
    seq: u64,
    failed: usize,
    url: String,
    /// How many page loads had started.
    loads: u64,
}

/// A command waiting for the page to finish loading.
struct LoadWait {
    reply: Reply,
    /// What to say before the report on the page, if anything.
    said: String,
    deadline: Instant,
    /// When loading finished. The answer then waits for the page to go quiet.
    stopped: Option<Instant>,
    /// Whether this is `check`, for which an unhealthy page is a failure.
    check: bool,
}

/// An action that is done but not yet answered: errors it causes take a
/// moment to arrive.
struct Settling {
    reply: Reply,
    message: String,
    since: Baseline,
    at: Instant,
}

/// A command waiting on a reply from Chrome.
enum Pending {
    /// Answer with the value the expression produced.
    Eval(Reply),
    /// An object's contents are coming, as JSON; if they can't be had,
    /// answer with this description of it.
    Contents(Reply, String),
    Navigate(Reply),
    /// The element's rectangle is coming; then act on it.
    Rect(Reply, Then),
    /// The page's full size is coming; then capture all of it.
    PageSize(Reply),
    Image(Reply),
    /// The history list is coming; then step this far through it.
    History(Reply, i64),
    /// Once Chrome confirms the action, answer with this text and whatever
    /// went wrong on the page since the baseline.
    Done(Reply, String, Baseline),
    /// The page's title is coming; answer with it between these two parts.
    Status(Reply, String, String),
    /// A script that acts on the page is running; answer with what it
    /// returns and whatever went wrong since the baseline.
    Acted(Reply, Baseline),
    /// One check of a `wait`; the number identifies the wait.
    WaitCheck(u64),
}

/// A `wait` in progress, checked on every tick so that it survives the page
/// being replaced.
struct Wait {
    id: u64,
    /// What is being waited for, in words, and the script that tests it.
    what: String,
    check: String,
    deadline: Instant,
    timeout: f64,
    checking: bool,
    reply: Reply,
}

/// What the app knows about the page that the commands report.
pub struct Page<'a> {
    pub url: &'a str,
    pub loading: bool,
    /// Viewport in CSS pixels.
    pub viewport: (u32, u32),
    pub scale: f64,
}

#[derive(Default)]
pub struct Devtools {
    console: VecDeque<Message>,
    next_seq: u64,
    /// First message of the current page.
    page_seq: u64,
    requests: Vec<NetRequest>,
    /// The document load that started the current page.
    loader: String,
    url: String,
    pending: HashMap<u64, (Pending, Instant)>,
    /// Commands waiting for the page to finish loading.
    loading: Vec<LoadWait>,
    /// Page loads started, and whether one is under way.
    loads: u64,
    busy: bool,
    /// When the page last requested or logged something.
    active: Option<Instant>,
    /// Why the page's own document last failed to load, if it did.
    document_error: Option<String>,
    /// `back` and `forward` waiting for the page to change.
    moving: Vec<(Reply, Instant)>,
    waits: Vec<Wait>,
    next_wait: u64,
    settling: Vec<Settling>,
    /// How dialogs are answered: dismissed if set, else accepted, and with
    /// this text typed into prompts if given.
    dismiss: bool,
    prompt_text: Option<String>,
}

fn text(reply: &Reply, text: impl Into<String>) {
    let mut text: String = text.into();
    if text.len() > MAX_OUTPUT {
        let cut = (0..=MAX_OUTPUT).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
        let more = text[cut..].chars().count();
        text.truncate(cut);
        let _ = write!(text, "\n… ({more} more characters not shown)");
    }
    let _ = reply.send(Ok(Output::Text(text)));
}

fn fail(reply: &Reply, error: impl Into<String>) {
    let _ = reply.send(Err(error.into()));
}

/// A JavaScript string literal.
fn quote(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

fn plural(count: usize, noun: &str) -> String {
    match count {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// The outcome of a `Runtime.evaluate`: the value, or what was thrown.
fn evaluated(result: Result<&Value, String>) -> Result<&Value, String> {
    let result = result?;
    if let Some(details) = result.get("exceptionDetails") {
        let thrown = details["exception"]["description"].as_str().or(details["text"].as_str());
        // The first line is the message; the rest is a stack through our own wrapper.
        let line = thrown.unwrap_or("the script threw").lines().next().unwrap_or("");
        // Our own scripts throw plain Errors; "TypeError:" and such stay.
        return Err(line.strip_prefix("Error: ").unwrap_or(line).to_owned());
    }
    Ok(&result["result"])
}

/// A value from the page as text: strings bare, everything else as JSON.
fn show(object: &Value) -> String {
    match object.get("value") {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => preview(object)
            .or(object["description"].as_str().map(str::to_owned))
            .unwrap_or_else(|| object["type"].as_str().unwrap_or("undefined").to_owned()),
    }
}

/// An object or array logged to the console, from the summary of its
/// contents that Chrome attaches: `{a: 1, b: "x"}`, `[1, 2, …]`.
fn preview(object: &Value) -> Option<String> {
    let preview = &object["preview"];
    let array = preview["subtype"] == "array";
    let mut parts: Vec<String> = preview["properties"]
        .as_array()?
        .iter()
        .map(|property| {
            let value = match (property["type"].as_str(), property["value"].as_str().unwrap_or("")) {
                (Some("string"), value) => quote(value),
                (_, value) => value.to_owned(),
            };
            match property["name"].as_str() {
                Some(name) if !array => format!("{name}: {value}"),
                _ => value,
            }
        })
        .collect();
    if preview["overflow"] == true {
        parts.push("…".to_owned());
    }
    Some(if array { format!("[{}]", parts.join(", ")) } else { format!("{{{}}}", parts.join(", ")) })
}

/// `https://site/app.js?v=2` and line 12 as `app.js:12`.
fn source(url: &str, line: Option<u64>) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit('/').find(|part| !part.is_empty()).unwrap_or("");
    match line {
        Some(line) if !name.is_empty() => format!("{name}:{}", line + 1),
        _ => name.to_owned(),
    }
}

/// `Ctrl+a`, `Shift+Tab`, `Enter`, `x`.
fn parse_key(name: &str) -> Option<Key> {
    let mut key = Key { code: KeyCode::Esc, ctrl: false, alt: false, shift: false };
    let mut parts = name.split('+').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => key.ctrl = true,
                "alt" | "option" => key.alt = true,
                "shift" => key.shift = true,
                _ => return None,
            }
            continue;
        }
        key.code = match part.to_ascii_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "escape" | "esc" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "arrowleft" | "left" => KeyCode::Left,
            "arrowright" | "right" => KeyCode::Right,
            "arrowup" | "up" => KeyCode::Up,
            "arrowdown" | "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "space" => KeyCode::Char(' '),
            _ => {
                let mut chars = part.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return None,
                }
            }
        };
    }
    Some(key)
}

/// Script that finds an element, brings it into view, and returns where it
/// is in the viewport.
fn locate(selector: &str, prepare: &str) -> String {
    format!(
        "(() => {{ const el = document.querySelector({sel}); \
         if (!el) throw new Error('no element matches ' + {sel}); \
         el.scrollIntoView({{block: 'center', inline: 'center', behavior: 'instant'}}); \
         let landed = ''; {prepare} const r = el.getBoundingClientRect(); \
         return {{x: r.x, y: r.y, width: r.width, height: r.height, landed}}; }})()",
        sel = quote(selector),
    )
}

/// For `locate`: focus a text field, refusing anything that isn't one.
const FOCUS: &str = "if (el.localName === 'select') throw new Error('that element is a <select>; use: kb ctl select'); \
     if (el.matches('input:is([type=checkbox], [type=radio])')) \
       throw new Error('that element is a ' + el.type + '; use: kb ctl click'); \
     if (!el.isContentEditable && !el.matches('textarea, input:not([type=button], [type=submit], [type=reset], \
       [type=image], [type=file], [type=range], [type=color], [type=hidden])')) \
       throw new Error('that element is a <' + el.localName + '>, not a text field'); \
     el.focus();";

/// For `locate`, after `FOCUS`: select the contents so typing replaces them.
const SELECT_ALL: &str = "if (el.select) el.select(); else document.execCommand('selectAll');";

/// Script fragment: the text of `el`, or what is typed in it if it is a field.
const TEXT_OF: &str = "(el.matches('input, textarea, select') ? el.value : el.innerText)";

/// Whether a request is the browser's own look for a site icon, which a
/// page under development rarely has and nobody asked for.
fn is_favicon(url: &str) -> bool {
    url.split(['?', '#']).next().is_some_and(|path| path.ends_with("/favicon.ico"))
}

impl Devtools {
    fn park(&mut self, id: u64, pending: Pending) {
        self.pending.insert(id, (pending, Instant::now() + REQUEST_TIMEOUT));
    }

    /// Run one of our own scripts, whose result comes back by value.
    fn evaluate(&mut self, chrome: &mut Chrome, expression: &str, pending: Pending) {
        let id = chrome.request(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "userGesture": true}),
        );
        self.park(id, pending);
    }

    fn baseline(&self) -> Baseline {
        Baseline {
            seq: self.next_seq,
            failed: self.requests.iter().filter(|r| r.failed()).count(),
            url: self.url.clone(),
            loads: self.loads,
        }
    }

    /// What went wrong on the page since `since`, as lines to add to an
    /// answer; empty if nothing did.
    fn fallout(&self, since: &Baseline) -> String {
        let mut out = String::new();
        if self.url != since.url {
            let _ = write!(out, "\nthe page is now {}", self.url);
        }
        let errors: Vec<String> =
            self.console.iter().filter(|m| m.seq >= since.seq && m.level == "error").map(Message::line).collect();
        if !errors.is_empty() {
            let _ = write!(out, "\nthis caused {}:\n{}", plural(errors.len(), "console error"), errors.join("\n"));
        }
        for dialog in self.console.iter().filter(|m| m.seq >= since.seq && m.level == DIALOG) {
            let _ = write!(out, "\na dialog appeared: {}", dialog.text);
        }
        let failed = self.requests.iter().filter(|r| r.failed()).count().saturating_sub(since.failed);
        if failed > 0 {
            let _ = write!(out, "\n{} since; see: kb ctl network --failed", plural(failed, "failed request"));
        }
        out
    }

    fn capture(&mut self, chrome: &mut Chrome, mut params: Value, reply: Reply) {
        params["format"] = json!("png");
        let id = chrome.request("Page.captureScreenshot", params);
        self.park(id, Pending::Image(reply));
    }

    fn errors(&self) -> usize {
        self.page_console().filter(|m| m.level == "error").count()
    }

    fn page_console(&self) -> impl Iterator<Item = &Message> {
        self.console.iter().filter(|m| m.seq >= self.page_seq && m.level != PAGE)
    }

    /// One line on how the page is doing, with where to look if it isn't.
    fn health(&self) -> String {
        let errors = self.errors();
        let failed = self.requests.iter().filter(|r| r.failed()).count();
        let mut line = format!("{}, {}", plural(errors, "console error"), plural(failed, "failed request"));
        if errors > 0 {
            line.push_str("\nsee: kb ctl console --errors");
        }
        if failed > 0 {
            line.push_str("\nsee: kb ctl network --failed");
        }
        line
    }

    /// Carry out one command. The answer goes to `request.reply`, now or
    /// when Chrome has replied.
    pub fn command(&mut self, request: Request, chrome: &mut Chrome, page: Page) {
        let Request { command, args, reply } = request;
        let arg = |name: &str| args[name].as_str().filter(|value| !value.is_empty());
        let flag = |name: &str| args[name] == true || args[name] == "true";
        let need = |name: &str| arg(name).ok_or(format!("{command} needs {name}"));

        let result: Result<(), String> = (|| {
            match command.as_str() {
                "status" => {
                    let (width, height) = page.viewport;
                    let url = if page.url.is_empty() { "about:blank" } else { page.url };
                    let rest = format!(
                        "loading: {}\nviewport: {width}x{height} CSS pixels, scale {}\n{}",
                        if page.loading { "yes" } else { "no" },
                        page.scale,
                        self.health(),
                    );
                    // The title has to be asked for; the rest is already known.
                    let pending = Pending::Status(reply.clone(), format!("url: {url}"), rest);
                    self.evaluate(chrome, "document.title", pending);
                }
                "open" => {
                    let url = ui::normalize_url(need("url")?).ok_or("open needs a URL")?;
                    chrome.cache_for(&url);
                    let id = chrome.request("Page.navigate", json!({"url": url}));
                    self.park(id, Pending::Navigate(reply.clone()));
                }
                "reload" => {
                    chrome.reload();
                    self.await_load(reply.clone(), String::new());
                }
                "back" | "forward" => {
                    let step = if command == "back" { -1 } else { 1 };
                    let id = chrome.request("Page.getNavigationHistory", json!({}));
                    self.park(id, Pending::History(reply.clone(), step));
                }
                "screenshot" => match arg("selector") {
                    Some(selector) => {
                        {
                        let find = format!("{SHOT_RECT}({})", quote(selector));
                        self.evaluate(chrome, &find, Pending::Rect(reply.clone(), Then::Screenshot))
                    }
                    }
                    None if flag("full") => {
                        let id = chrome.request("Page.getLayoutMetrics", json!({}));
                        self.park(id, Pending::PageSize(reply.clone()));
                    }
                    None => self.capture(chrome, json!({}), reply.clone()),
                },
                "console" => {
                    let mut out = String::new();
                    let (all, errors) = (flag("all"), flag("errors"));
                    for message in self.console.iter().filter(|m| all || m.seq >= self.page_seq) {
                        let wanted = match message.level.as_str() {
                            PAGE => all,
                            "error" | "warning" => true,
                            _ => !errors,
                        };
                        if wanted {
                            out.push_str(&message.line());
                            out.push('\n');
                        }
                    }
                    if !self.page_console().any(|m| !errors || matches!(m.level.as_str(), "error" | "warning")) {
                        let _ = write!(
                            out,
                            "no console {} since the page loaded",
                            if errors { "errors or warnings" } else { "messages" }
                        );
                    }
                    text(&reply, out.trim_end());
                }
                "network" => {
                    let mut out = String::new();
                    let failed = flag("failed");
                    for request in self.requests.iter().filter(|r| !failed || r.failed()) {
                        let _ = writeln!(out, "{}", request.line());
                    }
                    if out.is_empty() {
                        out = format!("no {}requests since the page loaded", if failed { "failed " } else { "" });
                    }
                    text(&reply, out.trim_end());
                }
                "eval" => {
                    // By reference, so that a promise or an element can be
                    // told from a plain object; `finish` fetches the rest.
                    let id = chrome.request(
                        "Runtime.evaluate",
                        json!({"expression": need("expression")?, "userGesture": true, "replMode": true}),
                    );
                    self.park(id, Pending::Eval(reply.clone()));
                }
                "style" => {
                    let names = match arg("property") {
                        Some(property) => format!("[{}]", quote(property)),
                        None => COMMON_STYLES.to_owned(),
                    };
                    let script = format!(
                        "(() => {{ const el = document.querySelector({sel}); \
                         if (!el) throw new Error('no element matches ' + {sel}); \
                         const style = getComputedStyle(el); \
                         return {names}.map(name => name + ': ' + (style.getPropertyValue(name) || '(not set)')).join('\\n'); }})()",
                        sel = quote(need("selector")?),
                    );
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "text" | "html" => {
                    let (value, whole) = match command.as_str() {
                        "text" => (TEXT_OF, "document.body.innerText"),
                        _ => ("el.outerHTML", "document.documentElement.outerHTML"),
                    };
                    let script = match arg("selector") {
                        Some(selector) => format!(
                            "(() => {{ const el = document.querySelector({sel}); \
                             if (!el) throw new Error('no element matches ' + {sel}); \
                             return {value}; }})()",
                            sel = quote(selector),
                        ),
                        None => whole.to_owned(),
                    };
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "box" => {
                    let script = format!("{}({})", script(BOX), quote(need("selector")?));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "scroll" => {
                    let script = format!("{SCROLL}({})", quote(need("to")?));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "select" => {
                    let script = format!("{SELECT}({}, {})", quote(need("selector")?), quote(need("option")?));
                    self.evaluate(chrome, &script, Pending::Acted(reply.clone(), self.baseline()));
                }
                "hover" => {
                    let selector = need("selector")?;
                    let then = Then::Hover(selector.to_owned());
                    self.evaluate(chrome, &locate(selector, &script(CLICKABLE)), Pending::Rect(reply.clone(), then));
                }
                "dialog" => {
                    self.dismiss = match need("answer")? {
                        "accept" => false,
                        "dismiss" => true,
                        other => return Err(format!("dialog takes accept or dismiss, not {other:?}")),
                    };
                    self.prompt_text = arg("text").map(str::to_owned);
                    let answer = if self.dismiss { "dismissed" } else { "accepted" };
                    text(&reply, format!("dialogs will be {answer} from now on"));
                }
                "check" => {
                    // Reported through the same wait as a page load: once quiet.
                    let stopped = (!self.busy).then(Instant::now);
                    let deadline = Instant::now() + LOAD_TIMEOUT;
                    self.loading.push(LoadWait { reply: reply.clone(), said: String::new(), deadline, stopped, check: true });
                }
                "click" => {
                    let selector = need("selector")?;
                    let then = Then::Click(selector.to_owned());
                    self.evaluate(chrome, &locate(selector, &script(CLICKABLE)), Pending::Rect(reply.clone(), then));
                }
                "type" => {
                    let selector = need("selector")?;
                    let then = Then::Type(selector.to_owned(), need("text")?.to_owned());
                    let clear = if flag("clear") { SELECT_ALL } else { "" };
                    let prepare = format!("{} {FOCUS} {clear}", script(CLICKABLE).replace("clicked", "typed into"));
                    self.evaluate(chrome, &locate(selector, &prepare), Pending::Rect(reply.clone(), then));
                }
                "press" => {
                    let name = need("key")?;
                    let key = parse_key(name).ok_or(format!(
                        "don't know the key {name:?}; try Enter, Tab, Escape, ArrowDown, a character, or Ctrl+a"
                    ))?;
                    let since = self.baseline();
                    chrome.key(&key);
                    let message = format!("pressed {name}");
                    self.settling.push(Settling { reply: reply.clone(), message, since, at: Instant::now() });
                }
                "wait" => {
                    let timeout = match arg("timeout") {
                        Some(seconds) => seconds
                            .parse::<f64>()
                            .ok()
                            .filter(|s| *s >= 0.0)
                            .ok_or("--timeout needs a number of seconds")?
                            .min(MAX_WAIT),
                        None => DEFAULT_WAIT,
                    };
                    let selector = need("selector")?;
                    let (what, condition) = match arg("text") {
                        _ if flag("gone") => (format!("{selector} to be gone"), "all.length === 0".to_owned()),
                        Some(wanted) => (
                            format!("{selector} containing {wanted:?}"),
                            format!("all.some(el => {TEXT_OF}.includes({}))", quote(wanted)),
                        ),
                        None => (selector.to_owned(), "all.length > 0".to_owned()),
                    };
                    self.next_wait += 1;
                    self.waits.push(Wait {
                        id: self.next_wait,
                        check: format!(
                            "(() => {{ const all = [...document.querySelectorAll({})]; return {condition}; }})()",
                            quote(selector)
                        ),
                        what,
                        deadline: Instant::now() + Duration::from_secs_f64(timeout),
                        timeout,
                        checking: false,
                        reply: reply.clone(),
                    });
                    self.tick(chrome);
                }
                other => return Err(format!("unknown command {other:?}; see kb ctl --help")),
            }
            Ok(())
        })();
        if let Err(error) = result {
            fail(&reply, error);
        }
    }

    /// Look at a message from Chrome before the app handles it: finish the
    /// command it answers, or log the event it reports.
    pub fn observe(&mut self, message: &Value, chrome: &mut Chrome) {
        if let Some(id) = message["id"].as_u64() {
            if let Some((pending, _)) = self.pending.remove(&id) {
                let result = match message.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("Chrome refused").to_owned()),
                    None => Ok(&message["result"]),
                };
                self.finish(pending, result, chrome);
            }
            return;
        }
        let params = &message["params"];
        let main_frame = params["frameId"] == chrome.main_frame();
        let method = message["method"].as_str().unwrap_or("");
        if ["Network.", "Runtime.", "Log."].iter().any(|domain| method.starts_with(domain)) {
            self.active = Some(Instant::now());
        }
        match method {
            "Runtime.consoleAPICalled" => {
                let parts: Vec<String> = params["args"].as_array().into_iter().flatten().map(show).collect();
                let frame = &params["stackTrace"]["callFrames"][0];
                let level = match params["type"].as_str().unwrap_or("log") {
                    "warning" => "warning",
                    "error" | "assert" => "error",
                    "info" => "info",
                    "debug" => "debug",
                    _ => "log",
                };
                let from = source(frame["url"].as_str().unwrap_or(""), frame["lineNumber"].as_u64());
                self.log(level, parts.join(" "), from);
            }
            "Runtime.exceptionThrown" => {
                let details = &params["exceptionDetails"];
                let thrown = details["exception"]["description"].as_str().or(details["text"].as_str());
                let message = thrown.unwrap_or("uncaught exception").lines().next().unwrap_or("").to_owned();
                let from = source(details["url"].as_str().unwrap_or(""), details["lineNumber"].as_u64());
                let message = message.strip_prefix("Uncaught ").unwrap_or(&message);
                self.log("error", format!("Uncaught {message}"), from);
            }
            "Log.entryAdded" => {
                let entry = &params["entry"];
                let level = match entry["level"].as_str().unwrap_or("info") {
                    "verbose" => "debug",
                    "warning" => "warning",
                    "error" => "error",
                    _ => "info",
                };
                let url = entry["url"].as_str().unwrap_or("");
                if !is_favicon(url) {
                    self.log(level, entry["text"].as_str().unwrap_or("").to_owned(), source(url, entry["lineNumber"].as_u64()));
                }
            }
            "Page.javascriptDialogOpening" => {
                // Answered at once: an open dialog freezes the page, and with
                // it every command.
                let kind = params["type"].as_str().unwrap_or("dialog");
                // Refusing a "leave this page?" would trap the browser on it.
                let accept = !self.dismiss || kind == "beforeunload";
                let typed = self.prompt_text.as_deref().or(params["defaultPrompt"].as_str()).unwrap_or("");
                chrome.request("Page.handleJavaScriptDialog", json!({"accept": accept, "promptText": typed}));
                let mut line = format!("{kind} {:?} was {}", params["message"].as_str().unwrap_or(""), if accept { "accepted" } else { "dismissed" });
                if kind == "prompt" && accept {
                    let _ = write!(line, " with {typed:?}");
                }
                self.log(DIALOG, line, String::new());
            }
            "Network.requestWillBeSent" => {
                let url = params["request"]["url"].as_str().unwrap_or("");
                let id = params["requestId"].as_str().unwrap_or("");
                let kind = params["type"].as_str().unwrap_or("Other");
                let page = kind == "Document" && main_frame;
                if page {
                    self.document_error = None;
                }
                // A redirect reuses the id: keep one line, for where it ended up.
                self.requests.retain(|request| request.id != id);
                if !url.starts_with("data:") && self.requests.len() < MAX_REQUESTS {
                    self.requests.push(NetRequest {
                        id: id.to_owned(),
                        method: params["request"]["method"].as_str().unwrap_or("GET").to_owned(),
                        url: url.to_owned(),
                        kind: kind.to_ascii_lowercase(),
                        status: None,
                        error: None,
                        finished: false,
                        cached: false,
                        loader: params["loaderId"].as_str().unwrap_or("").to_owned(),
                        page,
                    });
                }
            }
            "Network.responseReceived" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.status = params["response"]["status"].as_u64();
                    request.cached |= params["response"]["fromDiskCache"] == true;
                }
            }
            "Network.requestServedFromCache" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.cached = true;
                }
            }
            "Network.loadingFinished" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.finished = true;
                }
            }
            "Network.loadingFailed" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.finished = true;
                    // Requests dropped because the page moved on aren't failures.
                    if params["canceled"] != true {
                        let error = params["errorText"].as_str().unwrap_or("failed").to_owned();
                        request.error = Some(error.clone());
                        if request.page {
                            self.document_error = Some(error);
                        }
                    }
                }
            }
            "Page.frameNavigated" if params["frame"].get("parentId").is_none() => {
                // The new page starts here, when it replaces the old one, and
                // not at its request: a load that fails may replace nothing.
                let loader = params["frame"]["loaderId"].as_str().unwrap_or("");
                if loader != self.loader {
                    self.new_page(loader, params["frame"]["url"].as_str().unwrap_or(""));
                }
                self.url = params["frame"]["url"].as_str().unwrap_or("").to_owned();
                self.moved();
            }
            "Page.navigatedWithinDocument" if main_frame => {
                self.url = params["url"].as_str().unwrap_or("").to_owned();
                self.moved();
            }
            "Page.frameStartedLoading" if main_frame => {
                self.loads += 1;
                self.busy = true;
            }
            "Page.frameStoppedLoading" if main_frame => {
                self.busy = false;
                let now = Instant::now();
                for wait in &mut self.loading {
                    wait.stopped = Some(now);
                }
            }
            _ => {}
        }
    }

    fn log(&mut self, level: &str, text: String, source: String) {
        if self.console.len() == MAX_CONSOLE {
            self.console.pop_front();
        }
        self.console.push_back(Message { seq: self.next_seq, level: level.to_owned(), text, source });
        self.next_seq += 1;
    }

    fn request(&mut self, id: &Value) -> Option<&mut NetRequest> {
        let id = id.as_str()?;
        self.requests.iter_mut().rev().find(|request| request.id == id)
    }

    /// The page changed: answer the `back` and `forward` waiting on that.
    fn moved(&mut self) {
        for (reply, _) in std::mem::take(&mut self.moving) {
            text(&reply, format!("now at {}", self.url));
        }
    }

    fn new_page(&mut self, loader: &str, url: &str) {
        self.loader = loader.to_owned();
        self.log(PAGE, url.to_owned(), String::new());
        self.page_seq = self.next_seq;
        // Its own requests began before it took over; the rest were the old page's.
        self.requests.retain(|request| request.loader == loader);
    }

    fn await_load(&mut self, reply: Reply, said: String) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        self.loading.push(LoadWait { reply, said, deadline, stopped: None, check: false });
    }

    /// Tell a command that waited for the page how the load went.
    fn report_load(&self, wait: LoadWait) {
        let document = self.requests.iter().find(|request| request.page);
        if self.url.starts_with("chrome-error://") {
            let why = self.document_error.as_deref().unwrap_or("the browser shows an error page");
            return fail(&wait.reply, format!("{}couldn't load the page: {why}", wait.said));
        }
        let refused = match document.and_then(|document| document.status) {
            Some(status) if status >= 400 => format!(", but the server answered {status} for it"),
            _ => String::new(),
        };
        if !wait.check {
            return text(&wait.reply, format!("{}loaded {}{refused}\n{}", wait.said, self.url, self.health()));
        }
        let mut problems: Vec<String> = self.page_console().filter(|m| m.level == "error").map(Message::line).collect();
        problems.extend(self.requests.iter().filter(|r| r.failed()).map(NetRequest::line));
        if problems.is_empty() && refused.is_empty() {
            text(&wait.reply, format!("ok: {}, no console errors, no failed requests", self.url));
        } else {
            if let Some(status) = document.and_then(|document| document.status).filter(|status| *status >= 400) {
                problems.insert(0, format!("the server answered {status} for the page itself"));
            }
            fail(&wait.reply, format!("{} is not healthy:\n{}", self.url, problems.join("\n")));
        }
    }

    fn finish(&mut self, pending: Pending, result: Result<&Value, String>, chrome: &mut Chrome) {
        match pending {
            Pending::Eval(reply) => {
                let object = match evaluated(result) {
                    Ok(object) => object,
                    Err(error) => return fail(&reply, error),
                };
                let description = object["description"].as_str().unwrap_or("object");
                match (object["subtype"].as_str(), &object["objectId"]) {
                    // Wait for it, then look at what it resolved to the same way.
                    (Some("promise"), id @ Value::String(_)) => {
                        let id = chrome.request("Runtime.awaitPromise", json!({"promiseObjectId": id}));
                        self.park(id, Pending::Eval(reply));
                    }
                    (Some("node"), _) => text(&reply, format!("<{description}>")),
                    (None | Some("array"), id @ Value::String(_)) if object["type"] == "object" => {
                        let id = chrome.request(
                            "Runtime.callFunctionOn",
                            json!({
                                "objectId": id, "returnByValue": true,
                                "functionDeclaration": script(CONTENTS),
                            }),
                        );
                        self.park(id, Pending::Contents(reply, description.to_owned()));
                    }
                    _ => text(&reply, show(object)),
                }
            }
            Pending::Contents(reply, description) => match evaluated(result) {
                Ok(object) => text(&reply, show(object)),
                // Cyclic, or otherwise not expressible as JSON.
                Err(_) => text(&reply, description),
            },
            Pending::Navigate(reply) => match result {
                Err(error) => fail(&reply, error),
                Ok(result) => match result["errorText"].as_str() {
                    Some(error) => fail(&reply, format!("couldn't load the page: {error}")),
                    // No new document: only the fragment changed.
                    None if result.get("loaderId").is_none() => text(&reply, "moved within the page"),
                    None => self.await_load(reply, String::new()),
                },
            },
            Pending::Rect(reply, then) => {
                let rect = match evaluated(result) {
                    Ok(object) => &object["value"],
                    Err(error) => return fail(&reply, error),
                };
                let number = |name: &str| rect[name].as_f64().unwrap_or(0.0);
                let (x, y, width, height) = (number("x"), number("y"), number("width"), number("height"));
                if width <= 0.0 || height <= 0.0 {
                    return fail(&reply, "the element has no size, so it isn't visible");
                }
                match then {
                    Then::Hover(selector) => {
                        let since = self.baseline();
                        let id = chrome.request(
                            "Input.dispatchMouseEvent",
                            json!({"type": "mouseMoved", "x": x + width / 2.0, "y": y + height / 2.0}),
                        );
                        self.park(id, Pending::Done(reply, format!("the mouse is over {selector}"), since));
                    }
                    Then::Click(selector) => {
                        let since = self.baseline();
                        let (x, y) = (x + width / 2.0, y + height / 2.0);
                        // Clicking a container hits whatever is in its middle.
                        let selector = match rect["landed"].as_str() {
                            Some(inner) if !inner.is_empty() => format!("{selector}; the click landed on {inner} inside it"),
                            _ => selector,
                        };
                        chrome.mouse("mouseMoved", x, y, "none", 0, 0, 0);
                        chrome.mouse("mousePressed", x, y, "left", 1, 1, 0);
                        let id = chrome.request(
                            "Input.dispatchMouseEvent",
                            json!({
                                "type": "mouseReleased", "x": x, "y": y, "button": "left",
                                "buttons": 0, "clickCount": 1,
                            }),
                        );
                        self.park(id, Pending::Done(reply, format!("clicked {selector}"), since));
                    }
                    Then::Type(selector, typed) => {
                        let since = self.baseline();
                        let id = chrome.request("Input.insertText", json!({"text": typed}));
                        self.park(id, Pending::Done(reply, format!("typed into {selector}"), since));
                    }
                    Then::Screenshot => {
                        let clip = json!({"x": x, "y": y, "width": width, "height": height, "scale": 1});
                        self.capture(chrome, json!({"clip": clip, "captureBeyondViewport": true}), reply);
                    }
                }
            }
            Pending::PageSize(reply) => match result {
                Err(error) => fail(&reply, error),
                Ok(metrics) => {
                    let size = &metrics["cssContentSize"];
                    let clip = json!({"x": 0, "y": 0, "width": size["width"], "height": size["height"], "scale": 1});
                    self.capture(chrome, json!({"clip": clip, "captureBeyondViewport": true}), reply);
                }
            },
            Pending::Image(reply) => {
                let _ = reply.send(match result.map(|result| result["data"].as_str()) {
                    Ok(Some(png)) => Ok(Output::Image(png.to_owned())),
                    Ok(None) => Err("Chrome sent no image".to_owned()),
                    Err(error) => Err(error),
                });
            }
            Pending::History(reply, step) => {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => return fail(&reply, error),
                };
                let target = result["currentIndex"].as_i64().unwrap_or(0) + step;
                let entry = usize::try_from(target).ok().and_then(|at| result["entries"].get(at));
                match entry {
                    Some(entry) => {
                        chrome.request("Page.navigateToHistoryEntry", json!({"entryId": entry["id"]}));
                        self.moving.push((reply, Instant::now() + MOVE_TIMEOUT));
                    }
                    None => fail(&reply, format!("there is no {} page to go to", if step < 0 { "earlier" } else { "later" })),
                }
            }
            Pending::Done(reply, message, since) => match result {
                Ok(_) => self.settling.push(Settling { reply, message, since, at: Instant::now() }),
                Err(error) => fail(&reply, error),
            },
            Pending::Status(reply, before, after) => {
                let title = evaluated(result).map(show).unwrap_or_default();
                text(&reply, format!("{before}\ntitle: {title}\n{after}"));
            }
            Pending::Acted(reply, since) => match evaluated(result) {
                Ok(object) => self.settling.push(Settling { reply, message: show(object), since, at: Instant::now() }),
                Err(error) => fail(&reply, error),
            },
            Pending::WaitCheck(id) => {
                let found = evaluated(result).is_ok_and(|object| object["value"] == true);
                if let Some(at) = self.waits.iter().position(|wait| wait.id == id) {
                    if found {
                        let wait = self.waits.remove(at);
                        text(&wait.reply, format!("waited for {}", wait.what));
                    } else {
                        // Not there yet, or the page was mid-navigation: look again next tick.
                        self.waits[at].checking = false;
                    }
                }
            }
        }
    }

    /// Housekeeping, several times a second: poll waits and give up on
    /// whatever has run out of time.
    pub fn tick(&mut self, chrome: &mut Chrome) {
        let now = Instant::now();
        self.pending.retain(|_, (_, deadline)| *deadline > now);

        for wait in std::mem::take(&mut self.loading) {
            match wait.stopped {
                Some(stopped) => {
                    let quiet_since = self.active.map_or(stopped, |active| active.max(stopped));
                    if now.duration_since(quiet_since) >= QUIET || now.duration_since(stopped) >= QUIET_LIMIT {
                        self.report_load(wait);
                    } else {
                        self.loading.push(wait);
                    }
                }
                None if wait.deadline <= now => {
                    let seconds = LOAD_TIMEOUT.as_secs();
                    let state = format!("still loading after {seconds}s: {}\n{}", self.url, self.health());
                    text(&wait.reply, format!("{}{state}", wait.said));
                }
                None => self.loading.push(wait),
            }
        }

        self.moving.retain(|(reply, deadline)| {
            if *deadline <= now {
                fail(reply, "the page didn't change");
            }
            *deadline > now
        });

        let (settled, settling): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.settling).into_iter().partition(|action| action.at + SETTLE <= now);
        self.settling = settling;
        for action in settled {
            if self.busy && self.loads != action.since.loads {
                // It led to another page: answer when that has loaded.
                self.await_load(action.reply, format!("{}\n", action.message));
            } else {
                text(&action.reply, format!("{}{}", action.message, self.fallout(&action.since)));
            }
        }

        let mut checks = Vec::new();
        self.waits.retain_mut(|wait| {
            if wait.deadline <= now {
                fail(&wait.reply, format!("gave up after {}s waiting for {}", wait.timeout, wait.what));
                return false;
            }
            if !wait.checking {
                wait.checking = true;
                checks.push((wait.id, wait.check.clone()));
            }
            true
        });
        for (id, script) in checks {
            self.evaluate(chrome, &script, Pending::WaitCheck(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let key = |name| parse_key(name).unwrap();
        assert_eq!(key("Enter").code, KeyCode::Enter);
        assert_eq!(key("ArrowDown").code, KeyCode::Down);
        assert_eq!(key("x").code, KeyCode::Char('x'));
        let select_all = key("Ctrl+a");
        assert!(select_all.ctrl && select_all.code == KeyCode::Char('a'));
        let back_tab = key("Shift+Tab");
        assert!(back_tab.shift && back_tab.code == KeyCode::Tab);
        assert!(parse_key("Hyper+a").is_none());
        assert!(parse_key("Entre").is_none());
    }

    #[test]
    fn sources() {
        assert_eq!(source("https://site.test/js/app.js?v=2", Some(11)), "app.js:12");
        assert_eq!(source("https://site.test/", Some(0)), "site.test:1");
        assert_eq!(source("", Some(3)), "");
    }

    #[test]
    fn shows_values() {
        assert_eq!(show(&json!({"type": "string", "value": "hi"})), "hi");
        assert_eq!(show(&json!({"type": "number", "value": 3})), "3");
        assert_eq!(show(&json!({"type": "object", "value": {"a": [1, 2]}})), r#"{"a":[1,2]}"#);
        assert_eq!(show(&json!({"type": "undefined"})), "undefined");
        let object = json!({"type": "object", "description": "Object", "preview": {"overflow": true, "properties": [
            {"name": "a", "type": "number", "value": "1"}, {"name": "b", "type": "string", "value": "x"},
        ]}});
        assert_eq!(show(&object), r#"{a: 1, b: "x", …}"#);
        let array = json!({"type": "object", "subtype": "array", "preview": {"subtype": "array", "properties": [
            {"name": "0", "type": "number", "value": "1"}, {"name": "1", "type": "number", "value": "2"},
        ]}});
        assert_eq!(show(&array), "[1, 2]");
        assert_eq!(show(&json!({"type": "function", "description": "function f() {}"})), "function f() {}");
    }

    #[test]
    fn thrown_errors_become_one_line() {
        let result = json!({"exceptionDetails": {"exception": {"description": "Error: no element matches #x\n    at <anonymous>:1:1"}}});
        assert_eq!(evaluated(Ok(&result)).unwrap_err(), "no element matches #x");
    }

    #[test]
    fn long_answers_are_cut() {
        let (reply, answer) = std::sync::mpsc::channel();
        text(&reply, "é".repeat(MAX_OUTPUT));
        let Ok(Ok(Output::Text(out))) = answer.recv() else { panic!() };
        assert!(out.len() < MAX_OUTPUT + 100);
        assert!(out.ends_with(&format!("({} more characters not shown)", MAX_OUTPUT / 2)));
    }

    #[test]
    fn console_and_network_follow_the_page() {
        let mut tools = Devtools::default();
        tools.log("error", "old".into(), String::new());
        tools.new_page("loader-1", "https://site.test/");
        tools.log("error", "new".into(), "app.js:3".into());
        tools.log("log", "fine".into(), String::new());
        assert_eq!(tools.errors(), 1);
        assert_eq!(tools.page_console().count(), 2);
        assert_eq!(tools.health(), "1 console error, no failed requests\nsee: kb ctl console --errors");

        // An action's answer carries the errors that followed it, and only those.
        let since = tools.baseline();
        assert_eq!(tools.fallout(&since), "");
        tools.log("error", "Uncaught TypeError: x is null".into(), "app.js:9".into());
        tools.log("log", "noise".into(), String::new());
        assert_eq!(
            tools.fallout(&since),
            "\nthis caused 1 console error:\n[error] Uncaught TypeError: x is null (app.js:9)"
        );
        assert_eq!(tools.console[1].line(), "--- page: https://site.test/ ---");
    }
}
