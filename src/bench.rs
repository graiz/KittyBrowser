//! `kb bench`: time each way of getting a frame from Chrome to this
//! terminal, so "fast" is a number.
//!
//! It loads a page, then scrolls it a step at a time. For each step it
//! times three things: Chrome producing the frame, kb getting it ready, and
//! the terminal taking it. The terminal's share runs from writing the frame
//! to its reply to a status request sent right behind it, which it can only
//! answer once it has read the frame.

use crate::engine::chrome::{Chrome, PageEvent};
use crate::term::size::{self, Window};
use crate::term::{self, graphics};
use std::io::Read;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: kb bench [--steps N] [--out FILE] [--scale N] [--scene scroll|type] [--modes WORD] [URL]

Run it in the pane to measure. Loads the page (it needs to be a few screens
tall), changes it N times (default 30) for each way of sending frames, and
prints a table of median times in milliseconds. --scene scroll (the default)
scrolls the page; --scene type types into a box on it, where little changes
from one frame to the next. --modes keeps only the ways whose name contains
WORD (pty for the ones that work over ssh). --out also writes the table
to a file.";

const DEFAULT_URL: &str = "https://en.wikipedia.org/wiki/Web_browser";

#[derive(Clone, Copy)]
enum Source {
    Png,
    Jpeg(u32),
}

#[derive(Clone, Copy, PartialEq)]
enum Transfer {
    /// Chrome's PNG, passed through the pty as it came.
    Pty,
    /// Decoded here, and left in a file for the terminal to read.
    File,
    /// Decoded here, and only what changed left in files: what kb
    /// itself does when the terminal reads files.
    Tiles,
    /// Decoded here; what the terminal already has is moved where the
    /// page scrolled it, and only the rest sent through the pty.
    Layers,
}

struct Mode {
    name: &'static str,
    source: Source,
    transfer: Transfer,
}

const MODES: &[Mode] = &[
    Mode { name: "png through the pty", source: Source::Png, transfer: Transfer::Pty },
    Mode { name: "png, moved layers through the pty", source: Source::Png, transfer: Transfer::Layers },
    Mode { name: "png, pixels by file", source: Source::Png, transfer: Transfer::File },
    Mode { name: "png, changed tiles by file", source: Source::Png, transfer: Transfer::Tiles },
    Mode { name: "jpeg 100, pixels by file", source: Source::Jpeg(100), transfer: Transfer::File },
    Mode { name: "jpeg 90, pixels by file", source: Source::Jpeg(90), transfer: Transfer::File },
];

enum Msg {
    Chrome(serde_json::Value),
    ChromeClosed,
    Term(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq)]
enum Scene {
    Scroll,
    Type,
}

/// A box for `Scene::Type` to type into, over the page's top left.
const TYPING_BOX: &str = "(() => { const box = document.createElement('textarea'); \
    box.style.cssText = 'position:fixed;left:20px;top:20px;width:420px;height:160px;z-index:2147483647;font:16px sans-serif'; \
    document.body.append(box); box.focus(); })()";

/// One step, in milliseconds, the size of the frame Chrome sent, and how
/// much went to the terminal.
struct Sample {
    chrome: f64,
    prepare: f64,
    terminal: f64,
    bytes: usize,
    wire: usize,
}

struct Bench {
    chrome: Chrome,
    rx: mpsc::Receiver<Msg>,
    /// What the terminal has sent that hasn't been looked at.
    replies: Vec<u8>,
    /// Errors the terminal reported for images.
    errors: usize,
}

