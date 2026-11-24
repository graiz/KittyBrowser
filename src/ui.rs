//! Browser chrome drawn as ordinary terminal text above the page image: back,
//! forward and reload buttons, and the address field.

use crate::term::input::{Key, KeyCode};
use std::fmt::Write;

/// Columns (1-based) of the top bar. Each button is three cells wide.
const BACK: std::ops::RangeInclusive<usize> = 1..=3;
const FORWARD: std::ops::RangeInclusive<usize> = 4..=6;
const RELOAD: std::ops::RangeInclusive<usize> = 7..=9;
/// The address field starts here, with one cell of padding inside each end.
const FIELD: usize = 11;
/// The menu button, at the right end: a space, ⋮, a space.
const MENU: usize = 3;
/// The fewest columns of address kept when the switcher and the badge
/// share the bar with it.
const MIN_FIELD: usize = 8;
/// Panes narrower than this get an empty bar.
const MIN_COLS: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Back,
    Forward,
    Reload,
    Menu,
    /// The window switcher's halves.
    PreviousWindow,
    NextWindow,
    /// A click in the address field, as an offset into the visible text.
    Address(usize),
    Nothing,
}

/// The right of the bar, after the address field: the window switcher when
/// a page has opened windows ("‹ 2/3 ›" for the second of three shown), then
/// the badge (a note, the spinner, a download's progress, the zoom), then ⋮
/// at the end, as far as there's room, the address keeping MIN_FIELD columns.
/// Returns the switcher and the badge to show, and the field's width.
fn right_side(cols: usize, windows: (usize, usize), badge: Option<&str>) -> (Option<String>, Option<&str>, usize) {
    let mut width = cols.saturating_sub(FIELD + 2 + MENU);
    let text = format!("‹ {}/{} ›", windows.0, windows.1);
    let switcher = (windows.1 > 1 && width > MIN_FIELD + text.chars().count()).then_some(text);
    width -= switcher.as_ref().map_or(0, |text| text.chars().count() + 1);
    let badge = badge.filter(|badge| width > MIN_FIELD + badge.chars().count());
    width -= badge.map_or(0, |badge| badge.chars().count() + 1);
    (switcher, badge, width)
}

