//! The event loop: owns the terminal, the engine, and the UI state.
//!
//! Loop: wait for input or a message from the engine; update state; draw
//! only what changed. Row 1 is the top bar, the page image fills the rest.

use crate::control::{self, Output, Request};
use crate::devtools::{Devtools, Page};
use crate::engine::chrome::{self, Chrome, PageEvent};
use crate::history::History;
use crate::term::input::{Button, Event, Key, KeyCode, Mouse, MouseKind, Parser};
use crate::term::size::{self, Window};
use crate::term::{self, graphics};
use crate::ui::{self, AddressBar, Edit, Hit};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub struct Options {
    pub url: Option<String>,
    /// Device pixels per CSS pixel.
    pub scale: f64,
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
/// CSS pixels scrolled per wheel tick.
const WHEEL_STEP: f64 = 60.0;
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

struct App {
    chrome: Chrome,
    scale: f64,
    window: Window,
    /// Cell size from a terminal query or Rex, for terminals whose window
    /// size carries no pixels.
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
    /// A viewport size asked for with `kb ctl viewport`, in place of the pane's.
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
    /// The last frame written whole to a file.
    last_file: Option<std::path::PathBuf>,
    /// The page as tiles, when it is drawn that way.
    tiles: graphics::Tiles,
    /// Browser zoom: 1 is 100%. It scales the device pixels per CSS pixel,
    /// so the page reflows as in a desktop browser and stays sharp.
    zoom: f64,
    /// The id of the request for the page's width, after Ctrl+0.
    fit_request: Option<u64>,
    /// Whether the help is shown, and where it was last drawn.
    help: bool,
    help_rect: Option<ui::Rect>,
    /// This kb's number among those running, for `kb ctl -i`.
    instance: u32,
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
}

pub fn run(options: Options) -> Result<(), String> {
    // Before anything is written to the terminal: a failure after that
    // leaves the terminal's replies to our queries for the shell to read.
    let claim = control::claim()?;
    // Chrome won't share a profile between processes, so each kb running
    // beside the first has its own.
    let profile = crate::data_dir()?.join(match claim.instance {
        1 => "profile".to_owned(),
        n => format!("profile-{n}"),
    });
    std::fs::create_dir_all(&profile).map_err(|e| format!("creating {}: {e}", profile.display()))?;
    let window = size::window().ok_or("can't read the terminal size")?;

    term::enter().map_err(|e| e.to_string())?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));
    let socket = claim.socket.clone();
    let result = browse(options, &profile, window, claim);
    let _ = std::fs::remove_file(socket);
    term::leave();
    result
}

