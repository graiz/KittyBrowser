//! Browser chrome drawn as ordinary terminal text above the page image: back,
//! forward and reload buttons, and the address field.

use crate::term::input::{Key, KeyCode};
use std::fmt::Write;

/// Columns (1-based) of the top bar. Each button is three cells wide.
const BACK: std::ops::RangeInclusive<usize> = 1..=3;
const FORWARD: std::ops::RangeInclusive<usize> = 4..=6;
const RELOAD: std::ops::RangeInclusive<usize> = 7..=9;
/// The address field starts here and runs to just before the close
/// button, with one cell of padding inside each end.
const FIELD: usize = 11;
/// The close button, at the right end: a space, the cross, a space.
const CLOSE: usize = 3;
/// Panes narrower than this get an empty bar.
const MIN_COLS: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Back,
    Forward,
    Reload,
    Close,
    /// A click in the address field, as an offset into the visible text.
    Address(usize),
    Nothing,
}

/// What a column of the top bar holds.
pub fn hit(col: usize, cols: usize) -> Hit {
    if cols < MIN_COLS {
        Hit::Nothing
    } else if BACK.contains(&col) {
        Hit::Back
    } else if FORWARD.contains(&col) {
        Hit::Forward
    } else if RELOAD.contains(&col) {
        Hit::Reload
    } else if col > cols {
        Hit::Nothing
    } else if col > cols - CLOSE {
        Hit::Close
    } else if (FIELD..=cols - CLOSE).contains(&col) {
        Hit::Address(col.saturating_sub(FIELD + 1))
    } else {
        Hit::Nothing
    }
}

/// The question the bar asks before quitting.
pub const QUIT_QUESTION: &str = "Quit KittyBrowser?";

/// Draw `question` in the top bar with Yes and No buttons after it.
pub fn render_question(out: &mut Vec<u8>, question: &str, cols: usize) {
    let colors = palette();
    let mut s = format!("\x1b[1;1H{}\x1b[2K {question}  ", colors.bar);
    let _ = write!(s, "{} Yes {} {} No {}", colors.focused, colors.bar, colors.field, colors.bar);
    let used = question.chars().count() + 13;
    let _ = write!(s, "{:pad$}\x1b[0m\x1b[?25l", "", pad = cols.saturating_sub(used));
    out.extend_from_slice(s.as_bytes());
}

/// Which button of `render_question`'s a column is on: `Some(true)` for Yes.
pub fn answer(col: usize, question: &str) -> Option<bool> {
    // " question  " then " Yes ", a space, " No ".
    let yes = question.chars().count() + 4;
    if (yes..yes + 5).contains(&col) {
        Some(true)
    } else if (yes + 6..yes + 10).contains(&col) {
        Some(false)
    } else {
        None
    }
}

/// What a key did to the address field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edit {
    Changed,
    /// Text was typed at the end: the moment to offer a completion.
    Typed,
    Submit,
    Cancel,
}

/// The address field: a single line of text with a cursor. Focusing it
/// selects everything, so typing replaces the address, as in any browser.
///
/// A completion is a suggested ending shown highlighted after the typed
/// text. Enter, Right and End accept it, Backspace rejects it, and any other
/// edit drops it.
#[derive(Default)]
pub struct AddressBar {
    text: Vec<char>,
    cursor: usize,
    /// Index of the first visible character when the text overflows.
    scroll: usize,
    focused: bool,
    selected: bool,
    completion: Vec<char>,
}

impl AddressBar {
    /// The address as shown, completion included.
    pub fn text(&self) -> String {
        self.text.iter().chain(&self.completion).collect()
    }

    /// Only what was typed.
    pub fn typed(&self) -> String {
        self.text.iter().collect()
    }