impl Bench {
    /// The next message, with Chrome's digested into what happened on the
    /// page. `Ok(None)` is a message of no interest.
    fn next(&mut self, deadline: Instant) -> Result<Option<PageEvent>, String> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(wait) {
            Ok(Msg::Chrome(message)) => Ok(self.chrome.handle(message)),
            Ok(Msg::Term(bytes)) => {
                self.replies.extend_from_slice(&bytes);
                Ok(None)
            }
            Ok(Msg::ChromeClosed) | Err(RecvTimeoutError::Disconnected) => Err("Chrome exited".into()),
            Err(RecvTimeoutError::Timeout) => Err("timeout".into()),
        }
    }

    /// Wait for the next frame, or `None` if there is none in time.
    fn frame(&mut self, timeout: Duration) -> Result<Option<String>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.next(deadline) {
                Ok(Some(PageEvent::Frame(data))) => return Ok(Some(data)),
                Ok(_) => {}
                Err(error) if error == "timeout" => return Ok(None),
                Err(error) => return Err(error),
            }
        }
    }

    /// Let frames pass until none has come for `gap`, or for a second if
    /// the page never stops changing.
    fn settle(&mut self, gap: Duration) -> Result<(), String> {
        let limit = Instant::now() + Duration::from_secs(1);
        while Instant::now() < limit && self.frame(gap)?.is_some() {}
        Ok(())
    }

    fn loaded(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.next(deadline) {
                Ok(Some(PageEvent::Loading(false))) => return Ok(()),
                Ok(_) => {}
                Err(error) if error == "timeout" => return Err("the page didn't finish loading".into()),
                Err(error) => return Err(error),
            }
        }
    }

    /// Wait for the terminal's answer to a status request.
    fn answered(&mut self, timeout: Duration) -> Result<(), String> {
        const REPLY: &[u8] = b"\x1b[0n";
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(at) = self.replies.windows(REPLY.len()).position(|w| w == REPLY) {
                let read: Vec<u8> = self.replies.drain(..at + REPLY.len()).collect();
                self.errors += read.windows(3).filter(|w| w == b"\x1b_G").count();
                return Ok(());
            }
            match self.next(deadline) {
                Ok(_) => {}
                Err(error) if error == "timeout" => return Err("the terminal didn't answer a status request".into()),
                Err(error) => return Err(error),
            }
        }
    }
}

/// The pane's size, once it has one: a new Rex pane is 80x24 for a moment.
fn settled_window() -> Result<Window, String> {
    let start = Instant::now();
    let mut last = size::window().ok_or("can't read the terminal size")?;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let now = size::window().ok_or("can't read the terminal size")?;
        let placeholder = now.cols == 80 && now.rows == 24 && now.width_px == 0;
        if (now == last && !placeholder) || start.elapsed() > Duration::from_secs(3) {
            return Ok(now);
        }
        last = now;
    }
}

fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.collect();
    values.sort_by(|a, b| a.total_cmp(b));
    values.get(values.len() / 2).copied().unwrap_or(f64::NAN)
}

fn measure(url: &str, steps: usize, scale: f64, scene: Scene, modes: Option<&str>) -> Result<String, String> {
    // Its own profile, so that benches can run side by side.
    let profile = crate::data_dir()?.join(format!("profile-bench-{}", std::process::id()));
    std::fs::create_dir_all(&profile).map_err(|e| format!("creating {}: {e}", profile.display()))?;
    let (chrome, mut receiver) = match Chrome::launch(&profile, scale, false) {
        Ok(launched) => launched,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&profile);
            return Err(error);
        }
    };
    let (tx, rx) = mpsc::channel();
    let from_chrome = tx.clone();
    std::thread::spawn(move || {
        while let Ok(Some(message)) = receiver.recv() {
            if from_chrome.send(Msg::Chrome(message)).is_err() {
                return;
            }
        }
        let _ = from_chrome.send(Msg::ChromeClosed);
    });
    std::thread::spawn(move || {
        let mut buf = [0; 4096];
        let mut stdin = std::io::stdin().lock();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || tx.send(Msg::Term(buf[..n].to_vec())).is_err() {
                return;
            }
        }
    });

    let mut bench = Bench { chrome, rx, replies: Vec::new(), errors: 0 };
    // Chrome is closed and its profile removed however this ends.
    let report = run_modes(&mut bench, url, steps, scale, scene, modes);
    bench.chrome.close();
    let _ = std::fs::remove_dir_all(&profile);
    report
}

