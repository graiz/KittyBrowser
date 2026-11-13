//! Headless Chrome: start it, attach to its page, and translate between the
//! app and the DevTools protocol.
//!
//! Frames come from `Page.startScreencast` as base64 PNG. Chrome only renders
//! them at device resolution when the scale factor is set at launch, so the
//! scale is fixed for the life of the process.

use super::cdp::{Receiver, Sender};
use crate::term::input::{Key, KeyCode};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// What the app needs to hear about from the page.
pub enum PageEvent {
    /// A new frame, as a base64 PNG.
    Frame(String),
    Url(String),
    Loading(bool),
    /// The page copied this text (the selection, or a copy button's).
    Copy(String),
}

/// Run in every page before its own scripts: report what the page copies
/// through the `__kbCopy` binding, so kb can put it on the terminal's
/// clipboard. Headless Chrome's own clipboard goes nowhere.
const COPY_HOOK: &str = r#"(() => {
  if (window.__kbCopyHooked) return;
  window.__kbCopyHooked = true;
  const send = text => { try { if (typeof text === 'string' && text) __kbCopy(text); } catch (e) {} };
  const selected = () => {
    const el = document.activeElement;
    const field = el && (el.localName === 'textarea' ||
      (el.localName === 'input' && /^(text|search|url|tel|email|number|)$/.test(el.type)));
    if (field && el.selectionStart !== el.selectionEnd) return el.value.slice(el.selectionStart, el.selectionEnd);
    return String(getSelection());
  };
  // A page that sets the clipboard's text in its copy handler wins over the selection.
  let pending = null;
  const setData = DataTransfer.prototype.setData;
  DataTransfer.prototype.setData = function (type, value) {
    if (pending && /^text(\/plain)?$/i.test(type)) pending.text = String(value);
    return setData.call(this, type, value);
  };
  for (const type of ['copy', 'cut']) {
    addEventListener(type, () => {
      const mine = pending = {text: null, selection: selected()};
      setTimeout(() => { if (pending === mine) pending = null; send(mine.text ?? mine.selection); });
    }, true);
  }
  // Copy buttons write with the Clipboard API, which headless Chrome may refuse.
  if (navigator.clipboard) {
    const write = navigator.clipboard.writeText.bind(navigator.clipboard);
    navigator.clipboard.writeText = text => { send(String(text)); return write(text).catch(() => {}); };
  }
})()"#;

/// Modifier bits as the DevTools protocol defines them.
pub const ALT: u32 = 1;
pub const CTRL: u32 = 2;
pub const SHIFT: u32 = 8;

pub struct Chrome {
    child: Child,
    tx: Sender,
    session: String,
    target: String,
    main_frame: String,
    /// Pages opened by the page (target=_blank, window.open) whose URL isn't
    /// known yet. There are no tabs, so they get loaded here instead.
    popups: HashSet<String>,
    /// Whether the HTTP cache is switched off, as it is on local pages.
    cache_off: bool,
    /// Whether frames wait for `ack` rather than being acknowledged as they
    /// arrive, and the acknowledgements waiting, oldest first. Chrome sends
    /// a second frame before the first is acknowledged, and counts: one
    /// never acknowledged would stop the frames for good.
    hold_acks: bool,
    unacked: std::collections::VecDeque<Value>,
    binary: PathBuf,
    profile: PathBuf,
    /// Whether Chrome also listens on a local port for DevTools.
    devtools: bool,
    window: Option<Window>,
    /// The viewport asked for, in CSS pixels.
    viewport: Option<(u32, u32)>,
    /// How the screencast was started, while it runs, and the id of the
    /// reply to the last change to the window, after which it is started
    /// again: seen on a Mac whose display was asleep, frames stop when the
    /// window changes, until then.
    screencast: Option<Value>,
    window_reply: Option<u64>,
}

/// Chrome's headless window. Frames are usually the size of the viewport
/// set by `resize`, but some Chromes (seen on a Mac whose display was
/// asleep) make them the size of this window less a margin instead,
/// whatever the viewport. So the window is kept at the viewport plus that
/// margin, as frames show it. That also gives pages a window as big as
/// they are: headless, it is 500x256 whatever the viewport, smaller than
/// the page, which sites take for a robot's.
struct Window {
    id: u64,
    /// Its size as last set, or as Chrome started it, in CSS pixels.
    size: (i64, i64),
    /// How much bigger than its frames the window is: what headless Chrome
    /// on a Mac keeps for the browser's frame at first, then what frames of
    /// another size than the viewport show.
    margin: Option<(i64, i64)>,
    /// Viewports asked for before the current one, and sizes the window had
    /// before, newest last: frames made then can still be arriving.
    earlier_viewports: Vec<(i64, i64)>,
    earlier_windows: Vec<(i64, i64)>,
}

/// The browser's frame around the page in a headless window, in CSS
/// pixels, as measured on a Mac: room for tabs and a toolbar.
const MARGIN: (i64, i64) = (0, 143);

/// How many earlier sizes to remember: more than can change between two
/// frames.
const EARLIER: usize = 8;

fn remember(list: &mut Vec<(i64, i64)>, size: (i64, i64)) {
    list.push(size);
    if list.len() > EARLIER {
        list.remove(0);
    }
}

impl Window {
    fn new(id: u64, size: (i64, i64)) -> Window {
        Window { id, size, margin: Some(MARGIN), earlier_viewports: Vec::new(), earlier_windows: Vec::new() }
    }

