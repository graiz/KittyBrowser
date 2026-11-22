//! The file chooser a page opens, asked in the pane: a window over the page
//! with places on the left, the folder's files on the right as `ls -la`
//! lists them (fewer columns in a narrow pane), and a file name field to
//! type a name or path into, as in a desktop browser's Open dialog.
//!
//! The arrow keys move through the list and Enter opens a folder or
//! chooses a file; typing goes to the field, jumps to the first name that
//! starts with it, and Tab completes it. For a field that takes several
//! files, Space or Ctrl+click marks files, in any folder, and Enter
//! chooses those marked. Esc cancels, and the page hears nothing, as when
//! Chrome's chooser is cancelled.

use crate::modal::{inside, text_width, Canvas, Style};
use crate::term::input::{Key, KeyCode};
use crate::ui::{AddressBar, Edit, Rect};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The largest the window grows, in cells.
const MAX_WIDTH: usize = 112;
const MAX_HEIGHT: usize = 34;
/// The places column, and the narrowest box that has one.
const PLACES_WIDTH: usize = 18;
const PLACES_FROM: usize = 72;
/// Presses this close in time on one row count as a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: i64,
    pub mode: u32,
    pub link: bool,
    /// The ".." row, for the folder above.
    pub parent: bool,
}

/// What a key or click did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Stay,
    Choose(Vec<PathBuf>),
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pane {
    Places,
    List,
}

/// Where things were drawn, for clicks: rows and columns on the canvas.
#[derive(Default, Clone)]
struct Drawn {
    rect: Option<Rect>,
    list_top: usize,
    list_rows: usize,
    list_cols: std::ops::Range<usize>,
    places_top: usize,
    /// In a narrow pane, the places as a row above the list: its row, and
    /// each place's columns.
    strip: Option<(usize, Vec<std::ops::Range<usize>>)>,
    places_cols: std::ops::Range<usize>,
    up: Option<(usize, std::ops::Range<usize>)>,
    open: Option<(usize, std::ops::Range<usize>)>,
    cancel: Option<(usize, std::ops::Range<usize>)>,
    hidden: Option<(usize, std::ops::Range<usize>)>,
    field: Option<(usize, usize)>,
}

pub struct Picker {
    /// Which of the page's questions this is.
    pub id: u64,
    multiple: bool,
    accept: String,
    /// Who is asking: the page's host.
    origin: String,
    folder: PathBuf,
    entries: Vec<Entry>,
    hidden: bool,
    selected: usize,
    scroll: usize,
    /// Whether the selection may have moved since the last layout, which
    /// then scrolls it into view. The wheel scrolls without moving it.
    reveal: bool,
    marked: Vec<PathBuf>,
    field: AddressBar,
    places: Vec<(String, PathBuf)>,
    place: usize,
    pane: Pane,
    /// What went wrong last, shown until the next key.
    problem: Option<String>,
    /// Files that look like keys or passwords, chosen once: Enter again
    /// sends them, anything else goes back. Says which and why.
    confirming: Option<(Vec<PathBuf>, String)>,
    /// Whether the field shows the selection (the highlighted file, or
    /// those marked) rather than what the user typed, and for which
    /// folder, row and marks, so it follows them as they change.
    filled: bool,
    filled_for: (PathBuf, usize, usize),
    host: String,
    drawn: Drawn,
    last_click: Option<(Instant, usize)>,
}

impl Picker {
    pub fn new(id: u64, multiple: bool, accept: &str, url: &str, start: &Path) -> Picker {
        let origin = crate::ui::host_of(url);
        let mut field = AddressBar::default();
        field.focus();
        let mut picker = Picker {
            id,
            multiple,
            accept: accept.trim().to_owned(),
            origin: if origin.is_empty() { "this page".to_owned() } else { origin.to_owned() },
            folder: PathBuf::new(),
            entries: Vec::new(),
            hidden: false,
            selected: 0,
            scroll: 0,
            reveal: true,
            marked: Vec::new(),
            field,
            places: places(),
            place: 0,
            pane: Pane::List,
            problem: None,
            confirming: None,
            filled: true,
            filled_for: (PathBuf::new(), usize::MAX, 0),
            host: hostname(),
            drawn: Drawn::default(),
            last_click: None,
        };
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
        if !picker.go(start) {
            picker.go(&home);
        }
        picker.problem = None;
        picker.follow_selection();
        picker
    }

    /// Put the selection in the File name field, as a desktop Open dialog
    /// does: the highlighted file's name, or the marked files' names in
    /// quotes; nothing for a folder. Typing replaces it. Only when the
    /// selection has changed, so it doesn't undo what the user typed.
    fn follow_selection(&mut self) {
        let now = (self.folder.clone(), self.selected, self.marked.len());
        if now == self.filled_for {
            return;
        }
        self.filled_for = now;
        let text = if !self.marked.is_empty() {
            let names = self.marked.iter().filter_map(|path| path.file_name()).map(|name| name.to_string_lossy());
            names.map(|name| format!("\"{name}\"")).collect::<Vec<_>>().join(" ")
        } else {
            match self.entries.get(self.selected) {
                Some(entry) if !entry.dir => entry.name.clone(),
                _ => String::new(),
            }
        };
        self.field.set_text(&text);
        self.field.focus();
        self.filled = true;
    }

    /// What the user typed in the field: nothing if it shows the selection.
    fn typed(&self) -> String {
        if self.filled {
            String::new()
        } else {
            self.field.text()
        }
    }

    /// The field's `accept` attribute, which arrives after the chooser opens.
    pub fn accept(&self) -> &str {
        &self.accept
    }

    pub fn set_accept(&mut self, accept: &str) {
        self.accept = accept.trim().to_owned();
    }

