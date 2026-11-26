//! The ⋮ menu, as Chrome's at the right of its toolbar: a short list that
//! drops from the bar, for what has no button of its own. Grouped as
//! Chrome's is: the page first, then tools for developers, then help and
//! quitting. An item that can't do anything at the moment (closing the
//! only window, resetting a zoom of 100%) isn't shown, so the menu stays
//! short.

use crate::keys::Action;
use crate::term::input::{Key, KeyCode};
use crate::ui::{self, Rect};
use std::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    /// Find text on the page.
    Find,
    /// Close the window shown, when a page has opened others.
    CloseWindow,
    /// Back to 100%, when zoomed.
    ResetZoom,
    /// Inspect mode: point at an element for its details.
    SelectElement,
    /// The page's HTML, as it is now, in the user's editor.
    ViewSource,
    /// The page's requests, slowest first, and how long it took to load.
    Network,
    /// Chrome's DevTools, when kb was started with `--devtools`.
    Devtools,
    /// The keys, in the help box.
    Shortcuts,
    /// Quit kb, once it has asked.
    Quit,
}

impl Item {
    /// The action whose keys are shown beside the item, if it has one.
    pub fn action(self) -> Option<Action> {
        match self {
            Item::Find => Some(Action::Find),
            Item::CloseWindow => Some(Action::CloseWindow),
            Item::ResetZoom => Some(Action::ZoomReset),
            Item::SelectElement => Some(Action::Inspect),
            Item::Devtools => Some(Action::Devtools),
            Item::Shortcuts => Some(Action::Help),
            Item::Quit => Some(Action::Quit),
            Item::ViewSource | Item::Network => None,
        }
    }
}

/// What the menu can offer at the moment it opens.
#[derive(Clone, Copy, Debug)]
pub struct Offer {
    /// How many windows are open.
    pub windows: usize,
    /// The zoom, 1 being 100%.
    pub zoom: f64,
    /// Whether Chrome's DevTools can be opened.
    pub devtools: bool,
}

/// A line of the menu: an item and what it says, or a rule between groups.
#[derive(Clone, PartialEq, Debug)]
enum Row {
    Item(Item, String),
    Rule,
}

/// What a key or a click did to the menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Stay,
    Close,
    Choose(Item),
}

pub struct Menu {
    rows: Vec<Row>,
    /// The row Enter chooses: always an item, never a rule.
    selected: usize,
}

impl Menu {
    pub fn new(offer: Offer) -> Menu {
        let item = |item, label: &str| Some(Row::Item(item, label.to_owned()));
        let zoomed = (offer.zoom - 1.0).abs() > 0.001;
        let groups = [
            vec![
                item(Item::Find, "Find in page"),
                item(Item::CloseWindow, "Close window").filter(|_| offer.windows > 1),
                zoomed.then(|| Row::Item(Item::ResetZoom, format!("Reset zoom ({:.0}%)", offer.zoom * 100.0))),
            ],
            vec![
                item(Item::SelectElement, "Select element"),
                item(Item::ViewSource, "View source"),
                item(Item::Network, "Network performance"),
                item(Item::Devtools, "Developer tools").filter(|_| offer.devtools),
            ],
            vec![item(Item::Shortcuts, "Keyboard shortcuts"), item(Item::Quit, "Quit browser")],
        ];
        let mut rows = Vec::new();
        for group in groups {
            if !rows.is_empty() {
                rows.push(Row::Rule);
            }
            rows.extend(group.into_iter().flatten());
        }
        Menu { rows, selected: 0 }
    }

