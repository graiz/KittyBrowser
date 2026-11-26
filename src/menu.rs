//! The ⋮ menu, as Chrome's at the right of its toolbar: a short list that
//! drops from the bar, for what has no button of its own. Grouped as
//! Chrome's is: the page first, then tools for developers, then help and
//! quitting. An item that can't do anything at the moment (closing the
//! only window, DevTools without `--devtools`) isn't shown, so the menu
//! stays short. Zoom is a row of its own, as in Chrome's: − and + step it
//! with the menu left open, to step again, and the percentage between
//! them resets it, as Ctrl+0 does. Terminals often keep Ctrl+= and Ctrl+-
//! for their own font size, so the menu may be the only way to zoom.
//! ASCII mode, the page drawn in characters, is among the tools, after
//! Select element; once on, the same item leaves it. A terminal that can't
//! draw images has no other mode, so there the item isn't shown.

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
    /// The zoom row: − and + step the zoom, and its percentage resets it.
    Zoom,
    /// The page in characters, or (as "Exit ASCII mode") in pixels again.
    Ascii,
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
            Item::SelectElement => Some(Action::Inspect),
            Item::Devtools => Some(Action::Devtools),
            Item::Shortcuts => Some(Action::Help),
            Item::Quit => Some(Action::Quit),
            // The zoom row shows its buttons where the keys would go.
            Item::Zoom | Item::Ascii | Item::ViewSource | Item::Network => None,
        }
    }
}

/// What the menu can offer at the moment it opens.
#[derive(Clone, Copy, Debug)]
pub struct Offer {
    /// How many windows are open.
    pub windows: usize,
    /// Whether Chrome's DevTools can be opened.
    pub devtools: bool,
    /// Whether the terminal draws images, so that pages can be drawn in
    /// characters or in pixels: otherwise they stay in characters, and
    /// ASCII mode isn't offered.
    pub images: bool,
    /// Whether the page is in characters now, so that the item leaves it.
    pub in_ascii: bool,
}

/// A line of the menu: an item and what it says, or a rule between groups.
#[derive(Clone, PartialEq, Debug)]
enum Row {
    Item(Item, String),
    Rule,
}

/// A step of the zoom row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zoom {
    In,
    Out,
    /// As Ctrl+0: back to 100%, or out until a wide page fits.
    Reset,
}

/// What a key or a click did to the menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Stay,
    Close,
    Choose(Item),
    /// Zoom, and leave the menu open.
    Zoom(Zoom),
}

/// The zoom row's buttons, at the right of it: "−", the percentage, "+".
fn zoom_buttons(zoom: f64) -> String {
    format!("−  {:^5}  +", format!("{:.0}%", zoom * 100.0))
}

/// How wide `zoom_buttons` is: the same for every zoom, so the buttons stay
/// put as it changes.
const ZOOM_BUTTONS: usize = 11;

pub struct Menu {
    rows: Vec<Row>,
    /// The row Enter chooses: always an item, never a rule.
    selected: usize,
}