    /// The folder shown, to start in next time.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Show `folder`. Returns false, saying why, if it can't be read.
    fn go(&mut self, folder: &Path) -> bool {
        let folder = folder.canonicalize().unwrap_or_else(|_| folder.to_owned());
        match read_folder(&folder, self.hidden) {
            Ok(entries) => {
                // Going up, the folder just left stays highlighted.
                let came_from = self.folder.strip_prefix(&folder).ok().and_then(|rest| rest.iter().next());
                let came_from = came_from.map(|name| name.to_string_lossy().into_owned());
                self.selected = came_from
                    .and_then(|name| entries.iter().position(|entry| !entry.parent && entry.name == name))
                    .unwrap_or(0);
                self.entries = entries;
                self.folder = folder;
                self.scroll = 0;
                self.reveal = true;
                self.field.set_text("");
                self.field.focus();
                self.problem = None;
                true
            }
            Err(error) => {
                self.problem = Some(format!("can't open {}: {}", tilde(&folder), reason(&error)));
                false
            }
        }
    }

    fn up(&mut self) {
        if let Some(parent) = self.folder.parent().map(Path::to_owned) {
            self.go(&parent);
        }
    }

    fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        let folder = self.folder.clone();
        let name = self.entries.get(self.selected).map(|entry| entry.name.clone());
        self.go(&folder);
        if let Some(at) = name.and_then(|name| self.entries.iter().position(|entry| entry.name == name)) {
            self.selected = at;
        }
    }

    fn path_of(&self, entry: &Entry) -> PathBuf {
        if entry.parent {
            self.folder.parent().unwrap_or(&self.folder).to_owned()
        } else {
            self.folder.join(&entry.name)
        }
    }

    fn toggle_mark(&mut self, at: usize) {
        let Some(entry) = self.entries.get(at).filter(|entry| !entry.dir) else { return };
        let path = self.path_of(entry);
        match self.marked.iter().position(|marked| *marked == path) {
            Some(index) => {
                self.marked.remove(index);
            }
            None => self.marked.push(path),
        }
    }

    /// Hand `files` to the page, unless one looks like a key or password:
    /// then ask first, naming it and who will get it.
    fn deliver(&mut self, files: Vec<PathBuf>) -> Outcome {
        let secret = files.iter().find_map(|file| crate::secrets::sensitive(file).map(|why| (file.clone(), why)));
        match secret {
            Some((file, why)) => {
                let name = file.file_name().map_or(String::new(), |name| name.to_string_lossy().into_owned());
                let question = format!("Send {name} to {}? {why}. Enter sends, Esc goes back", self.origin);
                self.confirming = Some((files, question));
                Outcome::Stay
            }
            None => Outcome::Choose(files),
        }
    }

    /// Choose `path`, or say why it can't be.
    fn choose(&mut self, path: PathBuf) -> Outcome {
        match std::fs::File::open(&path) {
            Ok(_) => {
                let mut files = self.marked.clone();
                if !files.contains(&path) {
                    files.push(path);
                }
                self.deliver(files)
            }
            Err(error) => {
                self.problem = Some(format!("can't read {}: {}", tilde(&path), reason(&error)));
                Outcome::Stay
            }
        }
    }

    /// Enter on the highlighted row: open a folder, choose a file.
    fn activate(&mut self, at: usize) -> Outcome {
        let Some(entry) = self.entries.get(at).cloned() else { return Outcome::Stay };
        let path = self.path_of(&entry);
        if entry.dir {
            self.go(&path);
            Outcome::Stay
        } else if self.multiple && !self.marked.is_empty() {
            self.deliver(self.marked.clone())
        } else {
            self.choose(path)
        }
    }

    /// Enter, or the Open button.
    fn open(&mut self) -> Outcome {
        if self.pane == Pane::Places {
            if let Some((_, path)) = self.places.get(self.place).cloned() {
                self.pane = Pane::List;
                self.go(&path);
            }
            return Outcome::Stay;
        }
        let typed = self.typed();
        let typed = typed.trim();
        if typed.is_empty() {
            if self.multiple && !self.marked.is_empty() {
                let on_folder = self.entries.get(self.selected).is_some_and(|entry| entry.dir);
                if !on_folder {
                    return self.deliver(self.marked.clone());
                }
            }
            return self.activate(self.selected);
        }
        let path = resolve(&self.folder, typed);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {
                self.go(&path);
                Outcome::Stay
            }
            Ok(_) => self.choose(path),
            Err(error) => {
                self.problem = Some(format!("{typed}: {}", reason(&error)));
                Outcome::Stay
            }
        }
    }

    /// Highlight the first name that starts with what is typed.
    fn jump_to_typed(&mut self) {
        let typed = self.field.text().to_lowercase();
        if typed.is_empty() || typed.contains('/') || typed.starts_with('~') {
            return;
        }
        if let Some(at) =
            self.entries.iter().position(|entry| !entry.parent && entry.name.to_lowercase().starts_with(&typed))
        {
            self.selected = at;
        }
    }

    fn step(&mut self, by: isize) {
        let len = self.entries.len();
        if len > 0 {
            self.selected = (self.selected as isize + by).clamp(0, len as isize - 1) as usize;
        }
    }

    pub fn key(&mut self, key: &Key) -> Outcome {
        self.reveal = true;
        let outcome = self.key_inner(key);
        self.follow_selection();
        outcome
    }

    fn key_inner(&mut self, key: &Key) -> Outcome {
        self.problem = None;
        if let Some((files, _)) = self.confirming.take() {
            return match key.code {
                KeyCode::Enter => Outcome::Choose(files),
                _ => Outcome::Stay,
            };
        }
        let empty = self.typed().is_empty();
        let page = self.drawn.list_rows.max(1) as isize;
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter => return self.open(),
            // Show hidden files: Alt+H, or Cmd+Shift+. as in a Mac's own.
            KeyCode::Char('h' | 'H') if key.alt => self.toggle_hidden(),
            KeyCode::Char('.' | '>') if key.cmd || key.alt => self.toggle_hidden(),
            KeyCode::Up | KeyCode::Down if self.pane == Pane::Places => {
                let last = self.places.len().saturating_sub(1);
                self.place =
                    if key.code == KeyCode::Up { self.place.saturating_sub(1) } else { (self.place + 1).min(last) };
            }
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-page),
            KeyCode::PageDown => self.step(page),
            KeyCode::Home if empty => self.selected = 0,
            KeyCode::End if empty => self.selected = self.entries.len().saturating_sub(1),
            KeyCode::Tab | KeyCode::BackTab if empty => {
                if self.drawn.places_cols.is_empty() {
                    return Outcome::Stay;
                }
                self.pane = if self.pane == Pane::List { Pane::Places } else { Pane::List };
                if self.pane == Pane::Places {
                    self.place = self.places.iter().position(|(_, path)| *path == self.folder).unwrap_or(0);
                }
            }
            KeyCode::Tab => {
                if let Some(done) = complete(&self.folder, &self.field.text(), self.hidden) {
                    self.field.set_text(&done);
                    self.field.focus();
                    // Focused fields select all; carry on typing after it instead.
                    self.field.key(&Key::plain(KeyCode::End));
                    self.filled = false;
                    self.jump_to_typed();
                    self.filled_for = (self.folder.clone(), self.selected, self.marked.len());
                }
            }
            KeyCode::Backspace if empty => self.up(),
            KeyCode::Left if empty && self.pane == Pane::List => self.up(),
            KeyCode::Right if empty && self.pane == Pane::List => {
                if self.entries.get(self.selected).is_some_and(|entry| entry.dir && !entry.parent) {
                    return self.activate(self.selected);
                }
            }
            KeyCode::Char(' ') if empty && self.multiple && !key.ctrl && !key.alt => {
                self.toggle_mark(self.selected);
                self.step(1);
            }
            _ => {
                self.pane = Pane::List;
                if matches!(self.field.key(key), Edit::Typed | Edit::Changed) {
                    self.filled = false;
                    self.jump_to_typed();
                    // Jumping to what was typed mustn't replace it.
                    self.filled_for = (self.folder.clone(), self.selected, self.marked.len());
                }
            }
        }
        Outcome::Stay
    }

    pub fn paste(&mut self, text: &str) {
        self.field.insert(text.trim_end_matches(['\n', '\r']));
        self.filled = false;
    }

    /// The wheel over the list: move the view, not the highlight.
    pub fn wheel(&mut self, rows: isize) {
        let most = self.entries.len().saturating_sub(self.drawn.list_rows.max(1));
        self.scroll = (self.scroll as isize + rows).clamp(0, most as isize) as usize;
    }

    /// A left click at a cell of the pane (from 1).
    pub fn click(&mut self, col: usize, row: usize, ctrl: bool) -> Outcome {
        let outcome = self.click_inner(col, row, ctrl);
        self.follow_selection();
        outcome
    }

    fn click_inner(&mut self, col: usize, row: usize, ctrl: bool) -> Outcome {
        self.problem = None;
        let Some((x, y)) = self.drawn.rect.and_then(|rect| inside(rect, col, row)) else { return Outcome::Stay };
        let hit = |spot: &Option<(usize, std::ops::Range<usize>)>| {
            spot.as_ref().is_some_and(|(r, cols)| *r == y && cols.contains(&x))
        };
        if let Some((files, _)) = self.confirming.take() {
            return if hit(&self.drawn.open) { Outcome::Choose(files) } else { Outcome::Stay };
        }
        if hit(&self.drawn.open) {
            return self.open();
        }
        if hit(&self.drawn.cancel) {
            return Outcome::Cancel;
        }
        if hit(&self.drawn.up) {
            self.up();
            return Outcome::Stay;
        }
        if hit(&self.drawn.hidden) {
            self.toggle_hidden();
            return Outcome::Stay;
        }
        if let Some((field_row, field_col)) = self.drawn.field {
            if y == field_row && x >= field_col {
                self.field.click(x - field_col);
                return Outcome::Stay;
            }
        }
        let in_body = (self.drawn.list_top..self.drawn.list_top + self.drawn.list_rows).contains(&y);
        if in_body && self.drawn.list_cols.contains(&x) {
            let at = self.scroll + (y - self.drawn.list_top);
            if at >= self.entries.len() {
                return Outcome::Stay;
            }
            self.pane = Pane::List;
            if ctrl && self.multiple {
                self.toggle_mark(at);
                self.selected = at;
                return Outcome::Stay;
            }
            let double = self.last_click.is_some_and(|(when, row)| row == at && when.elapsed() < DOUBLE_CLICK);
            self.last_click = Some((Instant::now(), at));
            self.selected = at;
            if double {
                self.last_click = None;
                return self.activate(at);
            }
            return Outcome::Stay;
        }
        if let Some((strip_row, spots)) = self.drawn.strip.clone() {
            if let Some(at) = spots.iter().position(|cols| y == strip_row && cols.contains(&x)) {
                if let Some((_, path)) = self.places.get(at).cloned() {
                    self.go(&path);
                }
                return Outcome::Stay;
            }
        }
        if self.drawn.places_cols.contains(&x) && y >= self.drawn.places_top {
            if let Some((_, path)) = self.places.get(y - self.drawn.places_top).cloned() {
                self.place = y - self.drawn.places_top;
                self.pane = Pane::List;
                self.go(&path);
            }
        }
        Outcome::Stay
    }

    /// Whether a point of the pane is over something to click, for the
    /// pointer.
    pub fn over_button(&self, col: usize, row: usize) -> bool {
        let Some((x, y)) = self.drawn.rect.and_then(|rect| inside(rect, col, row)) else { return false };
        let hit = |spot: &Option<(usize, std::ops::Range<usize>)>| {
            spot.as_ref().is_some_and(|(r, cols)| *r == y && cols.contains(&x))
        };
        let in_places = self.drawn.places_cols.contains(&x)
            && (self.drawn.places_top..self.drawn.places_top + self.places.len()).contains(&y);
        let in_strip =
            self.drawn.strip.as_ref().is_some_and(|(r, spots)| *r == y && spots.iter().any(|c| c.contains(&x)));
        let in_places = in_places || in_strip;
        hit(&self.drawn.open) || hit(&self.drawn.cancel) || hit(&self.drawn.up) || hit(&self.drawn.hidden) || in_places
    }

    /// Lay the window out for a `cols` x `rows` pane. Returns the canvas and
    /// the field's cursor on it.
    pub fn layout(&mut self, cols: usize, rows: usize) -> (Canvas, Option<(usize, usize)>) {
        let width = MAX_WIDTH.min(cols.saturating_sub(4)).max(40.min(cols));
        let height = MAX_HEIGHT.min(rows.saturating_sub(2)).max(12.min(rows.saturating_sub(1)));
        let mut canvas = Canvas::new(width, height);
        let title = if self.multiple { "Choose files" } else { "Choose a file" };
        canvas.frame(&format!("{title} for {}", self.origin));
        let mut drawn = Drawn::default();

        // The folder, with a button for the one above.
        let up = canvas.put(1, 2, " ↑ ", Style::Field);
        drawn.up = Some((1, 2..up));
        // Whose files these are, always in sight: over ssh, not the user's own.
        let host = format!("on {}", self.host);
        let host_at = width.saturating_sub(3 + text_width(&host));
        canvas.put(1, host_at, &host, Style::Dim);
        canvas.put_limited(1, up + 1, &tilde(&self.folder), Style::Title, host_at.saturating_sub(up + 3));

        let places = width >= PLACES_FROM && !self.places.is_empty();
        let split = if places { PLACES_WIDTH } else { 0 };
        canvas.rule(2);
        let footer = height.saturating_sub(4);
        canvas.rule(footer);
        if places {
            canvas.put(2, split, "┬", Style::Plain);
            canvas.put(footer, split, "┴", Style::Plain);
            for row in 3..footer {
                canvas.put(row, split, "│", Style::Plain);
            }
            canvas.put(3, 2, "Places", Style::Dim);
            drawn.places_top = 4;
            drawn.places_cols = 1..split;
            for (n, (name, path)) in self.places.iter().enumerate() {
                let row = 4 + n;
                if row >= footer {
                    break;
                }
                let here = *path == self.folder;
                let style = match (self.pane == Pane::Places && n == self.place, here) {
                    (true, _) => Style::Focused,
                    (false, true) => Style::Accent,
                    _ => Style::Text,
                };
                canvas.paint(row, 1, split - 1, style);
                canvas.put_limited(row, 2, &format!("{} {name}", if here { '▸' } else { ' ' }), style, split - 3);
            }
        }

        // Too narrow for the column: the places go in a row of their own.
        let mut top = 3;
        if !places {
            let mut col = 2;
            let mut spots = Vec::new();
            for (name, path) in &self.places {
                if col + text_width(name) + 2 > width.saturating_sub(2) {
                    break;
                }
                let style = if *path == self.folder { Style::Accent } else { Style::Text };
                let end = canvas.put(top, col, name, style);
                spots.push(col..end);
                col = end + 2;
            }
            drawn.strip = Some((top, spots));
            canvas.rule(top + 1);
            top += 2;
        }

        // The list, with a header naming its columns.
        let left = if places { split + 1 } else { 1 };
        let list_width = width.saturating_sub(left + 1);
        let columns = Columns::for_width(list_width.saturating_sub(2));
        canvas.put(top, left + 1, &columns.header(list_width.saturating_sub(2)), Style::Dim);
        drawn.list_top = top + 1;
        drawn.list_rows = footer.saturating_sub(top + 1);
        drawn.list_cols = left..left + list_width;
        if std::mem::take(&mut self.reveal) {
            if self.selected < self.scroll {
                self.scroll = self.selected;
            } else if drawn.list_rows > 0 && self.selected >= self.scroll + drawn.list_rows {
                self.scroll = self.selected + 1 - drawn.list_rows;
            }
        }
        // A taller pane can show more: no empty rows below the last entry.
        self.scroll = self.scroll.min(self.entries.len().saturating_sub(drawn.list_rows.max(1)));
        let now = now();
        for (n, entry) in self.entries.iter().enumerate().skip(self.scroll).take(drawn.list_rows) {
            let row = drawn.list_top + n - self.scroll;
            let marked = !entry.parent && self.marked.contains(&self.folder.join(&entry.name));
            let selected = n == self.selected;
            let base = if selected { Style::Focused } else { Style::Plain };
            canvas.paint(row, left, list_width, base);
            let mut col = left + 1;
            col = canvas.put(row, col, if marked { "✓ " } else { "  " }, if selected { base } else { Style::Accent });
            let details = columns.details(entry, now);
            let details_style = if selected { base } else { Style::Dim };
            col = canvas.put(row, col, &details, details_style);
            let name = if entry.parent {
                "..".to_owned()
            } else if entry.dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            };
            let wanted = entry.dir || matches_accept(&entry.name, &self.accept);
            let name_style = match (selected, entry.dir, wanted) {
                (true, _, _) => Style::Focused,
                _ if entry.parent => Style::Dim,
                (false, true, _) => Style::Accent,
                (false, false, true) => Style::Text,
                (false, false, false) => Style::Dim,
            };
            canvas.put_limited(row, col, &name, name_style, (left + list_width).saturating_sub(col + 1));
        }
        if self.entries.len() <= 1 && drawn.list_rows > 1 {
            canvas.put(drawn.list_top + 1, left + 3, "This folder is empty.", Style::Dim);
        }

        // The file name field and the buttons.
        let row = footer + 1;
        let label_end = canvas.put(row, 2, "File name ", Style::Plain);
        let buttons_width = 8 + 2 + 10;
        let cancel_start = width.saturating_sub(2 + 10);
        let open_start = cancel_start.saturating_sub(2 + 8);
        let field_width = open_start.saturating_sub(label_end + 2);
        let cursor = canvas.field(row, label_end, field_width, &mut self.field).map(|col| (row, col));
        drawn.field = Some((row, label_end + 1));
        if field_width + label_end + buttons_width < width {
            drawn.open = Some((row, canvas.button(row, open_start, "Open", true)));
            drawn.cancel = Some((row, canvas.button(row, cancel_start, "Cancel", false)));
        }

        // A line of what matters: problems first, then what the page wants,
        // what's marked, the keys, and whose files these are.
        let row = footer + 2;
        let mut col = 2;
        let end = width.saturating_sub(2);
        if let Some((_, question)) = &self.confirming {
            canvas.paint(row, 1, width.saturating_sub(2), Style::Default);
            canvas.put_limited(row, col, &format!("⚠ {question}"), Style::Default, end.saturating_sub(col));
        } else if let Some(problem) = &self.problem {
            canvas.put_limited(row, col, &format!("⚠ {problem}"), Style::Accent, end.saturating_sub(col));
        } else {
            let mut parts: Vec<(String, Style)> = Vec::new();
            if !self.accept.is_empty() {
                parts.push((format!("Wants {}", self.accept), Style::Accent));
            }
            if !self.marked.is_empty() {
                parts.push((format!("{} marked", self.marked.len()), Style::Accent));
            }
            if self.multiple {
                parts.push(("Space or Ctrl+click marks".to_owned(), Style::Dim));
            }
            parts.push(("Tab completes".to_owned(), Style::Dim));
            for (n, (text, style)) in parts.iter().enumerate() {
                let text = if n == 0 { text.clone() } else { format!(" · {text}") };
                if col + text_width(&text) > end.saturating_sub(16) {
                    break;
                }
                col = canvas.put(row, col, &text, *style);
            }
            let toggle = if self.hidden { "[x] hidden (Alt+H)" } else { "[ ] hidden (Alt+H)" };
            let start = end.saturating_sub(toggle.chars().count());
            if start > col + 1 {
                let stop = canvas.put(row, start, toggle, Style::Dim);
                drawn.hidden = Some((row, start..stop));
            }
        }
        drawn.rect = self.drawn.rect;
        self.drawn = drawn;
        (canvas, cursor)
    }

    /// Draw the window over the page. Returns where it went and where the
    /// cursor belongs (column, row of the pane).
    pub fn render(
        &mut self,
        out: &mut Vec<u8>,
        cols: u16,
        rows: u16,
        shade: Option<&crate::modal::Shade>,
    ) -> (Rect, Option<(u16, u16)>) {
        let (canvas, cursor) = self.layout(cols as usize, rows as usize);
        let rect = canvas.draw(out, cols, rows, shade);
        self.drawn.rect = Some(rect);
        let cursor = cursor.map(|(row, col)| (rect.col + col as u16, rect.row + row as u16));
        (rect, cursor)
    }
}

