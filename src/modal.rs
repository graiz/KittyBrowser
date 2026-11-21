//! Boxes that ask the user something over the page, as Chrome's own
//! windows do: a page's dialog (dialog.rs) and its file chooser
//! (picker.rs). Both are laid out on a `Canvas` of cells, then drawn as
//! ordinary text above the page image, the way the help box is.

use crate::term::graphics::Pixels;
use crate::ui::{self, AddressBar, Rect, Rgb};
use std::fmt::Write;

/// The look of a run of cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// The box's own text and background.
    Plain,
    Title,
    Dim,
    /// Text in the accent colour.
    Accent,
    /// What the box says: the message, file names.
    Text,
    /// A text field.
    Input,
    /// A text field, or a button Enter doesn't press.
    Field,
    /// The field being typed in, or the row the arrow keys are on.
    Focused,
    /// The button Enter presses.
    Default,
}

/// The page as last drawn, decoded, to cast a shadow on: a cell can't
/// darken what is under it, only cover it, so the shadow is the page's own
/// colours, darker.
pub struct Shade {
    pub pixels: Pixels,
    /// The cells the page image covers, from the top left below the bar.
    pub page: Rect,
}

impl Shade {
    /// The page's colours in the top and bottom halves of a cell of the
    /// pane (from 1), or None off the page.
    fn at(&self, col: u16, row: u16) -> Option<[Rgb; 2]> {
        let page = self.page;
        if !(page.col..page.col + page.width).contains(&col) || !(page.row..page.row + page.height).contains(&row) {
            return None;
        }
        let (width, height) = (self.pixels.width as usize, self.pixels.height as usize);
        let channels = if self.pixels.alpha { 4 } else { 3 };
        let span = |at: u16, start: u16, cells: u16, pixels: usize| {
            let (at, cells) = ((at - start) as usize, cells.max(1) as usize);
            let from = at * pixels / cells;
            (from, ((at + 1) * pixels / cells).clamp(from + 1, pixels.max(from + 1)))
        };
        let (x0, x1) = span(col, page.col, page.width, width);
        let (y0, y1) = span(row, page.row, page.height, height);
        let middle = (y0 + y1).div_ceil(2);
        // A few samples across each half are enough for its colour.
        let average = |top: usize, bottom: usize| -> Rgb {
            let (mut sum, mut n) = ([0usize; 3], 0);
            let step = |a: usize, b: usize| (b.saturating_sub(a) / 4).max(1);
            for y in (top..bottom.max(top + 1)).step_by(step(top, bottom)) {
                for x in (x0..x1).step_by(step(x0, x1)) {
                    let at = (y * width + x) * channels;
                    if let Some(pixel) = self.pixels.data.get(at..at + 3) {
                        sum = [sum[0] + pixel[0] as usize, sum[1] + pixel[1] as usize, sum[2] + pixel[2] as usize];
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            ((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8)
        };
        Some([average(y0, middle), average(middle, y1)])
    }
}

/// A colour in shadow.
fn darken((r, g, b): Rgb) -> Rgb {
    let dim = |c: u8| (c as f64 * 0.45) as u8;
    (dim(r), dim(g), dim(b))
}

/// A cell drawn as an upper half block in `top` over `bottom`.
fn halves(s: &mut String, col: u16, row: u16, top: Rgb, bottom: Rgb) {
    let _ = write!(
        s,
        "\x1b[{row};{col}H\x1b[0;38;2;{};{};{};48;2;{};{};{}m▀",
        top.0, top.1, top.2, bottom.0, bottom.1, bottom.2
    );
}

/// A grid of cells to lay a box out on before drawing it. A cell holds a
/// character and any marks combined with it; the cell after a wide one (a
/// CJK character, most emoji) holds nothing, the wide one covering it.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    cells: Vec<(String, Style)>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Canvas {
        Canvas { width, height, cells: vec![(" ".to_owned(), Style::Plain); width * height] }
    }

    /// Put `text` in cell `at` (of the whole grid), clearing what is left of
    /// a wide character it overwrites half of.
    fn set(&mut self, at: usize, text: String, style: Style) {
        let row_start = at - at % self.width;
        if self.cells[at].0.is_empty() && at > row_start {
            self.cells[at - 1].0 = " ".to_owned();
        }
        if at + 1 < row_start + self.width && self.cells[at + 1].0.is_empty() {
            self.cells[at + 1].0 = " ".to_owned();
        }
        self.cells[at] = (text, style);
    }

    /// Write `text` from `col` on `row` (both from 0), cut at `limit` cells
    /// and at the edge. Returns the column after it.
    pub fn put_limited(&mut self, row: usize, col: usize, text: &str, style: Style, limit: usize) -> usize {
        let mut at = col;
        if row >= self.height {
            return at;
        }
        let end = self.width.min(col.saturating_add(limit));
        let mut last: Option<usize> = None;
        for c in crate::term::drawable(text).chars() {
            let cell = row * self.width;
            match width(c) {
                0 => {
                    if let Some(last) = last {
                        self.cells[cell + last].0.push(c);
                        // ❤ with VS16 is drawn as the two-cell emoji.
                        if c == VS16 && last + 1 == at && at < end {
                            self.set(cell + at, String::new(), style);
                            at += 1;
                        }
                    }
                }
                wide => {
                    if at + wide > end {
                        break;
                    }
                    self.set(cell + at, c.to_string(), style);
                    if wide == 2 {
                        self.set(cell + at + 1, String::new(), style);
                    }
                    last = Some(at);
                    at += wide;
                }
            }
        }
        at
    }

    pub fn put(&mut self, row: usize, col: usize, text: &str, style: Style) -> usize {
        self.put_limited(row, col, text, style, usize::MAX)
    }

    /// Paint `len` cells from `col` on `row` in `style`, keeping their text.
    pub fn paint(&mut self, row: usize, col: usize, len: usize, style: Style) {
        if row >= self.height {
            return;
        }
        for at in col..(col + len).min(self.width) {
            self.cells[row * self.width + at].1 = style;
        }
    }

    /// The frame round the edge, with `title` set into the top edge.
    pub fn frame(&mut self, title: &str) {
        self.frame_inset(0, 0, title);
    }

    /// The frame `top` rows and `side` columns in from the edge, leaving a
    /// margin of the box's colour round it.
    pub fn frame_inset(&mut self, top: usize, side: usize, title: &str) {
        let (w, h) = (self.width.saturating_sub(2 * side), self.height.saturating_sub(2 * top));
        if w < 2 || h < 2 {
            return;
        }
        self.put(top, side, &format!("╭{}╮", "─".repeat(w - 2)), Style::Plain);
        for row in top + 1..top + h - 1 {
            self.put(row, side, "│", Style::Plain);
            self.put(row, side + w - 1, "│", Style::Plain);
        }
        self.put(top + h - 1, side, &format!("╰{}╯", "─".repeat(w - 2)), Style::Plain);
        if !title.is_empty() {
            // In the middle of the top edge, as dialog(1) and whiptail set it.
            let title = format!(" {title} ");
            let col = side + 2 + w.saturating_sub(4 + text_width(&title)) / 2;
            self.put_limited(top, col, &title, Style::Accent, w.saturating_sub(4));
        }
    }

    /// A rule across the box at `row`, joining the frame.
    pub fn rule(&mut self, row: usize) {
        if self.width >= 2 {
            self.put(row, 0, &format!("├{}┤", "─".repeat(self.width - 2)), Style::Plain);
        }
    }

    /// A button, "< label >" as in dialog(1): filled in the accent colour
    /// when Enter presses it, plain on the box otherwise. Returns the
    /// columns it covers.
    pub fn button(&mut self, row: usize, col: usize, label: &str, default: bool) -> std::ops::Range<usize> {
        let text = format!("< {label} >");
        // Never over the frame at the right edge.
        let limit = self.width.saturating_sub(1).saturating_sub(col);
        let end = self.put_limited(row, col, &text, if default { Style::Default } else { Style::Plain }, limit);
        col..end
    }

    /// Draw `field` at `col` on `row`, `width` cells wide. Returns the
    /// cursor's column when the field has focus.
    pub fn field(&mut self, row: usize, col: usize, width: usize, field: &mut AddressBar) -> Option<usize> {
        let focused = field.focused();
        let style = Style::Input;
        let inner = width.saturating_sub(2);
        let (text, cursor, selected) = field.view(inner);
        self.put(row, col, &" ".repeat(width), style);
        let end = self.put_limited(row, col + 1, &text, style, inner);
        if selected && focused {
            // A selection shows as reversed text, as in the address field.
            self.paint(row, col + 1, end - (col + 1), Style::Default);
        }
        let cursor: usize = text.chars().take(cursor).map(self::width).sum();
        focused.then_some(col + 1 + cursor.min(inner))
    }

    /// Draw the canvas centred in a `cols` x `rows` screen below the top
    /// bar, with a shadow below it and to its right where it falls on the
    /// page `shade` holds. Returns where it went, shadow included.
    pub fn draw(&self, out: &mut Vec<u8>, cols: u16, rows: u16, shade: Option<&Shade>) -> Rect {
        let width = (self.width as u16).min(cols);
        let height = (self.height as u16).min(rows.saturating_sub(1));
        let col = (cols - width) / 2 + 1;
        let row = 2 + (rows.saturating_sub(1) - height) / 2;
        let colors = ui::palette();
        let escape = |style: Style| -> &str {
            match style {
                Style::Plain => &colors.box_style,
                Style::Title => &colors.box_title,
                Style::Dim => &colors.box_dim,
                Style::Accent => &colors.box_accent,
                Style::Text => &colors.box_text,
                Style::Input => &colors.input,
                Style::Field => &colors.field,
                Style::Focused => &colors.focused,
                Style::Default => &colors.accent,
            }
        };
        let mut s = String::new();
        for y in 0..height as usize {
            let _ = write!(s, "\x1b[{};{col}H", row + y as u16);
            let mut last = None;
            for (text, style) in &self.cells[y * self.width..y * self.width + width as usize] {
                if last != Some(*style) {
                    s.push_str(escape(*style));
                    last = Some(*style);
                }
                s.push_str(text);
            }
        }
        // A column at the right and half a row below, offset down and
        // right: about as thick both ways, cells being twice as tall as
        // wide. Only on the page: off it, it would hang over nothing.
        let (right, below) = (col + width, row + height);
        if let Some(shade) = shade {
            for y in row + 1..below {
                if let Some([top, bottom]) = shade.at(right, y) {
                    halves(&mut s, right, y, darken(top), darken(bottom));
                }
            }
            for x in col + 1..=right {
                if let Some([top, bottom]) = shade.at(x, below) {
                    halves(&mut s, x, below, darken(top), bottom);
                }
            }
        }
        let rect = Rect { col, row, width: (width + 1).min(cols + 1 - col), height: (height + 1).min(rows + 1 - row) };
        s.push_str("\x1b[0m");
        out.extend_from_slice(s.as_bytes());
        rect
    }

    /// The text of a row, for tests.
    #[cfg(test)]
    pub fn line(&self, row: usize) -> String {
        self.cells[row * self.width..(row + 1) * self.width].iter().map(|cell| cell.0.as_str()).collect()
    }
}

/// Where a click in the pane falls on a box drawn at `rect`: the cell
/// within it, from 0, or None if outside.
pub fn inside(rect: Rect, col: usize, row: usize) -> Option<(usize, usize)> {
    let (left, top) = (rect.col as usize, rect.row as usize);
    let within = (left..left + rect.width as usize).contains(&col) && (top..top + rect.height as usize).contains(&row);
    within.then(|| (col - left, row - top))
}

/// How many cells a character takes in a terminal: 2 for East Asian wide
/// characters and emoji, 0 for marks combined with the one before, else 1.
/// Emoji sequences are approximate: a skin tone joins the emoji before it,
/// and VS16 (U+FE0F) widens a narrow one, but ZWJ sequences count each part.
pub fn width(c: char) -> usize {
    match c as u32 {
        0x1F3FB..=0x1F3FF => 0,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F => 0,
        0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF => 0,
        0x200B..=0x200F | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F => 0,
        0xE0100..=0xE01EF => 0,
        0x1100..=0x115F | 0x231A..=0x231B | 0x2329..=0x232A | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 => 2,
        0x25FD..=0x25FE | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1 => 2,
        0x26AA..=0x26AB | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE | 0x26D4 | 0x26EA => 2,
        0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C => 2,
        0x274E | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C => 2,
        0x2B50 | 0x2B55 | 0x2E80..=0x303E | 0x3041..=0x3098 | 0x309B..=0x33FF | 0x3400..=0x4DBF => 2,
        0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xA960..=0xA97F | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF => 2,
        0xFE10..=0xFE19 | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x16FE0..=0x18CFF => 2,
        0x1B000..=0x1B2FF | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A | 0x1F200..=0x1F251 => 2,
        0x1F300..=0x1F320 | 0x1F32D..=0x1F335 | 0x1F337..=0x1F37C | 0x1F37E..=0x1F393 => 2,
        0x1F3A0..=0x1F3CA | 0x1F3CF..=0x1F3D3 | 0x1F3E0..=0x1F3F0 | 0x1F3F4 | 0x1F3F8..=0x1F43E => 2,
        0x1F440 | 0x1F442..=0x1F4FC | 0x1F4FF..=0x1F53D | 0x1F54B..=0x1F54E | 0x1F550..=0x1F567 => 2,
        0x1F57A | 0x1F595..=0x1F596 | 0x1F5A4 | 0x1F5FB..=0x1F64F | 0x1F680..=0x1F6C5 | 0x1F6CC => 2,
        0x1F6D0..=0x1F6D2 | 0x1F6D5..=0x1F6D7 | 0x1F6DC..=0x1F6DF | 0x1F6EB..=0x1F6EC => 2,
        0x1F6F4..=0x1F6FC | 0x1F7E0..=0x1F7EB | 0x1F7F0 | 0x1F90C..=0x1F93A | 0x1F93C..=0x1F945 => 2,
        0x1F947..=0x1F9FF | 0x1FA70..=0x1FAFF | 0x20000..=0x2FFFD | 0x30000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// How many cells `text` takes.
pub fn text_width(text: &str) -> usize {
    let mut last = 0;
    text.chars()
        .map(|c| {
            // VS16 asks for the emoji form, two cells wide.
            let wide = if c == VS16 && last == 1 { 1 } else { width(c) };
            last = if wide == 0 { last } else { wide };
            wide
        })
        .sum()
}

/// Variation selector 16, which asks for a character's emoji form.
const VS16: char = '\u{FE0F}';

/// Break `text` into lines at most `width` cells long, at spaces where it
/// can and mid-word where it must, keeping the lines it already has.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let word = crate::term::drawable(word);
            let used = text_width(&line);
            if used > 0 && used + 1 + text_width(&word) <= width {
                line.push(' ');
            } else if used > 0 {
                lines.push(std::mem::take(&mut line));
            }
            // Only a word longer than a whole line is left over: cut it.
            for c in word.chars() {
                let wide = self::width(c);
                if text_width(&line) + wide > width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(c);
            }
        }
        lines.push(line);
    }
    lines
}

/// The start of `text` that fits in `width` cells.
pub fn cut(text: &str, width: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += self::width(*c);
            used <= width
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces_and_keeps_lines() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("a\nb", 10), ["a", "b"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
        // Wide characters take two cells each.
        assert_eq!(wrap("保存しました", 6), ["保存し", "ました"]);
        assert_eq!(wrap("ok 保存", 5), ["ok", "保存"]);
    }

    #[test]
    fn wide_characters_take_two_cells() {
        assert_eq!(text_width("保存しました"), 12);
        assert_eq!(text_width("📁 a"), 4);
        assert_eq!(text_width("👍🏽"), 2);
        assert_eq!(text_width("❤\u{FE0F}x"), 3);
        let mut canvas = Canvas::new(4, 1);
        assert_eq!(canvas.put(0, 0, "❤\u{FE0F}x", Style::Text), 3);
        // A decomposed é, as macOS names files, is one cell.
        assert_eq!(text_width("cafe\u{301}"), 4);
        assert_eq!(cut("保存しました", 5), "保存");
        let mut canvas = Canvas::new(10, 1);
        assert_eq!(canvas.put(0, 0, "保存a", Style::Text), 5);
        assert_eq!(canvas.line(0), "保存a     ");
        // One that doesn't fit whole is left out, not cut in half.
        assert_eq!(canvas.put_limited(0, 0, "ab保存", Style::Text, 5), 4);
        assert_eq!(canvas.line(0), "ab保a     ");
        // Overwriting half of one clears the other half.
        let mut canvas = Canvas::new(4, 1);
        canvas.put(0, 0, "保存", Style::Text);
        canvas.put(0, 1, "x", Style::Text);
        assert_eq!(canvas.line(0), " x存");
        let mut canvas = Canvas::new(4, 1);
        canvas.put(0, 0, "cafe\u{301}", Style::Text);
        assert_eq!(canvas.line(0), "cafe\u{301}");
    }

    #[test]
    fn frame_and_title() {
        let mut canvas = Canvas::new(12, 3);
        canvas.frame("Hi");
        assert_eq!(canvas.line(0), "╭─── Hi ───╮");
        assert_eq!(canvas.line(1), "│          │");
        assert_eq!(canvas.line(2), "╰──────────╯");
    }

    #[test]
    fn the_shadow_is_the_page_darker() {
        // A page 4 cells wide and 2 high, 2x4 pixels a cell: white above
        // the middle of each cell, black below.
        let (width, height) = (8, 8);
        let mut data = Vec::new();
        for y in 0..height {
            let value = if y % 4 < 2 { 255 } else { 0 };
            data.extend(std::iter::repeat_n(value, width * 3));
        }
        let pixels = Pixels { width: width as u32, height: height as u32, alpha: false, data };
        let shade = Shade { pixels, page: Rect { col: 1, row: 2, width: 4, height: 2 } };
        assert_eq!(shade.at(1, 2), Some([(255, 255, 255), (0, 0, 0)]));
        assert_eq!(shade.at(4, 3), Some([(255, 255, 255), (0, 0, 0)]));
        assert_eq!(shade.at(5, 2), None);
        assert_eq!(shade.at(1, 1), None);
        assert_eq!(darken((200, 100, 0)), (90, 45, 0));
    }

    #[test]
    fn clicks_land_inside_the_box() {
        let rect = Rect { col: 5, row: 3, width: 10, height: 4 };
        assert_eq!(inside(rect, 5, 3), Some((0, 0)));
        assert_eq!(inside(rect, 14, 6), Some((9, 3)));
        assert_eq!(inside(rect, 15, 6), None);
        assert_eq!(inside(rect, 4, 3), None);
    }
}
