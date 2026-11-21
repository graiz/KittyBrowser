//! Boxes that ask the user something over the page, as Chrome's own
//! windows do: a page's dialog (dialog.rs) and its file chooser
//! (picker.rs). Both are laid out on a `Canvas` of cells, then drawn as
//! ordinary text above the page image, the way the help box is.

use crate::ui::{self, AddressBar, Rect};
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

/// A grid of cells to lay a box out on before drawing it.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    cells: Vec<(char, Style)>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Canvas {
        Canvas { width, height, cells: vec![(' ', Style::Plain); width * height] }
    }

    /// Write `text` from `col` on `row` (both from 0), cut at `limit` cells
    /// and at the edge. Returns the column after it.
    pub fn put_limited(&mut self, row: usize, col: usize, text: &str, style: Style, limit: usize) -> usize {
        let mut at = col;
        if row >= self.height {
            return at;
        }
        for c in text.chars().filter(|c| !c.is_control()).take(limit) {
            if at >= self.width {
                break;
            }
            self.cells[row * self.width + at] = (c, style);
            at += 1;
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
            self.put_limited(top, side + 2, &format!(" {title} "), Style::Title, w.saturating_sub(4));
        }
    }

    /// A rule across the box at `row`, joining the frame.
    pub fn rule(&mut self, row: usize) {
        if self.width >= 2 {
            self.put(row, 0, &format!("├{}┤", "─".repeat(self.width - 2)), Style::Plain);
        }
    }

    /// A button, "[ label ]"-shaped but drawn as a filled block. Returns the
    /// columns it covers.
    pub fn button(&mut self, row: usize, col: usize, label: &str, default: bool) -> std::ops::Range<usize> {
        let text = format!("  {label}  ");
        let end = self.put(row, col, &text, if default { Style::Default } else { Style::Field });
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
            for at in col + 1..end {
                self.cells[row * self.width + at].1 = Style::Default;
            }
        }
        focused.then_some(col + 1 + cursor.min(inner))
    }

    /// Draw the canvas centred in a `cols` x `rows` screen below the top
    /// bar. Returns where it went.
    pub fn draw(&self, out: &mut Vec<u8>, cols: u16, rows: u16) -> Rect {
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
            for &(c, style) in &self.cells[y * self.width..y * self.width + width as usize] {
                if last != Some(style) {
                    s.push_str(escape(style));
                    last = Some(style);
                }
                s.push(c);
            }
        }
        s.push_str("\x1b[0m");
        out.extend_from_slice(s.as_bytes());
        Rect { col, row, width, height }
    }

    /// The text of a row, for tests.
    #[cfg(test)]
    pub fn line(&self, row: usize) -> String {
        self.cells[row * self.width..(row + 1) * self.width].iter().map(|cell| cell.0).collect()
    }
}

/// Where a click in the pane falls on a box drawn at `rect`: the cell
/// within it, from 0, or None if outside.
pub fn inside(rect: Rect, col: usize, row: usize) -> Option<(usize, usize)> {
    let (left, top) = (rect.col as usize, rect.row as usize);
    let within = (left..left + rect.width as usize).contains(&col) && (top..top + rect.height as usize).contains(&row);
    within.then(|| (col - left, row - top))
}

/// Break `text` into lines at most `width` cells long, at spaces where it
/// can and mid-word where it must, keeping the lines it already has.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let mut word: Vec<char> = word.chars().filter(|c| !c.is_control()).collect();
            let used = line.chars().count();
            if used > 0 && used + 1 + word.len() <= width {
                line.push(' ');
            } else if used > 0 {
                lines.push(std::mem::take(&mut line));
            }
            // Only a word longer than a whole line is left over: cut it.
            while line.chars().count() + word.len() > width {
                let rest = word.split_off(width - line.chars().count());
                line.extend(word);
                lines.push(std::mem::take(&mut line));
                word = rest;
            }
            line.extend(word);
        }
        lines.push(line);
    }
    lines
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
    }

    #[test]
    fn frame_and_title() {
        let mut canvas = Canvas::new(12, 3);
        canvas.frame("Hi");
        assert_eq!(canvas.line(0), "╭─ Hi ─────╮");
        assert_eq!(canvas.line(1), "│          │");
        assert_eq!(canvas.line(2), "╰──────────╯");
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
