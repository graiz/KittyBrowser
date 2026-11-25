//! `kb --headless`: kb without a pane. The same browser, profile and
//! `kb ctl` commands, drawn nowhere: for an agent to carry on when nobody is
//! watching, such as after the user closed its pane to get it out of the way.
//!
//! `kb --headless URL` starts a copy of itself in the background, in a
//! session of its own so it outlives the shell that started it, and prints
//! the new kb's id once it answers `kb ctl`. `kb ctl -i ID quit` stops it.
//! Nobody sees one that is forgotten, so it also quits by itself after
//! `idle()` without a command, unless told never to.

use crate::app::Options;
use crate::control::{self, Output, Request};
use crate::devtools::{Devtools, Page};
use crate::engine::chrome::{Chrome, PageEvent};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// Set in the background copy, which runs the browser.
const CHILD: &str = "KB_HEADLESS";

/// A desktop's viewport, as no pane gives one.
const VIEWPORT: (u32, u32) = (1280, 800);

/// How long a headless kb waits for a command before quitting by itself:
/// ten minutes, or KB_HEADLESS_IDLE seconds, where 0 means it never does,
/// for a kb kept running as a service.
pub fn idle() -> Option<Duration> {
    idle_from(std::env::var("KB_HEADLESS_IDLE").ok().as_deref())
}

fn idle_from(setting: Option<&str>) -> Option<Duration> {
    match setting.and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(0) => None,
        Some(seconds) => Some(Duration::from_secs(seconds)),
        None => Some(Duration::from_secs(10 * 60)),
    }
}

/// When a headless kb quits by itself, in words: "it quits after 10
/// minutes without a command".
pub fn idle_note() -> String {
    match idle().map(|idle| idle.as_secs()) {
        None => "it runs until told to quit, as KB_HEADLESS_IDLE=0 asks".to_owned(),
        Some(seconds) if seconds % 60 == 0 => format!("it quits after {} minutes without a command", seconds / 60),
        Some(seconds) => format!("it quits after {seconds} seconds without a command"),
    }
}

/// How long the starting kb waits for the first page to load before
/// handing over anyway, so a slow site can't hold it up.
const FIRST_LOAD: Duration = Duration::from_secs(15);

/// How often waits are checked, as in a pane.
const TICK: Duration = Duration::from_millis(100);

/// What the starting kb is told this one is called: its id or name, which
/// no other kb takes while it runs.
fn ready_name() -> String {
    crate::control::id().unwrap_or_default()
}

/// Whether this kb runs headless: `status` says so in place of a pane.
pub fn running() -> bool {
    std::env::var_os(CHILD).is_some()
}

/// Start a headless kb in the background and print its id.
pub fn start(args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("finding kb itself: {e}"))?;
    let mut command = Command::new(exe);
    command
        .args(args)
        .env(CHILD, "1")
        // Not the pane of the shell that started it.
        .env_remove("REX_BLOCK")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own session: closing the terminal or pane that started it doesn't
    // stop it.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| format!("starting kb: {e}"))?;
    let mut first = String::new();
    let _ = BufReader::new(child.stdout.take().ok_or("no output from kb")?).read_line(&mut first);
    let mut words = first.split_whitespace();
    let name = words.next().unwrap_or("").to_owned();
    // It says nothing on its output unless it started.
    match name.is_empty() {
        false => {
            println!("kb {name}, headless");
            if words.next() == Some("loading") {
                println!(
                    "the page is still loading after {} s; kb ctl -i {name} status says when it's done",
                    FIRST_LOAD.as_secs()
                );
            }
            println!("kb ctl -i {name} ... drives it, and kb ctl -i {name} quit stops it ({})", idle_note());
            Ok(())
        }
        true => {
            let mut why = String::new();
            if let Some(stderr) = child.stderr.take() {
                let _ = BufReader::new(stderr).read_line(&mut why);
            }
            let _ = child.wait();
            let why = why.trim().trim_start_matches("kb: ");
            Err(if why.is_empty() { "kb didn't start".to_owned() } else { why.to_owned() })
        }
    }
}