/// Which details the list has room for, as `ls -la` gives them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Columns {
    /// Mode, size and date before the name.
    Long,
    /// Size and date.
    Medium,
    /// Size.
    Short,
    /// The name alone.
    Name,
}

impl Columns {
    /// For a list `width` cells wide, leaving the name at least 16.
    fn for_width(width: usize) -> Columns {
        // "✓ " is 2, the mode 12, the size 7 and the date 14 with gaps.
        match width {
            w if w >= 2 + 12 + 7 + 14 + 24 => Columns::Long,
            w if w >= 2 + 7 + 14 + 16 => Columns::Medium,
            w if w >= 2 + 7 + 12 => Columns::Short,
            _ => Columns::Name,
        }
    }

    fn header(self, width: usize) -> String {
        let text = match self {
            Columns::Long => "  Mode          Size  Modified      Name",
            Columns::Medium => "    Size  Modified      Name",
            Columns::Short => "    Size  Name",
            Columns::Name => "  Name",
        };
        text.chars().take(width).collect()
    }

    fn details(self, entry: &Entry, now: i64) -> String {
        if entry.parent {
            return match self {
                Columns::Long => format!("{:12}{:7}{:14}", "", "", ""),
                Columns::Medium => format!("{:7}{:14}", "", ""),
                Columns::Short => format!("{:7}", ""),
                Columns::Name => String::new(),
            };
        }
        let size = if entry.dir { "-".to_owned() } else { human_size(entry.size) };
        match self {
            Columns::Long => {
                format!("{}  {size:>5}  {:12}  ", mode_string(entry), when(entry.modified, now))
            }
            Columns::Medium => format!("{size:>5}  {:12}  ", when(entry.modified, now)),
            Columns::Short => format!("{size:>5}  "),
            Columns::Name => String::new(),
        }
    }
}