fn browse(options: Options, profile: &std::path::Path, window: Window, claim: control::Claim) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let instance = claim.instance;
    let commands = tx.clone();
    claim.listen(move |request| commands.send(Msg::Control(request)).is_ok());
    let input = tx.clone();
    std::thread::spawn(move || {
        let mut parser = Parser::default();
        let mut buf = [0; 4096];
        let mut stdin = std::io::stdin().lock();
        loop {
            match stdin.read(&mut buf) {
                Ok(n) if n > 0 => {
                    for event in parser.feed(&buf[..n]) {
                        if input.send(Msg::Term(event)).is_err() {
                            return;
                        }
                    }
                }
                _ => {
                    let _ = input.send(Msg::TermClosed);
                    return;
                }
            }
        }
    });

    let (chrome, mut receiver) = Chrome::launch(profile, options.scale)?;
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
        reported_cell: size::rex_cell(),
        pixel_mouse: false,
        bar: AddressBar::default(),
        find: None,
        find_text: String::new(),
        history: History::load(crate::data_dir()?.join("history")),
        devtools: Devtools::default(),
        viewport: (0, 0),
        fixed: None,
        shown: (window.cols, window.rows.saturating_sub(1).max(1)),
        url: String::new(),
        loading: false,
        frame: None,
        by_file: false,
        last_file: None,
        tiles: graphics::Tiles::default(),
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
    };
    app.layout();
    // KB_TRANSFER=pty keeps frames on the pty, should files misbehave.
    if std::env::var("KB_TRANSFER").as_deref() != Ok("pty") {
        graphics::ask_about_files();
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
        if let Some(splash_wait) = app.splash.as_ref().filter(|_| app.by_file).and_then(|splash| splash.wait()) {
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
                Msg::Term(Event::Mouse(mouse)) => {
                    // A burst of moves collapses to the last one.
                    let next_is_move = matches!(
                        messages.peek(),
                        Some((_, Msg::Term(Event::Mouse(Mouse { kind: MouseKind::Move, .. }))))
                    );
                    if !(mouse.kind == MouseKind::Move && next_is_move && index + 1 < count) {
                        redraw |= app.on_mouse(mouse);
                    }
                }
                Msg::Term(Event::Key(key)) => {
                    if key.ctrl && key.code == KeyCode::Char('q') {
                        quit = Some(Ok(()));
                        break;
                    }
                    // Any key closes the help, and does nothing else.
                    if app.help {
                        app.close_help();
                        continue;
                    }
                    app.on_key(key);
                    redraw = true;
                }
                Msg::Term(Event::Paste(text)) => {
                    if app.find.is_some() {
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
                    if id == graphics::FILE_QUERY_ID {
                        app.by_file = ok;
                        // If it said no, it didn't delete the file either.
                        let _ = std::fs::remove_file(graphics::temp_path(0));
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
                    app.devtools.observe(&message, &mut app.chrome);
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
                                // The blank page that comes first stays under the logo.
                                new_frame = app.splash.is_none() || app.committed;
                            }
                        }
                        Some(PageEvent::Url(url)) => {
                            log(format_args!("url {url}"));
                            app.committed |= url != "about:blank";
                            app.history.record(&url);
                            app.url = if url == "about:blank" { String::new() } else { url };
                            if !app.bar.focused() {
                                app.bar.set_text(&app.url);
                            }
                            redraw = true;
                        }
                        Some(PageEvent::Loading(loading)) => {
                            app.loading = loading;
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

        if checked.elapsed() >= TICK {
            checked = Instant::now();
            // The spinner in the bar turns while a page loads.
            redraw |= app.loading;
            app.history.save_if_due();
            app.devtools.tick(&mut app.chrome);
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
        if app.by_file && app.splash.as_ref().is_some_and(|splash| splash.due()) {
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
    app.history.save();
    if let Some(path) = app.last_file.take() {
        let _ = std::fs::remove_file(path);
    }
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

    /// Wipe the screen, images included. The help, if shown, is drawn
    /// again with the next frame.
    fn clear(&mut self) {
        term::write(b"\x1b[2J");
        term::write(graphics::DELETE);
        self.tiles.forget();
        self.help_rect = None;
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
        for (keys, does) in [
            ("Ctrl+L, Cmd+L", "edit the address"),
            ("Ctrl+F", "find on the page; Enter, Shift+Enter: next, previous"),
            ("Ctrl+G", "find the next match again"),
            ("Ctrl+[  Ctrl+]", "back, forward (also Alt+Left, Alt+Right)"),
            ("Ctrl+R", "reload"),
            ("Ctrl+=  Ctrl+-", "zoom in, out"),
            ("Ctrl+0", "zoom to 100%, or out until the page fits"),
            ("Ctrl+/", "this help"),
            ("Ctrl+Q", "quit"),
        ] {
            lines.push(format!("{keys:<17}{does}"));
        }
        let mut info = vec![String::new()];
        let (cell_w, cell_h) = self.cell();
        for (name, value) in [
            ("page", if self.url.is_empty() { "(none)".to_owned() } else { self.url.clone() }),
            ("zoom", format!("{:.0}%", self.zoom * 100.0)),
            ("viewport", format!("{}x{} CSS pixels", self.viewport.0, self.viewport.1)),
            ("pane", format!("{}x{} cells of {cell_w}x{cell_h} pixels", self.window.cols, self.window.rows)),
            (
                "frames",
                if self.by_file { "by file, changed blocks only".to_owned() } else { "through the pty".to_owned() },
            ),
            ("agents", format!("kb ctl -i {} ...   (kb ctl --help)", self.instance)),
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
        let mut out = b"\x1b[?2026h".to_vec();
        splash.draw(&mut out, cols, rows, cell, 2);
        // The splash paints the pane black, help and all.
        self.help_rect = None;
        self.render_help(&mut out);
        out.extend_from_slice(b"\x1b[?2026l");
        term::write(&out);
    }

    /// Draw the help box, if it is open, over whatever is below the bar.
    fn render_help(&mut self, out: &mut Vec<u8>) {
        if !self.help {
            return;
        }
        let lines = self.help_lines();
        if let Some(old) = self.help_rect.take() {
            ui::blank(out, old);
        }
        self.help_rect = Some(ui::render_box(out, &lines, self.window.cols, self.window.rows));
        // The box, not the address field, has the user's attention.
        out.extend_from_slice(b"\x1b[?25l");
    }

    fn close_help(&mut self) {
        self.help = false;
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
            // A width alone keeps the pane's height.
            None => match parse(size) {
                Some(width) => {
                    self.fixed = None;
                    self.layout();
                    Some((width, self.viewport.1))
                }
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
            let pixels = match self.by_file {
                true => crate::base64::decode(png).and_then(|bytes| graphics::decode_png(&bytes)),
                false => None,
            };
            // Only what changed, when the frame lines up with the cells;
            // otherwise (a fixed viewport, or a terminal that won't read
            // files) one image scaled to fit.
            let mut drawn = false;
            let pixels = match pixels {
                Some(pixels) if self.fixed.is_none() && graphics::Tiles::fits(&pixels, cols, rows, cell) => {
                    if !self.tiles.active() {
                        out.extend_from_slice(graphics::DELETE);
                    }
                    match self.tiles.draw(&mut out, pixels, cols, rows, cell, 2) {
                        Some(sent) => {
                            log(format_args!("drew {sent:?}"));
                            drawn = true;
                        }
                        None => out.extend_from_slice(graphics::DELETE),
                    }
                    None
                }
                other => other,
            };
            if !drawn {
                if self.tiles.active() {
                    out.extend_from_slice(graphics::DELETE);
                    self.tiles.forget();
                }
                out.extend_from_slice(b"\x1b[2;1H");
                let path = graphics::next_path();
                match pixels.filter(|pixels| std::fs::write(&path, &pixels.data).is_ok()) {
                    Some(pixels) => {
                        graphics::put_file(&mut out, &path, &pixels, cols, rows);
                        // The terminal deletes each file once read; this
                        // covers one it didn't get to.
                        if let Some(old) = self.last_file.replace(path) {
                            let _ = std::fs::remove_file(old);
                        }
                    }
                    None => graphics::put_png(&mut out, png, cols, rows),
                }
            }
        }
        let cols = self.window.cols as usize;
        let mut badge = String::new();
        if self.loading {
            const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let step = (self.started.elapsed().as_millis() / TICK.as_millis()) as usize;
            badge.push(SPINNER[step % SPINNER.len()]);
        }
        if (self.zoom - 1.0).abs() > 0.001 {
            if !badge.is_empty() {
                badge.push(' ');
            }
            badge.push_str(&format!("{:.0}%", self.zoom * 100.0));
        }
        let badge = (!badge.is_empty()).then_some(badge.as_str());
        match &mut self.find {
            Some(field) => ui::render_bar(&mut out, field, cols, self.loading, Some("Find"), badge),
            None => ui::render_bar(&mut out, &mut self.bar, cols, self.loading, None, badge),
        }
        self.render_help(&mut out);
        out.extend_from_slice(b"\x1b[?2026l");
        term::write(&out);
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

    fn on_key(&mut self, key: Key) {
        let focused = self.bar.focused();
        // Command where the terminal reports it, Ctrl where it doesn't.
        let chord = key.ctrl || key.cmd;
        match key.code {
            KeyCode::Char('l') if chord => {
                self.find = None;
                self.bar.focus();
            }
            KeyCode::Char('f') if chord => self.open_find(),
            KeyCode::Char('=' | '+') if key.ctrl => self.step_zoom(true),
            KeyCode::Char('-' | '_') if key.ctrl => self.step_zoom(false),
            KeyCode::Char('0') if key.ctrl => self.zoom_to_fit(),
            KeyCode::Char('/' | '?') if key.ctrl => self.help = true,
            KeyCode::Char('g') if chord && !self.find_text.is_empty() => self.find_in_page(key.shift, false),
            KeyCode::Char('r') if chord => self.chrome.reload(),
            // Only with the kitty keyboard protocol; in the legacy
            // encoding Ctrl+[ is Esc.
            KeyCode::Char('[') if key.ctrl => self.chrome.back(),
            KeyCode::Char(']') if key.ctrl => self.chrome.forward(),
            _ if self.find.is_some() => self.find_key(&key),
            KeyCode::Left if (key.alt || key.cmd) && !focused => self.chrome.back(),
            KeyCode::Right if (key.alt || key.cmd) && !focused => self.chrome.forward(),
            // The page hears Command+A as select all; other Command keys
            // have no meaning there.
            KeyCode::Char('a') if key.cmd && !focused => self.chrome.key(&Key { ctrl: true, cmd: false, ..key }),
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
    }

    /// Returns whether the top bar needs redrawing.
    fn on_mouse(&mut self, mouse: Mouse) -> bool {
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
        // A drag that began on the page stays with the page.
        if py < cell_h && self.buttons == 0 {
            if mouse.kind != MouseKind::Press || mouse.button != Some(Button::Left) {
                return false;
            }
            let col = (px / cell_w) as usize + 1;
            if let Some(field) = &mut self.find {
                if let Hit::Address(offset) = ui::hit(col, self.window.cols as usize) {
                    field.click(offset);
                }
                return true;
            }
            match ui::hit(col, self.window.cols as usize) {
                Hit::Back => self.chrome.back(),
                Hit::Forward => self.chrome.forward(),
                Hit::Reload if self.loading => self.chrome.stop(),
                Hit::Reload => self.chrome.reload(),
                Hit::Address(offset) if self.bar.focused() => self.bar.click(offset),
                Hit::Address(_) => self.bar.focus(),
                Hit::Nothing => {}
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
            None => ("none", 0),
        };
        if mouse.kind != MouseKind::Move {
            log(format_args!("{:?} {name} at page {x:.0},{y:.0}", mouse.kind));
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
            MouseKind::Move => self.chrome.mouse("mouseMoved", x, y, name, self.buttons, 0, modifiers),
            MouseKind::Wheel(dx, dy) => {
                self.chrome.wheel(x, y, dx as f64 * WHEEL_STEP, dy as f64 * WHEEL_STEP, modifiers)
            }
        }
        false
    }
}