impl Menu {
    pub fn new(offer: Offer) -> Menu {
        let item = |item, label: &str| Some(Row::Item(item, label.to_owned()));
        let groups = [
            vec![
                item(Item::Find, "Find in page"),
                item(Item::CloseWindow, "Close window").filter(|_| offer.windows > 1),
                item(Item::Zoom, "Zoom"),
            ],
            vec![
                item(Item::SelectElement, "Select element"),
                item(Item::Ascii, if offer.in_ascii { "Exit ASCII mode" } else { "ASCII mode" })
                    .filter(|_| offer.images),
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
        // On the zoom row, across and Enter zoom; up and down still move.
        if self.item(self.selected) == Some(Item::Zoom) {
            match key.code {
                KeyCode::Left | KeyCode::Char('-' | '_') => return Outcome::Zoom(Zoom::Out),
                KeyCode::Right | KeyCode::Char('+' | '=') => return Outcome::Zoom(Zoom::In),
                KeyCode::Enter | KeyCode::Char('0') => return Outcome::Zoom(Zoom::Reset),
                _ => {}
            }
        }
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
            Some(Item::Zoom) => {
                // Where in the buttons, which end a column before the
                // frame: − with the gap after it, the percentage, then
                // + with the gap before it and the column after.
                let buttons = (rect.col + rect.width) as usize - 2 - ZOOM_BUTTONS;
                match col.checked_sub(buttons) {
                    Some(0..=2) => Outcome::Zoom(Zoom::Out),
                    Some(3..=7) => Outcome::Zoom(Zoom::Reset),
                    Some(8..=11) => Outcome::Zoom(Zoom::In),
                    _ => Outcome::Stay,
                }
            }
            Some(item) => Outcome::Choose(item),
            None if inside(rect, col, row) => Outcome::Stay,
            None => Outcome::Close,
        }
    }

    /// Draw the menu below the ⋮ button of a bar `cols` wide, each item with
    /// its keys (`keys` says them for an item, if it has some), the zoom
    /// row with `zoom`. Returns where it is.
    pub fn render(&self, out: &mut Vec<u8>, cols: u16, zoom: f64, keys: impl Fn(Item) -> String) -> Rect {
        let lines: Vec<Option<(&str, String)>> = self
            .rows
            .iter()
            .map(|row| match row {
                Row::Item(Item::Zoom, label) => Some((label.as_str(), zoom_buttons(zoom))),
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

    const PLAIN: Offer = Offer { windows: 1, devtools: false, images: true, in_ascii: false };

    #[test]
    fn it_offers_only_what_can_be_done_now() {
        assert_eq!(
            Menu::new(PLAIN).items(),
            [
                Item::Find,
                Item::Zoom,
                Item::SelectElement,
                Item::Ascii,
                Item::ViewSource,
                Item::Network,
                Item::Shortcuts,
                Item::Quit
            ]
        );
        let all = Menu::new(Offer { windows: 2, devtools: true, images: true, in_ascii: false });
        assert_eq!(
            all.items(),
            [
                Item::Find,
                Item::CloseWindow,
                Item::Zoom,
                Item::SelectElement,
                Item::Ascii,
                Item::ViewSource,
                Item::Network,
                Item::Devtools,
                Item::Shortcuts,
                Item::Quit
            ]
        );
        // Three groups, two rules between them, none at either end.
        assert_eq!(all.rows.iter().filter(|row| **row == Row::Rule).count(), 2);
        assert!(all.rows.first() != Some(&Row::Rule) && all.rows.last() != Some(&Row::Rule));
    }

    #[test]
    fn keys_move_past_the_rules_and_choose() {
        let mut menu = Menu::new(PLAIN);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Find));
        // Down twice from Find: the zoom row, then across the rule to
        // Select element.
        press(&mut menu, KeyCode::Down);
        assert_eq!(press(&mut menu, KeyCode::Down), Outcome::Stay);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::SelectElement));
        // Up three times: the zoom row, Find, then round to Quit.
        for _ in 0..3 {
            press(&mut menu, KeyCode::Up);
        }
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Quit));
        // And Down from Quit goes round to Find.
        press(&mut menu, KeyCode::Down);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Find));
        assert_eq!(press(&mut menu, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn on_the_zoom_row_across_zooms_and_enter_resets() {
        let mut menu = Menu::new(PLAIN);
        press(&mut menu, KeyCode::Down);
        assert_eq!(press(&mut menu, KeyCode::Right), Outcome::Zoom(Zoom::In));
        assert_eq!(press(&mut menu, KeyCode::Char('+')), Outcome::Zoom(Zoom::In));
        assert_eq!(press(&mut menu, KeyCode::Char('=')), Outcome::Zoom(Zoom::In));
        assert_eq!(press(&mut menu, KeyCode::Left), Outcome::Zoom(Zoom::Out));
        assert_eq!(press(&mut menu, KeyCode::Char('-')), Outcome::Zoom(Zoom::Out));
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Zoom(Zoom::Reset));
        // Up and down still move off it.
        press(&mut menu, KeyCode::Up);
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Find));
        // Elsewhere across does nothing.
        assert_eq!(press(&mut menu, KeyCode::Right), Outcome::Stay);
    }

    /// The text drawn on `row` of the pane, escapes dropped, from where
    /// the cursor moves to that row to where it moves on.
    fn text_on(drawn: &str, row: u16) -> String {
        let start = drawn.find(&format!("\x1b[{row};")).unwrap();
        let line = &drawn[start..];
        let line = &line[..line[1..].find(&format!("\x1b[{};", row + 1)).map_or(line.len(), |end| end + 1)];
        let mut text = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                // A CSI escape, up to its final letter.
                '\x1b' => while chars.next().is_some_and(|c| !c.is_ascii_alphabetic()) {},
                c => text.push(c),
            }
        }
        text
    }

    #[test]
    fn it_hangs_below_the_buttons_and_clicks_choose() {
        let mut out = Vec::new();
        let menu = Menu::new(PLAIN);
        let keys = |item| if item == Item::SelectElement { "Ctrl+Shift+C".to_owned() } else { String::new() };
        let rect = menu.render(&mut out, 80, 1.1, keys);
        // Eight items and two rules between edges, right-aligned to the
        // pane's edge, below the bar.
        assert_eq!((rect.col + rect.width - 1, rect.row, rect.height), (80, 2, 12));
        let drawn = String::from_utf8(out).unwrap();
        assert!(drawn.contains("Select element") && drawn.contains("Ctrl+Shift+C"), "{drawn:?}");
        assert_eq!(drawn.matches('├').count(), 2, "{drawn:?}");
        let col = rect.col as usize + 2;
        // Row 2 is the top edge, then Find, Zoom, the rule, Select element...
        assert_eq!(menu.click(rect, col, 2), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 3), Outcome::Choose(Item::Find));
        assert_eq!(menu.click(rect, col, 5), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 6), Outcome::Choose(Item::SelectElement));
        assert_eq!(menu.click(rect, col, 7), Outcome::Choose(Item::Ascii));
        assert_eq!(menu.click(rect, col, 9), Outcome::Choose(Item::Network));
        assert_eq!(menu.click(rect, col, 11), Outcome::Choose(Item::Shortcuts));
        assert_eq!(menu.click(rect, col, 12), Outcome::Choose(Item::Quit));
        assert_eq!(menu.click(rect, col, 13), Outcome::Stay);
        assert_eq!(menu.click(rect, col, 14), Outcome::Close);
        assert_eq!(menu.click(rect, 1, 3), Outcome::Close);
        let mut menu = menu;
        assert!(menu.hover(rect, col, 8));
        assert!(!menu.hover(rect, col, 8));
        // A rule can't be selected.
        assert!(!menu.hover(rect, col, 5));
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::ViewSource));
    }

    #[test]
    fn the_zoom_buttons_are_hit_where_they_are_drawn() {
        let mut out = Vec::new();
        let menu = Menu::new(PLAIN);
        let rect = menu.render(&mut out, 80, 1.25, |_| String::new());
        let drawn = String::from_utf8(out).unwrap();
        let row = 4;
        let line = text_on(&drawn, row);
        assert!(line.contains("−  125%   +"), "{line:?}");
        // The columns of the pane the zoom row's characters are in.
        let column = |what: &str| rect.col as usize + line[..line.find(what).unwrap()].chars().count();
        assert_eq!(menu.click(rect, column("−"), row as usize), Outcome::Zoom(Zoom::Out));
        assert_eq!(menu.click(rect, column("125%"), row as usize), Outcome::Zoom(Zoom::Reset));
        assert_eq!(menu.click(rect, column("%"), row as usize), Outcome::Zoom(Zoom::Reset));
        assert_eq!(menu.click(rect, column("+"), row as usize), Outcome::Zoom(Zoom::In));
        // The word Zoom isn't a button.
        assert_eq!(menu.click(rect, column("Zoom"), row as usize), Outcome::Stay);
        // The buttons are as wide at any zoom, so they don't move.
        for zoom in [0.25, 1.0, 1.1, 3.0] {
            assert_eq!(zoom_buttons(zoom).chars().count(), ZOOM_BUTTONS, "{zoom}");
        }
    }

    #[test]
    fn ascii_mode_is_after_select_element() {
        let mut menu = Menu::new(PLAIN);
        // Find, Zoom, then the tools: Select element, ASCII mode.
        for _ in 0..3 {
            press(&mut menu, KeyCode::Down);
        }
        assert_eq!(press(&mut menu, KeyCode::Enter), Outcome::Choose(Item::Ascii));
    }

    #[test]
    fn the_same_item_leaves_ascii_mode_and_without_images_there_is_none() {
        let rows = |offer: Offer| Menu::new(offer).rows;
        let enter = Row::Item(Item::Ascii, "ASCII mode".to_owned());
        let exit = Row::Item(Item::Ascii, "Exit ASCII mode".to_owned());
        assert!(rows(PLAIN).contains(&enter) && !rows(PLAIN).contains(&exit));
        let leaving = rows(Offer { in_ascii: true, ..PLAIN });
        assert!(leaving.contains(&exit) && !leaving.contains(&enter));
        assert!(!Menu::new(Offer { images: false, in_ascii: true, ..PLAIN }).items().contains(&Item::Ascii));
    }
}
