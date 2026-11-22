//! A page's alert, confirm, prompt or "leave site?" dialog, asked in the
//! pane as Chrome would ask it: a small window over the page with the
//! page's message and OK and Cancel. Enter presses the highlighted button
//! (OK unless Tab moved it), Esc cancels.

use crate::modal::{cut, inside, text_width as text_width_of, wrap, Canvas, Style};
use crate::term::input::{Key, KeyCode};
use crate::ui::{AddressBar, Edit, Rect};

/// Every dialog is this wide, or the pane's width less a margin, so they
/// look alike and the keys' hint always fits.
const WIDTH: usize = 60;
/// Cells between the frame and the text.
const PAD: usize = 3;
/// "  Cancel  ", as a button.
const CANCEL_WIDTH: usize = 10;
/// The margin round the frame, in rows and columns, so the dialog stands
/// clear of the page behind it.
const MARGIN: (usize, usize) = (1, 2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Alert,
    Confirm,
    Prompt,
    BeforeUnload,
}

impl Kind {
    pub fn from_name(name: &str) -> Kind {
        match name {
            "alert" => Kind::Alert,
            "prompt" => Kind::Prompt,
            "beforeunload" => Kind::BeforeUnload,
            _ => Kind::Confirm,
        }
    }
}

/// What a key or click did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Stay,
    /// Answered: OK or Cancel, and for a prompt the text typed.
    Answer(bool, Option<String>),
}

pub struct Dialog {
    /// Which of the page's questions this is.
    pub id: u64,
    kind: Kind,
    title: String,
    message: String,
    field: Option<AddressBar>,
    /// Whether OK, rather than Cancel, is the highlighted button.
    on_ok: bool,
    /// From the last draw, for clicks: where it is, its buttons' columns
    /// on their row, and the field's row and first column.
    rect: Option<Rect>,
    buttons: Vec<(usize, std::ops::Range<usize>, bool)>,
    field_at: Option<(usize, usize)>,
}

/// How Chrome names a page in its dialogs: "example.com says", and "This
/// page says" for a file or anything without a host.
pub fn origin_title(url: &str) -> String {
    let host = crate::ui::host_of(url);
    if host.is_empty() {
        "This page says".to_owned()
    } else {
        format!("{host} says")
    }
}

impl Dialog {
    pub fn new(id: u64, kind: Kind, url: &str, message: &str, default: &str) -> Dialog {
        let (title, message) = match kind {
            // Chrome shows its own words for these, not the page's.
            Kind::BeforeUnload => ("Leave site?".to_owned(), "Changes you made may not be saved.".to_owned()),
            _ => (origin_title(url), message.to_owned()),
        };
        let field = (kind == Kind::Prompt).then(|| {
            let mut field = AddressBar::default();
            field.set_text(default);
            field.focus();
            field
        });
        Dialog { id, kind, title, message, field, on_ok: true, rect: None, buttons: Vec::new(), field_at: None }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    fn ok_label(&self) -> &'static str {
        if self.kind == Kind::BeforeUnload {
            "Leave"
        } else {
            "OK"
        }
    }

    fn answer(&self, ok: bool) -> Outcome {
        let text = self.field.as_ref().filter(|_| ok).map(|field| field.text());
        Outcome::Answer(ok, text)
    }

    pub fn key(&mut self, key: &Key) -> Outcome {
        match key.code {
            KeyCode::Enter => return self.answer(self.on_ok || self.kind == Kind::Alert),
            KeyCode::Esc => return self.answer(false),
            KeyCode::Tab | KeyCode::BackTab if self.kind != Kind::Alert => self.on_ok = !self.on_ok,
            KeyCode::Left | KeyCode::Right if self.field.is_none() && self.kind != Kind::Alert => {
                self.on_ok = key.code == KeyCode::Right;
            }
            _ => {
                if let Some(field) = &mut self.field {
                    if let Edit::Submit = field.key(key) {
                        return self.answer(true);
                    }
                }
            }
        }
        Outcome::Stay
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(field) = &mut self.field {
            field.insert(text);
        }
    }

    /// A left click at a cell of the pane (from 1).
    pub fn click(&mut self, col: usize, row: usize) -> Outcome {
        let Some((x, y)) = self.rect.and_then(|rect| inside(rect, col, row)) else { return Outcome::Stay };
        for (button_row, cols, ok) in &self.buttons {
            if *button_row == y && cols.contains(&x) {
                return self.answer(*ok);
            }
        }
        if let (Some(field), Some((field_row, field_col))) = (&mut self.field, self.field_at) {
            if y == field_row && x >= field_col {
                field.click(x - field_col);
            }
        }
        Outcome::Stay
    }