    /// The viewport changed from `old` to `new`: the size to make the
    /// window, if it needs another.
    fn resized(&mut self, old: Option<(i64, i64)>, new: (i64, i64)) -> Option<(i64, i64)> {
        if let Some(old) = old.filter(|&old| old != new) {
            remember(&mut self.earlier_viewports, old);
        }
        self.fit(new)
    }

    /// A frame of size `frame` came while the viewport was `viewport`: the
    /// size to make the window, if the frame shows it needs another. A
    /// frame that could have been made before the last change shows
    /// nothing; any other was made in the window as it is, which gives the
    /// margin. The margin is learnt again whenever a frame disagrees with
    /// it, and the window set to an absolute size, so a wrong guess is put
    /// right by the next frame rather than added to.
    fn frame(&mut self, viewport: (i64, i64), frame: (i64, i64)) -> Option<(i64, i64)> {
        if frame == viewport || self.late(frame) {
            return None;
        }
        // Never less than nothing: a window the system won't make as small
        // as asked (a Mac's are at least 500 wide) isn't a bigger margin.
        self.margin = Some(((self.size.0 - frame.0).max(0), (self.size.1 - frame.1).max(0)));
        self.fit(viewport)
    }

    /// Whether a frame of this size could have been made before the last
    /// change, at an earlier viewport or in an earlier window.
    fn late(&self, frame: (i64, i64)) -> bool {
        let made_in = |size: &(i64, i64)| match self.margin {
            Some(margin) => (size.0 - margin.0, size.1 - margin.1) == frame,
            None => false,
        };
        self.earlier_viewports.contains(&frame) || self.earlier_windows.iter().any(made_in)
    }

    /// Whether a frame of size `frame` holds the page at `viewport` in its
    /// top left corner, with more around it, to be cut out: frames come the
    /// size of the window, and a Mac won't make one narrower than 500 for a
    /// narrower viewport. A frame at an earlier viewport is no such thing.
    /// (One from an earlier window can't be told from it when the Mac
    /// widened both to 500, so it is cut too, and the next frame, a moment
    /// later, puts it right.)
    fn crop(&self, viewport: (i64, i64), frame: (i64, i64)) -> bool {
        let bigger = frame.0 >= viewport.0 && frame.1 >= viewport.1 && frame != viewport;
        bigger && !self.earlier_viewports.contains(&frame)
    }

    fn fit(&mut self, viewport: (i64, i64)) -> Option<(i64, i64)> {
        let (width, height) = self.margin?;
        let size = (viewport.0 + width, viewport.1 + height);
        if size == self.size {
            return None;
        }
        remember(&mut self.earlier_windows, std::mem::replace(&mut self.size, size));
        Some(size)
    }
}

/// How a base64 PNG starts.
const PNG: &str = "iVBORw0KGgo";

/// The top left part of a base64 PNG, `part` of its width and height, as a
/// base64 PNG.
fn crop_png(data: &str, part: (f64, f64)) -> Option<String> {
    let pixels = crate::term::graphics::decode_png(&crate::base64::decode(data)?, Vec::new())?;
    let width = ((pixels.width as f64 * part.0).round() as u32).clamp(1, pixels.width);
    let height = ((pixels.height as f64 * part.1).round() as u32).clamp(1, pixels.height);
    let depth = if pixels.alpha { 4 } else { 3 };
    let row = (pixels.width * depth) as usize;
    let mut cut = Vec::with_capacity((width * height * depth) as usize);
    for line in pixels.data.chunks_exact(row).take(height as usize) {
        cut.extend_from_slice(&line[..(width * depth) as usize]);
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(if pixels.alpha { png::ColorType::Rgba } else { png::ColorType::Rgb });
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&cut).ok()?;
    writer.finish().ok()?;
    Some(crate::base64::encode(&png))
}

/// Whether `url` is a file or a server on this machine or network: a page
/// under development, where a cached copy of a file just edited is a trap.
fn is_local(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    if scheme == "file" {
        return true;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    if host.starts_with("[::1]") {
        return true;
    }
    let name = host.rsplit_once(':').map_or(host, |(name, _port)| name);
    match name.parse::<std::net::Ipv4Addr>() {
        Ok(address) => address.is_loopback() || address.is_private(),
        Err(_) => {
            name == "localhost" || [".localhost", ".local", ".test", ".vibe"].iter().any(|end| name.ends_with(end))
        }
    }
}

fn find_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KB_CHROME") {
        return Some(path.into());
    }
    const APPS: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    const NAMES: &[&str] =
        &["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "brave-browser"];
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|path| {
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        dirs.into_iter().flat_map(|dir| NAMES.iter().map(move |name| dir.join(name)))
    });
    APPS.iter().map(PathBuf::from).chain(on_path).find(|path| path.is_file())
}