/// The folder's entries: ".." unless it is the root, then folders, then
/// files, each by name regardless of case. Dot files only if `hidden`.
fn read_folder(folder: &Path, hidden: bool) -> std::io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(folder)? {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let Ok(own) = item.metadata() else { continue };
        let link = own.file_type().is_symlink();
        // A link shows as what it points to.
        let meta = if link { std::fs::metadata(item.path()).unwrap_or(own.clone()) } else { own.clone() };
        entries.push(Entry {
            name,
            dir: meta.is_dir(),
            size: meta.len(),
            modified: meta.mtime(),
            mode: own.permissions().mode(),
            link,
            parent: false,
        });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    if folder.parent().is_some() {
        let parent = Entry { name: "..".into(), dir: true, size: 0, modified: 0, mode: 0, link: false, parent: true };
        entries.insert(0, parent);
    }
    Ok(entries)
}

/// Places to start from that exist: home and its usual folders, the
/// folder kb was started in, and the top of the disk.
fn places() -> Vec<(String, PathBuf)> {
    let mut places = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        places.push(("Home".to_owned(), home.clone()));
        for name in ["Desktop", "Documents", "Downloads", "Pictures"] {
            let path = home.join(name);
            if path.is_dir() {
                places.push((name.to_owned(), path));
            }
        }
    }
    if let Ok(here) = std::env::current_dir() {
        if !places.iter().any(|(_, path)| *path == here) && here.parent().is_some() {
            let name = here.file_name().map_or("Here".to_owned(), |name| name.to_string_lossy().into_owned());
            places.push((name, here));
        }
    }
    places.push(("Computer".to_owned(), PathBuf::from("/")));
    places.into_iter().map(|(name, path)| (name, path.canonicalize().unwrap_or(path))).collect()
}

