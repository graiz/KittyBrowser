//! Key bindings: which keys do what in kb itself, rather than going to the
//! page. The defaults can be changed in `~/.kittybrowser/keys`, one binding a
//! line:
//!
//! ```text
//! # key            action
//! ctrl+shift+i     devtools
//! alt+left         none          # let the page have it
//! ```
//!
//! The file is read again whenever it changes, so edits apply to running kbs.

use crate::term::input::{Key, KeyCode};
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Address,
    Find,
    FindNext,
    FindPrevious,
    Back,
    Forward,
    Reload,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Copy,
    Inspect,
    Devtools,
    Help,
    Quit,
    /// Unbound: the key goes to the page.
    None,
}

/// Each action's name in the file, and what it does, for `kb keys`.
const ACTIONS: &[(Action, &str, &str)] = &[
    (Action::Address, "address", "edit the address"),
    (Action::Find, "find", "find on the page"),
    (Action::FindNext, "find-next", "go to the next match"),
    (Action::FindPrevious, "find-previous", "go to the previous match"),
    (Action::Back, "back", "go back"),
    (Action::Forward, "forward", "go forward"),
    (Action::Reload, "reload", "reload, bypassing the cache"),
    (Action::ZoomIn, "zoom-in", "zoom in"),
    (Action::ZoomOut, "zoom-out", "zoom out"),
    (Action::ZoomReset, "zoom-reset", "zoom to 100%, or out until the page fits"),
    (Action::Copy, "copy", "copy the selection, or the address while editing it"),
    (Action::Inspect, "inspect", "inspect: hover to highlight elements, click one for its details"),
    (Action::Devtools, "devtools", "open Chrome DevTools for the page (needs kb --devtools)"),
    (Action::Help, "help", "show the help"),
    (Action::Quit, "quit", "quit"),
    (Action::None, "none", "nothing: the key goes to the page"),
];

const DEFAULTS: &[(&str, Action)] = &[
    ("ctrl+l", Action::Address),
    ("cmd+l", Action::Address),
    ("ctrl+f", Action::Find),
    ("cmd+f", Action::Find),
    ("ctrl+g", Action::FindNext),
    ("cmd+g", Action::FindNext),
    ("ctrl+shift+g", Action::FindPrevious),
    ("cmd+shift+g", Action::FindPrevious),
    ("ctrl+[", Action::Back),
    ("ctrl+]", Action::Forward),
    ("cmd+[", Action::Back),
    ("cmd+]", Action::Forward),
    ("alt+left", Action::Back),
    ("alt+right", Action::Forward),
    ("cmd+left", Action::Back),
    ("cmd+right", Action::Forward),
    ("ctrl+r", Action::Reload),
    ("cmd+r", Action::Reload),
    ("ctrl+=", Action::ZoomIn),
    ("ctrl+plus", Action::ZoomIn),
    ("ctrl+-", Action::ZoomOut),
    ("ctrl+_", Action::ZoomOut),
    ("ctrl+0", Action::ZoomReset),
    ("ctrl+c", Action::Copy),
    ("cmd+c", Action::Copy),
    ("ctrl+shift+c", Action::Inspect),
    ("cmd+shift+c", Action::Inspect),
    ("f12", Action::Devtools),
    ("ctrl+shift+i", Action::Devtools),
    ("cmd+alt+i", Action::Devtools),
    ("ctrl+/", Action::Help),
    ("ctrl+?", Action::Help),
    ("ctrl+q", Action::Quit),
];

/// A key as written in the file: modifiers and the key itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Spec {
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
}