/// Turn off the password manager and autofill in the profile before Chrome
/// The client hints a browser like `binary` sends, for `product` (as
/// Browser.getVersion has it, "HeadlessChrome/157.0.8103.78"): its brand
/// beside Chromium's and a made-up one, as Chrome sends, and the platform.
fn agent_metadata(product: &str, binary: &Path) -> Value {
    let full = product.rsplit('/').next().unwrap_or("");
    let major = full.split('.').next().unwrap_or("");
    let name = binary.to_string_lossy().to_lowercase();
    let brand = if name.contains("edge") {
        Some("Microsoft Edge")
    } else if name.contains("brave") {
        Some("Brave")
    } else if name.contains("chromium") {
        None
    } else {
        Some("Google Chrome")
    };
    let mut brands = vec![("Not.A/Brand", "99", "99.0.0.0"), ("Chromium", major, full)];
    brands.extend(brand.map(|brand| (brand, major, full)));
    let short: Vec<Value> = brands.iter().map(|b| json!({"brand": b.0, "version": b.1})).collect();
    let long: Vec<Value> = brands.iter().map(|b| json!({"brand": b.0, "version": b.2})).collect();
    let (platform, architecture) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("macOS", "arm"),
        ("macos", _) => ("macOS", "x86"),
        (_, "aarch64") => ("Linux", "arm"),
        _ => ("Linux", "x86"),
    };
    json!({
        "brands": short,
        "fullVersionList": long,
        "fullVersion": full,
        "platform": platform,
        "platformVersion": platform_version(),
        "architecture": architecture,
        "bitness": "64",
        "model": "",
        "mobile": false,
    })
}

/// The system's version, as Chrome reports it: macOS's ("26.0.1").
#[cfg(target_os = "macos")]
fn platform_version() -> String {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    let name = c"kern.osproductversion";
    let got = unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    match got {
        0 => String::from_utf8_lossy(&buf[..len]).trim_end_matches('\0').to_owned(),
        _ => String::new(),
    }
}

/// The system's version, as Chrome reports it: the kernel's release,
/// without what follows the numbers ("6.8.0" of "6.8.0-45-generic").
#[cfg(not(target_os = "macos"))]
fn platform_version() -> String {
    let mut system: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut system) } != 0 {
        return String::new();
    }
    let release: String = system.release.iter().take_while(|&&c| c != 0).map(|&c| c as u8 as char).collect();
    release.split(|c: char| !(c.is_ascii_digit() || c == '.')).next().unwrap_or("").to_owned()
}

/// starts. After a login form is submitted, Chrome offers to save the
/// password; headless, the offer has nowhere to show, and while it is
/// pending the page can stop receiving mouse and key events.
fn quiet_profile(profile: &Path) {
    let path = profile.join("Default").join("Preferences");
    let mut prefs: Value = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    prefs["credentials_enable_service"] = json!(false);
    if !prefs["profile"].is_object() {
        prefs["profile"] = json!({});
    }
    prefs["profile"]["password_manager_enabled"] = json!(false);
    if !prefs["autofill"].is_object() {
        prefs["autofill"] = json!({});
    }
    prefs["autofill"]["profile_enabled"] = json!(false);
    prefs["autofill"]["credit_card_enabled"] = json!(false);
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if let Ok(bytes) = serde_json::to_vec(&prefs) {
        let _ = std::fs::write(&path, bytes);
    }
}

/// Move `fd` clear of 3 and 4, which Chrome's pipe will occupy in the child,
/// and mark it close-on-exec.
fn lift(fd: RawFd) -> io::Result<OwnedFd> {
    let high = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    let result = if high < 0 { Err(io::Error::last_os_error()) } else { Ok(high) };
    unsafe { libc::close(fd) };
    result.map(|high| unsafe { OwnedFd::from_raw_fd(high) })
}

/// A pipe as (read end, write end).
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let read = lift(fds[0]);
    let write = lift(fds[1]);
    Ok((read?, write?))
}