enum Msg {
    Control(Request),
    Chrome(Value),
    ChromeClosed,
}

/// Run the browser, in the background copy. Prints the kb's id once it
/// answers `kb ctl`.
pub fn run(options: Options) -> Result<(), String> {
    crate::engine::chrome::find_binary()?;
    let claim = control::claim(options.name.as_deref())?;
    let socket = claim.socket.clone();
    let profile = claim.profile()?;
    let result = browse(options, &profile, claim);
    socket.remove();
    result
}

fn browse(options: Options, profile: &std::path::Path, claim: control::Claim) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let (chrome, mut receiver) = Chrome::launch(profile, options.scale, options.devtools)?;
    let mut chrome = chrome;
    let messages = tx.clone();
    std::thread::spawn(move || {
        while let Ok(Some(message)) = receiver.recv() {
            if messages.send(Msg::Chrome(message)).is_err() {
                return;
            }
        }
        let _ = messages.send(Msg::ChromeClosed);
    });
    let socket = claim.socket.clone();
    claim.listen(move |request| tx.send(Msg::Control(request)).is_ok());

    let mut viewport = VIEWPORT;
    chrome.resize(viewport.0, viewport.1, options.scale);
    if let Some(dark) = options.dark {
        chrome.color_scheme(Some(dark));
    }
    let mut url = String::new();
    let mut loading = false;
    let address = options.url.as_deref().and_then(crate::ui::normalize_url);
    if let Some(address) = &address {
        chrome.navigate(address);
    }
    let mut devtools = Devtools::default();
    // The starting kb waits for this line, with the number, as `kb ctl open`
    // would for the page: until it has loaded, or for FIRST_LOAD at most.
    let started = std::time::Instant::now();
    let mut announced = address.is_none();
    if announced {
        println!("{}", ready_name());
    }
    let mut seen_loading = false;

    let idle = idle();
    let mut last_command = std::time::Instant::now();
    loop {
        if idle.is_some_and(|idle| last_command.elapsed() > idle) {
            break;
        }
        match rx.recv_timeout(TICK) {
            Ok(Msg::Control(request)) => {
                // kb list asks every kb its status; that isn't using this one.
                if request.args["listing"] != true {
                    last_command = std::time::Instant::now();
                }
                match request.command.as_str() {
                    "quit" => {
                        let cancelled = devtools.cancel_downloads(&mut chrome);
                        let _ = request.reply.send(Ok(Output::Text(format!("kb is quitting{cancelled}"))));
                        break;
                    }
                    "pick" => {
                        let _ = request
                            .reply
                            .send(Err("a headless kb has no pane for the user to point in; pick needs a kb in a pane"
                                .to_owned()));
                    }
                    "window" => {
                        let (answer, event) = crate::devtools::window(&request.args, &mut chrome, &devtools);
                        let _ = request.reply.send(answer);
                        if let Some(PageEvent::Shown(now)) = event {
                            url = if now == "about:blank" { String::new() } else { now };
                            loading = false;
                        }
                    }
                    "viewport" => {
                        let size = request.args["size"].as_str().unwrap_or("");
                        let reply = set_viewport(&mut viewport, size).map(|(width, height)| {
                            chrome.resize(width, height, options.scale);
                            Output::Text(format!("the viewport is {width}x{height} CSS pixels"))
                        });
                        let _ = request.reply.send(reply);
                    }
                    "scheme" => {
                        let dark = match request.args["scheme"].as_str().unwrap_or("") {
                            "dark" => Ok(Some(true)),
                            "light" => Ok(Some(false)),
                            "auto" => Ok(None),
                            other => Err(format!("scheme takes dark, light or auto, not {other:?}")),
                        };
                        let reply = dark.map(|dark| {
                            chrome.color_scheme(dark);
                            Output::Text(match dark {
                                Some(true) => "pages are told the user prefers dark".to_owned(),
                                Some(false) => "pages are told the user prefers light".to_owned(),
                                None => "pages are told nothing, so they show their light scheme".to_owned(),
                            })
                        });
                        let _ = request.reply.send(reply);
                    }
                    _ => {
                        let page = Page {
                            url: &url,
                            loading,
                            viewport,
                            scale: options.scale,
                            zoom: 1.0,
                            viewport_set: viewport != VIEWPORT,
                        };
                        devtools.command(request, &mut chrome, page);
                    }
                }
            }
            Ok(Msg::Chrome(message)) => {
                if !chrome.shown(&message) {
                    // A window behind the one shown can still ask something,
                    // and freezes until answered.
                    devtools.dialog_event(&message, &mut chrome);
                    continue;
                }
                devtools.observe(&message, &mut chrome);
                match chrome.handle(message) {
                    Some(PageEvent::Url(now) | PageEvent::Shown(now)) => {
                        url = if now == "about:blank" { String::new() } else { now };
                    }
                    Some(PageEvent::Loading(now)) => {
                        loading = now;
                        seen_loading |= now;
                    }
                    _ => {}
                }
            }
            Ok(Msg::ChromeClosed) => return Err("Chrome exited".to_owned()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        devtools.tick(&mut chrome);
        if !announced && !seen_loading && started.elapsed() > FIRST_LOAD {
            // Not slow but stuck: Chrome never began to load the page, and
            // every command would wait on it in vain.
            chrome.close();
            return Err(format!(
                "Chrome didn't start loading {} within {} s, so kb stopped. On macOS, Chrome can be waiting \
                 for the keychain, as when HOME isn't the user's own; see Troubleshooting in kb's README",
                address.as_deref().unwrap_or(""),
                FIRST_LOAD.as_secs()
            ));
        }
        if !announced && ((seen_loading && !loading) || started.elapsed() > FIRST_LOAD) {
            announced = true;
            println!("{}{}", ready_name(), if loading { " loading" } else { "" });
        }
    }
    socket.remove();
    devtools.cancel_downloads(&mut chrome);
    chrome.close();
    Ok(())
}

/// The viewport for `kb ctl viewport SIZE`: WIDTHxHEIGHT, a width alone, or
/// reset, back to a desktop's.
fn set_viewport(viewport: &mut (u32, u32), size: &str) -> Result<(u32, u32), String> {
    let parse = |text: &str| text.parse::<u32>().ok().filter(|n| (50..=10_000).contains(n));
    *viewport = match size.split_once(['x', 'X']) {
        _ if matches!(size, "reset" | "pane") => VIEWPORT,
        Some((width, height)) => match (parse(width), parse(height)) {
            (Some(width), Some(height)) => (width, height),
            _ => return Err(format!("{size:?} isn't a size; give WIDTHxHEIGHT in CSS pixels, such as 1280x800")),
        },
        None => match parse(size) {
            Some(width) => (width, viewport.1),
            None => return Err(format!("{size:?} isn't a size; give WIDTH, WIDTHxHEIGHT, or reset")),
        },
    };
    Ok(*viewport)
}

#[cfg(test)]
mod tests {
    #[test]
    fn idle_times() {
        use std::time::Duration;
        assert_eq!(super::idle_from(None), Some(Duration::from_secs(600)));
        assert_eq!(super::idle_from(Some("90")), Some(Duration::from_secs(90)));
        assert_eq!(super::idle_from(Some("0")), None);
        assert_eq!(super::idle_from(Some("soon")), Some(Duration::from_secs(600)));
    }

    #[test]
    fn viewports() {
        let mut viewport = super::VIEWPORT;
        assert_eq!(super::set_viewport(&mut viewport, "375x800"), Ok((375, 800)));
        assert_eq!(super::set_viewport(&mut viewport, "600"), Ok((600, 800)));
        assert_eq!(super::set_viewport(&mut viewport, "reset"), Ok(super::VIEWPORT));
        assert!(super::set_viewport(&mut viewport, "huge").is_err());
    }
}
