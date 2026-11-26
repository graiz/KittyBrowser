//! The page drawn in characters: in a terminal that can't draw images, and
//! after `about:ascii` in one that can.
//!
//! The page's own text is printed where Chrome laid it out, letter by
//! letter in its own colour, read from the page by `READ_TEXT`. Everything
//! else (pictures, backgrounds, borders) is drawn from the frame's pixels:
//! each cell gets the character and the two colours that come closest to
//! the pixels under it. A character covers part of its cell (a slash the
//! diagonal, `▀` the upper half), so with the foreground colour on that
//! part and the background on the rest, a cell holds two colours split
//! along the shape that suits it best.
//!
//! For each shape, the best two colours are the average of the pixels it
//! covers and the average of the rest, and how close that comes is how much
//! of the pixels' spread about their average the split takes away. Sums
//! over the cell's pixels, cumulative from its corner, give each part's
//! total from four lookups, so trying every shape costs about as much as
//! reading the pixels once.

use super::graphics::{self, Pixels};
use std::fmt::Write;

/// How the parts of the page that aren't text are drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// ASCII art: punctuation and letters whose shapes suit the pixels.
    Letters,
    /// Block characters, which come closer to the pixels: halves, quarters
    /// and eighths, and with `sextants` the 2 by 3 blocks of Unicode 13,
    /// which many terminals draw themselves but older fonts lack.
    Blocks { sextants: bool },
}

/// The block shapes, as rectangles in a cell divided into 8 columns and 24
/// rows (so that eighths and thirds both fall on lines). Each shape's
/// complement is the same shape with the colours swapped, so it is left
/// out: `▄` is `▀` inverted, `▟` is `▘`, `▔` is `▇`.
const BLOCKS: &[(char, &[[u8; 4]])] = &[
    ('▀', &[[0, 0, 8, 12]]),
    ('▌', &[[0, 0, 4, 24]]),
    ('▘', &[[0, 0, 4, 12]]),
    ('▝', &[[4, 0, 8, 12]]),
    ('▖', &[[0, 12, 4, 24]]),
    ('▗', &[[4, 12, 8, 24]]),
    ('▚', &[[0, 0, 4, 12], [4, 12, 8, 24]]),
    ('▁', &[[0, 21, 8, 24]]),
    ('▂', &[[0, 18, 8, 24]]),
    ('▃', &[[0, 15, 8, 24]]),
    ('▅', &[[0, 9, 8, 24]]),
    ('▆', &[[0, 6, 8, 24]]),
    ('▇', &[[0, 3, 8, 24]]),
    ('▏', &[[0, 0, 1, 24]]),
    ('▎', &[[0, 0, 2, 24]]),
    ('▍', &[[0, 0, 3, 24]]),
    ('▋', &[[0, 0, 5, 24]]),
    ('▊', &[[0, 0, 6, 24]]),
    ('▉', &[[0, 0, 7, 24]]),
];

/// The ASCII shapes, roughly as a terminal's font draws them, on a grid of
/// 4 by 6 in the cell: the baseline under the fifth row, the sixth for
/// what goes below it.
const LETTERS: &[(char, [&str; 6])] = &[
    ('.', ["....", "....", "....", "....", ".##.", "...."]),
    (',', ["....", "....", "....", "....", ".##.", ".#.."]),
    (':', ["....", ".##.", "....", "....", ".##.", "...."]),
    ('\'', [".##.", ".##.", "....", "....", "....", "...."]),
    ('"', ["#..#", "#..#", "....", "....", "....", "...."]),
    ('^', [".##.", "#..#", "....", "....", "....", "...."]),
    ('-', ["....", "....", "....", "####", "....", "...."]),
    ('_', ["....", "....", "....", "....", "....", "####"]),
    ('=', ["....", "....", "####", "....", "####", "...."]),
    ('+', ["....", ".##.", "####", ".##.", "....", "...."]),
    ('*', ["....", "#..#", ".##.", "#..#", "....", "...."]),
    ('/', ["...#", "..#.", "..#.", ".#..", ".#..", "#..."]),
    ('\\', ["#...", ".#..", ".#..", "..#.", "..#.", "...#"]),
    ('|', [".##.", ".##.", ".##.", ".##.", ".##.", ".##."]),
    ('(', ["..#.", ".#..", ".#..", ".#..", ".#..", "..#."]),
    (')', [".#..", "..#.", "..#.", "..#.", "..#.", ".#.."]),
    ('[', [".##.", ".#..", ".#..", ".#..", ".#..", ".##."]),
    (']', [".##.", "..#.", "..#.", "..#.", "..#.", ".##."]),
    ('<', ["...#", "..#.", ".#..", "..#.", "...#", "...."]),
    ('>', ["#...", ".#..", "..#.", ".#..", "#...", "...."]),
    ('o', ["....", "....", ".##.", "#..#", ".##.", "...."]),
    ('c', ["....", "....", ".###", "#...", ".###", "...."]),
    ('u', ["....", "....", "#..#", "#..#", ".##.", "...."]),
    ('n', ["....", "....", ".##.", "#..#", "#..#", "...."]),
    ('x', ["....", "....", "#..#", ".##.", "#..#", "...."]),
    ('O', [".##.", "#..#", "#..#", "#..#", ".##.", "...."]),
    ('8', [".##.", "#..#", ".##.", "#..#", ".##.", "...."]),
    ('L', ["#...", "#...", "#...", "#...", "####", "...."]),
    ('J', ["...#", "...#", "...#", "...#", "####", "...."]),
    ('T', ["####", ".##.", ".##.", ".##.", ".##.", "...."]),
    ('M', ["#..#", "####", "####", "#..#", "#..#", "...."]),
    ('#', [".#.#", "####", ".#.#", "####", ".#.#", "...."]),
    ('%', ["#..#", "...#", "..#.", ".#..", "#...", "#..#"]),
    ('@', [".##.", "#..#", "#.##", "#.##", "#...", ".###"]),
];

/// How much closer than one flat colour a shape must come, per pixel, in
/// squared steps of a colour channel. Below it, the cell is a space: the
/// split would show noise, and flicker as it changes from frame to frame.
const WORTH: f64 = 12.0;