/// Send a command and wait for its reply, dropping any events in between.
/// Only for startup, before the reader thread owns the receiver.
fn call(
    tx: &mut Sender,
    rx: &mut Receiver,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Result<Value, String> {
    let closed = "Chrome exited during startup (is another kb already running?)";
    let id = tx.send(method, params, session).map_err(|_| closed)?;
    loop {
        let mut message = rx.recv().map_err(|e| format!("reading from Chrome: {e}"))?.ok_or(closed)?;
        if message["id"].as_u64() != Some(id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(format!("{method}: {}", error["message"]));
        }
        return Ok(message["result"].take());
    }
}

impl Chrome {
    /// Start Chrome on `profile` and attach to its page. The receiver is
    /// returned separately so a thread can block on it.
    /// Start Chrome. With `devtools`, it also serves DevTools on a port of
    /// 127.0.0.1, which any program on this machine can reach.
    pub fn launch(profile: &Path, scale: f64, devtools: bool) -> Result<(Chrome, Receiver), String> {
        let binary =
            find_binary().ok_or("can't find Chrome; install it or set KB_CHROME to a Chromium-based browser")?;
        let pipe_error = |e| format!("creating a pipe: {e}");
        let (child_in, to_chrome) = pipe().map_err(pipe_error)?;
        let (from_chrome, child_out) = pipe().map_err(pipe_error)?;
        let (in_fd, out_fd) = (child_in.as_raw_fd(), child_out.as_raw_fd());

        quiet_profile(profile);
        let mut command = Command::new(&binary);
        command
            .arg("--headless=new")
            .arg("--remote-debugging-pipe")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--force-device-scale-factor={scale}"))
            .args(["--hide-scrollbars", "--no-first-run", "--no-default-browser-check"])
            .arg("--disable-features=PasswordManagerOnboarding,AutofillServerCommunication")
            // Otherwise navigator.webdriver is true, which sites such as
            // Google take as a robot and answer with a captcha.
            .arg("--disable-blink-features=AutomationControlled")
            .args(if devtools {
                &["--remote-debugging-port=0", "--remote-debugging-address=127.0.0.1"][..]
            } else {
                &[]
            })
            // Extra flags to try, separated by spaces.
            .args(std::env::var("KB_CHROME_ARGS").unwrap_or_default().split_whitespace())
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Chrome reads commands on fd 3 and writes replies on fd 4.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(in_fd, 3) < 0 || libc::dup2(out_fd, 4) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|e| format!("starting {}: {e}", binary.display()))?;
        drop((child_in, child_out));
        let mut tx = Sender::new(File::from(to_chrome));
        let mut rx = Receiver::new(File::from(from_chrome));

        let targets = call(&mut tx, &mut rx, "Target.getTargets", json!({}), None)?;
        let page = targets["targetInfos"]
            .as_array()
            .and_then(|infos| infos.iter().find(|info| info["type"] == "page"))
            .and_then(|info| info["targetId"].as_str().map(str::to_owned));
        let target = match page {
            Some(target) => target,
            None => {
                let created = call(&mut tx, &mut rx, "Target.createTarget", json!({"url": "about:blank"}), None)?;
                created["targetId"].as_str().ok_or("Chrome gave no page")?.to_owned()
            }
        };
        let attached =
            call(&mut tx, &mut rx, "Target.attachToTarget", json!({"targetId": target, "flatten": true}), None)?;
        let session = attached["sessionId"].as_str().ok_or("Chrome gave no session")?.to_owned();
        let s = Some(session.as_str());
        // The window holding the page, and its size, for `check_frame_size`.
        let window = call(&mut tx, &mut rx, "Browser.getWindowForTarget", json!({"targetId": target}), None)
            .ok()
            .and_then(|reply| {
                let bounds = &reply["bounds"];
                Some(Window::new(reply["windowId"].as_u64()?, (bounds["width"].as_i64()?, bounds["height"].as_i64()?)))
            });

        call(&mut tx, &mut rx, "Page.enable", json!({}), s)?;
        // Console, log and network events feed `kb ctl console` and `network`.
        for domain in ["Runtime.enable", "Log.enable", "Network.enable"] {
            call(&mut tx, &mut rx, domain, json!({}), s)?;
        }
        let tree = call(&mut tx, &mut rx, "Page.getFrameTree", json!({}), s)?;
        let main_frame = tree["frameTree"]["frame"]["id"].as_str().unwrap_or(&target).to_owned();
        // Some sites turn away anything that calls itself headless. The
        // client hints go with the name: overriding it alone leaves them
        // empty, which is a robot's mark too.
        let version = call(&mut tx, &mut rx, "Browser.getVersion", json!({}), None)?;
        if let (Some(agent), Some(product)) = (version["userAgent"].as_str(), version["product"].as_str()) {
            let agent = agent.replace("HeadlessChrome", "Chrome");
            let metadata = agent_metadata(product, &binary);
            call(
                &mut tx,
                &mut rx,
                "Emulation.setUserAgentOverride",
                json!({"userAgent": agent, "userAgentMetadata": metadata}),
                s,
            )?;
        }
        // Without this the page believes it is in the background: no caret, no focus styles.
        call(&mut tx, &mut rx, "Emulation.setFocusEmulationEnabled", json!({"enabled": true}), s)?;
        call(&mut tx, &mut rx, "Target.setDiscoverTargets", json!({"discover": true}), None)?;
        call(&mut tx, &mut rx, "Runtime.addBinding", json!({"name": "__kbCopy"}), s)?;
        call(&mut tx, &mut rx, "Page.addScriptToEvaluateOnNewDocument", json!({"source": COPY_HOOK}), s)?;

        let chrome = Chrome {
            child,
            tx,
            session,
            target,
            main_frame,
            popups: HashSet::new(),
            cache_off: false,
            hold_acks: false,
            unacked: Default::default(),
            binary,
            profile: profile.to_owned(),
            devtools,
            window,
            viewport: None,
            screencast: None,
            window_reply: None,
        };
        Ok((chrome, rx))
    }

    // Send failures mean Chrome is gone; the reader thread reports that.
    fn send(&mut self, method: &str, params: Value) {
        let _ = self.tx.send(method, params, Some(&self.session));
    }

    /// Send a command whose reply the caller wants: returns the id the reply
    /// will carry.
    pub fn request(&mut self, method: &str, params: Value) -> u64 {
        self.tx.send(method, params, Some(&self.session)).unwrap_or(0)
    }

    pub fn main_frame(&self) -> &str {
        &self.main_frame
    }

    /// Switch the HTTP cache off if `url` is local, on if not. Call before
    /// going there.
    pub fn cache_for(&mut self, url: &str) {
        let off = is_local(url);
        if off != self.cache_off {
            self.cache_off = off;
            self.send("Network.setCacheDisabled", json!({"cacheDisabled": off}));
        }
    }

    fn send_browser(&mut self, method: &str, params: Value) {
        let _ = self.tx.send(method, params, None);
    }

    /// Digest one message from Chrome, answering what needs an answer, and
    /// return what the app should react to.
    pub fn handle(&mut self, mut message: Value) -> Option<PageEvent> {
        if message["id"].as_u64().is_some_and(|id| self.window_reply == Some(id)) {
            self.window_reply = None;
            if let Some(params) = self.screencast.clone() {
                // Frames from before need no acknowledging once it stops.
                self.unacked.clear();
                self.send("Page.stopScreencast", json!({}));
                self.send("Page.startScreencast", params);
            }
            return None;
        }
        let method = message["method"].as_str()?.to_owned();
        let params = &mut message["params"];
        match method.as_str() {
            "Runtime.bindingCalled" if params["name"] == "__kbCopy" => {
                Some(PageEvent::Copy(params["payload"].as_str()?.to_owned()))
            }
            "Page.screencastFrame" => {
                // Chrome stops sending frames until each one is acknowledged.
                self.check_frame_size(&params["metadata"]);
                let ack = json!({"sessionId": params["sessionId"]});
                let data = match params["data"].take() {
                    Value::String(data) => self.crop_frame(data, &params["metadata"]),
                    _ => None,
                };
                if self.hold_acks {
                    self.unacked.push_back(ack);
                } else {
                    self.send("Page.screencastFrameAck", ack);
                }
                data.map(PageEvent::Frame)
            }
            "Page.frameNavigated" => {
                let frame = &params["frame"];
                if frame.get("parentId").is_some() {
                    return None;
                }
                self.main_frame = frame["id"].as_str()?.to_owned();
                let mut url = frame["url"].as_str()?.to_owned();
                url.push_str(frame["urlFragment"].as_str().unwrap_or(""));
                // For what loads next: links followed from here aren't seen in advance.
                self.cache_for(&url);
                Some(PageEvent::Url(url))
            }
            "Page.navigatedWithinDocument" if params["frameId"] == self.main_frame.as_str() => {
                Some(PageEvent::Url(params["url"].as_str()?.to_owned()))
            }
            "Page.frameStartedLoading" if params["frameId"] == self.main_frame.as_str() => {
                Some(PageEvent::Loading(true))
            }
            "Page.frameStoppedLoading" if params["frameId"] == self.main_frame.as_str() => {
                Some(PageEvent::Loading(false))
            }
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                let created = method == "Target.targetCreated";
                self.popup(&params["targetInfo"], created);
                None
            }
            "Target.targetDestroyed" => {
                self.popups.remove(params["targetId"].as_str()?);
                None
            }
            _ => None,
        }
    }

    /// Load a page that opened in a new window here instead, and close it.
    fn popup(&mut self, info: &Value, created: bool) {
        let Some(id) = info["targetId"].as_str() else { return };
        if info["type"] != "page" || id == self.target || !(created || self.popups.contains(id)) {
            return;
        }
        let url = info["url"].as_str().unwrap_or("");
        if url.is_empty() || url == "about:blank" {
            self.popups.insert(id.to_owned());
            return;
        }
        self.popups.remove(id);
        self.navigate(url);
        self.send_browser("Target.closeTarget", json!({"targetId": id}));
    }

    pub fn navigate(&mut self, url: &str) {
        self.cache_for(url);
        self.send("Page.navigate", json!({"url": url}));
    }

    /// Reload from the network, not the cache: what was just edited is what shows.
    pub fn reload(&mut self) {
        self.send("Page.reload", json!({"ignoreCache": true}));
    }

    pub fn stop(&mut self) {
        self.send("Page.stopLoading", json!({}));
    }

    pub fn back(&mut self) {
        self.send("Runtime.evaluate", json!({"expression": "history.back()"}));
    }

    pub fn forward(&mut self) {
        self.send("Runtime.evaluate", json!({"expression": "history.forward()"}));
    }

    /// Set the viewport, in CSS pixels. Frames come at this size times the
    /// launch scale, whatever scale is asked for here, so `scale` should be
    /// the launch scale.
    pub fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.send(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": scale,
                "mobile": false,
                // Headless, the screen is 400x300, smaller than the page on
                // it: a robot's mark. A laptop's screen, or the page's size if
                // larger.
                "screenWidth": width.max(1512),
                "screenHeight": height.max(982),
            }),
        );
        let size = |(width, height): (u32, u32)| (width as i64, height as i64);
        let old = self.viewport.replace((width, height)).map(size);
        let fit = self.window.as_mut().and_then(|window| window.resized(old, size((width, height))));
        self.set_window(fit);
    }

    fn set_window(&mut self, size: Option<(i64, i64)>) {
        let (Some(window), Some((width, height))) = (&self.window, size) else { return };
        let bounds = json!({"windowId": window.id, "bounds": {"width": width, "height": height}});
        self.window_reply = self.tx.send("Browser.setWindowBounds", bounds, None).ok();
    }

    /// The frame, cut to the viewport if it holds more (see `Window::crop`).
    /// Only PNG frames are cut; kb draws JPEG ones only in kb bench.
    fn crop_frame(&self, data: String, metadata: &Value) -> Option<String> {
        let (Some(window), Some((width, height)), Some(css_width), Some(css_height)) =
            (&self.window, self.viewport, metadata["deviceWidth"].as_f64(), metadata["deviceHeight"].as_f64())
        else {
            return Some(data);
        };
        let viewport = (width as i64, height as i64);
        if !window.crop(viewport, (css_width.round() as i64, css_height.round() as i64)) || !data.starts_with(PNG) {
            return Some(data);
        }
        crop_png(&data, (width as f64 / css_width, height as f64 / css_height)).or(Some(data))
    }

    /// Fit the window to the viewport if this frame shows it cropping them.
    fn check_frame_size(&mut self, metadata: &Value) {
        let (Some(window), Some((width, height)), Some(got_width), Some(got_height)) =
            (&mut self.window, self.viewport, metadata["deviceWidth"].as_f64(), metadata["deviceHeight"].as_f64())
        else {
            return;
        };
        let fit = window.frame((width as i64, height as i64), (got_width.round() as i64, got_height.round() as i64));
        self.set_window(fit);
    }

    /// Evaluate JavaScript in the page and have its value in the reply to
    /// the id returned.
    /// Tell pages the user prefers a dark or light colour scheme, or with
    /// `None`, nothing (Chrome's default, light).
    pub fn color_scheme(&mut self, dark: Option<bool>) {
        let value = match dark {
            Some(true) => "dark",
            Some(false) => "light",
            None => "",
        };
        self.send(
            "Emulation.setEmulatedMedia",
            json!({"features": [{"name": "prefers-color-scheme", "value": value}]}),
        );
    }

    /// Open Chrome DevTools for this page in the desktop browser. Needs kb
    /// started with `--devtools`.
    pub fn open_devtools(&self) -> Result<(), String> {
        if !self.devtools {
            return Err("DevTools needs kb started with --devtools".to_owned());
        }
        let ports = std::fs::read_to_string(self.profile.join("DevToolsActivePort"))
            .map_err(|_| "Chrome hasn't opened its DevTools port yet".to_owned())?;
        let port = ports.lines().next().unwrap_or("").trim();
        let url = format!(
            "http://127.0.0.1:{port}/devtools/inspector.html?ws=127.0.0.1:{port}/devtools/page/{}",
            self.target
        );
        // The same browser kb runs, as a desktop app: DevTools is Chrome's.
        let bundle = self.binary.ancestors().find(|path| path.extension().is_some_and(|e| e == "app"));
        let mut command = match bundle {
            Some(app) => {
                let mut open = Command::new("open");
                open.arg("-a").arg(app).arg(&url);
                open
            }
            None => {
                let mut browser = Command::new(&self.binary);
                browser.arg("--new-window").arg(&url);
                browser
            }
        };
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("opening DevTools: {e}"))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }

    pub fn evaluate_value(&mut self, expression: &str) -> u64 {
        self.request("Runtime.evaluate", json!({"expression": expression, "returnByValue": true}))
    }

    pub fn start_screencast(&mut self) {
        self.screencast_with(json!({"format": "png", "everyNthFrame": 1}));
    }

    /// Start (or restart) the screencast as JPEG at `quality`, 0 to 100.
    pub fn start_screencast_jpeg(&mut self, quality: u32) {
        self.screencast_with(json!({"format": "jpeg", "quality": quality, "everyNthFrame": 1}));
    }

    fn screencast_with(&mut self, params: Value) {
        self.screencast = Some(params.clone());
        self.send("Page.startScreencast", params);
    }

    /// Leave each frame unacknowledged until `ack` is called. Chrome sends
    /// no more than a frame or two while any are waiting, so this sets the
    /// pace.
    pub fn hold_acks(&mut self) {
        self.hold_acks = true;
    }

    /// Whether a frame is waiting to be acknowledged.
    pub fn ack_pending(&self) -> bool {
        !self.unacked.is_empty()
    }

    /// Acknowledge the frame that has waited longest, letting Chrome send
    /// another.
    pub fn ack(&mut self) {
        if let Some(ack) = self.unacked.pop_front() {
            self.send("Page.screencastFrameAck", ack);
        }
    }

    pub fn stop_screencast(&mut self) {
        self.screencast = None;
        self.send("Page.stopScreencast", json!({}));
    }

    /// Run JavaScript in the page, ignoring the result.
    pub fn evaluate(&mut self, expression: &str) {
        self.send("Runtime.evaluate", json!({"expression": expression}));
    }

    /// `kind` is mousePressed, mouseReleased or mouseMoved; `button` is the
    /// one that changed (or "none"); `buttons` is the mask of those held.
    #[allow(clippy::too_many_arguments)]
    pub fn mouse(&mut self, kind: &str, x: f64, y: f64, button: &str, buttons: u32, clicks: u32, modifiers: u32) {
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": kind, "x": x, "y": y, "button": button, "buttons": buttons,
                "clickCount": clicks, "modifiers": modifiers,
            }),
        );
    }

    pub fn wheel(&mut self, x: f64, y: f64, dx: f64, dy: f64, modifiers: u32) {
        self.send(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy, "modifiers": modifiers}),
        );
    }

    pub fn insert_text(&mut self, text: &str) {
        self.send("Input.insertText", json!({"text": text}));
    }

    /// Press and release a key on the page.
    pub fn key(&mut self, key: &Key) {
        let mut modifiers = 0;
        if key.alt {
            modifiers |= ALT;
        }
        if key.ctrl {
            modifiers |= CTRL;
        }
        if key.shift {
            modifiers |= SHIFT;
        }
        // (key, code, Windows virtual key code, text typed)
        let (name, code, vk, text): (String, String, u32, Option<String>) = match key.code {
            KeyCode::Char(c) => {
                if !c.is_ascii() {
                    return self.insert_text(&c.to_string());
                }
                if c.is_ascii_uppercase() {
                    modifiers |= SHIFT;
                }
                let (code, vk) = match c {
                    'a'..='z' | 'A'..='Z' => (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as u32),
                    '0'..='9' => (format!("Digit{c}"), c as u32),
                    ' ' => ("Space".to_owned(), 32),
                    _ => (String::new(), 0),
                };
                let text = (!key.ctrl && !key.alt).then(|| c.to_string());
                (c.to_string(), code, vk, text)
            }
            KeyCode::Enter => ("Enter".into(), "Enter".into(), 13, Some("\r".into())),
            KeyCode::Tab => ("Tab".into(), "Tab".into(), 9, None),
            KeyCode::BackTab => {
                modifiers |= SHIFT;
                ("Tab".into(), "Tab".into(), 9, None)
            }
            KeyCode::Backspace => ("Backspace".into(), "Backspace".into(), 8, None),
            KeyCode::Delete => ("Delete".into(), "Delete".into(), 46, None),
            KeyCode::Esc => ("Escape".into(), "Escape".into(), 27, None),
            KeyCode::Left => ("ArrowLeft".into(), "ArrowLeft".into(), 37, None),
            KeyCode::Up => ("ArrowUp".into(), "ArrowUp".into(), 38, None),
            KeyCode::Right => ("ArrowRight".into(), "ArrowRight".into(), 39, None),
            KeyCode::Down => ("ArrowDown".into(), "ArrowDown".into(), 40, None),
            KeyCode::Home => ("Home".into(), "Home".into(), 36, None),
            KeyCode::End => ("End".into(), "End".into(), 35, None),
            KeyCode::PageUp => ("PageUp".into(), "PageUp".into(), 33, None),
            KeyCode::PageDown => ("PageDown".into(), "PageDown".into(), 34, None),
            KeyCode::F(n) => (format!("F{n}"), format!("F{n}"), 111 + n as u32, None),
        };
        let mut down = json!({
            "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
            "modifiers": modifiers, "key": name, "code": code, "windowsVirtualKeyCode": vk,
        });
        if let Some(text) = text {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        // Editing keys need their command named, as a browser's menu would.
        let command = match key.code {
            KeyCode::Char('a') if key.ctrl => Some("selectAll"),
            KeyCode::Char('c') if key.ctrl => Some("copy"),
            KeyCode::Char('x') if key.ctrl => Some("cut"),
            _ => None,
        };
        if let Some(command) = command {
            down["commands"] = json!([command]);
        }
        self.send("Input.dispatchKeyEvent", down);
        self.send(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp", "modifiers": modifiers, "key": name, "code": code,
                "windowsVirtualKeyCode": vk,
            }),
        );
    }

    /// Ask Chrome to exit, so it writes cookies to the profile, and wait
    /// briefly before forcing it.
    pub fn close(mut self) {
        self.send_browser("Browser.close", json!({}));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn client_hints_name_the_browser() {
        let hints = super::agent_metadata(
            "HeadlessChrome/157.0.8103.78",
            std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        );
        let brands: Vec<&str> =
            hints["brands"].as_array().unwrap().iter().map(|b| b["brand"].as_str().unwrap()).collect();
        assert_eq!(brands, ["Not.A/Brand", "Chromium", "Google Chrome"]);
        assert_eq!(hints["brands"][2]["version"], "157");
        assert_eq!(hints["fullVersionList"][2]["version"], "157.0.8103.78");
        let chromium = super::agent_metadata("HeadlessChrome/157.0.1.2", std::path::Path::new("/usr/bin/chromium"));
        assert_eq!(chromium["brands"].as_array().unwrap().len(), 2);
        let edge = super::agent_metadata("HeadlessChrome/157.0.1.2", std::path::Path::new("/usr/bin/microsoft-edge"));
        assert_eq!(edge["brands"][2]["brand"], "Microsoft Edge");
        assert!(super::platform_version().starts_with(|c: char| c.is_ascii_digit()));
    }

    use super::*;

    #[test]
    fn local_addresses() {
        for url in [
            "file:///tmp/index.html",
            "http://localhost:8000/",
            "http://127.0.0.1:49844/index.html",
            "http://192.168.1.20/",
            "http://[::1]:3000/",
            "https://myapp.vibe/",
            "http://user@dev.local:8080/x",
        ] {
            assert!(is_local(url), "{url}");
        }
        for url in ["https://example.com/", "https://localhost.example.com/", "about:blank", "http://8.8.8.8/"] {
            assert!(!is_local(url), "{url}");
        }
    }

    /// A Chrome whose frames are its window less `margin`, as one's were on
    /// a Mac whose display was asleep (a margin of 0x143, and windows no
    /// narrower than 500), or with a margin of `None`, the viewport.
    struct Model {
        window: (i64, i64),
        margin: Option<(i64, i64)>,
        /// How many times kb set the window.
        sets: usize,
    }

    impl Model {
        fn new(margin: Option<(i64, i64)>) -> Model {
            Model { window: (1368, 300), margin, sets: 0 }
        }

        /// The size of a frame made now at this viewport.
        fn frame(&self, viewport: (i64, i64)) -> (i64, i64) {
            match self.margin {
                Some(margin) => (self.window.0 - margin.0, self.window.1 - margin.1),
                None => viewport,
            }
        }

        fn set(&mut self, size: Option<(i64, i64)>) {
            if let Some((width, height)) = size {
                self.window = (width.max(500), height);
                self.sets += 1;
            }
        }
    }

    /// Resize through `viewports`, with each frame arriving `lag` resizes
    /// after it was made, then let the frames catch up. The first frame can
    /// be `odd` by some pixels, as one was: 500x128 from a 500x256 window
    /// whose frames after were 143 short.
    fn run(margin: Option<(i64, i64)>, odd: (i64, i64), lag: usize, viewports: &[(i64, i64)]) -> (Window, Model) {
        let mut window = Window::new(1, (1368, 300));
        let mut chrome = Model::new(margin);
        let mut in_flight = std::collections::VecDeque::new();
        let mut viewport = None;
        for &size in viewports {
            chrome.set(window.resized(viewport, size));
            let mut frame = chrome.frame(size);
            if viewport.is_none() {
                frame = (frame.0 + odd.0, frame.1 + odd.1);
            }
            viewport = Some(size);
            in_flight.push_back(frame);
            if in_flight.len() > lag {
                chrome.set(window.frame(size, in_flight.pop_front().unwrap()));
            }
        }
        let size = viewport.unwrap();
        for frame in in_flight {
            chrome.set(window.frame(size, frame));
        }
        for _ in 0..3 {
            chrome.set(window.frame(size, chrome.frame(size)));
        }
        (window, chrome)
    }

    const ZOOM: [(i64, i64); 9] = [
        (1280, 799),
        (1164, 726),
        (1067, 666),
        (1164, 726),
        (1280, 799),
        (1422, 888),
        (1600, 999),
        (1422, 888),
        (1280, 799),
    ];
    const DRAG: [(i64, i64); 6] = [(1300, 1000), (1400, 900), (1200, 700), (900, 500), (1500, 950), (1200, 700)];

    #[test]
    fn frames_that_fit_keep_the_window_a_browser_bigger() {
        // Frames the size of the viewport, however late: one command per
        // resize, and the window ends a browser's frame taller than the page.
        for lag in 0..4 {
            for viewports in [&ZOOM[..], &DRAG[..], &[(300, 100), (320, 110)][..]] {
                let (_, chrome) = run(None, (0, 0), lag, viewports);
                assert_eq!(chrome.sets, viewports.len(), "lag {lag}, {viewports:?}");
                let last = *viewports.last().unwrap();
                // A Mac makes no window narrower than 500.
                assert_eq!(chrome.window, (last.0.max(500), last.1 + 143), "lag {lag}, {viewports:?}");
            }
        }
    }

    #[test]
    fn frames_that_follow_the_window_end_whole() {
        // Two or three resizes before a frame from the first arrives, as
        // with Ctrl+= twice or a drag: the window used to be moved by C-A
        // and then by C-B, ending at 3C-A-B.
        for lag in 0..4 {
            for odd in [(0, 0), (0, 15)] {
                for viewports in [&ZOOM[..], &DRAG[..]] {
                    let (_, chrome) = run(Some((0, 143)), odd, lag, viewports);
                    let last = *viewports.last().unwrap();
                    assert_eq!(chrome.frame(last), last, "lag {lag}, odd {odd:?}, {viewports:?}");
                    assert_eq!(chrome.window, (last.0, last.1 + 143));
                }
            }
        }
    }

    #[test]
    fn the_first_frame_of_another_size_fits_the_window() {
        // As seen: a 1368x300 window, frames of 1368x157.
        let mut window = Window::new(1, (1368, 300));
        assert_eq!(window.frame((1280, 799), (1368, 157)), Some((1280, 942)));
        assert_eq!(window.frame((1280, 799), (1280, 799)), None);
        // Late, from the window before.
        assert_eq!(window.frame((1280, 799), (1368, 157)), None);
        // As seen too: a 500x256 window whose first frame was 500x128 and
        // the next ones 143 short. The margin is learnt again.
        let mut window = Window::new(1, (500, 256));
        assert_eq!(window.frame((1368, 833), (500, 128)), Some((1368, 961)));
        assert_eq!(window.frame((1368, 833), (1368, 818)), Some((1368, 976)));
        assert_eq!(window.frame((1368, 833), (1368, 833)), None);
    }

    #[test]
    fn a_viewport_narrower_than_a_window_can_be() {
        // The window is asked for once; that the Mac keeps it 500 wide
        // doesn't start a loop of asking again.
        let (mut window, chrome) = run(Some((0, 143)), (0, 0), 0, &[(300, 100)]);
        assert_eq!((chrome.window, chrome.sets), ((500, 243), 1));
        assert_eq!(window.frame((300, 100), chrome.frame((300, 100))), None);
        assert_eq!(window.resized(Some((300, 100)), (320, 110)), Some((320, 253)));
    }

    #[test]
    fn frames_bigger_than_the_viewport_are_cut() {
        let mut window = Window::new(1, (1368, 300));
        window.resized(None, (854, 406));
        window.resized(Some((854, 406)), (413, 406));
        // The Mac's narrowest window around a narrower viewport.
        assert!(window.crop((413, 406), (500, 406)));
        // Whole, or from before the resize, or not holding all of it.
        assert!(!window.crop((413, 406), (413, 406)));
        assert!(!window.crop((413, 406), (854, 406)));
        assert!(!window.crop((413, 406), (500, 300)));
        // Where frames follow the viewport, they never are.
        let mut window = Window::new(1, (500, 256));
        let mut viewport = None;
        for size in [(1280, 799), (413, 406), (300, 100), (854, 406)] {
            window.resized(viewport, size);
            assert!(!window.crop(size, size));
            viewport = Some(size);
        }
    }

    #[test]
    fn cutting_a_png_keeps_its_top_left() {
        use crate::term::graphics::decode_png;
        use crate::term::layers::tests::{encode, frame};
        let png = encode(&frame(1000, 812, |x, y| ((x / 10 + y) % 251) as u8));
        let cut = crop_png(&png, (413.0 / 500.0, 1.0)).unwrap();
        let cut = decode_png(&crate::base64::decode(&cut).unwrap(), Vec::new()).unwrap();
        assert_eq!((cut.width, cut.height), (826, 812));
        let at = |x: u32, y: u32| cut.data[((y * 826 + x) * 3) as usize];
        for (x, y) in [(0, 0), (825, 0), (0, 811), (825, 811), (400, 300)] {
            assert_eq!(at(x, y), ((x / 10 + y) % 251) as u8);
        }
    }
}