/// What a column of the top bar holds, with `badge` shown in it.
pub fn hit(col: usize, cols: usize, windows: (usize, usize), badge: Option<&str>) -> Hit {
    let (switcher, badge, _) = right_side(cols, windows, badge);
    let switcher = switcher.map_or(0, |text| text.chars().count());
    let badge = badge.map_or(0, |badge| badge.chars().count() + 1);
    let switcher_start = cols.saturating_sub(MENU + badge + switcher + 1) + 1;
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
    } else if col > cols - MENU {
        Hit::Menu
    } else if col > cols - MENU - badge {
        Hit::Nothing
    } else if switcher > 0 && col >= switcher_start {
        // Either half: wide enough to hit, the arrows being one column each.
        if col < switcher_start + switcher / 2 {
            Hit::PreviousWindow
        } else {
            Hit::NextWindow
        }
    } else if (FIELD..switcher_start).contains(&col) {
        Hit::Address(col.saturating_sub(FIELD + 1))
    } else {
        Hit::Nothing
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
        self.completion = crate::term::drawable(&completion.unwrap_or_default()).chars().collect();
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

    /// Show `text`, an address or a find, from the page or the user.
    pub fn set_text(&mut self, text: &str) {
        self.text = crate::term::drawable(text).chars().collect();
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
        for c in crate::term::drawable(text).chars() {
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

    /// What a field `width` cells wide shows, for fields drawn in a box: the
    /// visible text, the cursor's offset into it, and whether it is all
    /// selected. Completions belong to the address field and aren't shown.
    pub fn view(&mut self, width: usize) -> (String, usize, bool) {
        let window = self.window(width.max(1));
        let end = window.end.min(self.text.len());
        let start = window.start.min(end);
        let visible = self.text[start..end].iter().collect();
        (visible, self.cursor.saturating_sub(start), self.selected)
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
pub(crate) struct Palette {
    pub bar: String,
    pub field: String,
    pub focused: String,
    pub box_art: String,
    pub box_style: String,
    pub box_title: String,
    pub box_dim: String,
    /// The button Enter presses, and what is marked: the logo's colour.
    pub accent: String,
    /// Text in the logo's colour on the box.
    pub box_accent: String,
    /// What a box says, in the terminal's own text colour.
    pub box_text: String,
    /// A text field in a box: sunk to the terminal's background.
    pub input: String,
}

static PALETTE: std::sync::RwLock<Option<Palette>> = std::sync::RwLock::new(None);

pub(crate) fn palette() -> Palette {
    PALETTE.read().ok().and_then(|p| p.clone()).unwrap_or_else(|| Palette {
        bar: "\x1b[0;48;5;236;38;5;252m".into(),
        field: "\x1b[0;48;5;238;38;5;250m".into(),
        focused: "\x1b[0;48;5;240;38;5;231m".into(),
        box_art: "\x1b[0;48;5;236;38;5;141m".into(),
        box_style: "\x1b[0;48;5;236;38;5;252m".into(),
        box_title: "\x1b[0;48;5;236;38;5;231;1m".into(),
        box_dim: "\x1b[0;48;5;236;38;5;245m".into(),
        accent: "\x1b[0;48;5;141;38;5;235;1m".into(),
        box_accent: "\x1b[0;48;5;236;38;5;141;1m".into(),
        box_text: "\x1b[0;48;5;236;38;5;231m".into(),
        input: "\x1b[0;48;5;233;38;5;231m".into(),
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
        accent: style(art, bg, ";1"),
        box_accent: style(bar, art, ";1"),
        box_text: style(bar, fg, ""),
        input: style(bg, fg, ""),
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
    // The lines can be the page's: an inspected element.
    let shown = |line: &str| crate::term::drawable(unmark(line));
    let inner = lines.iter().map(|line| shown(line).chars().count()).max().unwrap_or(0);
    let width = inner.saturating_add(4).min(cols as usize) as u16;
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
        let text: String = shown(line).chars().take(text_width).collect();
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
/// `badge`, if given, is shown at the right end, after the field, and the
/// window switcher after it when `windows` (the one shown, how many) says
/// there's more than one.
pub fn render_bar(
    out: &mut Vec<u8>,
    bar: &mut AddressBar,
    cols: usize,
    loading: bool,
    label: Option<&str>,
    badge: Option<&str>,
    windows: (usize, usize),
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

    let (switcher, badge, width) = right_side(cols, windows, badge);
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
    if let Some(switcher) = switcher {
        let _ = write!(s, "{switcher} ");
    }
    if let Some(badge) = badge {
        let _ = write!(s, "{badge} ");
    }
    s.push_str(" ⋮ \x1b[0m");
    if bar.focused {
        // A bar-shaped cursor at the insertion point.
        let _ = write!(s, "\x1b[1;{}H\x1b[6 q\x1b[?25h", FIELD + 1 + bar.cursor - window.start);
    } else {
        s.push_str("\x1b[?25l");
    }
    out.extend_from_slice(s.as_bytes());
}

/// The host of a URL, with its port and without any "user@": "" for a file
/// or anything else without one.
pub fn host_of(url: &str) -> &str {
    let host = url.split_once("://").map_or("", |(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""));
    host.rsplit_once('@').map_or(host, |(_, host)| host)
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
    fn hosts_drop_the_user() {
        assert_eq!(super::host_of("https://example.com/a?b"), "example.com");
        assert_eq!(super::host_of("http://me:pw@localhost:8080/x"), "localhost:8080");
        assert_eq!(super::host_of("https://a.com#x@y"), "a.com");
        assert_eq!(super::host_of("file:///tmp/a.html"), "");
        assert_eq!(super::host_of("about:blank"), "");
    }

    #[test]
    fn page_text_drawn_in_the_pane_has_no_controls() {
        let page = "one\x1b]52;c;eA==\x07two\u{85}three\r";
        let mut out = Vec::new();
        super::render_box(&mut out, &[page.to_owned(), "x".repeat(70_000)], 80, 20);
        let drawn = String::from_utf8(out).unwrap();
        assert!(drawn.contains("one]52;c;eA==twothree "), "{drawn:?}");
        assert!(!drawn.contains('\x07') && !drawn.contains('\u{85}') && !drawn.contains('\r'));
        let mut bar = super::AddressBar::default();
        bar.set_text(page);
        bar.set_completion(Some(page.to_owned()));
        assert_eq!(bar.text(), "one]52;c;eA==twothree".repeat(2));
    }

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
        render_bar(&mut out, &mut bar, 40, false, None, None, (1, 1));
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
            render_bar(&mut out, &mut bar, 40, false, None, None, (1, 1));
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
        let hit = |col, windows| hit(col, 80, windows, None);
        assert_eq!(hit(2, (1, 1)), Hit::Back);
        assert_eq!(hit(5, (1, 1)), Hit::Forward);
        assert_eq!(hit(8, (1, 1)), Hit::Reload);
        assert_eq!(hit(10, (1, 1)), Hit::Nothing);
        assert_eq!(hit(11, (1, 1)), Hit::Address(0));
        assert_eq!(hit(14, (1, 1)), Hit::Address(2));
        assert_eq!(hit(76, (1, 1)), Hit::Address(64));
        assert_eq!(hit(77, (1, 1)), Hit::Nothing);
        // " ⋮ " takes columns 78 to 80.
        assert_eq!(hit(78, (1, 1)), Hit::Menu);
        assert_eq!(hit(80, (1, 1)), Hit::Menu);
        assert_eq!(hit(81, (1, 1)), Hit::Nothing);
        // "‹ 2/3 ›" takes columns 70 to 76, before ⋮.
        assert_eq!(hit(68, (2, 3)), Hit::Address(56));
        assert_eq!(hit(70, (2, 3)), Hit::PreviousWindow);
        assert_eq!(hit(72, (2, 3)), Hit::PreviousWindow);
        assert_eq!(hit(73, (2, 3)), Hit::NextWindow);
        assert_eq!(hit(76, (2, 3)), Hit::NextWindow);
        assert_eq!(hit(78, (2, 3)), Hit::Menu);
    }

    #[test]
    fn the_bar_is_drawn_where_it_is_hit() {
        // The switcher, then the badge, then ⋮, as drawn.
        let badge = "Downloading 1.3 of 20 MB";
        let mut out = Vec::new();
        render_bar(&mut out, &mut AddressBar::default(), 80, false, None, Some(badge), (2, 3));
        let drawn = String::from_utf8(out).unwrap();
        let mut text = String::new();
        let mut chars = drawn.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // An escape: CSI up to its final letter, or OSC up to ST.
                match chars.next() {
                    Some('[') => while chars.next().is_some_and(|c| !c.is_ascii_alphabetic() && c != '~') {},
                    _ => while chars.next().is_some_and(|c| c != '\\') {},
                }
            } else {
                text.push(c);
            }
        }
        let column = |what: &str| text.find(what).map(|at| text[..at].chars().count() + 1).unwrap();
        assert_eq!(text.chars().count(), 80, "{text:?}");
        assert!(column("‹") < column("Downloading") && column("Downloading") < column("⋮"), "{text:?}");
        let hit = |col| hit(col, 80, (2, 3), Some(badge));
        assert_eq!(hit(column("‹")), Hit::PreviousWindow);
        assert_eq!(hit(column("›")), Hit::NextWindow);
        assert_eq!(hit(column("Downloading")), Hit::Nothing);
        assert_eq!(hit(column("⋮")), Hit::Menu);
        assert_eq!(hit(column("‹") - 2), Hit::Address(column("‹") - 2 - 12));
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