/// Reads the text on screen, line by line as the page wrapped it: for each
/// line of each text node in view, its colour, whether it is in a fixed
/// box, its letters and the box they fill, in CSS pixels from the top left
/// of the document (of the viewport, if fixed), so that the lines can
/// follow the page as it scrolls until the text is read again. Open shadow
/// roots are read too, as components keep much of a page's text there.
/// Text there only for screen readers isn't returned, nor text under
/// something painted (a background, a picture); a transparent element
/// over it, as a link stretched over a whole card to make it clickable,
/// doesn't hide it.
///
/// A page too big to read within `BUDGET` milliseconds isn't read: what
/// was hidden is shown again, the answer says `complex`, and the page is
/// drawn from its pixels alone, until a try 5 seconds later.
///
/// Once read, it hides the page's own text, in the document and each shadow
/// root, so that frames show what is behind it: the letters are drawn over
/// that, and a blurred copy of the page's own around them would only be
/// noise. The text keeps its colour, to be read, and is only not painted;
/// fields keep theirs, as their values aren't read. `SHOW_TEXT` undoes it.
/// A constructed sheet, not a `<style>`, so a page's content security
/// policy doesn't refuse it.
pub const READ_TEXT: &str = r#"(() => {
  const BUDGET = 40, began = performance.now(), W = innerWidth, H = innerHeight;
  if (window.__kbTooMuch && began - window.__kbTooMuch < 5000) return JSON.stringify({ w: W, h: H, complex: true });
  if (!window.__kbHideText) {
    window.__kbHideText = new CSSStyleSheet();
    window.__kbHideText.replaceSync(`*:not(input, textarea, select) {
      -webkit-text-fill-color: transparent !important;
      text-decoration-color: transparent !important;
      text-shadow: none !important; }`);
    window.__kbHidden = new Set();
  }
  const roots = [document], colours = [], index = new Map(), lines = [];
  const range = document.createRange(), fixedness = new Map();
  const parent = el => el.parentElement || (el.parentNode && el.parentNode.host) || null;
  const fixed = el => {
    if (!el) return false;
    if (!fixedness.has(el)) fixedness.set(el, getComputedStyle(el).position === 'fixed' || fixed(parent(el)));
    return fixedness.get(el);
  };
  const onScreen = r => r.width > 0 && r.bottom > 0 && r.top < H && r.right > 0 && r.left < W;
  const painted = el => {
    if (/^(img|video|canvas|iframe|svg|picture|object|embed)$/.test(el.localName)) return true;
    const style = getComputedStyle(el);
    const alpha = (style.backgroundColor.match(/[\d.]+/g) || []).map(Number)[3];
    return style.backgroundImage !== 'none' || alpha === undefined || alpha > 0;
  };
  // Whether the text in `el`, at x, y, is the first thing painted there.
  const seen = (el, node, x, y) => {
    for (const hit of node.getRootNode().elementsFromPoint(x, y)) {
      if (hit === el || el.contains(hit) || hit.contains(el)) return true;
      if (painted(hit)) return false;
    }
    return false;
  };
  const read = node => {
    const el = node.parentElement;
    if (!el || !/\S/.test(node.data)) return;
    range.selectNodeContents(node);
    if (!onScreen(range.getBoundingClientRect())) return;
    const style = getComputedStyle(el);
    const rgba = (style.color.match(/[\d.]+/g) || []).map(Number);
    if (style.visibility !== 'visible' || rgba.length < 3 || (rgba.length > 3 && rgba[3] < 0.5)) return;
    // Text kept for screen readers alone: squeezed into a pixel and
    // clipped, or see-through.
    const box = el.getBoundingClientRect();
    if ((box.width < 3 && box.height < 3) || /rect\(0/.test(style.clip) || /inset\(50%/.test(style.clipPath)) return;
    if (el.checkVisibility && !el.checkVisibility({ opacityProperty: true })) return;
    const key = rgba.slice(0, 3).join();
    const f = fixed(el), dx = f ? 0 : scrollX, dy = f ? 0 : scrollY, transform = style.textTransform;
    let line = null, i = 0, checked = false;
    const end = () => {
      if (line) {
        if (!index.has(key)) { index.set(key, colours.length); colours.push(rgba.slice(0, 3)); }
        lines.push([index.get(key), f ? 1 : 0, line.text, line.l + dx, line.t + dy, line.r + dx, line.b + dy]);
      }
      line = null;
    };
    for (let ch of node.data) {
      range.setStart(node, i);
      range.setEnd(node, i + ch.length);
      i += ch.length;
      const r = range.getBoundingClientRect();
      if (r.top >= H && line) break;
      if (!onScreen(r)) continue;
      if (!checked) {
        checked = true;
        if (!seen(el, node, r.left + r.width / 2, r.top + r.height / 2)) break;
      }
      if (line && (Math.abs(r.top - line.t) > r.height / 2 || r.left < line.r - r.width)) end();
      const cased = transform === 'uppercase' ? ch.toUpperCase() : transform === 'lowercase' ? ch.toLowerCase() : ch;
      if ([...cased].length === 1) ch = cased;
      if (!line) line = { text: '', l: r.left, t: r.top, r: r.right, b: r.bottom };
      line.text += ch;
      line.r = Math.max(line.r, r.right);
      line.b = Math.max(line.b, r.bottom);
    }
    end();
  };
  const walk = root => {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    for (let node; (node = walker.nextNode()); ) {
      if (node.nodeType === Node.TEXT_NODE) read(node);
      else if (node.shadowRoot) { roots.push(node.shadowRoot); walk(node.shadowRoot); }
      if (performance.now() - began > BUDGET) throw 'complex';
    }
  };
  try {
    const root = document.body || document.documentElement;
    if (root) walk(root);
  } catch (e) {
    if (e !== 'complex') throw e;
    window.__kbTooMuch = performance.now();
    for (const root of window.__kbHidden) {
      root.adoptedStyleSheets = root.adoptedStyleSheets.filter(sheet => sheet !== window.__kbHideText);
    }
    window.__kbHidden.clear();
    return JSON.stringify({ w: W, h: H, complex: true });
  }
  for (const root of roots) {
    if (window.__kbHidden.has(root)) continue;
    window.__kbHidden.add(root);
    root.adoptedStyleSheets = [...root.adoptedStyleSheets, window.__kbHideText];
  }
  return JSON.stringify({ w: W, h: H, colours, lines });
})()"#;