/// Load `url`, then change it `steps` times for each mode, and return the
/// table.
fn run_modes(
    bench: &mut Bench,
    url: &str,
    steps: usize,
    scale: f64,
    scene: Scene,
    modes: Option<&str>,
) -> Result<String, String> {
    let window = settled_window()?;
    let (cell_w, cell_h) = if window.width_px > 0 && window.height_px > 0 {
        ((window.width_px / window.cols) as u32, (window.height_px / window.rows) as u32)
    } else {
        size::rex_cell().unwrap_or((16, 34))
    };
    let (cols, rows) = (window.cols, window.rows.saturating_sub(1).max(1));
    let (width_px, height_px) = (cols as u32 * cell_w, rows as u32 * cell_h);
    let viewport = ((width_px as f64 / scale).ceil() as u32, (height_px as f64 / scale).ceil() as u32);
    bench.chrome.resize(viewport.0, viewport.1, scale);
    bench.chrome.navigate(url);
    bench.loaded(Duration::from_secs(30))?;
    if scene == Scene::Type {
        bench.chrome.evaluate(TYPING_BOX);
    }

    let mut report = format!(
        "kb bench: {url}\npane {}x{} cells, frames {width_px}x{height_px} pixels, {steps} {} steps each, times in ms (median)\n\n\
         {:<34} {:>7} {:>8} {:>9} {:>7} {:>6} {:>9} {:>8}\n",
        window.cols,
        window.rows,
        if scene == Scene::Type { "typing" } else { "scroll" },
        "frames sent as",
        "chrome",
        "prepare",
        "terminal",
        "total",
        "fps",
        "frame KB",
        "wire KB"
    );
    let (x, y) = (viewport.0 as f64 / 2.0, viewport.1 as f64 / 2.0);
    for mode in MODES.iter().filter(|mode| modes.is_none_or(|word| mode.name.contains(word))) {
        bench.chrome.stop_screencast();
        if scene == Scene::Scroll {
            bench.chrome.evaluate("scrollTo(0, 0)");
        }
        match mode.source {
            Source::Png => bench.chrome.start_screencast(),
            Source::Jpeg(quality) => bench.chrome.start_screencast_jpeg(quality),
        }
        bench.settle(Duration::from_millis(500))?;

        term::write(graphics::DELETE);
        let mut tiles = graphics::Tiles::default();
        let mut layers = crate::term::layers::Layers::default();
        let mut samples = Vec::new();
        bench.errors = 0;
        let mut missed = 0;
        let mut odd = Vec::new();
        for step in 0..steps {
            let start = Instant::now();
            match scene {
                Scene::Scroll => {
                    // Down ten steps, back up ten, so the page never runs out.
                    let dy = if (step / 10) % 2 == 0 { 240.0 } else { -240.0 };
                    bench.chrome.wheel(x, y, 0.0, dy, 0);
                }
                Scene::Type => bench.chrome.insert_text(if step % 8 == 7 { "\n" } else { "kb " }),
            }
            let Some(data) = bench.frame(Duration::from_secs(2))? else {
                missed += 1;
                continue;
            };
            let got = Instant::now();

            let mut out = b"\x1b[2;1H".to_vec();
            match mode.transfer {
                Transfer::Pty => graphics::put_png(&mut out, &data, cols, rows),
                Transfer::Layers => {
                    if layers.draw(&mut out, &data, cols, rows, (cell_w, cell_h), 2).is_none() {
                        // What the app would do: the frame as it came.
                        layers.forget();
                        out.extend_from_slice(graphics::DELETE);
                        graphics::put_png(&mut out, &data, cols, rows);
                        odd.push(graphics::png_size(&data));
                    }
                }
                Transfer::Tiles => {
                    let bytes = crate::base64::decode(&data).ok_or("Chrome sent a damaged frame")?;
                    let pixels = graphics::decode_png(&bytes, Vec::new()).ok_or("couldn't decode a frame")?;
                    if !graphics::Tiles::fits(&pixels, cols, rows, (cell_w, cell_h)) {
                        return Err("frames don't line up with the cells, so they can't be tiled".into());
                    }
                    tiles.draw(&mut out, pixels, cols, rows, (cell_w, cell_h), 2).ok_or("couldn't write a tile")?;
                }
                Transfer::File => {
                    let bytes = crate::base64::decode(&data).ok_or("Chrome sent a damaged frame")?;
                    let pixels = match mode.source {
                        Source::Png => graphics::decode_png(&bytes, Vec::new()),
                        Source::Jpeg(_) => graphics::decode_jpeg(&bytes),
                    }
                    .ok_or("couldn't decode a frame")?;
                    let path = graphics::next_path();
                    std::fs::write(&path, &pixels.data).map_err(|e| format!("writing {}: {e}", path.display()))?;
                    graphics::put_file(&mut out, &path, &pixels, cols, rows);
                }
            }
            let wire = out.len();
            out.extend_from_slice(b"\x1b[5n");
            let ready = Instant::now();
            term::write(&out);
            bench.answered(Duration::from_secs(10))?;
            let done = Instant::now();

            let ms = |from: Instant, to: Instant| (to - from).as_secs_f64() * 1000.0;
            samples.push(Sample {
                chrome: ms(start, got),
                prepare: ms(got, ready),
                terminal: ms(ready, done),
                bytes: data.len() * 3 / 4,
                wire,
            });
            // A step can animate over several frames; let the rest pass.
            bench.settle(Duration::from_millis(80))?;
        }

        if samples.is_empty() {
            report.push_str(&format!("{:<34} no frames; is the page tall enough to scroll?\n", mode.name));
            continue;
        }
        let chrome = median(samples.iter().map(|s| s.chrome));
        let prepare = median(samples.iter().map(|s| s.prepare));
        let terminal = median(samples.iter().map(|s| s.terminal));
        let total = median(samples.iter().map(|s| s.chrome + s.prepare + s.terminal));
        let size = median(samples.iter().map(|s| s.bytes as f64)) / 1024.0;
        let wire = median(samples.iter().map(|s| s.wire as f64)) / 1024.0;
        report.push_str(&format!(
            "{:<34} {chrome:>7.1} {prepare:>8.1} {terminal:>9.1} {total:>7.1} {:>6.0} {size:>9.0} {wire:>8.0}",
            mode.name,
            1000.0 / total
        ));
        if bench.errors > 0 {
            report.push_str(&format!("   ({} graphics errors from the terminal)", bench.errors));
        }
        if !odd.is_empty() {
            report.push_str(&format!("   ({} frames didn't fit the cells: {:?})", odd.len(), &odd[..odd.len().min(3)]));
        }
        if missed > 0 {
            report.push_str(&format!("   ({missed} steps gave no frame)"));
        }
        report.push('\n');
    }
    report.push_str(
        "\nchrome: from the change to the frame arriving. prepare: decoding and writing the file.\n\
         terminal: from sending the frame to the terminal having read it. wire: bytes sent to\n\
         the terminal, which is what crosses the network over ssh.\n",
    );
    Ok(report)
}

