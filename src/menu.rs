//! The ⋮ menu, as Chrome's at the right of its toolbar: a short list that
//! drops from the bar, for the tools that have no button of their own.

use crate::term::input::{Key, KeyCode};
use crate::ui::{self, Rect};
use std::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    /// Inspect mode: point at an element for its details.
    SelectElement,
    /// The page's HTML, as it is now, in the user's editor.
    ViewSource,
    /// The page's requests, slowest first, and how long it took to load.
    Network,
    /// Quit kb, once the bar has asked.
    Quit,
}

/// The items, with what the menu says for each.
const ITEMS: [(Item, &str); 4] = [
    (Item::SelectElement, "Select element"),
    (Item::ViewSource, "View source"),
    (Item::Network, "Network performance"),
    (Item::Quit, "Quit browser"),
];

/// What a key or a click did to the menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Stay,
    Close,
    Choose(Item),
}

#[derive(Default)]
pub struct Menu {
    selected: usize,
}

impl Menu {
    pub fn key(&mut self, key: &Key) -> Outcome {
        match key.code {
            KeyCode::Up => self.selected = (self.selected + ITEMS.len() - 1) % ITEMS.len(),
            KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % ITEMS.len(),
            KeyCode::Enter => return Outcome::Choose(ITEMS[self.selected].0),
            KeyCode::Esc => return Outcome::Close,
            _ => {}
        }
        Outcome::Stay
    }

    /// Show an item as the one Enter chooses, as the mouse moves over it.
    pub fn hover(&mut self, rect: Rect, col: usize, row: usize) -> bool {
        match item_at(rect, col, row) {
            Some(at) if at != self.selected => {
                self.selected = at;
                true
            }
            _ => false,
        }
    }

    /// What a click at `col`, `row` of the pane does: an item chooses it, and
    /// anywhere else closes the menu.
    pub fn click(&self, rect: Rect, col: usize, row: usize) -> Outcome {
        match item_at(rect, col, row) {
            Some(at) => Outcome::Choose(ITEMS[at].0),
            None if inside(rect, col, row) => Outcome::Stay,
            None => Outcome::Close,
        }
    }

    /// Draw the menu below the ⋮ button of a bar `cols` wide, each item with
    /// its keys (`keys` says them for an item, if it has some). Returns
    /// where it is.
    pub fn render(&self, out: &mut Vec<u8>, cols: u16, keys: impl Fn(Item) -> String) -> Rect {
        let lines: Vec<(String, String)> = ITEMS.iter().map(|(item, label)| (label.to_string(), keys(*item))).collect();
        let inner =
            lines.iter().map(|(label, keys)| label.chars().count() + 3 + keys.chars().count()).max().unwrap_or(0);
        let width = (inner as u16 + 4).min(cols);
        // Its right edge under ⋮, as Chrome's menu hangs from its button.
        let col = cols.saturating_sub(width) + 1;
        let row = 2;
        let text_width = width.saturating_sub(4) as usize;
        let colors = ui::palette();
        let style = colors.box_style.as_str();
        let rule = "─".repeat(width.saturating_sub(2) as usize);
        let mut s = format!("\x1b[{row};{col}H{style}╭{rule}╮");
        for (n, (label, keys)) in lines.iter().enumerate() {
            let pad = text_width.saturating_sub(label.chars().count() + keys.chars().count());
            let text: String = format!("{label}{:pad$}{keys}", "").chars().take(text_width).collect();
            let line = row + 1 + n as u16;
            let _ = write!(s, "\x1b[{line};{col}H{style}│");
            match n == self.selected {
                true => {
                    let _ = write!(s, "{} {text} {style}", colors.box_accent);
                }
                false => {
                    let _ = write!(s, " {text} ");
                }
            }
            s.push('│');
        }
        let _ = write!(s, "\x1b[{};{col}H{style}╰{rule}╯\x1b[0m\x1b[?25l", row + 1 + lines.len() as u16);
        out.extend_from_slice(s.as_bytes());
        Rect { col, row, width, height: lines.len() as u16 + 2 }
    }
}

fn inside(rect: Rect, col: usize, row: usize) -> bool {
    (rect.col as usize..(rect.col + rect.width) as usize).contains(&col)
        && (rect.row as usize..(rect.row + rect.height) as usize).contains(&row)
}

/// The item at a cell of the pane, if one is there: the rows between the
/// box's top and bottom edges.
fn item_at(rect: Rect, col: usize, row: usize) -> Option<usize> {
    let first = rect.row as usize + 1;
    (inside(rect, col, row) && row >= first).then(|| row - first).filter(|at| *at < ITEMS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(menu: &mut Menu, code: KeyCode) -> Outcome {
        menu.key(&Key { code, ctrl: false, alt: false, shift: false, cmd: false })
    }

    #[test]
    fn keys_move_and_choose() {
        let mut menu = Menu::default();
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::SelectElement));
        assert_eq!(press(&mut menu, KeyCode::Down), Outcome::Stay);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::ViewSource));
        // Up from the top goes round to the bottom.
        press(&mut menu, KeyCode::Up);
        press(&mut menu, KeyCode::Up);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Quit));
        assert_eq!(press(&mut menu, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn it_hangs_below_the_buttons_and_clicks_choose() {
        let mut out = Vec::new();
        let menu = Menu::default();
        let keys = |item| if item == Item::SelectElement { "Ctrl+Shift+C".to_owned() } else { String::new() };
        let rect = menu.render(&mut out, 80, keys);
        // "Network performance" is the widest line, with "Ctrl+Shift+C" on
        // the first: right-aligned to the pane's edge, below the bar.
        assert_eq!((rect.col + rect.width - 1, rect.row, rect.height), (80, 2, 6));
        let drawn = String::from_utf8(out).unwrap();
        assert!(drawn.contains("Select element") && drawn.contains("Ctrl+Shift+C"), "{drawn:?}");
        let col = rect.col as usize + 2;
        assert_eq!(menu.click(rect, col, 2), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 3), Outcome::Choose(Item::SelectElement));
        assert_eq!(menu.click(rect, col, 5), Outcome::Choose(Item::Network));
        assert_eq!(menu.click(rect, col, 6), Outcome::Choose(Item::Quit));
        assert_eq!(menu.click(rect, col, 7), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 8), Outcome::Close);
        assert_eq!(menu.click(rect, 1, 3), Outcome::Close);
        let mut menu = menu;
        assert!(menu.hover(rect, col, 4));
        assert!(!menu.hover(rect, col, 4));
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::ViewSource));
    }
}