impl Spec {
    pub fn parse(text: &str) -> Result<Spec, String> {
        let mut spec = Spec { code: KeyCode::Esc, ctrl: false, alt: false, shift: false, cmd: false };
        let lower = text.trim().to_ascii_lowercase();
        let mut parts: Vec<&str> = lower.split('+').collect();
        // "ctrl++" is Ctrl and the plus key.
        if lower.ends_with("++") {
            parts.truncate(parts.len() - 2);
            parts.push("plus");
        }
        let (last, modifiers) = parts.split_last().ok_or("an empty key")?;
        for modifier in modifiers {
            match *modifier {
                "ctrl" | "control" => spec.ctrl = true,
                "alt" | "option" | "opt" => spec.alt = true,
                "shift" => spec.shift = true,
                "cmd" | "command" | "super" | "meta" => spec.cmd = true,
                other => return Err(format!("{other:?} isn't a modifier (ctrl, alt, shift, cmd)")),
            }
        }
        spec.code = match *last {
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "esc" | "escape" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "space" => KeyCode::Char(' '),
            "plus" => KeyCode::Char('+'),
            name => match name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                Some(n @ 1..=12) => KeyCode::F(n),
                _ => {
                    let mut chars = name.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => KeyCode::Char(c),
                        _ => return Err(format!("{name:?} isn't a key")),
                    }
                }
            },
        };
        Ok(spec)
    }

    /// Whether `key` is this one. Letters ignore case, with an uppercase
    /// letter counting as Shift; for other characters, Shift only matters
    /// when the binding names it, since terminals differ in reporting it.
    fn matches(&self, key: &Key) -> bool {
        if (key.ctrl, key.alt, key.cmd) != (self.ctrl, self.alt, self.cmd) {
            return false;
        }
        match (self.code, key.code) {
            (KeyCode::Char(want), KeyCode::Char(got)) if want.is_ascii_alphabetic() => {
                want == got.to_ascii_lowercase() && self.shift == (key.shift || got.is_ascii_uppercase())
            }
            (KeyCode::Char(want), KeyCode::Char(got)) => want == got && (!self.shift || key.shift),
            (want, got) => want == got && self.shift == key.shift,
        }
    }

    /// Whether the binding should give way to a text field that has focus:
    /// keys without Ctrl or Command edit the text (Alt+Left moves a word).
    fn yields_to_fields(&self) -> bool {
        !self.ctrl && !self.cmd && !matches!(self.code, KeyCode::F(_))
    }
}

pub struct Bindings {
    list: Vec<(Spec, Action)>,
    path: Option<PathBuf>,
    modified: Option<SystemTime>,
    /// Problems in the file, to show once.
    pub errors: Vec<String>,
}

fn action_named(name: &str) -> Option<Action> {
    ACTIONS.iter().find(|(_, n, _)| *n == name).map(|(action, _, _)| *action)
}

fn defaults() -> Vec<(Spec, Action)> {
    DEFAULTS.iter().map(|(text, action)| (Spec::parse(text).expect("a default binding"), *action)).collect()
}

/// Apply the lines of a bindings file over the defaults.
fn apply(list: &mut Vec<(Spec, Action)>, text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(key), Some(name), None) = (words.next(), words.next(), words.next()) else {
            errors.push(format!("line {}: expected a key and an action, as in: ctrl+shift+i devtools", n + 1));
            continue;
        };
        let spec = match Spec::parse(key) {
            Ok(spec) => spec,
            Err(error) => {
                errors.push(format!("line {}: {error}", n + 1));
                continue;
            }
        };
        let Some(action) = action_named(name) else {
            errors.push(format!("line {}: {name:?} isn't an action; kb keys lists them", n + 1));
            continue;
        };
        list.retain(|(existing, _)| *existing != spec);
        list.push((spec, action));
    }
    errors
}

impl Bindings {
    pub fn load(path: Option<PathBuf>) -> Bindings {
        let mut bindings = Bindings { list: defaults(), path, modified: None, errors: Vec::new() };
        bindings.reload_if_changed();
        bindings
    }

    /// Read the file again if it changed. Returns whether it did.
    pub fn reload_if_changed(&mut self) -> bool {
        let Some(path) = &self.path else { return false };
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if modified == self.modified {
            return false;
        }
        self.modified = modified;
        self.list = defaults();
        self.errors = match std::fs::read_to_string(path) {
            Ok(text) => apply(&mut self.list, &text),
            Err(_) => Vec::new(),
        };
        true
    }

    /// What `key` does, if anything. `editing` is whether a text field has
    /// focus.
    pub fn action(&self, key: &Key, editing: bool) -> Option<Action> {
        // Later lines win, so the file overrides the defaults.
        let (spec, action) = self.list.iter().rev().find(|(spec, _)| spec.matches(key))?;
        if *action == Action::None || (editing && spec.yields_to_fields()) {
            return None;
        }
        Some(*action)
    }

    /// The keys bound to `action`, as written, for the help.
    pub fn keys_for(&self, action: Action) -> Vec<String> {
        self.list.iter().filter(|(_, a)| *a == action).map(|(spec, _)| spec.to_string()).collect()
    }
}