    pub fn set_completion(&mut self, completion: Option<String>) {
        self.completion = completion.unwrap_or_default().chars().collect();
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn focus(&mut self) {
        self.focused = true;
        self.selected = !self.text.is_empty();
        self.cursor = self.text.len();
    }

    /// Drop focus and show `url`.
    pub fn blur(&mut self, url: &str) {
        self.focused = false;
        self.set_text(url);
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.chars().collect();
        self.cursor = self.text.len();
        self.scroll = 0;
        self.selected = false;
        self.completion.clear();
    }

    /// Put the cursor where a click landed, `offset` cells into the visible text.
    pub fn click(&mut self, offset: usize) {
        self.selected = false;
        self.completion.clear();
        self.cursor = (self.scroll + offset).min(self.text.len());
    }

    pub fn insert(&mut self, text: &str) {
        self.completion.clear();
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        for c in text.chars().filter(|c| !c.is_control()) {
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    fn word_left(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && !self.text[at - 1].is_alphanumeric() {
            at -= 1;
        }
        while at > 0 && self.text[at - 1].is_alphanumeric() {
            at -= 1;
        }
        at
    }

    fn word_right(&self) -> usize {
        let mut at = self.cursor;
        while at < self.text.len() && !self.text[at].is_alphanumeric() {
            at += 1;
        }
        while at < self.text.len() && self.text[at].is_alphanumeric() {
            at += 1;
        }
        at
    }

    pub fn key(&mut self, key: &Key) -> Edit {
        let selected = std::mem::take(&mut self.selected);
        let completion = std::mem::take(&mut self.completion);
        match key.code {
            KeyCode::Enter => {
                self.text.extend(completion);
                self.cursor = self.text.len();
                return Edit::Submit;
            }
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char(c) if !key.ctrl && !key.alt => {
                self.selected = selected;
                self.insert(c.encode_utf8(&mut [0; 4]));
                if self.cursor == self.text.len() {
                    return Edit::Typed;
                }
            }
            KeyCode::Backspace | KeyCode::Delete if !completion.is_empty() => {}
            KeyCode::Right | KeyCode::End if !completion.is_empty() => {
                self.text.extend(completion);
                self.cursor = self.text.len();
            }
            KeyCode::Backspace | KeyCode::Delete if selected => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            KeyCode::Left if key.alt || key.ctrl => self.cursor = self.word_left(),
            KeyCode::Right if key.alt || key.ctrl => self.cursor = self.word_right(),
            // Leaving a selection puts the cursor at the end it was left from.
            KeyCode::Left if selected => self.cursor = 0,
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Char('a') if key.ctrl => self.cursor = 0,
            KeyCode::Char('e') if key.ctrl => self.cursor = self.text.len(),
            KeyCode::Char('u') if key.ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('k') if key.ctrl => self.text.truncate(self.cursor),
            KeyCode::Char('w') if key.ctrl => {
                let start = self.word_left();
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            _ => {}
        }
        Edit::Changed
    }

    /// The range of characters to show in `width` cells, scrolled so the
    /// cursor stays in view.
    fn window(&mut self, width: usize) -> std::ops::Range<usize> {
        if !self.focused {
            self.scroll = 0;
        } else if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + width {
            self.scroll = self.cursor + 1 - width;
        }
        // Don't stay scrolled past text that has since got shorter.
        let len = self.text.len() + self.completion.len();
        self.scroll = self.scroll.min((len + 1).saturating_sub(width));
        self.scroll..len.min(self.scroll + width)
    }
}

/// The colours of the bar and of boxes, as escapes. They start as greys from
/// the 256-colour palette and follow the terminal's own colours once it
/// says what they are.
#[derive(Clone)]
struct Palette {
    bar: String,
    field: String,
    focused: String,
    box_art: String,
    box_style: String,
    box_title: String,
    box_dim: String,
}

static PALETTE: std::sync::RwLock<Option<Palette>> = std::sync::RwLock::new(None);

fn palette() -> Palette {
    PALETTE.read().ok().and_then(|p| p.clone()).unwrap_or_else(|| Palette {
        bar: "\x1b[0;48;5;236;38;5;252m".into(),
        field: "\x1b[0;48;5;238;38;5;250m".into(),
        focused: "\x1b[0;48;5;240;38;5;231m".into(),
        box_art: "\x1b[0;48;5;236;38;5;141m".into(),
        box_style: "\x1b[0;48;5;236;38;5;252m".into(),
        box_title: "\x1b[0;48;5;236;38;5;231;1m".into(),
        box_dim: "\x1b[0;48;5;236;38;5;245m".into(),
    })
}

pub type Rgb = (u8, u8, u8);

/// `t` of the way from `a` to `b`.
fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let channel = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

/// Whether a background is dark: its relative luminance is under half.
pub fn is_dark(bg: Rgb) -> bool {
    (0.2126 * bg.0 as f64 + 0.7152 * bg.1 as f64 + 0.0722 * bg.2 as f64) / 255.0 < 0.5
}

/// Draw the bar and boxes in the terminal's colours: shades between its
/// background and its text, so kb looks like part of the terminal.
pub fn use_terminal_colors(fg: Rgb, bg: Rgb) {
    let style = |back: Rgb, fore: Rgb, extra: &str| {
        format!("\x1b[0;48;2;{};{};{};38;2;{};{};{}{extra}m", back.0, back.1, back.2, fore.0, fore.1, fore.2)
    };
    let bar = mix(bg, fg, 0.10);
    let art = if is_dark(bg) { (175, 135, 255) } else { (110, 60, 200) };
    let palette = Palette {
        bar: style(bar, mix(bg, fg, 0.85), ""),
        field: style(mix(bg, fg, 0.18), mix(bg, fg, 0.80), ""),
        focused: style(mix(bg, fg, 0.27), fg, ""),
        box_art: style(bar, art, ""),
        box_style: style(bar, mix(bg, fg, 0.85), ""),
        box_title: style(bar, fg, ";1"),
        box_dim: style(bar, mix(bg, fg, 0.55), ""),
    };
    if let Ok(mut current) = PALETTE.write() {
        *current = Some(palette);
    }
}

/// Append the escapes that draw the top bar on row 1 and leave the cursor
/// in the address field (or hidden, if the field isn't focused).
/// The banner at the top of the help.
pub const ART: [&str; 5] = [
    r" /\_/\    _  ___ _   _        ___",
    r"( o.o )  | |/ (_) |_| |_ _  _| _ )_ _ _____ __ _____ ___ _ _",
    r" > ^ <   | ' <| |  _|  _| || | _ \ '_/ _ \ V  V (_-</ -_) '_|",
    r"         |_|\_\_|\__|\__|\_, |___/_| \___/\_/\_//__/\___|_|",
    r"                         |__/",
];

/// A box of text in the middle of the cells from `top` down: its column,
/// row, width and height, all in cells and 1-based.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub col: u16,
    pub row: u16,
    pub width: u16,
    pub height: u16,
}

/// Draw `lines` in a framed box centred in a `cols` x `rows` screen, below
/// the top bar. A line starting with `ART_LINE` is drawn in the logo's
/// colour, one starting with `TITLE_LINE` in bold, one starting with a tab
/// dimmed. Returns where the box is, to blank it later with `blank`. Lines
/// too long for the screen are cut, and lines that don't fit are dropped.
pub fn render_box(out: &mut Vec<u8>, lines: &[String], cols: u16, rows: u16) -> Rect {
    let inner = lines.iter().map(|line| unmark(line).chars().count()).max().unwrap_or(0);
    let width = (inner as u16 + 4).min(cols);
    let height = (lines.len() as u16 + 2).min(rows.saturating_sub(1));
    let col = (cols - width) / 2 + 1;
    let row = 2 + (rows.saturating_sub(1) - height) / 2;
    let text_width = width.saturating_sub(4) as usize;
    let mut s = String::new();
    let rule = "─".repeat(width.saturating_sub(2) as usize);
    let colors = palette();
    let box_style = colors.box_style.as_str();
    let _ = write!(s, "\x1b[{row};{col}H{box_style}╭{rule}╮");
    for (n, line) in lines.iter().take(height.saturating_sub(2) as usize).enumerate() {
        let style = match line.chars().next() {
            Some(ART_LINE) => colors.box_art.as_str(),
            Some(TITLE_LINE) => colors.box_title.as_str(),
            Some('\t') => colors.box_dim.as_str(),
            _ => box_style,
        };
        let text: String = unmark(line).chars().take(text_width).collect();
        let pad = text_width - text.chars().count();
        let _ = write!(s, "\x1b[{};{col}H{box_style}│ {style}{text}{:pad$}{box_style} │", row + 1 + n as u16, "");
    }
    let _ = write!(s, "\x1b[{};{col}H{box_style}╰{rule}╯\x1b[0m", row + height - 1);
    out.extend_from_slice(s.as_bytes());
    Rect { col, row, width, height }
}

/// Marks the start of a line of `render_box` to style it.
pub const ART_LINE: char = '\u{1}';
pub const TITLE_LINE: char = '\u{2}';

fn unmark(line: &str) -> &str {
    line.strip_prefix([ART_LINE, TITLE_LINE, '\t']).unwrap_or(line)
}

/// Blank the cells a box covered, so the page under them shows again.
pub fn blank(out: &mut Vec<u8>, rect: Rect) {
    let mut s = String::from("\x1b[0m");
    for row in rect.row..rect.row + rect.height {
        let _ = write!(s, "\x1b[{row};{}H{:width$}", rect.col, "", width = rect.width as usize);
    }
    out.extend_from_slice(s.as_bytes());
}

/// Draw the top row: the buttons and `bar`, or `label` in the buttons'
/// place when the field is for something other than the address.
/// `badge`, if given, is shown at the right end, after the field.
pub fn render_bar(
    out: &mut Vec<u8>,
    bar: &mut AddressBar,
    cols: usize,
    loading: bool,
    label: Option<&str>,
    badge: Option<&str>,
) {
    let mut s = String::from("\x1b[1;1H");
    let colors = palette();
    s.push_str(&colors.bar);
    if cols < MIN_COLS {
        s.push_str("\x1b[2K\x1b[0m\x1b[?25l");
        out.extend_from_slice(s.as_bytes());
        return;
    }
    // While loading, reload becomes stop.
    match label {
        Some(label) => {
            let _ = write!(s, " {label:<width$}", width = FIELD - 2);
        }
        None => {
            let _ = write!(s, " ←  →  {}  ", if loading { '×' } else { '↻' });
        }
    }

    let badge = badge.filter(|badge| cols > MIN_COLS + badge.chars().count());
    let badge_width = badge.map_or(0, |badge| badge.chars().count() + 1);
    let width = cols - FIELD - 2 - CLOSE - badge_width;
    let window = bar.window(width);
    let shown: Vec<char> = bar.text.iter().chain(&bar.completion).copied().collect();
    let typed_end = bar.text.len().clamp(window.start, window.end);
    let visible: String = shown[window.start..typed_end].iter().collect();
    let completion: String = shown[typed_end..window.end].iter().collect();
    s.push_str(if bar.focused { &colors.focused } else { &colors.field });
    s.push(' ');
    if bar.selected {
        let _ = write!(s, "\x1b[7m{visible}\x1b[27m");
    } else {
        s.push_str(&visible);
        if !completion.is_empty() {
            let _ = write!(s, "\x1b[7m{completion}\x1b[27m");
        }
    }
    let _ = write!(s, "{:pad$}", "", pad = width - window.len() + 1);
    s.push_str(&colors.bar);
    s.push(' ');
    if let Some(badge) = badge {
        let _ = write!(s, "{badge} ");
    }
    s.push_str(" ✕ \x1b[0m");
    if bar.focused {
        // A bar-shaped cursor at the insertion point.
        let _ = write!(s, "\x1b[1;{}H\x1b[6 q\x1b[?25h", FIELD + 1 + bar.cursor - window.start);
    } else {
        s.push_str("\x1b[?25l");
    }
    out.extend_from_slice(s.as_bytes());
}

/// Turn what was typed into a URL: pass URLs through, add a scheme to bare
/// hosts, resolve local files, and search for anything else.
pub fn normalize_url(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let scheme_end = input.find(':').unwrap_or(0);
    let scheme = &input[..scheme_end];
    let has_scheme = input[scheme_end..].starts_with("://")
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    if has_scheme || ["about", "data", "file", "view-source", "chrome"].contains(&scheme) {
        return Some(input.to_owned());
    }
    if input.starts_with(['/', '.', '~']) {
        let path = match (input.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
            _ => input.into(),
        };
        if let Ok(path) = path.canonicalize() {
            return Some(format!("file://{}", path.display()));
        }
    }
    let host = input.split(['/', '?', '#']).next().unwrap_or("");
    let name = host.rsplit_once(':').map_or(host, |(name, _port)| name);
    let local = name == "localhost" || name.parse::<std::net::Ipv4Addr>().is_ok();
    if input.contains(char::is_whitespace) || !(local || name.contains('.')) {
        let mut query = String::new();
        for byte in input.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => query.push(byte as char),
                b' ' => query.push('+'),
                _ => {
                    let _ = write!(query, "%{byte:02X}");
                }
            }
        }
        return Some(format!("https://www.google.com/search?q={query}"));
    }
    Some(format!("{}://{input}", if local { "http" } else { "https" }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn box_is_centred_below_the_bar() {
        let mut out = Vec::new();
        let lines = vec!["\u{2}Title".to_owned(), "\tdim".to_owned(), "a longer line".to_owned()];
        let rect = super::render_box(&mut out, &lines, 41, 21);
        // 13 characters plus a border and a space each side; 3 lines plus borders.
        assert_eq!(rect, super::Rect { col: 13, row: 9, width: 17, height: 5 });
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[9;13H"), "{text:?}");
        assert!(text.contains("dim          "), "{text:?}");
        let mut out = Vec::new();
        super::blank(&mut out, rect);
        assert_eq!(String::from_utf8(out).unwrap().matches("                 ").count(), 5);
    }

    use super::*;

    fn press(bar: &mut AddressBar, code: KeyCode) -> Edit {
        bar.key(&Key { code, ctrl: false, alt: false, shift: false, cmd: false })
    }

    fn bar(text: &str) -> AddressBar {
        let mut bar = AddressBar::default();
        bar.set_text(text);
        bar.focus();
        bar
    }

    #[test]
    fn typing_replaces_the_selected_address() {
        let mut bar = bar("https://old.example");
        press(&mut bar, KeyCode::Char('n'));
        press(&mut bar, KeyCode::Char('e'));
        press(&mut bar, KeyCode::Char('w'));
        assert_eq!(bar.text(), "new");
    }

    #[test]
    fn moving_keeps_the_address() {
        let mut bar = bar("abc");
        press(&mut bar, KeyCode::Right);
        press(&mut bar, KeyCode::Char('d'));
        assert_eq!(bar.text(), "abcd");
        press(&mut bar, KeyCode::Home);
        press(&mut bar, KeyCode::Delete);
        press(&mut bar, KeyCode::Char('x'));
        assert_eq!(bar.text(), "xbcd");
        press(&mut bar, KeyCode::End);
        press(&mut bar, KeyCode::Backspace);
        assert_eq!(bar.text(), "xbc");
    }

    #[test]
    fn backspace_clears_a_selection() {
        let mut bar = bar("abc");
        press(&mut bar, KeyCode::Backspace);
        assert_eq!(bar.text(), "");
    }

    #[test]
    fn click_places_the_cursor() {
        let mut bar = bar("abcdef");
        bar.click(2);
        press(&mut bar, KeyCode::Char('X'));
        assert_eq!(bar.text(), "abXcdef");
        bar.click(99);
        press(&mut bar, KeyCode::Char('!'));
        assert_eq!(bar.text(), "abXcdef!");
    }

    #[test]
    fn word_keys() {
        let mut bar = bar("one.two/three");
        bar.key(&Key { code: KeyCode::Char('w'), ctrl: true, alt: false, shift: false, cmd: false });
        assert_eq!(bar.text(), "one.two/");
        bar.key(&Key { code: KeyCode::Left, ctrl: false, alt: true, shift: false, cmd: false });
        press(&mut bar, KeyCode::Char('_'));
        assert_eq!(bar.text(), "one._two/");
    }

    #[test]
    fn completion_is_offered_and_accepted() {
        let mut bar = bar("old");
        assert_eq!(press(&mut bar, KeyCode::Char('g')), Edit::Typed);
        bar.set_completion(Some("regraiz.com".into()));
        assert_eq!((bar.typed().as_str(), bar.text().as_str()), ("g", "gregraiz.com"));
        assert_eq!(press(&mut bar, KeyCode::Enter), Edit::Submit);
        assert_eq!(bar.text(), "gregraiz.com");

        bar.set_text("g");
        bar.set_completion(Some("regraiz.com".into()));
        press(&mut bar, KeyCode::Right);
        press(&mut bar, KeyCode::Char('/'));
        assert_eq!(bar.typed(), "gregraiz.com/");
    }

    #[test]
    fn completion_is_rejected_or_dropped() {
        let mut bar = bar("");
        press(&mut bar, KeyCode::Char('g'));
        bar.set_completion(Some("regraiz.com".into()));
        assert_eq!(press(&mut bar, KeyCode::Backspace), Edit::Changed);
        assert_eq!(bar.text(), "g");
        press(&mut bar, KeyCode::Backspace);
        assert_eq!(bar.text(), "");

        bar.set_text("g");
        bar.set_completion(Some("regraiz.com".into()));
        press(&mut bar, KeyCode::Left);
        assert_eq!(bar.text(), "g");
        // Typing in the middle of the text doesn't ask for a completion.
        assert_eq!(press(&mut bar, KeyCode::Char('x')), Edit::Changed);
        assert_eq!(bar.text(), "xg");
    }

    #[test]
    fn completion_is_drawn_after_the_typed_text() {
        let mut bar = bar("");
        press(&mut bar, KeyCode::Char('g'));
        bar.set_completion(Some("o.com".into()));
        let mut out = Vec::new();
        render_bar(&mut out, &mut bar, 40, false, None, None);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(" g\x1b[7mo.com\x1b[27m"), "{text:?}");
        // The cursor sits after the typed text, before the completion.
        assert!(text.contains("\x1b[1;13H"), "{text:?}");
    }

    #[test]
    fn enter_and_esc() {
        let mut bar = bar("abc");
        assert_eq!(press(&mut bar, KeyCode::Enter), Edit::Submit);
        assert_eq!(press(&mut bar, KeyCode::Esc), Edit::Cancel);
    }

    #[test]
    fn long_text_scrolls_with_the_cursor() {
        let mut bar = bar("0123456789");
        assert_eq!(bar.window(4), 7..10);
        press(&mut bar, KeyCode::Home);
        assert_eq!(bar.window(4), 0..4);
        bar.blur("0123456789");
        assert_eq!(bar.window(4), 0..4);
        bar.focus();
        assert_eq!(bar.window(4), 7..10);
        press(&mut bar, KeyCode::Char('a'));
        press(&mut bar, KeyCode::Char('b'));
        assert_eq!(bar.window(4), 0..2);
    }

    #[test]
    fn bar_fills_the_row() {
        for (text, focused) in [("", false), ("https://example.com", true), ("x".repeat(200).as_str(), true)] {
            let mut bar = AddressBar::default();
            bar.set_text(text);
            if focused {
                bar.focus();
            }
            let mut out = Vec::new();
            render_bar(&mut out, &mut bar, 40, false, None, None);
            // Count printed cells: drop escape sequences, which end in a letter.
            let text = String::from_utf8(out).unwrap();
            let mut cells = 0;
            let mut chars = text.chars();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    chars.by_ref().find(|c| c.is_ascii_alphabetic());
                } else {
                    cells += 1;
                }
            }
            assert_eq!(cells, 40, "{text:?}");
        }
    }

    #[test]
    fn hit_testing() {
        assert_eq!(hit(2, 80), Hit::Back);
        assert_eq!(hit(5, 80), Hit::Forward);
        assert_eq!(hit(8, 80), Hit::Reload);
        assert_eq!(hit(10, 80), Hit::Nothing);
        assert_eq!(hit(11, 80), Hit::Address(0));
        assert_eq!(hit(14, 80), Hit::Address(2));
        assert_eq!(hit(77, 80), Hit::Address(65));
        assert_eq!(hit(78, 80), Hit::Close);
        assert_eq!(hit(80, 80), Hit::Close);
        assert_eq!(hit(81, 80), Hit::Nothing);
        // " Quit?  " puts Yes at columns 9 to 13 and No at 15 to 18.
        assert_eq!(answer(8, "Quit?"), None);
        assert_eq!(answer(9, "Quit?"), Some(true));
        assert_eq!(answer(13, "Quit?"), Some(true));
        assert_eq!(answer(14, "Quit?"), None);
        assert_eq!(answer(15, "Quit?"), Some(false));
        assert_eq!(answer(18, "Quit?"), Some(false));
        assert_eq!(answer(19, "Quit?"), None);
    }

    #[test]
    fn urls() {
        let url = |s| normalize_url(s).unwrap();
        assert_eq!(normalize_url("  "), None);
        assert_eq!(url("https://a.com/x"), "https://a.com/x");
        assert_eq!(url("about:blank"), "about:blank");
        assert_eq!(url("gregraiz.com"), "https://gregraiz.com");
        assert_eq!(url("a.com/path?q=1"), "https://a.com/path?q=1");
        assert_eq!(url("localhost:3000"), "http://localhost:3000");
        assert_eq!(url("127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
        assert_eq!(url("rust traits"), "https://www.google.com/search?q=rust+traits");
        assert_eq!(url("what is 1+1?"), "https://www.google.com/search?q=what+is+1%2B1%3F");
        assert_eq!(url("kittens"), "https://www.google.com/search?q=kittens");
        assert_eq!(url("/"), "file:///");
    }
}