    /// The items, in order, without the rules.
    #[cfg(test)]
    fn items(&self) -> Vec<Item> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Item(item, _) => Some(*item),
                Row::Rule => None,
            })
            .collect()
    }

    fn item(&self, row: usize) -> Option<Item> {
        match self.rows.get(row) {
            Some(Row::Item(item, _)) => Some(*item),
            _ => None,
        }
    }

    /// Move the selection to the next item, or the one before, round from
    /// one end to the other, past the rules.
    fn step(&mut self, forward: bool) {
        let count = self.rows.len();
        for _ in 0..count {
            self.selected = match forward {
                true => (self.selected + 1) % count,
                false => (self.selected + count - 1) % count,
            };
            if self.item(self.selected).is_some() {
                return;
            }
        }
    }

    pub fn key(&mut self, key: &Key) -> Outcome {
        match key.code {
            KeyCode::Up => self.step(false),
            KeyCode::Down | KeyCode::Tab => self.step(true),
            KeyCode::Enter => return self.item(self.selected).map_or(Outcome::Stay, Outcome::Choose),
            KeyCode::Esc => return Outcome::Close,
            _ => {}
        }
        Outcome::Stay
    }

    /// Show an item as the one Enter chooses, as the mouse moves over it.
    pub fn hover(&mut self, rect: Rect, col: usize, row: usize) -> bool {
        match self.row_at(rect, col, row).filter(|&at| self.item(at).is_some()) {
            Some(at) if at != self.selected => {
                self.selected = at;
                true
            }
            _ => false,
        }
    }

    /// What a click at `col`, `row` of the pane does: an item chooses it, a
    /// rule or the frame does nothing, and anywhere else closes the menu.
    pub fn click(&self, rect: Rect, col: usize, row: usize) -> Outcome {
        match self.row_at(rect, col, row).and_then(|at| self.item(at)) {
            Some(item) => Outcome::Choose(item),
            None if inside(rect, col, row) => Outcome::Stay,
            None => Outcome::Close,
        }
    }

    /// Draw the menu below the ⋮ button of a bar `cols` wide, each item with
    /// its keys (`keys` says them for an item, if it has some). Returns
    /// where it is.
    pub fn render(&self, out: &mut Vec<u8>, cols: u16, keys: impl Fn(Item) -> String) -> Rect {
        let lines: Vec<Option<(&str, String)>> = self
            .rows
            .iter()
            .map(|row| match row {
                Row::Item(item, label) => Some((label.as_str(), keys(*item))),
                Row::Rule => None,
            })
            .collect();
        let inner = lines
            .iter()
            .flatten()
            .map(|(label, keys)| label.chars().count() + 3 + keys.chars().count())
            .max()
            .unwrap_or(0);
        let width = (inner as u16 + 4).min(cols);
        // Its right edge under ⋮, as Chrome's menu hangs from its button.
        let col = cols.saturating_sub(width) + 1;
        let row = 2;
        let text_width = width.saturating_sub(4) as usize;
        let colors = ui::palette();
        let style = colors.box_style.as_str();
        let rule = "─".repeat(width.saturating_sub(2) as usize);
        let mut s = format!("\x1b[{row};{col}H{style}╭{rule}╮");
        for (n, line) in lines.iter().enumerate() {
            let at = row + 1 + n as u16;
            let Some((label, keys)) = line else {
                let _ = write!(s, "\x1b[{at};{col}H{style}├{rule}┤");
                continue;
            };
            let pad = text_width.saturating_sub(label.chars().count() + keys.chars().count());
            let text: String = format!("{label}{:pad$}{keys}", "").chars().take(text_width).collect();
            let _ = write!(s, "\x1b[{at};{col}H{style}│");
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

    /// The row at a cell of the pane, if one is there: the lines between
    /// the box's top and bottom edges.
    fn row_at(&self, rect: Rect, col: usize, row: usize) -> Option<usize> {
        let first = rect.row as usize + 1;
        (inside(rect, col, row) && row >= first).then(|| row - first).filter(|at| *at < self.rows.len())
    }
}

fn inside(rect: Rect, col: usize, row: usize) -> bool {
    (rect.col as usize..(rect.col + rect.width) as usize).contains(&col)
        && (rect.row as usize..(rect.row + rect.height) as usize).contains(&row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(menu: &mut Menu, code: KeyCode) -> Outcome {
        menu.key(&Key { code, ctrl: false, alt: false, shift: false, cmd: false })
    }

    const PLAIN: Offer = Offer { windows: 1, zoom: 1.0, devtools: false };

    #[test]
    fn it_offers_only_what_can_be_done_now() {
        assert_eq!(
            Menu::new(PLAIN).items(),
            [Item::Find, Item::SelectElement, Item::ViewSource, Item::Network, Item::Shortcuts, Item::Quit]
        );
        let all = Menu::new(Offer { windows: 2, zoom: 1.25, devtools: true });
        assert_eq!(
            all.items(),
            [
                Item::Find,
                Item::CloseWindow,
                Item::ResetZoom,
                Item::SelectElement,
                Item::ViewSource,
                Item::Network,
                Item::Devtools,
                Item::Shortcuts,
                Item::Quit
            ]
        );
        assert!(all.rows.contains(&Row::Item(Item::ResetZoom, "Reset zoom (125%)".to_owned())));
        // Three groups, two rules between them, none at either end.
        assert_eq!(all.rows.iter().filter(|row| **row == Row::Rule).count(), 2);
        assert!(all.rows.first() != Some(&Row::Rule) && all.rows.last() != Some(&Row::Rule));
    }

    #[test]
    fn keys_move_past_the_rules_and_choose() {
        let mut menu = Menu::new(PLAIN);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Find));
        // Down from Find crosses the rule to Select element.
        assert_eq!(press(&mut menu, KeyCode::Down), Outcome::Stay);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::SelectElement));
        // Up twice: back to Find, then round to Quit at the bottom.
        press(&mut menu, KeyCode::Up);
        press(&mut menu, KeyCode::Up);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Quit));
        // And Down from Quit goes round to Find.
        press(&mut menu, KeyCode::Down);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Find));
        assert_eq!(press(&mut menu, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn it_hangs_below_the_buttons_and_clicks_choose() {
        let mut out = Vec::new();
        let menu = Menu::new(PLAIN);
        let keys = |item| if item == Item::SelectElement { "Ctrl+Shift+C".to_owned() } else { String::new() };
        let rect = menu.render(&mut out, 80, keys);
        // Six items and two rules between edges, right-aligned to the
        // pane's edge, below the bar.
        assert_eq!((rect.col + rect.width - 1, rect.row, rect.height), (80, 2, 10));
        let drawn = String::from_utf8(out).unwrap();
        assert!(drawn.contains("Select element") && drawn.contains("Ctrl+Shift+C"), "{drawn:?}");
        assert_eq!(drawn.matches('├').count(), 2, "{drawn:?}");
        let col = rect.col as usize + 2;
        // Row 2 is the top edge, then Find, the rule, Select element...
        assert_eq!(menu.click(rect, col, 2), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 3), Outcome::Choose(Item::Find));
        assert_eq!(menu.click(rect, col, 4), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 5), Outcome::Choose(Item::SelectElement));
        assert_eq!(menu.click(rect, col, 7), Outcome::Choose(Item::Network));
        assert_eq!(menu.click(rect, col, 9), Outcome::Choose(Item::Shortcuts));
        assert_eq!(menu.click(rect, col, 10), Outcome::Choose(Item::Quit));
        assert_eq!(menu.click(rect, col, 11), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 12), Outcome::Close);
        assert_eq!(menu.click(rect, 1, 3), Outcome::Close);
        let mut menu = menu;
        assert!(menu.hover(rect, col, 6));
        assert!(!menu.hover(rect, col, 6));
        // A rule can't be selected.
        assert!(!menu.hover(rect, col, 4));
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::ViewSource));
    }
}