/// Shows the text `READ_TEXT` hid, for the page in pixels again.
pub const SHOW_TEXT: &str = r#"(() => {
  for (const root of window.__kbHidden || []) {
    root.adoptedStyleSheets = root.adoptedStyleSheets.filter(sheet => sheet !== window.__kbHideText);
  }
  delete window.__kbHideText;
  delete window.__kbHidden;
  delete window.__kbTooMuch;
})()"#;

/// One shape: its character and its rectangles, in eighths across and
/// twenty-fourths down.
struct Glyph {
    ch: char,
    rects: Vec<[u8; 4]>,
}

/// What a cell shows: a character, and the colours of its two parts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cell {
    ch: char,
    fg: [u8; 3],
    bg: [u8; 3],
}

impl Cell {
    /// A cell of one colour: a space, whose foreground doesn't show.
    const fn flat(colour: [u8; 3]) -> Cell {
        Cell { ch: ' ', fg: colour, bg: colour }
    }
}

/// Where a line goes: its row, the cell it starts in, and its left and
/// right edges and height in the page, as the pane shows it.
type Spot = (usize, usize, f64, f64, f64);

/// A letter of the page's text in a cell, and its colour.
type Letter = Option<(char, [u8; 3])>;

/// The page's text as `READ_TEXT` read it.
#[derive(Default, Debug, PartialEq)]
pub struct Text {
    /// The viewport it was read in, in CSS pixels.
    width: f64,
    height: f64,
    lines: Vec<Line>,
}

/// One line of a text node, as the page wrapped it.
#[derive(Debug, PartialEq)]
struct Line {
    colour: [u8; 3],
    fixed: bool,
    text: String,
    /// The box its letters fill, left, top, right, bottom.
    rect: [f64; 4],
}

impl Text {
    /// The text from `READ_TEXT`'s answer; None if it isn't one, or the
    /// page was too complex to read.
    pub fn parse(json: &str) -> Option<Text> {
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        if value["complex"] == true {
            return None;
        }
        let colours: Vec<[u8; 3]> = value["colours"]
            .as_array()?
            .iter()
            .map(|c| [0, 1, 2].map(|i| c[i].as_f64().unwrap_or(0.0).clamp(0.0, 255.0) as u8))
            .collect();
        let lines = value["lines"]
            .as_array()?
            .iter()
            .filter_map(|line| {
                let number = |i: usize| line[i].as_f64();
                Some(Line {
                    colour: *colours.get(line[0].as_u64()? as usize)?,
                    fixed: line[1].as_u64() == Some(1),
                    text: line[2].as_str()?.to_owned(),
                    rect: [number(3)?, number(4)?, number(5)?, number(6)?],
                })
            })
            .collect();
        Some(Text { width: value["w"].as_f64()?, height: value["h"].as_f64()?, lines })
    }
}

pub struct AsciiPage {
    glyphs: Vec<Glyph>,
    /// Colours as `38;2;r;g;b`, or the nearest of the 256 (`38;5;n`).
    truecolor: bool,
    /// The page's text, once read.
    text: Option<Text>,
    /// What each cell of the page shows, row by row, as last sent; empty
    /// when the screen is in no known state.
    shown: Vec<Cell>,
    /// The letter of the text in each cell, as last sent.
    letters: Vec<Letter>,
    /// The cells the page covers, and its first screen row.
    cols: u16,
    rows: u16,
    top: u16,
    /// The last frame, to leave the cells it didn't change alone.
    last: Option<Pixels>,
    /// Running sums over one cell's pixels, kept between cells.
    sums: Vec<[u32; 3]>,
}

impl AsciiPage {
    pub fn new(style: Style) -> AsciiPage {
        let glyphs = match style {
            Style::Letters => LETTERS.iter().map(|(ch, rows)| Glyph { ch: *ch, rects: letter_rects(rows) }).collect(),
            Style::Blocks { sextants } => {
                let blocks = BLOCKS.iter().map(|(ch, rects)| Glyph { ch: *ch, rects: rects.to_vec() });
                blocks.chain(sextant_glyphs().filter(|_| sextants)).collect()
            }
        };
        AsciiPage {
            glyphs,
            truecolor: std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit")),
            text: None,
            shown: Vec::new(),
            letters: Vec::new(),
            cols: 0,
            rows: 0,
            top: 0,
            last: None,
            sums: Vec::new(),
        }
    }

    /// The screen was wiped: the next draw sends every cell.
    pub fn forget(&mut self) {
        self.shown.clear();
        self.last = None;
    }

    /// The page's text, read again.
    pub fn set_text(&mut self, text: Option<Text>) {
        self.text = text;
    }

    /// Append the escapes that draw `png`, one of Chrome's frames, over
    /// `cols` x `rows` cells from screen row `top`. `scroll` is how far the
    /// page was scrolled in the frame, in CSS pixels. Returns what was sent,
    /// to log.
    pub fn draw(&mut self, out: &mut Vec<u8>, png: &str, cols: u16, rows: u16, top: u16, scroll: (f64, f64)) -> String {
        let pixels = crate::base64::decode(png)
            .and_then(|bytes| graphics::decode_png(&bytes, Vec::new()).or_else(|| graphics::decode_jpeg(&bytes)));
        match pixels {
            Some(pixels) => self.draw_pixels(out, pixels, cols, rows, top, scroll),
            None => "nothing: the frame didn't decode".to_owned(),
        }
    }