    /// Whether a point of the pane is over a button, for the pointer.
    pub fn over_button(&self, col: usize, row: usize) -> bool {
        let Some((x, y)) = self.rect.and_then(|rect| inside(rect, col, row)) else { return false };
        self.buttons.iter().any(|(button_row, cols, _)| *button_row == y && cols.contains(&x))
    }

    /// Lay the dialog out for a `cols` x `rows` pane. Returns the canvas and
    /// the field's cursor on it, if there is one.
    pub fn layout(&mut self, cols: usize, rows: usize) -> (Canvas, Option<(usize, usize)>) {
        let (top, mut side) = MARGIN;
        // A pane too narrow for the margin goes without it.
        if cols < 24 + 4 + 2 * side {
            side = 0;
        }
        let width = WIDTH.min(cols.saturating_sub(4 + 2 * side)).max(24.min(cols));
        let text_width = width.saturating_sub(2 * PAD + 2).max(1);
        let mut message = wrap(&self.message, text_width);
        // The buttons sit in a row at the right, PAD in from the frame if
        // there's room; in a pane too narrow for the row, one above the other.
        let ok = self.ok_label();
        let ok_width = text_width_of(ok) + 4;
        let row_width = ok_width + if self.kind == Kind::Alert { 0 } else { 2 + CANCEL_WIDTH };
        let inside_width = width.saturating_sub(2);
        let stacked = self.kind != Kind::Alert && row_width + 2 > inside_width;
        let pad = if row_width + 2 * PAD <= inside_width { PAD } else { 1 };
        // Room below the bar for the frame, gaps, field and buttons.
        let fixed = 7 + 2 * top + if self.field.is_some() { 2 } else { 0 } + if stacked { 1 } else { 0 };
        let room = rows.saturating_sub(2 + fixed).max(1);
        if message.len() > room {
            message.truncate(room);
            if let Some(last) = message.last_mut() {
                *last = format!("{}…", cut(last, text_width.saturating_sub(1)));
            }
        }
        let height = fixed + message.len();
        let mut canvas = Canvas::new(width + 2 * side, height);
        canvas.frame_inset(top, side, &self.title);
        let left = side + 1 + PAD;
        let mut row = top + 2;
        for line in &message {
            canvas.put(row, left, line, Style::Text);
            row += 1;
        }
        row += 1;
        let mut cursor = None;
        self.field_at = None;
        if let Some(field) = &mut self.field {
            let col = left - 1;
            cursor = canvas.field(row, col, text_width + 2, field).map(|at| (row, at));
            self.field_at = Some((row, col + 1));
            row += 2;
        }
        // A row clear above the buttons, which sit at the right, the one
        // Enter presses last, as in Chrome.
        row += 1;
        self.buttons.clear();
        let right = (side + width).saturating_sub(1 + pad);
        let at = |button_width: usize| right.saturating_sub(button_width).max(side + 1);
        let (ok_row, cancel_row) = if stacked { (row + 1, row) } else { (row, row) };
        let ok_cols = canvas.button(ok_row, at(ok_width), ok, self.on_ok || self.kind == Kind::Alert);
        let mut end = ok_cols.start;
        self.buttons.push((ok_row, ok_cols, true));
        if self.kind != Kind::Alert {
            let cancel_at = if stacked { at(CANCEL_WIDTH) } else { at(row_width) };
            let cancel_cols = canvas.button(cancel_row, cancel_at, "Cancel", !self.on_ok);
            end = cancel_cols.start;
            self.buttons.push((cancel_row, cancel_cols, false));
        }
        let hint = match self.kind {
            Kind::Alert => "Enter OK",
            Kind::BeforeUnload => "Enter leaves · Esc stays",
            _ => "Enter OK · Esc Cancel",
        };
        if text_width_of(hint) + 1 < end.saturating_sub(left) {
            canvas.put(row, left, hint, Style::Dim);
        }
        (canvas, cursor)
    }