impl std::fmt::Display for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+"), (self.cmd, "Cmd+")] {
            if on {
                f.write_str(name)?;
            }
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char('+') => f.write_str("Plus"),
            KeyCode::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            KeyCode::F(n) => write!(f, "F{n}"),
            code => write!(f, "{code:?}"),
        }
    }
}

/// `kb keys`: where the file is, the actions, and the bindings in effect.
pub fn print(path: Option<PathBuf>) {
    let bindings = Bindings::load(path.clone());
    let place = path.map_or("~/.kittybrowser/keys".to_owned(), |path| path.display().to_string());
    println!("Key bindings. Change them in {place}, one per line:\n");
    println!("  ctrl+shift+i  devtools\n  alt+left      none      # let the page have it\n");
    println!("Keys: ctrl, alt, shift and cmd joined with +, then a character, f1-f12, left,");
    println!("right, up, down, home, end, pageup, pagedown, enter, tab, esc, space or plus.");
    println!("Changes apply to running kbs at once. Cmd needs the kitty keyboard protocol.\n");
    println!("Actions:");
    for (_, name, does) in ACTIONS {
        println!("  {name:<15}{does}");
    }
    println!("\nIn effect:");
    for (action, name, _) in ACTIONS.iter().filter(|(action, _, _)| *action != Action::None) {
        let keys = bindings.keys_for(*action);
        if !keys.is_empty() {
            println!("  {name:<15}{}", keys.join(", "));
        }
    }
    for error in &bindings.errors {
        eprintln!("kb keys: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Key {
        Key { code, ctrl: false, alt: false, shift: false, cmd: false }
    }

    #[test]
    fn specs() {
        let spec = Spec::parse("Ctrl+Shift+I").unwrap();
        assert!(spec.matches(&Key { ctrl: true, shift: true, ..key(KeyCode::Char('i')) }));
        assert!(spec.matches(&Key { ctrl: true, ..key(KeyCode::Char('I')) }));
        assert!(!spec.matches(&Key { ctrl: true, ..key(KeyCode::Char('i')) }));
        assert_eq!(Spec::parse("ctrl++").unwrap(), Spec::parse("ctrl+plus").unwrap());
        assert!(Spec::parse("f12").unwrap().matches(&key(KeyCode::F(12))));
        assert!(Spec::parse("hyper+a").is_err());
        assert!(Spec::parse("ctrl+banana").is_err());
        assert_eq!(Spec::parse("cmd+alt+i").unwrap().to_string(), "Alt+Cmd+I");
    }

    #[test]
    fn defaults_and_fields() {
        let bindings = Bindings::load(None);
        let ctrl_l = Key { ctrl: true, ..key(KeyCode::Char('l')) };
        assert_eq!(bindings.action(&ctrl_l, false), Some(Action::Address));
        assert_eq!(bindings.action(&ctrl_l, true), Some(Action::Address));
        let alt_left = Key { alt: true, ..key(KeyCode::Left) };
        assert_eq!(bindings.action(&alt_left, false), Some(Action::Back));
        // In the address field, Alt+Left moves by a word.
        assert_eq!(bindings.action(&alt_left, true), None);
        assert_eq!(bindings.action(&key(KeyCode::Char('l')), false), None);
        // Ctrl+? arrives as ? in the legacy encoding and as Shift+/ with the kitty protocol.
        assert_eq!(bindings.action(&Key { ctrl: true, ..key(KeyCode::Char('?')) }, false), Some(Action::Help));
        let shift_slash = Key { ctrl: true, shift: true, ..key(KeyCode::Char('/')) };
        assert_eq!(bindings.action(&shift_slash, false), Some(Action::Help));
    }

    #[test]
    fn the_file_overrides() {
        let mut list = defaults();
        let errors = apply(&mut list, "# mine\nalt+left none\nctrl+d   devtools  # inspect\nctrl+x\nctrl+y fly\n");
        assert_eq!(errors.len(), 2, "{errors:?}");
        let bindings = Bindings { list, path: None, modified: None, errors };
        assert_eq!(bindings.action(&Key { alt: true, ..key(KeyCode::Left) }, false), None);
        assert_eq!(bindings.action(&Key { ctrl: true, ..key(KeyCode::Char('d')) }, false), Some(Action::Devtools));
        assert_eq!(bindings.keys_for(Action::Devtools).len(), 4);
    }
}
