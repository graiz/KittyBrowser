//! The event loop: owns the terminal, the engine, and the UI state.
//!
//! Loop: wait for input or a message from the engine; update state; draw
//! only what changed. Row 1 is the top bar, the page image fills the rest.

use crate::control::{self, Output, Request};
use crate::devtools::{Asking, Devtools, Page};
use crate::dialog::{self, Dialog, Kind};
use crate::engine::chrome::{self, Chrome, PageEvent};
use crate::history::History;
use crate::keys::{Action, Bindings};
use crate::menu::{self, Menu};
use crate::picker::{self, Picker};
use crate::term::input::{Button, Event, Key, KeyCode, Mouse, MouseKind, Parser};
use crate::term::page::{PageImage, Route};
use crate::term::size::{self, Window};
use crate::term::{self, graphics};
use crate::ui::{self, AddressBar, Edit, Hit};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub struct Options {
    pub url: Option<String>,
    /// Device pixels per CSS pixel.
    pub scale: f64,
    /// Serve Chrome DevTools on a local port, for `devtools` (F12).
    pub devtools: bool,
    /// Tell pages to use a dark (true) or light colour scheme, rather than
    /// following the terminal's background.
    pub dark: Option<bool>,
    /// The name given with --name: the kb answers to it, and keeps its own
    /// profile, rather than taking a number.
    pub name: Option<String>,
}

/// How long a note in the bar stays.
const NOTICE: Duration = Duration::from_millis(2500);
/// How old the shadow's copy of the page may get while the page moves
/// under a box: its colour, darkened, changes too little to see sooner.
const SHADE_AGE: Duration = Duration::from_millis(500);

/// Find the element under a point and the pointer it calls for, as CSS
/// names it: what the page asks for, or where it says `auto`, a text cursor
/// over text and fields and an arrow elsewhere.
const POINTER: &str = "((x, y) => { let e = document.elementFromPoint(x, y); \
  while (e && e.shadowRoot) { const i = e.shadowRoot.elementFromPoint(x, y); if (!i || i === e) break; e = i; } \
  if (!e) return 'default'; \
  const c = getComputedStyle(e).cursor.split(',').pop().trim(); \
  if (c !== 'auto') return c; \
  if (e.isContentEditable || e.matches('textarea, input:not([type=button], [type=submit], [type=reset], \
    [type=checkbox], [type=radio], [type=range], [type=color], [type=file], [type=image])')) return 'text'; \
  const r = document.caretRangeFromPoint && document.caretRangeFromPoint(x, y); \
  if (r && r.startContainer.nodeType === 3) { const t = document.createRange(); t.selectNodeContents(r.startContainer); \
    for (const b of t.getClientRects()) if (x >= b.left && x <= b.right && y >= b.top && y <= b.bottom) return 'text'; } \
  return 'default'; })";

/// The pointer shapes terminals know, by their CSS names.
const POINTERS: &[&str] = &[
    "default",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "e-resize",
    "n-resize",
    "ne-resize",
    "nw-resize",
    "s-resize",
    "se-resize",
    "sw-resize",
    "w-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "col-resize",
    "row-resize",
    "all-scroll",
    "zoom-in",
    "zoom-out",
];

/// Inspect mode: outline the element under a point the way Chrome's
/// inspector does (margin, border, padding and content, with a label), and
/// remember it as the one a click would pick. Drawn into the page, in a
/// layer that ignores the mouse.
const HOVER: &str = "((x, y) => { let e = document.elementFromPoint(x, y); \
  while (e && e.shadowRoot) { const i = e.shadowRoot.elementFromPoint(x, y); if (!i || i === e) break; e = i; } \
  let root = document.getElementById('__kb_inspect'); \
  if (!root) { root = document.createElement('div'); root.id = '__kb_inspect'; \
    root.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647'; \
    for (let i = 0; i < 5; i++) root.appendChild(document.createElement('div')); \
    document.documentElement.appendChild(root); } \
  if (!e || root.contains(e)) return; \
  globalThis.__kbInspected = e; \
  const r = e.getBoundingClientRect(), s = getComputedStyle(e), n = v => Math.max(0, parseFloat(v) || 0); \
  const sides = what => ['Top', 'Right', 'Bottom', 'Left'].map(k => n(s[what.replace('*', k)])); \
  const m = sides('margin*'), b = sides('border*Width'), p = sides('padding*'); \
  const [margin, border, padding, content, label] = root.children; \
  const layer = (el, left, top, w, h, edge, color) => { el.style.cssText = 'position:fixed;box-sizing:content-box;' + \
    'left:' + left + 'px;top:' + top + 'px;width:' + Math.max(0, w) + 'px;height:' + Math.max(0, h) + 'px;' + \
    'border-style:solid;border-color:' + color + ';border-width:' + edge.map(v => v + 'px').join(' '); }; \
  layer(margin, r.left - m[3], r.top - m[0], r.width, r.height, m, 'rgba(246,178,107,.66)'); \
  layer(border, r.left, r.top, r.width - b[1] - b[3], r.height - b[0] - b[2], b, 'rgba(255,229,153,.66)'); \
  layer(padding, r.left + b[3], r.top + b[0], r.width - b[1] - b[3] - p[1] - p[3], r.height - b[0] - b[2] - p[0] - p[2], p, 'rgba(147,196,125,.55)'); \
  layer(content, r.left + b[3] + p[3], r.top + b[0] + p[0], r.width - b[1] - b[3] - p[1] - p[3], r.height - b[0] - b[2] - p[0] - p[2], [0, 0, 0, 0], 'transparent'); \
  content.style.background = 'rgba(111,168,220,.66)'; \
  const name = e.localName + (e.id ? '#' + e.id : '') + [...e.classList].slice(0, 3).map(c => '.' + c).join(''); \
  label.textContent = name + '  ' + Math.round(r.width) + ' × ' + Math.round(r.height); \
  const top = r.top - m[0] > 24 ? r.top - m[0] - 22 : Math.min(innerHeight - 22, r.bottom + m[2] + 4); \
  label.style.cssText = 'position:fixed;left:' + Math.max(0, Math.min(r.left, innerWidth - 200)) + 'px;top:' + top + 'px;' + \
    'background:#202124;color:#e8eaed;font:12px/20px ui-monospace,Menlo,monospace;padding:0 6px;border-radius:3px;' + \
    'white-space:nowrap;max-width:90vw;overflow:hidden;text-overflow:ellipsis'; })";

/// Take inspect mode's outline off the page.
const UNHOVER: &str = "document.getElementById('__kb_inspect')?.remove(); delete globalThis.__kbInspected;";

/// Describe the element inspected, as lines for a box: what it is, a
/// selector for it, its box and its main styles.
const DESCRIBE: &str = "(function() { const el = this, s = getComputedStyle(el), r = el.getBoundingClientRect(), n = Math.round; \
  const name = el.localName + (el.id ? '#' + el.id : '') + [...el.classList].map(c => '.' + c).join(''); \
  const path = []; \
  for (let e = el; e && e.nodeType === 1; e = e.parentElement) { \
    if (e.id && document.querySelectorAll('#' + CSS.escape(e.id)).length === 1) { path.unshift('#' + CSS.escape(e.id)); break; } \
    const same = e.parentElement ? [...e.parentElement.children].filter(c => c.localName === e.localName) : []; \
    path.unshift(e.localName + (same.length > 1 ? ':nth-of-type(' + (same.indexOf(e) + 1) + ')' : '')); \
    if (e.localName === 'body') break; } \
  const text = (el.innerText || el.value || '').replace(/\\s+/g, ' ').trim(); \
  const lines = ['\\u0002' + name, 'selector  ' + path.join(' > '), \
    'size      ' + n(r.width) + 'x' + n(r.height) + ' at ' + n(r.left + scrollX) + ',' + n(r.top + scrollY), \
    'display   ' + s.display + (s.position !== 'static' ? ', position ' + s.position : ''), \
    'font      ' + s.fontSize + ' ' + s.fontWeight + ' ' + s.fontFamily.split(',')[0], \
    'color     ' + s.color + ' on ' + s.backgroundColor, \
    'margin    ' + s.margin, 'padding   ' + s.padding]; \
  if (text) lines.push('text      ' + (text.length > 60 ? text.slice(0, 60) + '…' : text)); \
  return {lines, selector: path.join(' > ')}; }).call(globalThis.__kbInspected)";

/// How long `kb ctl pick` waits for the user: at most a minute, so that the
/// answer comes before `kb ctl` stops waiting for it.
const PICK_WAIT: f64 = 60.0;

/// `kb ctl pick --timeout`, in seconds.
fn pick_timeout(seconds: Option<&str>) -> Result<Duration, String> {
    let seconds = match seconds {
        Some(seconds) => {
            seconds.parse::<f64>().ok().filter(|s| *s > 0.0).ok_or("--timeout needs a number of seconds")?
        }
        None => PICK_WAIT,
    };
    Ok(Duration::from_secs_f64(seconds.min(PICK_WAIT)))
}

/// The answer to `kb ctl pick`: the element's details as inspect mode
/// shows them, its name first.
fn picked(lines: &[String]) -> String {
    let lines: Vec<String> = lines
        .iter()
        .map(|line| match line.strip_prefix('\u{2}') {
            Some(name) => format!("picked {name}"),
            None => line.clone(),
        })
        .collect();
    lines.join("\n")
}

/// Set while an editor has the terminal, so that the input thread leaves
/// the keys typed to it alone.
static INPUT_PAUSED: AtomicBool = AtomicBool::new(false);

/// The page's HTML as it is now, with its doctype, for View source.
const SOURCE: &str = "(document.doctype ? new XMLSerializer().serializeToString(document.doctype) + '\\n' : '') + \
  document.documentElement.outerHTML";

/// The page's timings, from its own Performance API, for Network
/// performance: when its first byte came, when the DOM was ready and when
/// it loaded (0 for not yet), in milliseconds from the start, and each
/// resource's time and bytes over the network.
const TIMINGS: &str = "(() => { const nav = performance.getEntriesByType('navigation')[0]; \
  const resources = performance.getEntriesByType('resource').map(r => ({url: r.name, kind: r.initiatorType, \
    ms: r.duration, bytes: r.transferSize})); \
  return {first_byte: nav ? nav.responseStart : 0, ready: nav ? nav.domContentLoadedEventEnd : 0, \
    load: nav ? nav.loadEventEnd : 0, bytes: nav ? nav.transferSize : 0, resources}; })()";