/// A typed path, from `folder` unless it starts at / or ~.
fn resolve(folder: &Path, typed: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (typed.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => folder.join(typed),
    }
}

/// Complete a typed name or path as far as it is the same for every
/// match, adding / to a single folder. None if nothing matches.
fn complete(folder: &Path, typed: &str, hidden: bool) -> Option<String> {
    let (dir_part, prefix) = match typed.rfind('/') {
        Some(at) => (&typed[..=at], &typed[at + 1..]),
        None => ("", typed),
    };
    let dir = if dir_part.is_empty() { folder.to_owned() } else { resolve(folder, dir_part) };
    let lower = prefix.to_lowercase();
    let mut matches: Vec<(String, bool)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|item| item.ok())
        .map(|item| (item.file_name().to_string_lossy().into_owned(), item.path().is_dir()))
        .filter(|(name, _)| name.to_lowercase().starts_with(&lower))
        .filter(|(name, _)| hidden || prefix.starts_with('.') || !name.starts_with('.'))
        .collect();
    matches.sort();
    let (first, first_dir) = matches.first()?.clone();
    if matches.len() == 1 {
        return Some(format!("{dir_part}{first}{}", if first_dir { "/" } else { "" }));
    }
    // The longest start every match shares, ignoring case, in the first's case.
    let mut common: Vec<char> = first.chars().collect();
    for (name, _) in &matches[1..] {
        let same = common.iter().zip(name.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
        common.truncate(same);
    }
    let common: String = common.into_iter().collect();
    (common.chars().count() >= prefix.chars().count()).then(|| format!("{dir_part}{common}"))
}

