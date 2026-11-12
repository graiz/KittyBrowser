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
    /// Mouse buttons held, as a DevTools mask.
    buttons: u32,
    last_press: Option<(Instant, f64, f64, u32)>,
}

pub fn run(options: Options) -> Result<(), String> {
    let profile = crate::data_dir()?.join("profile");
    std::fs::create_dir_all(&profile).map_err(|e| format!("creating {}: {e}", profile.display()))?;
    let window = size::window().ok_or("can't read the terminal size")?;

    term::enter().map_err(|e| e.to_string())?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));
    let result = browse(options, &profile, window);
    term::leave();
    result
}

fn browse(options: Options, profile: &std::path::Path, window: Window) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let socket = control::socket_path()?;
    let commands = tx.clone();
    control::listen(&socket, move |request| commands.send(Msg::Control(request)).is_ok())?;
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
        history: History::load(profile.with_file_name("history")),
        devtools: Devtools::default(),
        viewport: (0, 0),
        fixed: None,
        shown: (window.cols, window.rows.saturating_sub(1).max(1)),
        url: String::new(),
        loading: false,
        frame: None,
        buttons: 0,
        last_press: None,
    };
    app.layout();
    app.chrome.start_screencast();
    match options.url.as_deref().and_then(ui::normalize_url) {
        Some(url) => app.navigate(&url),
        None => app.bar.focus(),
    }
    app.draw(false);

    let mut checked = Instant::now();
    let result = loop {
        let mut messages = Vec::new();
        match rx.recv_timeout(TICK) {
            Ok(message) => messages.push(message),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
        messages.extend(rx.try_iter());

        let (mut new_frame, mut redraw) = (false, false);
        let mut quit = None;
        let count = messages.len();
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
                    app.on_key(key);
                    redraw = true;
                }
                Msg::Term(Event::Paste(text)) => {
                    if app.bar.focused() {
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
                Msg::Term(Event::PixelMouse(on)) => {
                    log(format_args!("pixel mouse: {on}"));
                    app.pixel_mouse = on;
                }
                Msg::Control(request) => {
                    log(format_args!("control: {} {}", request.command, request.args));
                    if request.command == "viewport" {
                        // The one command about the pane rather than the page.
                        let _ = request.reply.send(app.set_viewport(request.args["size"].as_str().unwrap_or("")));
                        term::write(b"\x1b[2J");
                        term::write(graphics::DELETE);
                        new_frame = true;
                        continue;
                    }
                    let page = Page { url: &app.url, loading: app.loading, viewport: app.viewport, scale: app.scale };
                    app.devtools.command(request, &mut app.chrome, page);
                }
                Msg::Chrome(message) => {
                    app.devtools.observe(&message, &mut app.chrome);
                    match app.chrome.handle(message) {
                    Some(PageEvent::Frame(png)) => {
                        let fits = app.fits(&png);
                        log(format_args!("frame {:?}, {} bytes, fits: {fits}", graphics::png_size(&png), png.len()));
                        // A frame rendered for another size (the pane's
                        // first guess, or the size before a resize) would be
                        // stretched to fill the pane: wait for the right one.
                        if fits {
                            app.frame = Some(png);
                            new_frame = true;
                        }
                    }
                    Some(PageEvent::Url(url)) => {
                        log(format_args!("url {url}"));
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
            app.history.save_if_due();
            app.devtools.tick(&mut app.chrome);
            if let Some(window) = size::window().filter(|w| *w != app.window) {
                app.window = window;
                if window.width_px == 0 {
                    term::query_cell_size();
                }
                app.layout();
                // Whatever was on screen is at the old size: start clean.
                term::write(b"\x1b[2J");
                term::write(graphics::DELETE);
                new_frame = true;
            }
        }
        if new_frame || redraw {
            app.draw(new_frame);
        }
    };
    app.history.save();
    app.chrome.close();
    let _ = std::fs::remove_file(&socket);
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
                let (frame_w, frame_h) = (width as f64 * self.scale, height as f64 * self.scale);
                let fit = (space_w / frame_w).min(space_h / frame_h).min(1.0);
                let cells = |pixels: f64, cell: u32, most: u16| ((pixels * fit / cell as f64).round() as u16).clamp(1, most);
                self.shown = (cells(frame_w, cell_w, cols), cells(frame_h, cell_h, rows));
                (width as f64, height as f64)
            }
            None => {
                self.shown = (cols, rows);
                (space_w / self.scale, space_h / self.scale)
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
        self.chrome.resize(self.viewport.0, self.viewport.1);
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
        let percent = (self.shown.0 as f64 * cell_w as f64 / (width as f64 * self.scale) * 100.0).round();
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
        if let Some(png) = self.frame.as_deref().filter(|_| frame) {
            out.extend_from_slice(b"\x1b[2;1H");
            graphics::put_png(&mut out, png, self.shown.0, self.shown.1);
        }
        ui::render_bar(&mut out, &mut self.bar, self.window.cols as usize, self.loading);
        out.extend_from_slice(b"\x1b[?2026l");
        term::write(&out);
    }

    fn on_key(&mut self, key: Key) {
        let focused = self.bar.focused();
        match key.code {
            KeyCode::Char('l') if key.ctrl => self.bar.focus(),
            KeyCode::Char('r') if key.ctrl => self.chrome.reload(),
            KeyCode::Left if key.alt && !focused => self.chrome.back(),
            KeyCode::Right if key.alt && !focused => self.chrome.forward(),
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