/// The slowest requests the network box lists.
const SLOWEST: usize = 12;

/// The lines of the network box, from `TIMINGS`, with `failed` requests
/// counted by kb: the page's milestones, its totals, and its slowest
/// requests.
fn network_lines(timings: &serde_json::Value, failed: usize) -> Vec<String> {
    let number = |value: &serde_json::Value| value.as_f64().unwrap_or(0.0);
    let seconds = |ms: f64| format!("{:.2} s", ms / 1000.0);
    let mut lines = vec![format!("{}Network performance", ui::TITLE_LINE)];
    let milestone =
        |name: &str, ms: f64| format!("{name} {}", if ms > 0.0 { seconds(ms) } else { "not yet".to_owned() });
    lines.push(
        [
            milestone("first byte", number(&timings["first_byte"])),
            milestone("DOM ready", number(&timings["ready"])),
            milestone("loaded", number(&timings["load"])),
        ]
        .join(" · "),
    );
    let mut resources: Vec<&serde_json::Value> = timings["resources"].as_array().into_iter().flatten().collect();
    let bytes = number(&timings["bytes"]) + resources.iter().map(|r| number(&r["bytes"])).sum::<f64>();
    let mut totals =
        format!("{} requests · {} over the network", resources.len() + 1, crate::downloads::size(bytes as u64));
    if failed > 0 {
        let _ = std::fmt::Write::write_fmt(&mut totals, format_args!(" · {failed} failed"));
    }
    lines.push(totals);
    if !resources.is_empty() {
        resources.sort_by(|a, b| number(&b["ms"]).total_cmp(&number(&a["ms"])));
        lines.push(String::new());
        lines.push("\tSlowest first (time, bytes, kind, address)".to_owned());
        for resource in resources.iter().take(SLOWEST) {
            // Nothing counted: from the cache, or another site that keeps
            // its sizes to itself.
            let size = match number(&resource["bytes"]) as u64 {
                0 => "–".to_owned(),
                bytes => crate::downloads::size(bytes),
            };
            let url = resource["url"].as_str().unwrap_or("");
            let url = url.split_once("://").map_or(url, |(_, rest)| rest);
            let kind = resource["kind"].as_str().unwrap_or("");
            lines.push(format!("{:>7}  {size:>8}  {kind:<6}  {url}", seconds(number(&resource["ms"]))));
        }
        if resources.len() > SLOWEST {
            lines.push(format!("\t…and {} more", resources.len() - SLOWEST));
        }
    }
    lines.push(String::new());
    lines.push("\tkb ctl network lists every request. Any key closes this.".to_owned());
    lines
}

/// A file name for a page's source, from its host: example.com.html.
fn source_name(url: &str) -> String {
    let host: String = ui::host_of(url)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    let host = host.trim_matches('.');
    format!("{}.html", if host.is_empty() { "page" } else { host })
}

enum Msg {
    Term(Event),
    TermClosed,
    Chrome(serde_json::Value),
    /// A command from `kb ctl` or the MCP server.
    Control(Request),
    ChromeClosed,
}

/// How often to check whether the pane changed size.
const TICK: Duration = Duration::from_millis(100);
/// Presses this close in time and place count as a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// How long after the last key, click or command the page counts as in use.
const ACTIVE: Duration = Duration::from_millis(500);
/// The least time between frames when the page isn't in use, and when the
/// pane doesn't have focus either.
const IDLE_GAP: Duration = Duration::from_millis(100);
const UNFOCUSED_GAP: Duration = Duration::from_millis(500);
/// Cell size to assume, in device pixels, until the terminal tells us.
const FALLBACK_CELL: (u32, u32) = (16, 34);

/// Append a line to the file named by KB_LOG, if set. For diagnosing
/// terminals: what size was detected, what frames arrived, where clicks went.
fn log(line: std::fmt::Arguments) {
    static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    let file = FILE.get_or_init(|| {
        let path = std::env::var_os("KB_LOG")?;
        File::options().create(true).append(true).open(path).ok().map(Mutex::new)
    });
    if let Some(file) = file {
        let _ = writeln!(file.lock().unwrap(), "{line}");
    }
}

/// How far one wheel tick scrolls the page, in CSS pixels across and down:
/// one column or one row of the pane it is shown in. Terminals report a
/// trackpad by sending a tick each time the fingers move a row's height (a
/// column's width across), and a mouse wheel as a few rows a notch, so this
/// keeps the page under the fingers and moves it as far as the terminal's
/// own scrollback would. `viewport` is in CSS pixels and `shown` in cells.
fn wheel_step(viewport: (u32, u32), shown: (u16, u16)) -> (f64, f64) {
    (viewport.0 as f64 / shown.0.max(1) as f64, viewport.1 as f64 / shown.1.max(1) as f64)
}

struct App {
    chrome: Chrome,
    scale: f64,
    window: Window,
    /// Cell size from the terminal's reply to `CSI 16 t`, for terminals
    /// whose window size carries no pixels.
    reported_cell: Option<(u32, u32)>,
    pixel_mouse: bool,
    bar: AddressBar,
    /// The find field, while it is open in the address bar's place.
    find: Option<AddressBar>,
    /// What was last searched for, offered again next time.
    find_text: String,
    history: History,
    devtools: Devtools,
    /// The page's viewport in CSS pixels.
    viewport: (u32, u32),
    /// A viewport size asked for with `kb ctl viewport`, in place of the
    /// pane's; a height of 0 means as tall as fills the pane.
    fixed: Option<(u32, u32)>,
    /// The cells the page image covers: the whole pane below the bar, or
    /// less when a fixed viewport has another shape.
    shown: (u16, u16),
    url: String,
    loading: bool,
    /// The newest frame, kept to redraw at a new size.
    frame: Option<String>,
    /// Whether the terminal reads frames from temporary files.
    by_file: bool,
    /// The file the terminal was asked whether it can read.
    file_query: Option<std::path::PathBuf>,
    /// The page's image on the screen.
    page: PageImage,
    /// Whether frames go through the pty only whole (`KB_TRANSFER=png`).
    png_only: bool,
    /// Browser zoom: 1 is 100%. It scales the device pixels per CSS pixel,
    /// so the page reflows as in a desktop browser and stays sharp.
    zoom: f64,
    /// The id of the request for the page's width, after Ctrl+0.
    fit_request: Option<u64>,
    /// Whether the help is shown, and where it was last drawn.
    help: bool,
    help_rect: Option<ui::Rect>,
    /// This kb's id or name, for `kb ctl -i`.
    instance: String,
    /// The logo, until the first page is ready to draw.
    splash: Option<crate::splash::Splash>,
    /// Whether a page other than the blank one has been committed, so that
    /// its frames replace the logo.
    committed: bool,
    /// When kb started, to turn the loading spinner by.
    started: Instant,
    /// Whether the pane has keyboard focus, as far as the terminal says.
    focused: bool,
    /// When the user or an agent last did something.
    last_input: Instant,
    /// When Chrome was last told it may send another frame.
    last_ack: Instant,
    /// Mouse buttons held, as a DevTools mask.
    buttons: u32,
    last_press: Option<(Instant, f64, f64, u32)>,
    bindings: Bindings,
    /// A note shown at the right of the bar for a moment: "Copied", ...
    notice: Option<(String, Instant)>,
    /// The request for the pointer shape under the mouse, and where to ask
    /// next once it is answered.
    pointer_request: Option<u64>,
    pointer_next: Option<(f64, f64)>,
    pointer: String,
    /// The terminal's text and background colours, once it has said.
    term_fg: Option<ui::Rgb>,
    term_bg: Option<ui::Rgb>,
    /// The colour scheme pages are told: set with --dark or --light or by
    /// an agent, or else following the terminal.
    dark: Option<bool>,
    /// Whether inspect mode is on, and the request describing the element
    /// picked.
    inspecting: bool,
    describe_request: Option<u64>,
    /// `kb ctl pick` waiting for the user to pick an element, and when each
    /// gives up.
    picks: Vec<(control::Reply, Instant)>,
    /// The ⋮ menu while it is open, and where it was drawn.
    menu: Option<Menu>,
    menu_rect: Option<ui::Rect>,
    /// The page's HTML, asked for to view it in the user's editor.
    source_request: Option<u64>,
    /// The page's timings, asked for to show its network performance.
    network_request: Option<u64>,
    /// Lines for the box, in place of the help: an element's details.
    panel: Option<Vec<String>>,
    /// Whether the bar is asking to quit, after a click on its close
    /// button, and whether it was answered yes.
    asking_quit: bool,
    quit_clicked: bool,
    /// What the page is asking the user, shown over it: a dialog, and a
    /// file chooser (under the dialog, if both), and where they were drawn.
    dialog: Option<Dialog>,
    picker: Option<Picker>,
    modal_rects: Vec<ui::Rect>,
    /// The folder the file chooser was last left in, to open there next.
    last_folder: Option<std::path::PathBuf>,
    /// Frames from Chrome so far, to tell one from the next.
    frames: u64,
    /// The frame decoded, for the shadow the boxes cast on the page: which
    /// frame it was, and when it was decoded.
    shade: Option<(u64, Instant, crate::modal::Shade)>,
}

pub fn run(options: Options) -> Result<(), String> {
    // Checked before the terminal is taken over, so the reason is printed
    // plainly instead of behind a blank pane.
    crate::engine::chrome::find_binary().map_err(|why| format!("{why}\nkb doctor checks your setup."))?;
    let window = size::window().ok_or("can't read the terminal size; to run kb without a pane, use kb --headless")?;
    // Before anything is written to the terminal: a failure after that
    // leaves the terminal's replies to our queries for the shell to read.
    let claim = control::claim(options.name.as_deref())?;
    let profile = claim.profile()?;

    // Asked last, just before raw mode, with nothing left that can fail:
    // when the terminal answers too late for the question, kb's input
    // reader takes the replies instead of the shell.
    if std::env::var_os("KB_FORCE").is_none() && term::draws_graphics(crate::doctor::ASK_KB) == Some(false) {
        claim.socket.remove();
        eprint!("{}", crate::doctor::no_graphics());
        // Explained already; main has nothing to add.
        return Err(String::new());
    }
    term::enter().map_err(|e| e.to_string())?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));
    let socket = claim.socket.clone();
    let result = browse(options, &profile, window, claim);
    socket.remove();
    term::leave();
    result
}