/// Whether a file name fits an `accept` attribute, for dimming the rest.
/// Chrome doesn't enforce it, and neither does kb: a type it doesn't know
/// counts as a match.
fn matches_accept(name: &str, accept: &str) -> bool {
    if accept.is_empty() {
        return true;
    }
    let extension = name.rsplit_once('.').map_or(String::new(), |(_, ext)| ext.to_lowercase());
    accept.split(',').map(str::trim).filter(|token| !token.is_empty()).any(|token| {
        let token = token.to_lowercase();
        let known: &[&str] = match token.as_str() {
            "image/*" => {
                &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "heic", "heif", "avif", "ico", "tif", "tiff"]
            }
            "video/*" => &["mp4", "mov", "webm", "mkv", "avi", "m4v", "mpeg", "mpg"],
            "audio/*" => &["mp3", "wav", "m4a", "aac", "ogg", "oga", "flac", "opus"],
            "application/pdf" => &["pdf"],
            "text/plain" => &["txt", "text"],
            "text/csv" => &["csv"],
            "application/json" => &["json"],
            "image/png" => &["png"],
            "image/jpeg" => &["jpg", "jpeg"],
            _ => match token.strip_prefix('.') {
                Some(ext) => return extension == ext,
                None => return true,
            },
        };
        known.contains(&extension.as_str())
    })
}

/// A size as `ls -lh` gives it: 512, 1.2K, 34K, 5.0M.
fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        return bytes.to_string();
    }
    let mut size = bytes as f64;
    for unit in ['K', 'M', 'G', 'T'] {
        size /= 1024.0;
        if size < 1024.0 || unit == 'T' {
            return if size < 10.0 { format!("{size:.1}{unit}") } else { format!("{size:.0}{unit}") };
        }
    }
    unreachable!()
}

/// The mode column of `ls -l`: drwxr-xr-x.
fn mode_string(entry: &Entry) -> String {
    let kind = if entry.link {
        'l'
    } else if entry.dir {
        'd'
    } else {
        '-'
    };
    let mut text = String::from(kind);
    for shift in [6, 3, 0] {
        let bits = entry.mode >> shift;
        text.push(if bits & 4 != 0 { 'r' } else { '-' });
        text.push(if bits & 2 != 0 { 'w' } else { '-' });
        text.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    text
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// A time as `ls -l` gives it, in local time: "Nov 21 14:03" for the last
/// six months, "Nov 21  2025" before that.
fn when(seconds: i64, now: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time = seconds as libc::time_t;
    // SAFETY: localtime_r only writes the tm it is given.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return String::new();
    }
    let month = MONTHS[tm.tm_mon.clamp(0, 11) as usize];
    let recent = (now - seconds).abs() < 182 * 24 * 3600;
    if recent {
        format!("{month} {:>2} {:02}:{:02}", tm.tm_mday, tm.tm_hour, tm.tm_min)
    } else {
        format!("{month} {:>2}  {}", tm.tm_mday, tm.tm_year + 1900)
    }
}

/// A path with the home folder as ~.
fn tilde(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() { "~".to_owned() } else { format!("~/{}", rest.display()) };
        }
    }
    path.display().to_string()
}

/// Why a file couldn't be had, in a few words.
fn reason(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "no such file or folder".to_owned(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        _ => error.to_string(),
    }
}