    /// Draw the dialog over the page. Returns where it went and where the
    /// cursor belongs (column, row of the pane), if anywhere.
    pub fn render(
        &mut self,
        out: &mut Vec<u8>,
        cols: u16,
        rows: u16,
        shade: Option<&crate::modal::Shade>,
    ) -> (Rect, Option<(u16, u16)>) {
        let (canvas, cursor) = self.layout(cols as usize, rows as usize);
        let rect = canvas.draw(out, cols, rows, shade);
        self.rect = Some(rect);
        let cursor = cursor.map(|(row, col)| (rect.col + col as u16, rect.row + row as u16));
        (rect, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(dialog: &mut Dialog, code: KeyCode) -> Outcome {
        dialog.key(&Key::plain(code))
    }

    #[test]
    fn titles_name_the_page() {
        assert_eq!(origin_title("https://example.com/a?b"), "example.com says");
        assert_eq!(origin_title("http://localhost:8080/"), "localhost:8080 says");
        assert_eq!(origin_title("file:///tmp/a.html"), "This page says");
        assert_eq!(origin_title("about:blank"), "This page says");
    }

    #[test]
    fn enter_accepts_and_esc_cancels() {
        let mut confirm = Dialog::new(1, Kind::Confirm, "https://a.com/", "Sure?", "");
        assert_eq!(press(&mut confirm, KeyCode::Enter), Outcome::Answer(true, None));
        assert_eq!(press(&mut confirm, KeyCode::Esc), Outcome::Answer(false, None));
        // Tab moves the highlight to Cancel, and Enter presses it.
        assert_eq!(press(&mut confirm, KeyCode::Tab), Outcome::Stay);
        assert_eq!(press(&mut confirm, KeyCode::Enter), Outcome::Answer(false, None));
    }

    #[test]
    fn a_prompt_answers_with_what_was_typed() {
        let mut prompt = Dialog::new(1, Kind::Prompt, "https://a.com/", "Name?", "Greg");
        // The default is selected, so typing replaces it.
        press(&mut prompt, KeyCode::Char('A'));
        press(&mut prompt, KeyCode::Char('l'));
        assert_eq!(press(&mut prompt, KeyCode::Enter), Outcome::Answer(true, Some("Al".to_owned())));
        assert_eq!(press(&mut prompt, KeyCode::Esc), Outcome::Answer(false, None));
    }

    #[test]
    fn an_alert_has_only_ok() {
        let mut alert = Dialog::new(1, Kind::Alert, "https://a.com/", "Hi", "");
        press(&mut alert, KeyCode::Tab);
        assert_eq!(press(&mut alert, KeyCode::Enter), Outcome::Answer(true, None));
        let (canvas, _) = alert.layout(80, 24);
        assert_eq!(alert.buttons.len(), 1);
        assert!(canvas.line(canvas.height - 4).contains("OK"));
        assert!(!canvas.line(canvas.height - 4).contains("Cancel"));
    }

    #[test]
    fn leave_site_uses_chromes_words() {
        let mut leave = Dialog::new(1, Kind::BeforeUnload, "https://a.com/", "ignored", "");
        let (canvas, _) = leave.layout(80, 24);
        assert!(canvas.line(1).contains("Leave site?"));
        assert!(canvas.line(3).contains("Changes you made may not be saved."));
        assert!(canvas.line(canvas.height - 4).contains("Leave"));
    }

    #[test]
    fn buttons_answer_clicks() {
        let mut confirm = Dialog::new(1, Kind::Confirm, "https://a.com/", "Sure?", "");
        let (rect, _) = confirm.render(&mut Vec::new(), 80, 24, None);
        let (row, ok, _) = confirm.buttons[0].clone();
        let (_, cancel, _) = confirm.buttons[1].clone();
        let at = |col: usize| (rect.col as usize + col, rect.row as usize + row);
        assert_eq!(confirm.click(at(ok.start).0, at(ok.start).1), Outcome::Answer(true, None));
        assert_eq!(confirm.click(at(cancel.start).0, at(cancel.start).1), Outcome::Answer(false, None));
        assert_eq!(confirm.click(1, 1), Outcome::Stay);
    }

    #[test]
    fn narrow_panes_keep_every_button_in_the_box() {
        for kind in [Kind::Alert, Kind::Confirm, Kind::Prompt, Kind::BeforeUnload] {
            for cols in 6..=40 {
                let mut dialog = Dialog::new(1, kind, "https://a.com/", "Sure?", "x");
                let (canvas, _) = dialog.layout(cols, 24);
                assert!(canvas.width <= cols, "{kind:?} at {cols}: {} wide", canvas.width);
                for (row, range, _) in &dialog.buttons {
                    assert!(*row < canvas.height && range.end <= canvas.width, "{kind:?} at {cols}");
                }
                // No two buttons overlap.
                if let [(a_row, a, _), (b_row, b, _)] = dialog.buttons.as_slice() {
                    assert!(a_row != b_row || a.end <= b.start || b.end <= a.start, "{kind:?} at {cols}");
                }
            }
        }
    }

    #[test]
    fn wide_messages_wrap_by_width() {
        let mut alert = Dialog::new(1, Kind::Alert, "https://a.com/", &"保存しました".repeat(10), "");
        let (canvas, _) = alert.layout(80, 24);
        let text = canvas.line(3);
        assert_eq!(text_width_of(&text), canvas.width);
        assert!(text.contains("保存"));
    }

    #[test]
    fn long_messages_wrap_and_are_cut_to_fit() {
        let mut alert = Dialog::new(1, Kind::Alert, "https://a.com/", &"word ".repeat(400), "");
        let (canvas, _) = alert.layout(80, 20);
        assert!(canvas.height <= 18);
        assert!(canvas.width <= WIDTH + 2 * MARGIN.1);
    }
}