/// Run `kb bench` with the words after it. Returns the exit status.
pub fn run(words: &[String]) -> i32 {
    let mut url = DEFAULT_URL.to_owned();
    let mut steps = 30;
    let mut out = None;
    let mut scale = if cfg!(target_os = "macos") { 2.0 } else { 1.0 };
    let mut scene = Scene::Scroll;
    let mut modes = None;
    let mut args = words.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--steps" => match args.next().and_then(|n| n.parse().ok()).filter(|n| *n > 0) {
                Some(n) => steps = n,
                None => {
                    eprintln!("kb bench: --steps needs a number\n{USAGE}");
                    return 2;
                }
            },
            "--scale" => match args.next().and_then(|n| n.parse::<f64>().ok()).filter(|n| n.is_finite() && *n > 0.0) {
                Some(n) => scale = n,
                None => {
                    eprintln!("kb bench: --scale needs a positive number\n{USAGE}");
                    return 2;
                }
            },
            "--scene" => match args.next().map(String::as_str) {
                Some("scroll") => scene = Scene::Scroll,
                Some("type") => scene = Scene::Type,
                _ => {
                    eprintln!("kb bench: --scene is scroll or type\n{USAGE}");
                    return 2;
                }
            },
            "--modes" => match args.next() {
                Some(word) => modes = Some(word.clone()),
                None => {
                    eprintln!("kb bench: --modes needs a word\n{USAGE}");
                    return 2;
                }
            },
            "--out" => match args.next() {
                Some(path) => out = Some(path.clone()),
                None => {
                    eprintln!("kb bench: --out needs a file\n{USAGE}");
                    return 2;
                }
            },
            address => match crate::ui::normalize_url(address) {
                Some(address) => url = address,
                None => {
                    eprintln!("kb bench: {address:?} isn't an address\n{USAGE}");
                    return 2;
                }
            },
        }
    }

    if let Err(error) = term::enter() {
        eprintln!("kb bench: {error}");
        return 1;
    }
    let result = measure(&url, steps, scale, scene, modes.as_deref());
    term::leave();
    match result {
        Ok(report) => {
            print!("{report}");
            if let Some(path) = out {
                if let Err(error) = std::fs::write(&path, &report) {
                    eprintln!("kb bench: writing {path}: {error}");
                    return 1;
                }
            }
            0
        }
        Err(error) => {
            eprintln!("kb bench: {error}");
            1
        }
    }
}