/// This computer's name, so it's plain whose files these are: over ssh,
/// not the one the user is typing on.
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let name = if ok { String::from_utf8_lossy(&buf[..end]).into_owned() } else { String::new() };
    let name = name.strip_suffix(".local").unwrap_or(&name).to_owned();
    match (name.is_empty(), std::env::var_os("SSH_CONNECTION").is_some()) {
        (true, _) => "this computer".to_owned(),
        (false, true) => format!("{name} (over ssh)"),
        (false, false) => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own under the temporary directory, removed after.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let path = std::env::temp_dir().join(format!("kb-picker-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("Photos")).unwrap();
            std::fs::create_dir_all(path.join("archive")).unwrap();
            for (file, text) in [("notes.txt", "hello"), ("Report.pdf", "%PDF"), ("photo.png", "png"), (".secret", "x")]
            {
                std::fs::write(path.join(file), text).unwrap();
            }
            Scratch(path.canonicalize().unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(picker: &Picker) -> Vec<&str> {
        picker.entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    fn press(picker: &mut Picker, code: KeyCode) -> Outcome {
        picker.key(&Key::plain(code))
    }

    fn type_text(picker: &mut Picker, text: &str) {
        for c in text.chars() {
            press(picker, KeyCode::Char(c));
        }
    }

    #[test]
    fn lists_folders_first_then_files_without_dot_files() {
        let scratch = Scratch::new("list");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        assert_eq!(names(&picker), ["..", "archive", "Photos", "notes.txt", "photo.png", "Report.pdf"]);
        picker.toggle_hidden();
        assert!(names(&picker).contains(&".secret"));
    }

    #[test]
    fn arrows_and_enter_open_folders_and_choose_files() {
        let scratch = Scratch::new("enter");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(press(&mut picker, KeyCode::Enter), Outcome::Stay);
        assert_eq!(picker.folder(), scratch.0.join("Photos"));
        // Backspace goes back up, to the folder just left.
        press(&mut picker, KeyCode::Backspace);
        assert_eq!(picker.folder(), scratch.0);
        assert_eq!(picker.entries[picker.selected].name, "Photos");
        press(&mut picker, KeyCode::Down);
        assert_eq!(press(&mut picker, KeyCode::Enter), Outcome::Choose(vec![scratch.0.join("notes.txt")]));
    }

    #[test]
    fn typing_jumps_and_a_typed_name_is_chosen() {
        let scratch = Scratch::new("typing");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        type_text(&mut picker, "rep");
        assert_eq!(picker.entries[picker.selected].name, "Report.pdf");
        press(&mut picker, KeyCode::Tab);
        assert_eq!(picker.field.text(), "Report.pdf");
        assert_eq!(press(&mut picker, KeyCode::Enter), Outcome::Choose(vec![scratch.0.join("Report.pdf")]));
    }

    #[test]
    fn a_missing_name_says_so_and_stays_open() {
        let scratch = Scratch::new("missing");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        type_text(&mut picker, "nope.txt");
        assert_eq!(press(&mut picker, KeyCode::Enter), Outcome::Stay);
        assert_eq!(picker.problem.as_deref(), Some("nope.txt: no such file or folder"));
        let (canvas, _) = picker.layout(100, 30);
        assert!(canvas.line(canvas.height - 2).contains("nope.txt: no such file or folder"));
    }

    #[test]
    fn a_typed_folder_is_opened() {
        let scratch = Scratch::new("folder");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        type_text(&mut picker, "arch");
        press(&mut picker, KeyCode::Tab);
        assert_eq!(picker.field.text(), "archive/");
        press(&mut picker, KeyCode::Enter);
        assert_eq!(picker.folder(), scratch.0.join("archive"));
        assert_eq!(picker.field.text(), "");
    }

    #[test]
    fn space_marks_several_and_enter_chooses_them() {
        let scratch = Scratch::new("marks");
        let mut picker = Picker::new(1, true, "", "https://a.com/", &scratch.0);
        // .., archive, Photos, notes.txt, photo.png, Report.pdf
        picker.selected = 3;
        press(&mut picker, KeyCode::Char(' '));
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Char(' '));
        assert_eq!(picker.marked.len(), 2);
        // A folder can't be marked.
        picker.selected = 1;
        press(&mut picker, KeyCode::Char(' '));
        assert_eq!(picker.marked.len(), 2);
        picker.selected = 5;
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Choose(vec![scratch.0.join("notes.txt"), scratch.0.join("Report.pdf")])
        );
    }

    #[test]
    fn a_single_file_field_types_spaces() {
        let scratch = Scratch::new("single");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        press(&mut picker, KeyCode::Char(' '));
        assert!(picker.marked.is_empty());
        assert_eq!(picker.field.text(), " ");
    }

    #[test]
    fn a_key_is_sent_only_when_asked_twice() {
        let scratch = Scratch::new("secret");
        std::fs::write(scratch.0.join("deploy.pem"), "x").unwrap();
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        type_text(&mut picker, "deploy.pem");
        assert_eq!(press(&mut picker, KeyCode::Enter), Outcome::Stay);
        let (canvas, _) = picker.layout(100, 30);
        assert!(canvas.line(canvas.height - 2).contains("Send deploy.pem to a.com?"));
        // Esc goes back to the picker, not out of it.
        assert_eq!(press(&mut picker, KeyCode::Esc), Outcome::Stay);
        assert!(picker.confirming.is_none());
        press(&mut picker, KeyCode::Enter);
        assert_eq!(press(&mut picker, KeyCode::Enter), Outcome::Choose(vec![scratch.0.join("deploy.pem")]));
    }

    #[test]
    fn the_field_shows_the_selected_file() {
        let scratch = Scratch::new("fill");
        let mut picker = Picker::new(1, true, "", "https://a.com/", &scratch.0);
        // .., archive, Photos, notes.txt, photo.png, Report.pdf
        assert_eq!(picker.field.text(), "");
        picker.key(&Key::plain(KeyCode::End));
        assert_eq!(picker.field.text(), "Report.pdf");
        press(&mut picker, KeyCode::Up);
        assert_eq!(picker.field.text(), "photo.png");
        // A folder empties it; marks fill it with every marked name.
        picker.selected = 1;
        picker.follow_selection();
        assert_eq!(picker.field.text(), "");
        picker.selected = 3;
        press(&mut picker, KeyCode::Char(' '));
        press(&mut picker, KeyCode::Char(' '));
        assert_eq!(picker.field.text(), "\"notes.txt\" \"photo.png\"");
        // Typing replaces it, and moving doesn't undo the typing until the
        // selection moves.
        let mut single = Picker::new(2, false, "", "https://a.com/", &scratch.0);
        press(&mut single, KeyCode::Down);
        press(&mut single, KeyCode::Down);
        press(&mut single, KeyCode::Down);
        assert_eq!(single.field.text(), "notes.txt");
        type_text(&mut single, "rep");
        assert_eq!(single.field.text(), "rep");
        assert_eq!(single.entries[single.selected].name, "Report.pdf");
        assert_eq!(press(&mut single, KeyCode::Up), Outcome::Stay);
        assert_eq!(single.field.text(), "photo.png");
    }

    #[test]
    fn esc_cancels() {
        let scratch = Scratch::new("esc");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        assert_eq!(press(&mut picker, KeyCode::Esc), Outcome::Cancel);
    }

    #[test]
    fn columns_follow_the_room() {
        assert_eq!(Columns::for_width(100), Columns::Long);
        assert_eq!(Columns::for_width(50), Columns::Medium);
        assert_eq!(Columns::for_width(30), Columns::Short);
        assert_eq!(Columns::for_width(15), Columns::Name);
    }

    #[test]
    fn a_narrow_pane_drops_the_places() {
        let scratch = Scratch::new("narrow");
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        let (wide, _) = picker.layout(120, 30);
        assert!(wide.line(3).contains("Places"));
        assert!(wide.line(3).contains("Mode"));
        let (narrow, _) = picker.layout(60, 30);
        assert!(!narrow.line(5).contains("Places"));
        assert!(narrow.line(3).contains("Home"));
        assert!(!narrow.line(5).contains("Mode"));
        assert!(narrow.line(5).contains("Size"));
        assert!(narrow.width <= 60);
    }

    #[test]
    fn tiny_panes_lay_out_without_panicking() {
        let scratch = Scratch::new("tiny");
        std::fs::write(scratch.0.join("写真📷.png"), "").unwrap();
        let mut picker = Picker::new(1, true, "image/*", "https://a.com/", &scratch.0);
        for cols in 4..=80 {
            for rows in 3..=30 {
                let (canvas, _) = picker.layout(cols, rows);
                assert!(canvas.width <= cols.max(1), "{cols}x{rows}");
            }
        }
    }

    #[test]
    fn clicks_select_and_double_clicks_open() {
        let scratch = Scratch::new("clicks");
        let mut picker = Picker::new(1, true, "", "https://a.com/", &scratch.0);
        let (rect, _) = picker.render(&mut Vec::new(), 120, 30, None);
        let col = rect.col as usize + picker.drawn.list_cols.start + 4;
        let top = rect.row as usize + picker.drawn.list_top;
        let row = |n: usize| top + n;
        assert_eq!(picker.click(col, row(3), false), Outcome::Stay);
        assert_eq!(picker.selected, 3);
        // Ctrl+click marks.
        picker.click(col, row(4), true);
        assert_eq!(picker.marked, [scratch.0.join("photo.png")]);
        // A double click on a folder opens it.
        picker.click(col, row(1), false);
        picker.click(col, row(1), false);
        assert_eq!(picker.folder(), scratch.0.join("archive"));
    }

    #[test]
    fn the_wheel_scrolls_past_the_selection() {
        let scratch = Scratch::new("wheel");
        for n in 0..100 {
            std::fs::write(scratch.0.join(format!("file{n:03}.txt")), "").unwrap();
        }
        let mut picker = Picker::new(1, false, "", "https://a.com/", &scratch.0);
        picker.layout(100, 30);
        assert_eq!((picker.selected, picker.scroll), (0, 0));
        picker.wheel(3);
        let (canvas, _) = picker.layout(100, 30);
        assert_eq!((picker.selected, picker.scroll), (0, 3));
        // The list's top row is the fourth entry, past "..", archive and Photos.
        let top = canvas.line(picker.drawn.list_top);
        assert!(top.contains(&picker.entries[3].name) && !top.contains("archive"), "{top}");
        // A key brings the selection back into view.
        press(&mut picker, KeyCode::Down);
        picker.layout(100, 30);
        assert_eq!((picker.selected, picker.scroll), (1, 1));
        // The wheel stops at the end of the list.
        picker.wheel(1000);
        picker.layout(100, 30);
        assert_eq!(picker.scroll, picker.entries.len() - picker.drawn.list_rows);
    }

    #[test]
    fn accept_dims_what_the_page_does_not_want() {
        assert!(matches_accept("a.PNG", "image/*"));
        assert!(!matches_accept("a.pdf", "image/*"));
        assert!(matches_accept("a.pdf", ".pdf, .doc"));
        assert!(matches_accept("a.doc", ".pdf,.doc"));
        assert!(matches_accept("anything", ""));
        assert!(matches_accept("a.xyz", "application/x-unknown"));
    }

    #[test]
    fn sizes_modes_and_times_read_like_ls() {
        assert_eq!(human_size(512), "512");
        assert_eq!(human_size(1229), "1.2K");
        assert_eq!(human_size(34 * 1024), "34K");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0M");
        let entry = |dir, link, mode| Entry { name: "x".into(), dir, size: 0, modified: 0, mode, link, parent: false };
        assert_eq!(mode_string(&entry(true, false, 0o755)), "drwxr-xr-x");
        assert_eq!(mode_string(&entry(false, false, 0o644)), "-rw-r--r--");
        assert_eq!(mode_string(&entry(false, true, 0o777)), "lrwxrwxrwx");
        let now = now();
        assert!(when(now, now).contains(':'));
        assert!(!when(now - 400 * 24 * 3600, now).contains(':'));
    }

    #[test]
    fn paths_resolve_from_the_folder_or_home() {
        let folder = Path::new("/data/work");
        assert_eq!(resolve(folder, "a.txt"), Path::new("/data/work/a.txt"));
        assert_eq!(resolve(folder, "/etc/hosts"), Path::new("/etc/hosts"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(resolve(folder, "~/x"), Path::new(&home).join("x"));
            assert_eq!(resolve(folder, "~"), Path::new(&home));
        }
    }

    #[test]
    fn completion_goes_as_far_as_the_matches_agree() {
        let scratch = Scratch::new("complete");
        std::fs::write(scratch.0.join("notebook.md"), "").unwrap();
        assert_eq!(complete(&scratch.0, "no", false).as_deref(), Some("note"));
        assert_eq!(complete(&scratch.0, "Pho", false).as_deref(), Some("Photo"));
        assert_eq!(complete(&scratch.0, "arc", false).as_deref(), Some("archive/"));
        assert_eq!(complete(&scratch.0, "zzz", false), None);
        assert_eq!(complete(&scratch.0, "Photos/", false), None);
    }
}