    fn draw_pixels(
        &mut self,
        out: &mut Vec<u8>,
        pixels: Pixels,
        cols: u16,
        rows: u16,
        top: u16,
        scroll: (f64, f64),
    ) -> String {
        let count = cols as usize * rows as usize;
        if (cols, rows, top) != (self.cols, self.rows, self.top) || self.shown.len() != count {
            (self.cols, self.rows, self.top) = (cols, rows, top);
            self.shown = Vec::new();
            self.last = None;
        }
        let last = self.last.take().filter(|last| last.width == pixels.width && last.height == pixels.height);
        let size = (cell_size(pixels.width, cols), cell_size(pixels.height, rows));
        // A cell in CSS pixels, as the text was laid out.
        let css = self.text.as_ref().map_or((1.0, 1.0), |text| {
            (size.0 * text.width / pixels.width as f64, size.1 * text.height / pixels.height as f64)
        });
        let letters = self.place(cols, rows, scroll, css);
        let mut cells = Vec::with_capacity(count);
        let mut changed = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let area = cell_area(&pixels, col, row, size);
                let at = cells.len();
                let unchanged = self.letters.get(at) == Some(&letters[at])
                    && last.as_ref().is_some_and(|last| same_pixels(last, &pixels, area));
                let cell = match (self.shown.get(at), letters[at]) {
                    (Some(shown), _) if unchanged => *shown,
                    (_, None) => self.fit(&pixels, area),
                    // The letter in its colour, on whichever of the cell's
                    // two colours is further from it: the page behind it.
                    (_, Some((ch, colour))) => {
                        let Cell { fg, bg, .. } = self.fit(&pixels, area);
                        let bg = if distance(fg, colour) > distance(bg, colour) { fg } else { bg };
                        match ch {
                            ' ' => Cell::flat(bg),
                            _ => Cell { ch, fg: colour, bg },
                        }
                    }
                };
                let cell = self.palette(cell);
                if self.shown.get(at) != Some(&cell) {
                    changed.push(at);
                }
                cells.push(cell);
            }
        }
        self.shown = cells;
        self.letters = letters;
        self.last = Some(pixels);
        self.emit(out, &changed);
        format!("{} of {count} cells in characters", changed.len())
    }

    /// The text's letters in the cells, with the page scrolled by `scroll`.
    /// Each line starts in the cell where the page starts it, on the row
    /// through its middle, with a letter in each cell after: lines break
    /// where the page breaks them, though a terminal's letters are wider
    /// or narrower than the page's. A line that runs long stops a cell
    /// short of where the next text on its row starts; one that follows another straight on
    /// (a link in a sentence) goes on after it. `cell` is a cell's size in
    /// CSS pixels.
    ///
    /// The text scrolls by whole cells, to the nearest: lines sit at
    /// different heights in their cells, and moved by the scroll itself
    /// each would cross into the next row at a different moment, so that
    /// the text staggers as it goes. Moved together, it is at most half a
    /// cell from the page under it, and on it once a scroll stops at a
    /// whole cell.
    fn place(&self, cols: u16, rows: u16, scroll: (f64, f64), cell: (f64, f64)) -> Vec<Letter> {
        let (cols, rows) = (cols as usize, rows as usize);
        let mut grid: Vec<Letter> = vec![None; cols * rows];
        let Some(text) = self.text.as_ref().filter(|text| text.width > 0.0 && text.height > 0.0) else {
            return grid;
        };
        let scroll = ((scroll.0 / cell.0).round() * cell.0, (scroll.1 / cell.1).round() * cell.1);
        let spots: Vec<Option<Spot>> = text
            .lines
            .iter()
            .map(|line| {
                let (dx, dy) = if line.fixed { (0.0, 0.0) } else { scroll };
                let [l, t, r, b] = line.rect;
                let (l, r, middle) = (l - dx, r - dx, (t + b) / 2.0 - dy);
                let row = (middle / cell.1).floor();
                let own = (l / cell.0).max(0.0) as usize;
                (row >= 0.0 && (row as usize) < rows).then_some((row as usize, own, l, r, b - t))
            })
            .collect();
        // Whether a line starting at `left` goes on from one ending at
        // `right` in type `height` high: no further on than a wide space,
        // which is some fraction of the type's height, not of a cell. If it
        // does, whether there is a space between them (even a thin one, in a
        // text node of its own, which isn't read).
        let follows = |left: f64, right: f64, height: f64| {
            let gap = left - right;
            (gap > -1.0 && gap < 0.6 * height).then_some(gap > 0.08 * height)
        };
        // Whether each line goes on from the one before it on its row: the
        // lines of a run (a sentence and the links in it) go one after
        // another, and only the start of another run stops one.
        let mut last_right: Vec<Option<f64>> = vec![None; rows];
        let chained: Vec<bool> = spots
            .iter()
            .map(|spot| {
                let Some((row, _, left, right, height)) = *spot else { return false };
                let before = last_right[row].replace(right);
                before.is_some_and(|before| follows(left, before, height).is_some())
            })
            .collect();
        // On each row, the cells the last line placed took, and where it
        // ended in the page.
        let mut taken: Vec<Option<(usize, usize, f64)>> = vec![None; rows];
        for (i, line) in text.lines.iter().enumerate() {
            let Some((row, own, left, right, height)) = spots[i] else { continue };
            let start = match taken[row] {
                Some((start, end, last_right)) => match follows(left, last_right, height) {
                    Some(space) => end + space as usize,
                    None if own >= start && own < end => end,
                    None => own,
                },
                None => own,
            };
            let stop = spots
                .iter()
                .zip(&chained)
                .filter_map(|(spot, chained)| spot.filter(|_| !chained))
                .filter(|(r, o, _, _, _)| *r == row && *o > own)
                // A cell short of it: they don't touch in the page.
                .map(|(_, o, _, _, _)| o - 1)
                .min()
                .unwrap_or(cols)
                .clamp(start, cols);
            let mut at = start;
            for ch in line.text.chars().filter(|ch| fits_a_cell(*ch)).take(stop - start) {
                let cell = &mut grid[row * cols + at];
                if !cell.is_some_and(|(letter, _)| letter != ' ') {
                    *cell = Some((if ch == '\u{a0}' { ' ' } else { ch }, line.colour));
                }
                at += 1;
            }
            taken[row] = Some((start, at, right));
        }
        grid
    }

    /// Draw again the page's cells within `rect`, once something drawn over
    /// them (a box, a menu) is gone.
    pub fn repaint(&self, out: &mut Vec<u8>, rect: crate::ui::Rect) {
        if self.shown.is_empty() {
            return;
        }
        let mut cells = Vec::new();
        for row in rect.row..rect.row + rect.height {
            let Some(row) = row.checked_sub(self.top).filter(|row| *row < self.rows) else { continue };
            for col in rect.col.saturating_sub(1)..(rect.col - 1 + rect.width).min(self.cols) {
                cells.push(row as usize * self.cols as usize + col as usize);
            }
        }
        self.emit(out, &cells);
    }

    /// Append the escapes for the cells at `at`, indices in `shown` in
    /// order, moving the cursor only between runs and setting only the
    /// colours that change.
    fn emit(&self, out: &mut Vec<u8>, at: &[usize]) {
        if at.is_empty() {
            return;
        }
        let mut s = String::from("\x1b[0m");
        let (mut fg, mut bg) = (None, None);
        let mut next = None;
        let width = self.cols as usize;
        for &i in at {
            if next != Some(i) {
                let _ = write!(s, "\x1b[{};{}H", self.top as usize + i / width, i % width + 1);
            }
            let cell = self.shown[i];
            if bg != Some(cell.bg) {
                s.push_str(&self.sgr(48, cell.bg));
                bg = Some(cell.bg);
            }
            if cell.ch != ' ' && fg != Some(cell.fg) {
                s.push_str(&self.sgr(38, cell.fg));
                fg = Some(cell.fg);
            }
            s.push(cell.ch);
            // The cursor stays at the last column after writing it, rather
            // than wrapping, so the next row always moves it.
            next = (i % width + 1 < width).then_some(i + 1);
        }
        s.push_str("\x1b[0m");
        out.extend_from_slice(s.as_bytes());
    }

    fn sgr(&self, layer: u8, [r, g, b]: [u8; 3]) -> String {
        match self.truecolor {
            true => format!("\x1b[{layer};2;{r};{g};{b}m"),
            false => format!("\x1b[{layer};5;{}m", nearest_256([r, g, b])),
        }
    }

    /// Without truecolor, each colour becomes the one it will show as, so
    /// that cells that look the same compare the same and aren't sent again.
    fn palette(&self, cell: Cell) -> Cell {
        match self.truecolor {
            true => cell,
            false => Cell { ch: cell.ch, fg: rgb_256(nearest_256(cell.fg)), bg: rgb_256(nearest_256(cell.bg)) },
        }
    }

    /// The character and colours closest to the pixels in `area`.
    fn fit(&mut self, pixels: &Pixels, area: [u32; 4]) -> Cell {
        let [x0, y0, x1, y1] = area;
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        if w == 0 || h == 0 {
            return Cell::flat([0, 0, 0]);
        }
        // sums[y][x] is the total of the pixels above and left of (x, y).
        let stride = w + 1;
        self.sums.clear();
        self.sums.resize(stride * (h + 1), [0; 3]);
        let step = if pixels.alpha { 4 } else { 3 };
        for y in 0..h {
            let start = ((y0 as usize + y) * pixels.width as usize + x0 as usize) * step;
            let mut run = [0u32; 3];
            for x in 0..w {
                let p = &pixels.data[start + x * step..start + x * step + 3];
                for c in 0..3 {
                    run[c] += p[c] as u32;
                }
                let above = self.sums[y * stride + x + 1];
                self.sums[(y + 1) * stride + x + 1] = [above[0] + run[0], above[1] + run[1], above[2] + run[2]];
            }
        }
        let sum = |x0: usize, y0: usize, x1: usize, y1: usize| {
            let [a, b, c, d] = [
                self.sums[y1 * stride + x1],
                self.sums[y0 * stride + x1],
                self.sums[y1 * stride + x0],
                self.sums[y0 * stride + x0],
            ];
            [0, 1, 2].map(|i| (a[i] + d[i] - b[i] - c[i]) as f64)
        };
        let total = sum(0, 0, w, h);
        let n = (w * h) as f64;
        let mean = |s: [f64; 3], n: f64| s.map(|v| (v / n).round().clamp(0.0, 255.0) as u8);
        let energy = |s: [f64; 3], n: f64| (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]) / n;
        // How far the pixels spread about one flat colour is the sum of
        // their squares less `flat`; each split takes `gain` off that.
        let flat = energy(total, n);
        let mut best: Option<(f64, &Glyph, [f64; 3], f64)> = None;
        for glyph in &self.glyphs {
            let (mut inside, mut count) = ([0.0; 3], 0.0);
            for &[gx0, gy0, gx1, gy1] in &glyph.rects {
                let px = |v: u8| (v as usize * w + 4) / 8;
                let py = |v: u8| (v as usize * h + 12) / 24;
                let (rx0, ry0, rx1, ry1) = (px(gx0), py(gy0), px(gx1), py(gy1));
                let part = sum(rx0, ry0, rx1, ry1);
                for c in 0..3 {
                    inside[c] += part[c];
                }
                count += ((rx1 - rx0) * (ry1 - ry0)) as f64;
            }
            if count == 0.0 || count == n {
                continue;
            }
            let outside = [0, 1, 2].map(|c| total[c] - inside[c]);
            let gain = energy(inside, count) + energy(outside, n - count) - flat;
            if best.as_ref().is_none_or(|(g, ..)| gain > *g) {
                best = Some((gain, glyph, inside, count));
            }
        }
        match best {
            Some((gain, glyph, inside, count)) if gain > WORTH * n => {
                let outside = [0, 1, 2].map(|c| total[c] - inside[c]);
                Cell { ch: glyph.ch, fg: mean(inside, count), bg: mean(outside, n - count) }
            }
            _ => Cell::flat(mean(total, n)),
        }
    }
}