fn browse(options: Options, profile: &std::path::Path, window: Window, claim: control::Claim) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let instance = control::id().unwrap_or_default();
    let commands = tx.clone();
    let socket = claim.socket.clone();
    claim.listen(move |request| commands.send(Msg::Control(request)).is_ok());
    let input = tx.clone();
    std::thread::spawn(move || {
        let mut parser = Parser::default();
        let mut buf = [0; 4096];
        loop {
            // While an editor has the terminal, its keys are its own. The
            // descriptor is read directly, with nothing buffered in between,
            // so that what poll says is there is all there is.
            if INPUT_PAUSED.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            let mut ready = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut ready, 1, 50) } <= 0 || INPUT_PAUSED.load(Ordering::SeqCst) {
                continue;
            }
            let read = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            match usize::try_from(read) {
                Ok(n) if n > 0 => {
                    for event in parser.feed(&buf[..n]) {
                        if input.send(Msg::Term(event)).is_err() {
                            return;
                        }
                    }
                }
                // Interrupted, as by a resize: read again.
                Err(_) if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
                _ => {
                    let _ = input.send(Msg::TermClosed);
                    return;
                }
            }
        }
    });

    let (chrome, mut receiver) = Chrome::launch(profile, options.scale, options.devtools)?;
    std::thread::spawn(move || {
        while let Ok(Some(message)) = receiver.recv() {
            if tx.send(Msg::Chrome(message)).is_err() {
                return;
            }
        }
        let _ = tx.send(Msg::ChromeClosed);
    });

    let mut app = App {
        chrome,
        scale: options.scale,
        window,
        reported_cell: None,
        pixel_mouse: false,
        bar: AddressBar::default(),
        find: None,
        find_text: String::new(),
        history: History::load(crate::data_dir()?.join("history")),
        devtools: Devtools::in_pane(),
        viewport: (0, 0),
        fixed: None,
        shown: (window.cols, window.rows.saturating_sub(1).max(1)),
        url: String::new(),
        loading: false,
        frame: None,
        by_file: false,
        file_query: None,
        page: Default::default(),
        png_only: false,
        zoom: 1.0,
        fit_request: None,
        help: false,
        help_rect: None,
        instance,
        splash: crate::splash::Splash::new(),
        committed: false,
        started: Instant::now(),
        focused: true,
        last_input: Instant::now(),
        last_ack: Instant::now(),
        buttons: 0,
        last_press: None,
        bindings: Bindings::load(crate::data_dir().ok().map(|dir| dir.join("keys"))),
        notice: None,
        pointer_request: None,
        pointer_next: None,
        pointer: "default".to_owned(),
        term_fg: None,
        term_bg: None,
        dark: options.dark,
        inspecting: false,
        describe_request: None,
        picks: Vec::new(),
        menu: None,
        menu_rect: None,
        source_request: None,
        network_request: None,
        panel: None,
        asking_quit: false,
        quit_clicked: false,
        dialog: None,
        picker: None,
        modal_rects: Vec::new(),
        last_folder: None,
        frames: 0,
        shade: None,
    };
    if let Some(dark) = options.dark {
        app.chrome.color_scheme(Some(dark));
    }
    if let Some(error) = app.bindings.errors.first() {
        app.note(&format!("~/.kittybrowser/keys {error}"));
    }
    app.layout();
    // KB_TRANSFER=pty keeps frames on the pty, should files misbehave;
    // KB_TRANSFER=png also sends them whole, for a terminal that can't
    // place parts of images.
    match std::env::var("KB_TRANSFER").as_deref() {
        Ok("png") => app.png_only = true,
        Ok("pty") => {}
        _ => app.file_query = graphics::ask_about_files(),
    }
    app.chrome.hold_acks();
    app.chrome.start_screencast();
    match options.url.as_deref().and_then(ui::normalize_url) {
        Some(url) => app.navigate(&url),
        None => app.bar.focus(),
    }
    app.draw(false);

    let mut checked = Instant::now();
    let result = loop {
        let mut messages = Vec::new();
        // Wake in time to let Chrome send its next frame.
        let mut wait = match app.chrome.ack_pending() {
            true => app.frame_gap().saturating_sub(app.last_ack.elapsed()).min(TICK),
            false => TICK,
        };
        if let Some(splash_wait) =
            app.splash.as_ref().filter(|_| app.by_file).and_then(|splash| splash.wait(app.loading))
        {
            wait = wait.min(splash_wait);
        }
        match rx.recv_timeout(wait) {
            Ok(message) => messages.push(message),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
        messages.extend(rx.try_iter());

        let (mut new_frame, mut redraw) = (false, false);
        let mut quit = None;
        let count = messages.len();
        if messages.iter().any(|message| {
            matches!(message, Msg::Control(_) | Msg::Term(Event::Key(_) | Event::Mouse(_) | Event::Paste(_)))
        }) {
            app.last_input = Instant::now();
        }
        let mut messages = messages.into_iter().enumerate().peekable();
        while let Some((index, message)) = messages.next() {
            match message {
                Msg::Term(Event::Mouse(mut mouse)) => {
                    // A burst of wheel ticks scrolls once, by all of them,
                    // rather than sending Chrome a scroll for each row.
                    while let Some((_, Msg::Term(Event::Mouse(next)))) = messages.peek() {
                        let Some(sum) = mouse.add_wheel(next) else { break };
                        mouse = sum;
                        messages.next();
                    }
                    // A burst of moves collapses to the last one.
                    let next_is_move = matches!(
                        messages.peek(),
                        Some((_, Msg::Term(Event::Mouse(Mouse { kind: MouseKind::Move, .. }))))
                    );
                    if !(mouse.kind == MouseKind::Move && next_is_move && index + 1 < count) {
                        redraw |= app.on_mouse(mouse);
                    }
                    if app.quit_clicked {
                        quit = Some(Ok(()));
                        break;
                    }
                }
                Msg::Term(Event::Key(key)) if app.dialog.is_some() || app.picker.is_some() => {
                    // What the page asks has the keys, all but quitting.
                    if app.bindings.action(&key, true) == Some(Action::Quit) {
                        quit = Some(Ok(()));
                        break;
                    }
                    app.modal_key(&key);
                    redraw = true;
                }
                Msg::Term(Event::Key(key)) if app.asking_quit => {
                    match key.code {
                        KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                            quit = Some(Ok(()));
                            break;
                        }
                        _ if app.bindings.action(&key, false) == Some(Action::Quit) => {
                            quit = Some(Ok(()));
                            break;
                        }
                        KeyCode::Char('n' | 'N') | KeyCode::Esc => app.asking_quit = false,
                        _ => {}
                    }
                    redraw = true;
                }
                Msg::Term(Event::Key(key)) => {
                    // Any key closes the help, and does nothing else.
                    if app.help && app.bindings.action(&key, false) != Some(Action::Quit) {
                        app.close_help();
                        continue;
                    }
                    if app.on_key(key) {
                        quit = Some(Ok(()));
                        break;
                    }
                    redraw = true;
                }
                Msg::Term(Event::Color { slot, rgb }) => {
                    log(format_args!("terminal colour {slot}: {rgb:?}"));
                    match slot {
                        10 => app.term_fg = Some(rgb),
                        _ => app.term_bg = Some(rgb),
                    }
                    app.follow_terminal_colors();
                    redraw = true;
                }
                Msg::Term(Event::Paste(text)) => {
                    if let Some(dialog) = &mut app.dialog {
                        dialog.paste(&text);
                        redraw = true;
                    } else if let Some(picker) = &mut app.picker {
                        picker.paste(&text);
                        redraw = true;
                    } else if app.find.is_some() {
                        app.find_edit(|field| field.insert(&text));
                        redraw = true;
                    } else if app.bar.focused() {
                        app.bar.insert(&text);
                        redraw = true;
                    } else {
                        app.chrome.insert_text(&text);
                    }
                }
                Msg::Term(Event::CellSize { width, height }) => {
                    log(format_args!("terminal reports cell {width}x{height}"));
                    if app.reported_cell != Some((width, height)) {
                        app.reported_cell = Some((width, height));
                        app.layout();
                        new_frame = true;
                    }
                }
                Msg::Term(Event::Graphics { id, ok }) => {
                    log(format_args!("graphics reply for image {id}: {ok}"));
                    if !ok && app.page.lost(id) {
                        // The terminal dropped an image the page was drawn
                        // with: start again from the whole page.
                        term::write(graphics::DELETE);
                        new_frame = true;
                    }
                    if id == graphics::FILE_QUERY_ID {
                        app.by_file = ok;
                        // If it said no, it didn't delete the file either.
                        if let Some(path) = app.file_query.take() {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
                Msg::Term(Event::Focus(focused)) => {
                    log(format_args!("focused: {focused}"));
                    app.focused = focused;
                }
                Msg::Term(Event::PixelMouse(on)) => {
                    log(format_args!("pixel mouse: {on}"));
                    app.pixel_mouse = on;
                }
                Msg::Control(request) => {
                    log(format_args!("control: {} {}", request.command, request.args));
                    if request.command == "quit" {
                        let cancelled = app.devtools.cancel_downloads(&mut app.chrome);
                        let _ =
                            request.reply.send(Ok(Output::Text(format!("kb is quitting{cancelled}; its pane closes"))));
                        quit = Some(Ok(()));
                        break;
                    }
                    if request.command == "pick" {
                        match pick_timeout(request.args["timeout"].as_str()) {
                            Ok(timeout) => {
                                app.close_help();
                                app.inspecting = true;
                                app.picks.push((request.reply, Instant::now() + timeout));
                                redraw = true;
                            }
                            Err(error) => {
                                let _ = request.reply.send(Err(error));
                            }
                        }
                        continue;
                    }
                    if request.command == "scheme" {
                        let _ = request.reply.send(app.set_scheme(request.args["scheme"].as_str().unwrap_or("")));
                        continue;
                    }
                    if request.command == "window" {
                        let (answer, event) = crate::devtools::window(&request.args, &mut app.chrome, &app.devtools);
                        let _ = request.reply.send(answer);
                        if let Some(PageEvent::Shown(url)) = event {
                            app.shown(url);
                            redraw = true;
                        }
                        continue;
                    }
                    if request.command == "viewport" {
                        // The one command about the pane rather than the page.
                        let _ = request.reply.send(app.set_viewport(request.args["size"].as_str().unwrap_or("")));
                        app.clear();
                        new_frame = true;
                        continue;
                    }
                    let page = Page {
                        url: &app.url,
                        loading: app.loading,
                        viewport: app.viewport,
                        scale: app.scale,
                        zoom: app.zoom,
                        viewport_set: app.fixed.is_some(),
                    };
                    app.devtools.command(request, &mut app.chrome, page);
                }
                Msg::Chrome(message) => {
                    if app.fit_request.is_some() && message["id"].as_u64() == app.fit_request {
                        app.fit_request = None;
                        app.fit(message["result"]["result"]["value"].as_f64());
                        new_frame = true;
                        continue;
                    }
                    if let Some(id) = message["id"].as_u64() {
                        if app.reply(id, &message) {
                            redraw = true;
                            continue;
                        }
                    }
                    if !app.chrome.shown(&message) {
                        // A window behind the one shown can still ask something.
                        app.devtools.dialog_event(&message, &mut app.chrome);
                        continue;
                    }
                    app.devtools.observe(&message, &mut app.chrome);
                    // The bar shows how a download is going.
                    redraw |= message["method"].as_str().is_some_and(|method| method.starts_with("Browser.download"));
                    // A small file is saved between redraws: say it was, as
                    // Chrome's download bubble does.
                    if let Some(note) = app.devtools.take_download_note() {
                        app.note(&note);
                    }
                    match app.chrome.handle(message) {
                        Some(PageEvent::Frame(png)) => {
                            let fits = app.fits(&png);
                            log(format_args!(
                                "frame {:?}, {} bytes, fits: {fits}",
                                graphics::png_size(&png),
                                png.len()
                            ));
                            // A frame rendered for another size (the pane's
                            // first guess, or the size before a resize) would be
                            // stretched to fill the pane: wait for the right one.
                            // Some pages repaint without changing: the same
                            // frame again is nothing to draw.
                            if fits && app.frame.as_deref() != Some(png.as_str()) {
                                app.frame = Some(png);
                                app.frames += 1;
                                // The blank page that comes first stays under the logo.
                                new_frame = app.splash.is_none() || app.committed;
                            }
                        }
                        Some(PageEvent::Url(url)) => {
                            app.went_to(url);
                            redraw = true;
                        }
                        Some(PageEvent::Shown(url)) => {
                            app.shown(url);
                            redraw = true;
                        }
                        Some(PageEvent::Loading(loading)) => {
                            app.loading = loading;
                            redraw = true;
                        }
                        Some(PageEvent::Copy(text)) => {
                            app.copy(&text);
                            redraw = true;
                        }
                        None => {}
                    }
                }
                Msg::TermClosed => {
                    quit = Some(Ok(()));
                    break;
                }
                Msg::ChromeClosed => {
                    quit = Some(Err("Chrome exited".to_owned()));
                    break;
                }
            }
        }
        if let Some(result) = quit {
            break result;
        }
        redraw |= app.sync_questions();

        if checked.elapsed() >= TICK {
            checked = Instant::now();
            // The spinner in the bar turns while a page loads.
            redraw |= app.loading;
            app.history.save_if_due();
            app.devtools.tick(&mut app.chrome);
            if app.bindings.reload_if_changed() {
                let note = match app.bindings.errors.first() {
                    Some(error) => format!("keys {error}"),
                    None => "Key bindings reloaded".to_owned(),
                };
                app.note(&note);
                redraw = true;
            }
            if app.notice.as_ref().is_some_and(|(_, at)| at.elapsed() >= NOTICE) {
                app.notice = None;
                redraw = true;
            }
            if app.picks.iter().any(|(_, deadline)| *deadline <= Instant::now()) {
                app.expire_picks();
                redraw = true;
            }
            if let Some(window) = size::window().filter(|w| *w != app.window) {
                app.window = window;
                if window.width_px == 0 {
                    term::query_cell_size();
                }
                app.layout();
                // Whatever was on screen is at the old size: start clean.
                app.clear();
                new_frame = true;
            }
        }
        if new_frame || redraw {
            app.draw(new_frame);
        }
        if app.by_file && app.splash.as_ref().is_some_and(|splash| splash.due(app.loading)) {
            app.draw_splash();
        }
        // One frame per gap, or all of them while the page is in use.
        while app.chrome.ack_pending() && app.last_ack.elapsed() >= app.frame_gap() {
            app.chrome.ack();
            if !app.frame_gap().is_zero() {
                app.last_ack = Instant::now();
            }
        }
    };
    socket.remove();
    app.history.save();
    app.page.close();
    app.devtools.cancel_downloads(&mut app.chrome);
    app.chrome.close();
    result
}

impl App {
    /// One cell in device pixels.
    fn cell(&self) -> (u32, u32) {
        let w = self.window;
        if w.width_px > 0 && w.height_px > 0 {
            ((w.width_px / w.cols) as u32, (w.height_px / w.rows) as u32)
        } else {
            self.reported_cell.unwrap_or(FALLBACK_CELL)
        }
    }

    /// The least time between frames from Chrome. None while someone is
    /// using the page; otherwise a page that moves by itself gets ten frames
    /// a second, or two if the pane isn't the one in use, and Chrome is
    /// spared drawing and encoding the rest.
    fn frame_gap(&self) -> Duration {
        if self.last_input.elapsed() < ACTIVE {
            Duration::ZERO
        } else if self.focused {
            IDLE_GAP
        } else {
            UNFOCUSED_GAP
        }
    }

    /// Wipe the screen, images included. The help and what the page asks,
    /// if shown, are drawn again with the next frame.
    fn clear(&mut self) {
        term::write(b"\x1b[2J");
        term::write(graphics::DELETE);
        self.page.forget();
        self.help_rect = None;
        self.modal_rects.clear();
    }

    fn page_rows(&self) -> u16 {
        self.window.rows.saturating_sub(1).max(1)
    }

    /// Size the page's viewport to the cells below the top bar.
    fn layout(&mut self) {
        let (cell_w, cell_h) = self.cell();
        let (cols, rows) = (self.window.cols, self.page_rows());
        // The space for the page, in device pixels.
        let (space_w, space_h) = (cols as f64 * cell_w as f64, rows as f64 * cell_h as f64);
        let (width, height) = match self.fixed {
            // A width alone: as tall as fills the pane once that width fits it.
            Some((width, 0)) => {
                let fit = (space_w / (width as f64 * self.page_scale())).min(1.0);
                let height = (space_h / (self.page_scale() * fit)).floor();
                let cells = |pixels: f64, cell: u32, most: u16| ((pixels / cell as f64).round() as u16).clamp(1, most);
                let shown_w = cells(width as f64 * self.page_scale() * fit, cell_w, cols);
                self.shown = (shown_w, rows);
                (width as f64, height)
            }
            Some((width, height)) => {
                // Shown at its own size if it fits, else shrunk to fit,
                // keeping its shape as nearly as whole cells allow.
                let (frame_w, frame_h) = (width as f64 * self.page_scale(), height as f64 * self.page_scale());
                let fit = (space_w / frame_w).min(space_h / frame_h).min(1.0);
                let cells =
                    |pixels: f64, cell: u32, most: u16| ((pixels * fit / cell as f64).round() as u16).clamp(1, most);
                self.shown = (cells(frame_w, cell_w, cols), cells(frame_h, cell_h, rows));
                (width as f64, height as f64)
            }
            None => {
                self.shown = (cols, rows);
                // Rounded up, so a frame always covers the cells and can be
                // cut into tiles.
                ((space_w / self.page_scale()).ceil(), (space_h / self.page_scale()).ceil())
            }
        };
        log(format_args!(
            "window {:?}, cell {cell_w}x{cell_h} (reported {:?}), viewport {width}x{height}, shown in {:?} cells",
            self.window, self.reported_cell, self.shown
        ));
        self.viewport = (width.round().max(1.0) as u32, height.round().max(1.0) as u32);
        if self.frame.as_deref().is_some_and(|png| !self.fits(png)) {
            self.frame = None;
        }
        self.chrome.resize(self.viewport.0, self.viewport.1, self.scale);
    }

    /// Pane pixels per CSS pixel, zoom included.
    fn page_scale(&self) -> f64 {
        self.scale * self.zoom
    }

    /// Step the zoom up or down through the levels desktop browsers use.
    fn step_zoom(&mut self, up: bool) {
        const LEVELS: [f64; 15] = [0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
        let next = match up {
            true => LEVELS.iter().find(|level| **level > self.zoom + 0.001),
            false => LEVELS.iter().rev().find(|level| **level < self.zoom - 0.001),
        };
        if let Some(level) = next {
            self.set_zoom(*level);
        }
    }

    fn set_zoom(&mut self, zoom: f64) {
        self.zoom = zoom;
        self.layout();
        self.clear();
    }

    /// Ctrl+0: back to 100%, then ask how wide the page is there, so that
    /// `fit` can zoom out a page too wide for the pane.
    fn zoom_to_fit(&mut self) {
        self.set_zoom(1.0);
        self.fit_request = Some(self.chrome.evaluate_value("document.documentElement.scrollWidth"));
    }

    /// Zoom out so a page `width` CSS pixels wide fits the pane, if it is
    /// wider than the pane at 100%.
    fn fit(&mut self, width: Option<f64>) {
        let Some(width) = width.filter(|width| *width > self.viewport.0 as f64 + 1.0) else { return };
        let zoom = (self.viewport.0 as f64 / width).max(0.25);
        log(format_args!("zoom to fit a page {width} wide: {zoom}"));
        self.set_zoom(zoom);
    }

    /// The help: keys, and what kb knows about the pane and page.
    fn help_lines(&self) -> Vec<String> {
        let mut banner = Vec::new();
        if ui::ART.iter().all(|line| line.chars().count() + 4 <= self.window.cols as usize) {
            banner.extend(ui::ART.iter().map(|line| format!("{}{line}", ui::ART_LINE)));
            banner.push(String::new());
        }
        let mut lines = Vec::new();
        lines.push(format!("{}kb {}: a browser in your terminal", ui::TITLE_LINE, env!("CARGO_PKG_VERSION")));
        lines.push(String::new());
        // The keys as bound now, the first two of each.
        let keys = |action: Action| {
            let all = self.bindings.keys_for(action);
            all.iter().take(2).cloned().collect::<Vec<_>>().join(", ")
        };
        let rows = [
            (Action::Address, "edit the address"),
            (Action::Find, "find on the page; Enter, Shift+Enter: next, previous"),
            (Action::Back, "back (or the mouse's back button)"),
            (Action::Forward, "forward"),
            (Action::Reload, "reload"),
            (Action::Copy, "copy the selection, or the address"),
            (Action::ZoomIn, "zoom in"),
            (Action::ZoomOut, "zoom out"),
            (Action::ZoomReset, "zoom to 100%, or out until the page fits"),
            (Action::Inspect, "inspect an element"),
            (Action::Devtools, "Chrome DevTools (start kb with --devtools)"),
            (Action::NextWindow, "next window: a popup, a new tab (‹ › in the bar)"),
            (Action::PreviousWindow, "previous window"),
            (Action::CloseWindow, "close the window shown (✕); on the last one, ask to quit"),
            (Action::Help, "this help"),
            (Action::Quit, "quit (the ✕ in the bar closes a popup, or asks first)"),
        ]
        .map(|(action, does)| (keys(action), does))
        .into_iter()
        .filter(|(keys, _)| !keys.is_empty())
        .collect::<Vec<_>>();
        // As wide as the widest keys, so each description starts in line.
        let width = rows.iter().map(|(keys, _)| keys.chars().count() + 2).max().unwrap_or(0);
        for (keys, does) in rows {
            lines.push(format!("{keys:<width$}{does}"));
        }
        lines.push(String::new());
        lines.push("\tChange keys in ~/.kittybrowser/keys; kb keys lists them.".to_owned());
        let mut info = vec![String::new()];
        let (cell_w, cell_h) = self.cell();
        for (name, value) in [
            ("page", if self.url.is_empty() { "(none)".to_owned() } else { self.url.clone() }),
            ("zoom", format!("{:.0}%", self.zoom * 100.0)),
            ("viewport", format!("{}x{} CSS pixels", self.viewport.0, self.viewport.1)),
            ("pane", format!("{}x{} cells of {cell_w}x{cell_h} pixels", self.window.cols, self.window.rows)),
            (
                "frames",
                match (self.by_file, self.png_only) {
                    (true, _) => "by file, changed blocks only".to_owned(),
                    (false, false) => "through the pty, what moved or changed".to_owned(),
                    (false, true) => "through the pty, whole PNG frames (KB_TRANSFER=png)".to_owned(),
                },
            ),
            ("agents", format!("kb ctl -i {} ...   (kb ctl --help)", self.instance)),
            ("trouble", "kb doctor checks the terminal and Chrome".to_owned()),
        ] {
            info.push(format!("\t{name:<10}{value}"));
        }
        let footer = [String::new(), "\tAny key closes this.                                 by @graiz".to_owned()];

        // In a short pane the banner goes first, then the details, so the
        // keys and the footer always show.
        let room = self.page_rows().saturating_sub(2) as usize;
        let mut all = Vec::new();
        for (with_banner, with_info) in [(true, true), (false, true), (false, false)] {
            all.clear();
            if with_banner {
                all.extend(banner.iter().cloned());
            }
            all.extend(lines.iter().cloned());
            if with_info {
                all.extend(info.iter().cloned());
            }
            all.extend(footer.iter().cloned());
            if all.len() <= room {
                break;
            }
        }
        all
    }

    fn draw_splash(&mut self) {
        let cell = self.cell();
        let (cols, rows) = (self.window.cols, self.page_rows());
        let Some(splash) = &mut self.splash else { return };
        // The bar isn't drawn again here, so the cursor goes back to the
        // address field, not left by the logo for each frame.
        let mut out = b"\x1b[?2026h\x1b7".to_vec();
        if self.help || self.menu.is_some() {
            crate::splash::Splash::hide(&mut out);
        } else {
            splash.draw(&mut out, cols, rows, cell, 2, self.loading);
        }
        out.extend_from_slice(b"\x1b8");
        // The splash paints the pane black, help, dialogs and all.
        self.help_rect = None;
        self.modal_rects.clear();
        self.render_help(&mut out);
        self.render_menu(&mut out);
        self.render_modal(&mut out);
        out.extend_from_slice(b"\x1b[?2026l");
        term::write(&out);
    }

    /// Draw the help box, if it is open, over whatever is below the bar.
    fn render_help(&mut self, out: &mut Vec<u8>) {
        if !self.help {
            return;
        }
        let lines = self.panel.clone().unwrap_or_else(|| self.help_lines());
        if let Some(old) = self.help_rect.take() {
            ui::blank(out, old);
        }
        self.help_rect = Some(ui::render_box(out, &lines, self.window.cols, self.window.rows));
        // The box, not the address field, has the user's attention.
        out.extend_from_slice(b"\x1b[?25l");
    }

    /// Another window is shown, at `url`. Whether it is loading comes in
    /// its own events.
    fn shown(&mut self, url: String) {
        self.loading = false;
        self.went_to(url);
    }

    /// Close the window shown, as a browser's tab closes; the last one is
    /// kb itself, so that asks first.
    fn close_window(&mut self) {
        if self.chrome.windows().1 > 1 {
            self.chrome.close_window();
        } else {
            self.asking_quit = true;
        }
    }

    fn switch_window(&mut self, step: isize) {
        if let Some(PageEvent::Shown(url)) = self.chrome.switch(step) {
            self.shown(url);
        }
    }

    /// The page shown is at `url` now.
    fn went_to(&mut self, url: String) {
        log(format_args!("url {url}"));
        self.committed |= url != "about:blank";
        self.history.record(&url);
        self.url = if url == "about:blank" { String::new() } else { url };
        // An empty field waiting for an address gives way to
        // a page opened some other way (by an agent, a link).
        if self.bar.focused() && self.bar.text().is_empty() && !self.url.is_empty() {
            self.bar.blur(&self.url);
        } else if !self.bar.focused() {
            self.bar.set_text(&self.url);
        }
    }

    /// Show the help box, or the panel set for it. The logo, an image,
    /// would cover it, so it is hidden meanwhile.
    fn open_help(&mut self) {
        self.help = true;
        if let Some(splash) = &mut self.splash {
            splash.again();
        }
    }

    fn close_help(&mut self) {
        self.help = false;
        if let Some(splash) = &mut self.splash {
            splash.again();
        }
        if self.panel.take().is_some() {
            self.chrome.evaluate(UNHOVER);
        }
        if let Some(rect) = self.help_rect.take() {
            let mut out = Vec::new();
            ui::blank(&mut out, rect);
            term::write(&out);
        }
    }

    /// Carry out `kb ctl viewport`: lay the page out at a given size, or go
    /// back to the pane's.
    fn set_viewport(&mut self, size: &str) -> Result<Output, String> {
        let parse = |text: &str| text.parse::<u32>().ok().filter(|n| (50..=10_000).contains(n));
        self.fixed = match size.split_once(['x', 'X']) {
            _ if matches!(size, "reset" | "pane") => None,
            Some((width, height)) => match (parse(width), parse(height)) {
                (Some(width), Some(height)) => Some((width, height)),
                _ => return Err(format!("{size:?} isn't a size; give WIDTHxHEIGHT in CSS pixels, such as 1280x800")),
            },
            // A width alone: as tall as fills the pane (see layout).
            None => match parse(size) {
                Some(width) => Some((width, 0)),
                None => return Err(format!("{size:?} isn't a size; give WIDTH, WIDTHxHEIGHT, or reset")),
            },
        };
        self.layout();
        let (width, height) = self.viewport;
        let (cell_w, _) = self.cell();
        let percent = (self.shown.0 as f64 * cell_w as f64 / (width as f64 * self.page_scale()) * 100.0).round();
        Ok(Output::Text(match self.fixed {
            Some(_) if percent < 99.0 => {
                format!("the viewport is {width}x{height} CSS pixels, shown at {percent}% to fit the pane")
            }
            Some(_) => format!("the viewport is {width}x{height} CSS pixels"),
            None => format!("the viewport follows the pane again: {width}x{height} CSS pixels"),
        }))
    }

    /// Whether a frame was rendered at the size the page is laid out at now,
    /// give or take rounding.
    fn fits(&self, png: &str) -> bool {
        let Some((width, height)) = graphics::png_size(png) else { return false };
        let close = |actual: u32, css: u32| {
            // Chrome renders at its launch scale whatever the zoom: zoomed
            // in, the frame has fewer pixels than the cells and is stretched
            // to fill them; zoomed out, more, and is shrunk.
            let expected = css as f64 * self.scale;
            (actual as f64 - expected).abs() <= (expected / 100.0).max(2.0)
        };
        close(width, self.viewport.0) && close(height, self.viewport.1)
    }

    fn navigate(&mut self, url: &str) {
        self.url = url.to_owned();
        self.loading = true;
        self.bar.blur(url);
        self.chrome.navigate(url);
    }

    fn draw(&mut self, frame: bool) {
        // Synchronized update, so the terminal shows the result in one go.
        let mut out = b"\x1b[?2026h".to_vec();
        let cell = self.cell();
        let frame = frame && (self.splash.is_none() || self.committed);
        if frame && self.splash.take().is_some() {
            crate::splash::Splash::remove(&mut out, 2);
        }
        if let Some(png) = self.frame.as_deref().filter(|_| frame) {
            let (cols, rows) = self.shown;
            let scaled = self.fixed.is_some();
            let route = match (self.by_file, self.png_only) {
                (true, _) => Route::Files,
                (false, false) => Route::Layers,
                (false, true) => Route::Png,
            };
            let drawn = self.page.draw(&mut out, png, route, scaled, cols, rows, cell, 2);
            log(format_args!("drew {drawn}"));
        }
        let cols = self.window.cols as usize;
        let badge = self.badge();
        let badge = (!badge.is_empty()).then_some(badge.as_str());
        match &mut self.find {
            _ if self.asking_quit => ui::render_question(&mut out, ui::QUIT_QUESTION, cols),
            Some(field) => {
                ui::render_bar(&mut out, field, cols, self.loading, Some("Find"), badge, self.chrome.windows())
            }
            None => ui::render_bar(&mut out, &mut self.bar, cols, self.loading, None, badge, self.chrome.windows()),
        }
        self.render_help(&mut out);
        self.render_menu(&mut out);
        self.render_modal(&mut out);
        out.extend_from_slice(b"\x1b[?2026l");
        term::write(&out);
    }

    /// Draw what the page asks over it: the file chooser, then a dialog on
    /// top. The cursor goes to the field being typed in, if any.
    fn render_modal(&mut self, out: &mut Vec<u8>) {
        for rect in self.modal_rects.drain(..) {
            ui::blank(out, rect);
        }
        let (cols, rows) = (self.window.cols, self.window.rows);
        if self.picker.is_none() && self.dialog.is_none() {
            // Not kept while nothing needs it: a frame is megabytes decoded.
            self.shade = None;
            return;
        }
        self.update_shade();
        let shade = self.shade.as_ref().map(|(_, _, shade)| shade);
        let mut cursor = None;
        if let Some(picker) = &mut self.picker {
            let (rect, at) = picker.render(out, cols, rows, shade);
            self.modal_rects.push(rect);
            cursor = at;
        }
        if let Some(dialog) = &mut self.dialog {
            let (rect, at) = dialog.render(out, cols, rows, shade);
            self.modal_rects.push(rect);
            cursor = at;
        }
        if self.modal_rects.is_empty() {
            return;
        }
        match cursor {
            Some((col, row)) => out.extend_from_slice(format!("\x1b[{row};{col}H\x1b[6 q\x1b[?25h").as_bytes()),
            None => out.extend_from_slice(b"\x1b[?25l"),
        }
    }

    /// Decode the page's frame for the boxes' shadow when a box opens, and
    /// again for a newer frame at most every SHADE_AGE: a page that moves
    /// under the file chooser would otherwise be decoded on every frame, on
    /// the thread that must never wait, for a colour no one sees change.
    /// None before the first frame, and while the logo shows.
    fn update_shade(&mut self) {
        let Some(frame) = self.frame.as_deref().filter(|_| self.splash.is_none()) else {
            self.shade = None;
            return;
        };
        let page = ui::Rect { col: 1, row: 2, width: self.shown.0, height: self.shown.1 };
        match &mut self.shade {
            Some((seen, when, shade)) if *seen == self.frames || when.elapsed() < SHADE_AGE => shade.page = page,
            _ => {
                let started = Instant::now();
                let pixels = crate::base64::decode(frame).and_then(|bytes| graphics::decode_png(&bytes, Vec::new()));
                if let Some(pixels) = &pixels {
                    log(format_args!(
                        "decoded the frame for the shadow, {}x{}, in {:?}",
                        pixels.width,
                        pixels.height,
                        started.elapsed()
                    ));
                }
                self.shade = pixels.map(|pixels| (self.frames, started, crate::modal::Shade { pixels, page }));
            }
        }
    }

    /// Show what the pages ask that is new, and stop showing what they no
    /// longer ask. Returns whether anything changed.
    fn sync_questions(&mut self) -> bool {
        let mut changed = false;
        match self.devtools.dialog().cloned() {
            Some(question) if self.dialog.as_ref().map(|dialog| dialog.id) != Some(question.id) => {
                if let Asking::Dialog { kind, message, default } = &question.asking {
                    self.dialog =
                        Some(Dialog::new(question.id, Kind::from_name(kind), &question.url, message, default));
                    self.close_help();
                    changed = true;
                }
            }
            Some(_) => {}
            None => changed |= self.dialog.take().is_some(),
        }
        match self.devtools.chooser().cloned() {
            Some(question) => {
                let Asking::Files { multiple, accept, .. } = &question.asking else { return changed };
                match &mut self.picker {
                    Some(picker) if picker.id == question.id => {
                        if picker.accept() != accept {
                            picker.set_accept(accept);
                            changed = true;
                        }
                    }
                    _ => {
                        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| "/".into());
                        let start = self.last_folder.clone().unwrap_or(home);
                        self.picker = Some(Picker::new(question.id, *multiple, accept, &question.url, &start));
                        self.close_help();
                        changed = true;
                    }
                }
            }
            None => changed |= self.picker.take().is_some(),
        }
        changed
    }

    /// A key while the page asks something: it goes to the dialog, or the
    /// file chooser under it.
    fn modal_key(&mut self, key: &Key) {
        if let Some(dialog) = &mut self.dialog {
            if let dialog::Outcome::Answer(ok, typed) = dialog.key(key) {
                self.answer_dialog(ok, typed);
            }
        } else if let Some(picker) = &mut self.picker {
            match picker.key(key) {
                picker::Outcome::Stay => {}
                picker::Outcome::Choose(files) => self.answer_files(&files),
                picker::Outcome::Cancel => self.answer_files(&[]),
            }
        }
    }

    fn answer_dialog(&mut self, ok: bool, typed: Option<String>) {
        let Some(dialog) = self.dialog.take() else { return };
        self.devtools.answer_dialog(dialog.id, ok, typed, &mut self.chrome);
        if dialog.kind() == Kind::BeforeUnload && !ok {
            // Staying: the page and its address are as they were.
            self.loading = false;
            self.url = self.chrome.url().to_owned();
            self.bar.blur(&self.url);
        }
    }

    fn answer_files(&mut self, files: &[std::path::PathBuf]) {
        let Some(picker) = self.picker.take() else { return };
        self.last_folder = Some(picker.folder().to_owned());
        self.devtools.answer_files(picker.id, files, &mut self.chrome);
    }

    /// The mouse while the page asks something: only the box answers it.
    fn modal_mouse(&mut self, mouse: Mouse) -> bool {
        let (cell_w, cell_h) = self.cell();
        let (col, row) = if self.pixel_mouse {
            (mouse.x as usize / cell_w.max(1) as usize + 1, mouse.y as usize / cell_h.max(1) as usize + 1)
        } else {
            (mouse.x as usize, mouse.y as usize)
        };
        match mouse.kind {
            MouseKind::Move => {
                let over = match (&self.dialog, &self.picker) {
                    (Some(dialog), _) => dialog.over_button(col, row),
                    (None, Some(picker)) => picker.over_button(col, row),
                    _ => false,
                };
                self.set_pointer(if over { "pointer" } else { "default" });
                return false;
            }
            MouseKind::Press if mouse.button == Some(Button::Left) => {
                if let Some(dialog) = &mut self.dialog {
                    if let dialog::Outcome::Answer(ok, typed) = dialog.click(col, row) {
                        self.answer_dialog(ok, typed);
                    }
                } else if let Some(picker) = &mut self.picker {
                    match picker.click(col, row, mouse.ctrl) {
                        picker::Outcome::Stay => {}
                        picker::Outcome::Choose(files) => self.answer_files(&files),
                        picker::Outcome::Cancel => self.answer_files(&[]),
                    }
                }
            }
            MouseKind::Wheel(_, dy) if self.dialog.is_none() => {
                if let Some(picker) = &mut self.picker {
                    picker.wheel(dy as isize);
                }
            }
            _ => return false,
        }
        true
    }

    /// Open the find field in the address bar's place.
    fn open_find(&mut self) {
        let mut field = AddressBar::default();
        field.set_text(&self.find_text);
        field.focus();
        self.find = Some(field);
    }

    /// Change the find field's text and search for the result as it is
    /// typed, staying on the current match while it still matches.
    fn find_edit(&mut self, change: impl FnOnce(&mut AddressBar)) {
        let Some(field) = &mut self.find else { return };
        change(field);
        let text = field.text();
        if text != self.find_text {
            self.find_text = text;
            self.find_in_page(false, true);
        }
    }

    /// Select the next (or previous) place the page shows the find text and
    /// scroll to it. `restart` searches from the start of the current match
    /// rather than past it.
    fn find_in_page(&mut self, backwards: bool, restart: bool) {
        let text = serde_json::to_string(&self.find_text).unwrap_or_default();
        self.chrome.evaluate(&format!(
            "(() => {{ const s = getSelection(); if ({text} === '') return s.removeAllRanges(); \
             if ({restart} && s.rangeCount) s.collapseToStart(); \
             return window.find({text}, false, {backwards}, true); }})()"
        ));
    }

    fn find_key(&mut self, key: &Key) {
        let Some(field) = &mut self.find else { return };
        match (key.code, field.key(key)) {
            (KeyCode::Enter, _) => self.find_in_page(key.shift, false),
            (_, Edit::Cancel) => self.find = None,
            (_, Edit::Typed | Edit::Changed) => self.find_edit(|_| {}),
            (_, Edit::Submit) => {}
        }
    }

    /// Handle a key. Returns whether to quit.
    fn on_key(&mut self, key: Key) -> bool {
        let focused = self.bar.focused();
        if let Some(menu) = &mut self.menu {
            match menu.key(&key) {
                menu::Outcome::Stay => {}
                menu::Outcome::Close => self.close_menu(),
                menu::Outcome::Choose(item) => self.choose(item),
            }
            return false;
        }
        if self.inspecting && key.code == KeyCode::Esc {
            self.stop_inspecting();
            return false;
        }
        match self.bindings.action(&key, focused || self.find.is_some()) {
            Some(Action::Quit) => return true,
            Some(action) if self.perform(action, &key) => return false,
            _ => {}
        }
        match key.code {
            _ if self.find.is_some() => self.find_key(&key),
            // The page hears Command+A and Command+X as select all and cut;
            // other Command keys have no meaning there.
            KeyCode::Char('a' | 'x') if key.cmd && !focused => self.chrome.key(&Key { ctrl: true, cmd: false, ..key }),
            _ if key.cmd => {}
            _ if focused => match self.bar.key(&key) {
                Edit::Changed => {}
                Edit::Typed => {
                    let completion = self.history.complete(&self.bar.typed());
                    self.bar.set_completion(completion);
                }
                Edit::Cancel => self.bar.blur(&self.url),
                Edit::Submit => match ui::normalize_url(&self.bar.text()) {
                    Some(url) => self.navigate(&url),
                    None => self.bar.blur(&self.url),
                },
            },
            _ => self.chrome.key(&key),
        }
        false
    }

    /// Carry out a bound action. Returns false if it doesn't apply now, so
    /// the key goes on as if unbound.
    fn perform(&mut self, action: Action, key: &Key) -> bool {
        match action {
            Action::Address => {
                self.find = None;
                self.bar.focus();
            }
            Action::Find => self.open_find(),
            Action::FindNext | Action::FindPrevious if self.find_text.is_empty() => return false,
            Action::FindNext => self.find_in_page(false, false),
            Action::FindPrevious => self.find_in_page(true, false),
            Action::Back => self.chrome.back(),
            Action::Forward => self.chrome.forward(),
            Action::Reload => self.chrome.reload(),
            // With one window, the key goes to the page.
            Action::NextWindow | Action::PreviousWindow if self.chrome.windows().1 == 1 => return false,
            Action::NextWindow => self.switch_window(1),
            Action::PreviousWindow => self.switch_window(-1),
            Action::CloseWindow => self.close_window(),
            Action::ZoomIn => self.step_zoom(true),
            Action::ZoomOut => self.step_zoom(false),
            Action::ZoomReset => self.zoom_to_fit(),
            Action::Help => self.open_help(),
            Action::Copy => {
                // The field being edited, or the page: the page reports what
                // it copies, and kb passes it to the clipboard.
                let field = match &self.find {
                    Some(field) => Some(field.text()),
                    None => self.bar.focused().then(|| self.bar.text()),
                };
                match field {
                    Some(text) => self.copy(&text),
                    None => self.chrome.key(&Key { code: KeyCode::Char('c'), ctrl: true, cmd: false, ..*key }),
                }
            }
            Action::Menu => self.open_menu(),
            Action::Inspect if self.inspecting => self.stop_inspecting(),
            Action::Inspect => {
                self.close_help();
                self.inspecting = true;
            }
            Action::Devtools => {
                if let Err(error) = self.chrome.open_devtools() {
                    self.note(&format!("{error} (Ctrl+Shift+C inspects without it)"));
                } else {
                    self.note("DevTools opened in Chrome");
                }
            }
            Action::Quit | Action::None => return false,
        }
        true
    }

    /// What the bar shows after the address: a note or the mode, the
    /// spinner, a download's progress, the zoom. Empty when there's none.
    fn badge(&self) -> String {
        let mut badge = String::new();
        if let Some((note, _)) = &self.notice {
            badge.push_str(note);
            badge.push(' ');
        } else if self.inspecting && !self.picks.is_empty() {
            badge.push_str("Pick: click the element you mean, Esc to cancel ");
        } else if self.inspecting {
            badge.push_str("Inspect: click an element, Esc to stop ");
        }
        if self.loading {
            const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let step = (self.started.elapsed().as_millis() / TICK.as_millis()) as usize;
            badge.push(SPINNER[step % SPINNER.len()]);
        }
        if let Some(downloading) = self.devtools.downloading() {
            if !badge.is_empty() {
                badge.push(' ');
            }
            badge.push_str(&downloading);
        }
        if (self.zoom - 1.0).abs() > 0.001 {
            if !badge.is_empty() {
                badge.push(' ');
            }
            badge.push_str(&format!("{:.0}%", self.zoom * 100.0));
        }
        badge.trim_end().to_owned()
    }

    /// Draw the ⋮ menu, if it is open, below the bar.
    fn render_menu(&mut self, out: &mut Vec<u8>) {
        if let Some(old) = self.menu_rect.take() {
            ui::blank(out, old);
            // What the menu covered of the page is drawn again.
            self.page.forget();
        }
        let Some(menu) = &self.menu else { return };
        let inspect = self.bindings.keys_for(Action::Inspect).into_iter().next().unwrap_or_default();
        let keys = |item| if item == menu::Item::SelectElement { inspect.clone() } else { String::new() };
        self.menu_rect = Some(menu.render(out, self.window.cols, keys));
    }

    fn open_menu(&mut self) {
        self.close_help();
        self.menu = Some(Menu::default());
        if let Some(splash) = &mut self.splash {
            splash.again();
        }
    }

    fn close_menu(&mut self) {
        self.menu = None;
        if let Some(splash) = &mut self.splash {
            splash.again();
        }
    }

    /// Do what the menu's item is for.
    fn choose(&mut self, item: menu::Item) {
        self.close_menu();
        match item {
            menu::Item::SelectElement => {
                self.close_help();
                self.inspecting = true;
            }
            menu::Item::ViewSource => {
                self.source_request = Some(self.chrome.evaluate_value(SOURCE));
            }
            menu::Item::Network => {
                self.network_request = Some(self.chrome.evaluate_value(TIMINGS));
            }
        }
    }

    /// Open `html`, the page's source, in the user's editor in this pane, as
    /// git does for a commit message: kb gives the terminal to the editor and
    /// takes it back when the editor exits. The file is kb's own and private,
    /// and is gone afterwards.
    fn view_source(&mut self, html: &str) {
        let folder = match crate::files::private_temp_dir("kb-source-") {
            Ok(folder) => folder,
            Err(error) => return self.note(&format!("Couldn't save the source: {error}")),
        };
        let path = folder.join(source_name(&self.url));
        if let Err(error) = crate::files::write_new(&path, html.as_bytes(), 0o600) {
            let _ = std::fs::remove_dir_all(&folder);
            return self.note(&format!("Couldn't save the source: {error}"));
        }
        let editor = ["VISUAL", "EDITOR"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()))
            .unwrap_or_else(|| "vi".to_owned());
        INPUT_PAUSED.store(true, Ordering::SeqCst);
        term::leave();
        // Through the shell, as $EDITOR can carry arguments (code --wait);
        // the file is passed as $1, never read as part of the command.
        let status =
            std::process::Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(&path).status();
        let _ = term::enter();
        INPUT_PAUSED.store(false, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&folder);
        self.clear();
        self.draw(true);
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => self.note(&format!("{editor} ended: {status}")),
            Err(error) => self.note(&format!("Couldn't start {editor}: {error}")),
        }
    }

    /// What a column of the top bar holds, as it is drawn now.
    fn bar_hit(&self, col: usize) -> Hit {
        let badge = self.badge();
        ui::hit(col, self.window.cols as usize, self.chrome.windows(), (!badge.is_empty()).then_some(badge.as_str()))
    }

    /// Show a short note at the right of the bar.
    fn note(&mut self, text: &str) {
        // A note can quote the page.
        self.notice = Some((term::drawable(text), Instant::now()));
    }

    /// Put text on the system clipboard, through the terminal.
    fn copy(&mut self, text: &str) {
        log(format_args!("copy {} characters", text.chars().count()));
        if text.is_empty() {
            self.note("Nothing to copy");
            return;
        }
        term::copy(text);
        let count = text.chars().count();
        self.note(&format!("Copied {count} character{}", if count == 1 { "" } else { "s" }));
    }

    fn stop_inspecting(&mut self) {
        self.inspecting = false;
        self.chrome.evaluate(UNHOVER);
        for (reply, _) in self.picks.drain(..) {
            let _ = reply.send(Err("the user stopped picking without choosing an element".to_owned()));
        }
    }

    /// Fail the picks no one answered in time, and leave inspect mode once
    /// none is waiting.
    fn expire_picks(&mut self) {
        let now = Instant::now();
        self.picks.retain(|(reply, deadline)| {
            if *deadline <= now {
                let _ = reply.send(Err(
                    "nothing was picked in time; ask the user to click the element in the pane, then pick again"
                        .to_owned(),
                ));
            }
            *deadline > now
        });
        if self.picks.is_empty() && self.inspecting {
            self.stop_inspecting();
        }
    }

    /// Handle Chrome's reply to a request the app made. Returns whether it
    /// was one.
    fn reply(&mut self, id: u64, message: &serde_json::Value) -> bool {
        if self.pointer_request == Some(id) {
            self.pointer_request = None;
            let shape = message["result"]["result"]["value"].as_str().unwrap_or("default");
            let shape = if POINTERS.contains(&shape) { shape } else { "default" };
            self.set_pointer(shape);
            if let Some((x, y)) = self.pointer_next.take() {
                self.ask_pointer(x, y);
            }
            return true;
        }
        if self.source_request == Some(id) {
            self.source_request = None;
            match message["result"]["result"]["value"].as_str() {
                Some(html) => {
                    let html = html.to_owned();
                    self.view_source(&html);
                }
                None => self.note("Couldn't read the page's source"),
            }
            return true;
        }
        if self.network_request == Some(id) {
            self.network_request = None;
            let lines = network_lines(&message["result"]["result"]["value"], self.devtools.failed_requests());
            self.panel = Some(lines);
            self.open_help();
            return true;
        }
        if self.describe_request == Some(id) {
            self.describe_request = None;
            let value = &message["result"]["result"]["value"];
            let mut lines: Vec<String> =
                value["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_owned)).collect();
            if lines.is_empty() {
                // Nothing was under the click: keep picking for whoever waits.
                self.inspecting = !self.picks.is_empty();
                return true;
            }
            if !self.picks.is_empty() {
                // Asked for by kb ctl pick: the answer goes there, and the
                // user's clipboard is left alone.
                let answer = picked(&lines);
                for (reply, _) in self.picks.drain(..) {
                    let _ = reply.send(Ok(Output::Text(answer.clone())));
                }
                self.chrome.evaluate(UNHOVER);
                let selector = value["selector"].as_str().unwrap_or("the element");
                self.note(&format!("Picked {selector}"));
                return true;
            }
            if let Some(selector) = value["selector"].as_str() {
                term::copy(selector);
            }
            let again = self.bindings.keys_for(Action::Inspect).into_iter().next().unwrap_or_default();
            lines.push(String::new());
            lines.push(format!("\tThe selector is copied. {again} inspects another; any key closes this."));
            self.panel = Some(lines);
            self.open_help();
            return true;
        }
        false
    }

    /// Ask which pointer the page wants at a point, one question at a time.
    fn ask_pointer(&mut self, x: f64, y: f64) {
        if self.pointer_request.is_some() {
            self.pointer_next = Some((x, y));
            return;
        }
        self.pointer_request = Some(self.chrome.evaluate_value(&format!("{POINTER}({x}, {y})")));
    }

    fn set_pointer(&mut self, shape: &str) {
        if self.pointer != shape {
            log(format_args!("pointer {shape}"));
            self.pointer = shape.to_owned();
            term::pointer(shape);
        }
    }

    /// Match the bar to the terminal, and pages' colour scheme to its
    /// background, unless one was chosen.
    fn follow_terminal_colors(&mut self) {
        let (Some(fg), Some(bg)) = (self.term_fg, self.term_bg) else { return };
        ui::use_terminal_colors(fg, bg);
        if self.dark.is_none() {
            self.chrome.color_scheme(Some(ui::is_dark(bg)));
        }
    }

    /// Carry out `kb ctl scheme`: dark, light, or auto (follow the terminal).
    fn set_scheme(&mut self, scheme: &str) -> Result<Output, String> {
        self.dark = match scheme {
            "dark" => Some(true),
            "light" => Some(false),
            "auto" => None,
            other => return Err(format!("scheme takes dark, light or auto, not {other:?}")),
        };
        let effective = self.dark.or(self.term_bg.map(ui::is_dark));
        self.chrome.color_scheme(effective);
        let said = match (self.dark, effective) {
            (Some(true), _) => "pages are told the user prefers dark".to_owned(),
            (Some(false), _) => "pages are told the user prefers light".to_owned(),
            (None, Some(dark)) => {
                format!("following the terminal: {}", if dark { "dark" } else { "light" })
            }
            (None, None) => "following the terminal, which hasn't said its colours: pages get their default".to_owned(),
        };
        Ok(Output::Text(said))
    }

    /// Returns whether the top bar needs redrawing.
    fn on_mouse(&mut self, mouse: Mouse) -> bool {
        // A press that went to the page before the box opened is released
        // there too, or Chrome would think the button still held.
        let held = mouse.kind == MouseKind::Release && self.buttons != 0;
        if (self.dialog.is_some() || self.picker.is_some()) && !held {
            return self.modal_mouse(mouse);
        }
        // The menu takes the mouse while it is open: an item chooses, and a
        // click anywhere else closes it.
        if let (true, Some(rect)) = (self.menu.is_some(), self.menu_rect) {
            let (cell_w, cell_h) = self.cell();
            let (col, row) = if self.pixel_mouse {
                (mouse.x as usize / cell_w.max(1) as usize + 1, mouse.y as usize / cell_h.max(1) as usize + 1)
            } else {
                (mouse.x as usize, mouse.y as usize)
            };
            let Some(menu) = &mut self.menu else { return false };
            match mouse.kind {
                MouseKind::Move => return menu.hover(rect, col, row),
                MouseKind::Press => match menu.click(rect, col, row) {
                    menu::Outcome::Stay => {}
                    menu::Outcome::Close => self.close_menu(),
                    menu::Outcome::Choose(item) => self.choose(item),
                },
                _ => {}
            }
            return true;
        }
        // A click closes the help, and does nothing else.
        if self.help {
            if mouse.kind == MouseKind::Press {
                self.close_help();
            }
            return false;
        }
        let (cell_w, cell_h) = self.cell();
        let (cell_w, cell_h) = (cell_w as f64, cell_h as f64);
        // Position in device pixels from the pane's top left. Cell reports
        // are taken at the middle of the cell.
        let (px, py) = if self.pixel_mouse {
            (mouse.x as f64, mouse.y as f64)
        } else {
            ((mouse.x as f64 - 0.5) * cell_w, (mouse.y as f64 - 0.5) * cell_h)
        };
        // The mouse's back and forward buttons, as in any browser.
        if mouse.kind == MouseKind::Press {
            match mouse.button {
                Some(Button::Back) => return self.perform(Action::Back, &Key::default()),
                Some(Button::Forward) => return self.perform(Action::Forward, &Key::default()),
                _ => {}
            }
        }
        if matches!(mouse.button, Some(Button::Back | Button::Forward)) {
            return false;
        }
        // While the bar asks whether to quit, only its buttons answer.
        if self.asking_quit {
            let answer = (py < cell_h).then(|| ui::answer((px / cell_w) as usize + 1, ui::QUIT_QUESTION)).flatten();
            match mouse.kind {
                MouseKind::Move => self.set_pointer(if answer.is_some() { "pointer" } else { "default" }),
                MouseKind::Press if mouse.button == Some(Button::Left) => match answer {
                    Some(true) => self.quit_clicked = true,
                    Some(false) => self.asking_quit = false,
                    None => {}
                },
                _ => {}
            }
            return true;
        }
        // A drag that began on the page stays with the page.
        if py < cell_h && self.buttons == 0 {
            let col = (px / cell_w) as usize + 1;
            if mouse.kind == MouseKind::Move {
                let shape = match self.bar_hit(col) {
                    Hit::Address(_) => "text",
                    Hit::Back
                    | Hit::Forward
                    | Hit::Reload
                    | Hit::Menu
                    | Hit::Close
                    | Hit::PreviousWindow
                    | Hit::NextWindow => "pointer",
                    Hit::Nothing => "default",
                };
                self.set_pointer(shape);
            }
            if mouse.kind != MouseKind::Press || mouse.button != Some(Button::Left) {
                return false;
            }
            if self.bar_hit(col) == Hit::Close {
                self.close_window();
                return true;
            }
            let hit = self.bar_hit(col);
            if let Some(field) = &mut self.find {
                if let Hit::Address(offset) = hit {
                    field.click(offset);
                }
                return true;
            }
            match self.bar_hit(col) {
                Hit::Back => self.chrome.back(),
                Hit::Forward => self.chrome.forward(),
                Hit::Reload if self.loading => self.chrome.stop(),
                Hit::Reload => self.chrome.reload(),
                Hit::Address(offset) if self.bar.focused() => self.bar.click(offset),
                Hit::Address(_) => self.bar.focus(),
                Hit::PreviousWindow => self.switch_window(-1),
                Hit::NextWindow => self.switch_window(1),
                Hit::Menu => self.open_menu(),
                Hit::Close | Hit::Nothing => {}
            }
            return true;
        }

        // The image covers `shown` cells and holds the whole viewport.
        let x = px / (self.shown.0 as f64 * cell_w) * self.viewport.0 as f64;
        let y = (py - cell_h) / (self.shown.1 as f64 * cell_h) * self.viewport.1 as f64;
        let mut modifiers = 0;
        if mouse.alt {
            modifiers |= chrome::ALT;
        }
        if mouse.ctrl {
            modifiers |= chrome::CTRL;
        }
        if mouse.shift {
            modifiers |= chrome::SHIFT;
        }
        let (name, bit) = match mouse.button {
            Some(Button::Left) => ("left", 1),
            Some(Button::Right) => ("right", 2),
            Some(Button::Middle) => ("middle", 4),
            Some(Button::Back | Button::Forward) | None => ("none", 0),
        };
        if mouse.kind != MouseKind::Move {
            log(format_args!("{:?} {name} at page {x:.0},{y:.0}", mouse.kind));
        }
        // Inspect mode: the mouse points elements out, and a click picks one
        // rather than reaching the page. The wheel still scrolls.
        if self.inspecting && !matches!(mouse.kind, MouseKind::Wheel(..)) {
            match mouse.kind {
                MouseKind::Move => {
                    self.set_pointer("crosshair");
                    self.chrome.evaluate(&format!("{HOVER}({x}, {y})"));
                }
                MouseKind::Press if mouse.button == Some(Button::Left) => {
                    self.chrome.evaluate(&format!("{HOVER}({x}, {y})"));
                    self.inspecting = false;
                    self.describe_request = Some(self.chrome.evaluate_value(DESCRIBE));
                }
                _ => {}
            }
            return true;
        }
        match mouse.kind {
            MouseKind::Press => {
                let now = Instant::now();
                let clicks = match self.last_press {
                    Some((at, px, py, clicks))
                        if now - at < DOUBLE_CLICK && (px - x).abs() < 4.0 && (py - y).abs() < 4.0 =>
                    {
                        clicks + 1
                    }
                    _ => 1,
                };
                self.last_press = Some((now, x, y, clicks));
                self.buttons |= bit;
                self.chrome.mouse("mousePressed", x, y, name, self.buttons, clicks, modifiers);
                if self.bar.focused() {
                    self.bar.blur(&self.url);
                    return true;
                }
            }
            MouseKind::Release => {
                self.buttons &= !bit;
                let clicks = self.last_press.map_or(1, |press| press.3);
                self.chrome.mouse("mouseReleased", x, y, name, self.buttons, clicks, modifiers);
            }
            MouseKind::Move => {
                self.chrome.mouse("mouseMoved", x, y, name, self.buttons, 0, modifiers);
                if self.buttons == 0 {
                    self.ask_pointer(x, y);
                }
            }
            MouseKind::Wheel(dx, dy) => {
                let (step_x, step_y) = wheel_step(self.viewport, self.shown);
                // A burst that cancels out (up and down) is no scroll at all.
                if (dx, dy) != (0, 0) {
                    self.chrome.wheel(x, y, dx as f64 * step_x, dy as f64 * step_y, modifiers);
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_network_box_puts_the_slowest_first() {
        let timings = serde_json::json!({
            "first_byte": 120.0, "ready": 800.0, "load": 0.0, "bytes": 20_000,
            "resources": [
                {"url": "https://cdn.test/app.js", "kind": "script", "ms": 840.0, "bytes": 240_000},
                {"url": "https://other.test/font.woff2", "kind": "css", "ms": 95.5, "bytes": 0},
                {"url": "https://site.test/logo.png", "kind": "img", "ms": 300.0, "bytes": 12_000},
            ],
        });
        let lines = network_lines(&timings, 2);
        assert_eq!(lines[0], format!("{}Network performance", ui::TITLE_LINE));
        assert_eq!(lines[1], "first byte 0.12 s · DOM ready 0.80 s · loaded not yet");
        assert_eq!(lines[2], "4 requests · 272 KB over the network · 2 failed");
        assert_eq!(lines[5], " 0.84 s    240 KB  script  cdn.test/app.js");
        assert_eq!(lines[6], " 0.30 s     12 KB  img     site.test/logo.png");
        // Cached, or another site's that hides its size.
        assert_eq!(lines[7], " 0.10 s         –  css     other.test/font.woff2");
        // A page with no timings yet still gets a box.
        let empty = network_lines(&serde_json::Value::Null, 0);
        assert_eq!(empty[2], "1 requests · 0 bytes over the network");
    }

    #[test]
    fn source_is_named_for_its_site() {
        assert_eq!(source_name("https://www.example.com/a/b?c"), "www.example.com.html");
        assert_eq!(source_name("http://localhost:3000/"), "localhost_3000.html");
        assert_eq!(source_name("file:///tmp/a.html"), "page.html");
        assert_eq!(source_name("https://../../etc/"), "page.html");
    }

    #[test]
    fn pick_waits_at_most_a_minute_and_answers_with_the_details() {
        assert_eq!(pick_timeout(None).unwrap(), Duration::from_secs(60));
        assert_eq!(pick_timeout(Some("5")).unwrap(), Duration::from_secs(5));
        assert_eq!(pick_timeout(Some("600")).unwrap(), Duration::from_secs(60));
        assert!(pick_timeout(Some("0")).is_err() && pick_timeout(Some("soon")).is_err());
        let lines = ["\u{2}button#signup.primary", "selector  #signup", "text      Sign up"].map(str::to_owned);
        assert_eq!(picked(&lines), "picked button#signup.primary\nselector  #signup\ntext      Sign up");
    }

    #[test]
    fn a_wheel_tick_scrolls_the_page_one_row_of_the_pane() {
        // A Retina pane of 100x40 cells of 16x34 pixels, at 2 pixels per
        // CSS pixel: a row is 17 CSS pixels tall and a column 8 wide.
        assert_eq!(wheel_step((800, 680), (100, 40)), (8.0, 17.0));
        // Zoomed to 200%, the same pane holds half as much page, so a row
        // moves the page half as far and keeps up with the fingers.
        assert_eq!(wheel_step((400, 340), (100, 40)), (4.0, 8.5));
    }
}
