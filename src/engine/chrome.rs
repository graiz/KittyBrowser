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
}

/// Chrome's headless window. Frames are usually the size of the viewport
/// set by `resize`, but some Chromes (seen on a Mac whose display was
/// asleep) crop them to this window instead, which can be far smaller, so
/// it is sized to fit.
struct Window {
    id: u64,
    /// Its size as last set, in CSS pixels.
    width: i64,
    height: i64,
    /// Whether it has been resized to fit frames, after which it follows
    /// the viewport.
    steered: bool,
    /// The size, in CSS pixels, of frames from before the last change,
    /// which are no measure of the window.
    stale: Option<(i64, i64)>,
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
        // The window holding the page, and its size, for `fit_window`.
        let window = call(&mut tx, &mut rx, "Browser.getWindowForTarget", json!({"targetId": target}), None)
            .ok()
            .and_then(|reply| {
                let bounds = &reply["bounds"];
                Some(Window {
                    id: reply["windowId"].as_u64()?,
                    width: bounds["width"].as_u64()? as i64,
                    height: bounds["height"].as_u64()? as i64,
                    steered: false,
                    stale: None,
                })
            });

        call(&mut tx, &mut rx, "Page.enable", json!({}), s)?;
        // Console, log and network events feed `kb ctl console` and `network`.
        for domain in ["Runtime.enable", "Log.enable", "Network.enable"] {
            call(&mut tx, &mut rx, domain, json!({}), s)?;
        }
        let tree = call(&mut tx, &mut rx, "Page.getFrameTree", json!({}), s)?;
        let main_frame = tree["frameTree"]["frame"]["id"].as_str().unwrap_or(&target).to_owned();
        // Some sites turn away anything that calls itself headless.
        let version = call(&mut tx, &mut rx, "Browser.getVersion", json!({}), None)?;
        if let Some(agent) = version["userAgent"].as_str() {
            let agent = agent.replace("HeadlessChrome", "Chrome");
            call(&mut tx, &mut rx, "Emulation.setUserAgentOverride", json!({"userAgent": agent}), s)?;
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
                let data = params["data"].take();
                if self.hold_acks {
                    self.unacked.push_back(ack);
                } else {
                    self.send("Page.screencastFrameAck", ack);
                }
                match data {
                    Value::String(png) => Some(PageEvent::Frame(png)),
                    _ => None,
                }
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
            json!({"width": width, "height": height, "deviceScaleFactor": scale, "mobile": false}),
        );
        let (Some(window), Some((old_width, old_height))) = (&mut self.window, self.viewport.replace((width, height)))
        else {
            return;
        };
        // Frames still coming at the old size say nothing about the window.
        window.stale = Some((old_width as i64, old_height as i64));
        // A window already fitted keeps what it learnt of its edges.
        if window.steered {
            self.fit_window(width as i64 - old_width as i64, height as i64 - old_height as i64, None);
        }
    }

    /// Grow or shrink the window by this many CSS pixels. `stale` is the
    /// size of frames that are from before.
    fn fit_window(&mut self, dw: i64, dh: i64, stale: Option<(i64, i64)>) {
        let Some(window) = &mut self.window else { return };
        if dw == 0 && dh == 0 {
            return;
        }
        window.width = (window.width + dw).max(100);
        window.height = (window.height + dh).max(100);
        window.steered = true;
        window.stale = stale.or(window.stale);
        let bounds = json!({"windowId": window.id, "bounds": {"width": window.width, "height": window.height}});
        self.send_browser("Browser.setWindowBounds", bounds);
    }

    /// When a frame isn't the size of the viewport, the window is cropping
    /// it: make the window larger or smaller by the difference.
    fn check_frame_size(&mut self, metadata: &Value) {
        let (Some((width, height)), Some(got_width), Some(got_height)) =
            (self.viewport, metadata["deviceWidth"].as_f64(), metadata["deviceHeight"].as_f64())
        else {
            return;
        };
        let got = (got_width.round() as i64, got_height.round() as i64);
        if self.window.as_ref().is_none_or(|window| window.stale == Some(got)) {
            return;
        }
        self.fit_window(width as i64 - got.0, height as i64 - got.1, Some(got));
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
        self.send("Page.startScreencast", json!({"format": "png", "everyNthFrame": 1}));
    }

    /// Start (or restart) the screencast as JPEG at `quality`, 0 to 100.
    pub fn start_screencast_jpeg(&mut self, quality: u32) {
        self.send("Page.startScreencast", json!({"format": "jpeg", "quality": quality, "everyNthFrame": 1}));
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
}