/// How many of a frame's `size` pixels make a cell, of `cells` across it.
/// Chrome's frames can come a pixel or two larger than the cells (see
/// `graphics::fits`), and those extra pixels are left off the end rather
/// than shared out, which would shift the cells off the pixels further
/// down the page. A frame that doesn't line up with the cells (a fixed
/// viewport, one from before a resize) is shared out among them.
fn cell_size(size: u32, cells: u16) -> f64 {
    let whole = size / cells as u32;
    match size - whole * cells as u32 {
        0..=2 => whole as f64,
        _ => size as f64 / cells as f64,
    }
}

/// The pixels of the frame under cell (`col`, `row`), with cells of `size`
/// frame pixels: left, top, right, bottom.
fn cell_area(pixels: &Pixels, col: u16, row: u16, size: (f64, f64)) -> [u32; 4] {
    let x = |c: u16| ((c as f64 * size.0).round() as u32).min(pixels.width);
    let y = |r: u16| ((r as f64 * size.1).round() as u32).min(pixels.height);
    [x(col), y(row), x(col + 1), y(row + 1)]
}

/// Whether two frames of one size have the same pixels in `area`.
fn same_pixels(a: &Pixels, b: &Pixels, [x0, y0, x1, y1]: [u32; 4]) -> bool {
    if a.alpha != b.alpha {
        return false;
    }
    let step = if a.alpha { 4 } else { 3 };
    (y0..y1).all(|y| {
        let start = (y as usize * a.width as usize + x0 as usize) * step;
        let end = start + (x1 - x0) as usize * step;
        a.data[start..end] == b.data[start..end]
    })
}

/// The 2 by 3 blocks, U+1FB00 on. Bit k of a pattern is the k-th sixth,
/// left to right, then top to bottom. Patterns with the bottom right sixth
/// are left out as the complements of those without; so are the empty and
/// full ones, and the left half, which `▌` draws.
fn sextant_glyphs() -> impl Iterator<Item = Glyph> {
    (1u32..32).filter(|&v| v != 21).map(|v| {
        let rects = (0..6)
            .filter(|k| v & (1 << k) != 0)
            .map(|k| {
                let (col, row) = ((k % 2) as u8, (k / 2) as u8);
                [col * 4, row * 8, col * 4 + 4, row * 8 + 8]
            })
            .collect();
        Glyph { ch: sextant(v), rects }
    })
}

/// The character for sextant pattern `v`. Unicode skips the two halves,
/// 21 and 42, which were already `▌` and `▐`.
fn sextant(v: u32) -> char {
    let skipped = (v > 21) as u32 + (v > 42) as u32;
    char::from_u32(0x1FB00 + v - 1 - skipped).unwrap_or(' ')
}

/// The steps of each channel in the 6x6x6 cube of the 256 colours.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The nearest of the 256 colours that aren't the terminal's own sixteen:
/// in the colour cube, or on the grey ramp.
fn nearest_256(rgb: [u8; 3]) -> u8 {
    let step = |v: u8| CUBE.iter().enumerate().min_by_key(|(_, s)| (**s as i32 - v as i32).abs()).map_or(0, |(i, _)| i);
    let [r, g, b] = rgb.map(step);
    let cube = 16 + 36 * r + 6 * g + b;
    let average = (rgb[0] as u32 + rgb[1] as u32 + rgb[2] as u32) / 3;
    let grey = 232 + ((average.saturating_sub(3)) / 10).min(23) as usize;
    let distance = |c: [u8; 3]| (0..3).map(|i| (c[i] as i32 - rgb[i] as i32).pow(2)).sum::<i32>();
    match distance(rgb_256(grey as u8)) < distance(rgb_256(cube as u8)) {
        true => grey as u8,
        false => cube as u8,
    }
}

/// The colour that one of the 256, from 16 on, shows as.
fn rgb_256(n: u8) -> [u8; 3] {
    match n {
        232.. => [8 + 10 * (n - 232); 3],
        16.. => {
            let i = n - 16;
            [CUBE[(i / 36) as usize], CUBE[(i / 6 % 6) as usize], CUBE[(i % 6) as usize]]
        }
        _ => [0; 3],
    }
}

/// The rectangles of an ASCII shape drawn on its 4 by 6 grid, in the
/// 8 by 24 units of the blocks.
fn letter_rects(rows: &[&str; 6]) -> Vec<[u8; 4]> {
    let mut rects = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        for (x, _) in row.bytes().enumerate().filter(|(_, b)| *b == b'#') {
            let (x, y) = (x as u8 * 2, y as u8 * 4);
            rects.push([x, y, x + 2, y + 4]);
        }
    }
    rects
}

/// Whether `ch` takes exactly one cell, as the page's letters must to go
/// where it laid them: not a control character (page text sent to the
/// terminal as one could drive it), not one of no width that joins or
/// reorders others, and not a wide one (CJK, most emoji), which takes two.
/// Nor one of the scripts written right to left: a terminal would print
/// them left to right a letter at a time, without joining them, and they
/// are better left to the pixels.
fn fits_a_cell(ch: char) -> bool {
    let c = ch as u32;
    let zero =
        matches!(c, 0x300..=0x36F | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFE00..=0xFE0F | 0xFEFF);
    let wide = matches!(
        c,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..
    );
    let right_to_left = matches!(c, 0x590..=0x8FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFC);
    !ch.is_control() && !zero && !wide && !right_to_left
}

fn distance(a: [u8; 3], b: [u8; 3]) -> i32 {
    (0..3).map(|i| (a[i] as i32 - b[i] as i32).pow(2)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [u8; 3] = [255; 3];
    const BLACK: [u8; 3] = [0; 3];

    /// A frame `cols` cells of 8x24 across and `rows` down, coloured by
    /// `paint` from its pixel coordinates.
    fn frame(cols: u32, rows: u32, paint: impl Fn(u32, u32) -> [u8; 3]) -> Pixels {
        let (width, height) = (cols * 8, rows * 24);
        let paint = &paint;
        let data = (0..height).flat_map(|y| (0..width).flat_map(move |x| paint(x, y))).collect();
        Pixels { width, height, alpha: false, data }
    }

    fn page() -> AsciiPage {
        let mut page = AsciiPage::new(Style::Blocks { sextants: true });
        page.truecolor = true;
        page
    }

    fn fit(paint: impl Fn(u32, u32) -> [u8; 3]) -> Cell {
        let pixels = frame(1, 1, paint);
        page().fit(&pixels, [0, 0, 8, 24])
    }

    #[test]
    fn picks_the_shape_and_colours_under_a_cell() {
        let top = fit(|_, y| if y < 12 { WHITE } else { BLACK });
        assert_eq!(top, Cell { ch: '▀', fg: WHITE, bg: BLACK });
        // The lower half is the upper half with the colours swapped.
        let bottom = fit(|_, y| if y < 12 { [10, 20, 30] } else { WHITE });
        assert_eq!(bottom, Cell { ch: '▀', fg: [10, 20, 30], bg: WHITE });
        assert_eq!(fit(|x, _| if x < 3 { WHITE } else { BLACK }).ch, '▍');
        assert_eq!(fit(|_, y| if y >= 21 { WHITE } else { BLACK }).ch, '▁');
        assert_eq!(fit(|x, y| if (x < 4) == (y < 12) { WHITE } else { BLACK }).ch, '▚');
        // The top right and middle left sixths: sextant 2 + 4.
        assert_eq!(
            fit(|x, y| if (x >= 4 && y < 8) || (x < 4 && (8..16).contains(&y)) { WHITE } else { BLACK }).ch,
            sextant(6)
        );
    }

    #[test]
    fn a_flat_or_barely_varied_cell_is_a_space() {
        assert_eq!(fit(|_, _| [40, 50, 60]), Cell::flat([40, 50, 60]));
        let noise = fit(|x, y| if (x + y) % 2 == 0 { [100; 3] } else { [102; 3] });
        assert_eq!(noise.ch, ' ');
    }

    #[test]
    fn sextants_are_numbered_as_unicode_numbers_them() {
        assert_eq!(sextant(1), '\u{1FB00}');
        assert_eq!(sextant(20), '\u{1FB13}');
        assert_eq!(sextant(22), '\u{1FB14}');
        assert_eq!(sextant(62), '\u{1FB3B}');
        assert_eq!(sextant_glyphs().count(), 30);
    }

    #[test]
    fn colours_come_to_the_nearest_of_256() {
        assert_eq!(nearest_256([255, 0, 0]), 196);
        assert_eq!(nearest_256([0, 0, 0]), 16);
        assert_eq!(nearest_256([128, 128, 128]), 244);
        assert_eq!(rgb_256(nearest_256([95, 135, 175])), [95, 135, 175]);
    }

    #[test]
    fn only_cells_that_changed_are_sent() {
        let mut page = page();
        let mut out = Vec::new();
        let first = frame(3, 2, |x, _| if x < 8 { WHITE } else { BLACK });
        assert_eq!(page.draw_pixels(&mut out, first, 3, 2, 2, (0.0, 0.0)), "6 of 6 cells in characters");
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[0m\x1b[2;1H\x1b[48;2;255;255;255m \x1b[48;2;0;0;0m  \x1b[3;1H"), "{text:?}");

        let mut out = Vec::new();
        let same = frame(3, 2, |x, _| if x < 8 { WHITE } else { BLACK });
        assert_eq!(page.draw_pixels(&mut out, same, 3, 2, 2, (0.0, 0.0)), "0 of 6 cells in characters");
        assert!(out.is_empty());

        let mut out = Vec::new();
        let one = frame(3, 2, |x, y| if x < 8 || (x >= 16 && y >= 24) { WHITE } else { BLACK });
        assert_eq!(page.draw_pixels(&mut out, one, 3, 2, 2, (0.0, 0.0)), "1 of 6 cells in characters");
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[0m\x1b[3;3H\x1b[48;2;255;255;255m \x1b[0m");
    }

    #[test]
    fn a_box_gone_shows_the_page_under_it_again() {
        let mut page = page();
        page.draw_pixels(&mut Vec::new(), frame(4, 3, |_, _| [9, 9, 9]), 4, 3, 2, (0.0, 0.0));
        let mut out = Vec::new();
        // Rows 1 to 3, columns 2 and 3: row 1 is the bar's, not the page's.
        page.repaint(&mut out, crate::ui::Rect { col: 2, row: 1, width: 2, height: 3 });
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "\x1b[0m\x1b[2;2H\x1b[48;2;9;9;9m  \x1b[3;2H  \x1b[0m");
    }

    #[test]
    fn extra_pixels_at_a_frames_edge_are_left_off() {
        assert_eq!(cell_size(1282, 80), 16.0);
        assert_eq!(cell_size(1280, 80), 16.0);
        assert_eq!(cell_size(1300, 80), 16.25);
    }

    #[test]
    fn a_frame_of_another_size_is_shared_among_the_cells() {
        let mut page = page();
        let mut out = Vec::new();
        // Twice the size: each cell takes 2x2 of its pixels.
        let pixels = frame(4, 2, |x, _| if x < 16 { WHITE } else { BLACK });
        page.draw_pixels(&mut out, pixels, 2, 1, 2, (0.0, 0.0));
        assert_eq!(page.shown, vec![Cell::flat(WHITE), Cell::flat(BLACK)]);
    }

    #[test]
    fn ascii_shapes_fit_what_they_look_like() {
        let mut page = AsciiPage::new(Style::Letters);
        page.truecolor = true;
        let mut fit = |paint: &dyn Fn(u32, u32) -> bool| {
            let pixels = frame(1, 1, |x, y| if paint(x, y) { WHITE } else { BLACK });
            page.fit(&pixels, [0, 0, 8, 24]).ch
        };
        assert_eq!(fit(&|_, y| y >= 20), '_');
        assert_eq!(fit(&|x, _| (2..6).contains(&x)), '|');
        assert_eq!(fit(&|x, y| x / 2 == [3, 2, 2, 1, 1, 0][y as usize / 4]), '/');
        assert_eq!(fit(&|_, y| (8..12).contains(&y) || (16..20).contains(&y)), '=');
    }

    /// Text on a 40 by 20 viewport, cells of 10 by 10 at 4 by 2 cells:
    /// "Hi" at the top left, and "ab" in red in a fixed box.
    fn text() -> Text {
        let json = r#"{"w":40,"h":20,"colours":[[0,0,0],[255,0,0]],
            "lines":[[0,0,"Hi",0,0,20,10],[1,1,"ab",20,10,40,20]]}"#;
        Text::parse(json).unwrap()
    }

    fn letters(grid: &[Letter], cols: usize) -> Vec<String> {
        grid.chunks(cols).map(|row| row.iter().map(|l| l.map_or('.', |(ch, _)| ch)).collect()).collect()
    }

    #[test]
    fn reads_the_pages_text() {
        let text = text();
        assert_eq!((text.width, text.height, text.lines.len()), (40.0, 20.0, 2));
        let red = Line { colour: [255, 0, 0], fixed: true, text: "ab".into(), rect: [20.0, 10.0, 40.0, 20.0] };
        assert_eq!(text.lines[1], red);
        assert_eq!(Text::parse("not json"), None);
        assert_eq!(Text::parse(r#"{"w":40,"h":20,"complex":true}"#), None);
    }

    #[test]
    fn lines_go_where_the_page_put_them_and_follow_its_scroll() {
        let mut page = AsciiPage::new(Style::Letters);
        page.text = Some(text());
        assert_eq!(letters(&page.place(4, 2, (0.0, 0.0), (10.0, 10.0)), 4), ["Hi..", "..ab"]);
        // Scrolled down 10 pixels: the page's text moves up out of view, and
        // the fixed box's stays put.
        assert_eq!(letters(&page.place(4, 2, (0.0, 10.0), (10.0, 10.0)), 4), ["....", "..ab"]);
    }

    #[test]
    fn lines_scroll_together_a_whole_row_at_a_time() {
        let mut page = AsciiPage::new(Style::Letters);
        // Two lines, one near the bottom of its row, one near the top.
        let json = r#"{"w":10,"h":30,"colours":[[0,0,0]],"lines":[[0,0,"a",0,8,10,10],[0,0,"b",0,20,10,22]]}"#;
        page.text = Text::parse(json);
        let rows = |scroll: f64| letters(&page.place(1, 3, (0.0, scroll), (10.0, 10.0)), 1).concat();
        assert_eq!(rows(0.0), "a.b");
        // Scrolled 3 pixels, "b" would have gone up a row and "a" not: both
        // stay. Scrolled 6, both go up.
        assert_eq!(rows(3.0), "a.b");
        assert_eq!(rows(6.0), ".b.");
    }

    #[test]
    fn text_split_in_nodes_runs_on_in_small_cells() {
        let mut page = AsciiPage::new(Style::Letters);
        // "(c." then "150", 2 pixels on, with a space of its own between:
        // more than half a cell of 4 pixels, but a space in type 18 high.
        let json = r#"{"w":40,"h":20,"colours":[[0,0,0]],"lines":[
            [0,0,"(",0,0,6,18],[0,0,"c.",6,0,18,18],[0,0,"150",20,0,40,18]]}"#;
        page.text = Text::parse(json);
        // The space is a cell left empty: it shows the page behind.
        assert_eq!(letters(&page.place(10, 1, (0.0, 0.0), (4.0, 20.0)), 10), ["(c..150..."]);
    }

    #[test]
    fn a_run_of_links_and_separators_stops_only_at_other_text() {
        let mut page = AsciiPage::new(Style::Letters);
        // "ask | show | jobs" in type narrower than the cells, then "login"
        // further along: each piece goes on from the last, and the run stops
        // a cell short of where "login" starts.
        let json = r#"{"w":80,"h":10,"colours":[[0,0,0]],"lines":[
            [0,0,"ask",0,0,9,10],[0,0," | ",9,0,15,10],[0,0,"show",15,0,27,10],
            [0,0," | ",27,0,33,10],[0,0,"jobs",33,0,45,10],[0,0,"login",60,0,75,10]]}"#;
        page.text = Text::parse(json);
        let grid = page.place(20, 1, (0.0, 0.0), (4.0, 10.0));
        assert_eq!(letters(&grid, 20), ["ask | show | j.login"]);
    }

    #[test]
    fn a_line_keeps_its_letters_and_its_breaks() {
        let mut page = AsciiPage::new(Style::Letters);
        // Large type: "Big " fills 6 cells over two rows, and a link follows
        // it straight on. Small type: "small text" in 3 cells, and on the
        // next row "long words" with "end" further along.
        let json = r#"{"w":80,"h":40,"colours":[[0,0,0]],"lines":[
            [0,0,"Big ",0,0,60,20],[0,0,"link",60,0,80,20],
            [0,0,"small text",0,22,30,32],
            [0,0,"long words",0,32,30,40],[0,0,"end",50,32,80,40]]}"#;
        page.text = Text::parse(json);
        let grid = page.place(8, 4, (0.0, 0.0), (10.0, 10.0));
        // The big line on the row through its middle, the link after it; a
        // small line runs on to the pane's edge, or to the next text.
        assert_eq!(letters(&grid, 8), ["........", "Big link", "small te", "long.end"]);
    }

    #[test]
    fn a_letter_is_drawn_in_its_colour_on_the_page_behind_it() {
        let mut page = AsciiPage::new(Style::Letters);
        page.truecolor = true;
        let json = r#"{"w":8,"h":24,"colours":[[200,0,0]],"lines":[[0,0,"A",0,0,8,24]]}"#;
        page.text = Text::parse(json);
        // Dark red strokes on white, in the cell's middle.
        let pixels = frame(1, 1, |x, y| if (3..5).contains(&x) && (4..20).contains(&y) { [150, 0, 0] } else { WHITE });
        page.draw_pixels(&mut Vec::new(), pixels, 1, 1, 2, (0.0, 0.0));
        assert_eq!(page.shown, vec![Cell { ch: 'A', fg: [200, 0, 0], bg: WHITE }]);
    }

    #[test]
    fn only_letters_of_one_cell_are_drawn_as_letters() {
        assert!(fits_a_cell('a') && fits_a_cell('é') && fits_a_cell('—'));
        assert!(!fits_a_cell('\x1b') && !fits_a_cell('\u{9b}') && !fits_a_cell('\u{202e}'));
        assert!(!fits_a_cell('漢') && !fits_a_cell('😀') && !fits_a_cell('\u{301}'));
        assert!(!fits_a_cell('ذ') && !fits_a_cell('ש'));
    }
}
