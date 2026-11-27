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

/// Whether a computed `font-family` is an icon font's, by the first face
/// in it: a text font may name a symbols font after it, and the system's
/// symbol fonts are text.
macro_rules! icon_face {
    () => {
        r#"(family => { const first = family.split(',')[0].replace(/["']/g, '').trim();
  return /icon|symbol|awesome|glyph|fontello|icomoon/i.test(first) && !/^(noto sans symbols|apple symbols|segoe ui symbol)/i.test(first); })"#
    };
}

/// Text an icon font draws as an icon: a name in lower case and
/// underscores (a ligature's, `font_download`) or letters from the private
/// use area, which only that font has.
macro_rules! icon_text {
    () => {
        r#"/^\s*([a-z0-9_]{2,40}|[\ue000-\uf8ff]+)\s*$/"#
    };
}

/// The elements that draw icons with a font, by the classes of the common
/// icon fonts, kept in their own face and left to the pixels. Only these
/// classes: one like `icon-home` is often on a link with words in it,
/// whose icon is drawn before them; such icons are found by their face.
macro_rules! icons {
    () => {
        r#"[class*="material-icons"], [class*="material-symbols"], [class*="google-symbols"], mat-icon, .fa, .fas, .far, .fal, .fad, .fab, .fa-solid, .fa-regular, .fa-brands, .glyphicon, .bi, .codicon, .octicon"#
    };
}

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
/// noise. What a page draws before and after an element (`::before`, an
/// icon or an arrow) isn't text to read, so it is left to the pixels. Nor
/// is the selection's highlight painted: the selected letters are drawn
/// the other way round instead, and the highlight around them would come
/// out as blocks. The text keeps its colour, to be read, and is only not
/// painted; fields keep theirs, as their values aren't read. `SHOW_TEXT`
/// undoes it.
/// A constructed sheet, not a `<style>`, so a page's content security
/// policy doesn't refuse it.
pub const READ_TEXT: &str = concat!(
    r#"(() => {
  const ICONS = `"#,
    icons!(),
    r#"`;
  const ICON_FACE = "#,
    icon_face!(),
    r#";
  const ICON_TEXT = "#,
    icon_text!(),
    r#";
  const BUDGET = 40, began = performance.now(), W = innerWidth, H = innerHeight;
  if (window.__kbTooMuch && began - window.__kbTooMuch < 5000) return JSON.stringify({ w: W, h: H, complex: true });
  if (!window.__kbHideText) {
    window.__kbHideText = new CSSStyleSheet();
    window.__kbHideText.replaceSync(`*:not(input, textarea, select) {
      -webkit-text-fill-color: transparent !important;
      text-decoration-color: transparent !important;
      text-shadow: none !important; }
    :is(${ICONS}), *::before, *::after { -webkit-text-fill-color: currentcolor !important; }
    *:not(input, textarea)::selection { background-color: transparent !important; }`);
    window.__kbHidden = new Set();
  }
  const roots = [document], colours = [], index = new Map(), lines = [];
  // The page's selection, to show which letters are in it.
  const selection = getSelection();
  const chosen = selection && !selection.isCollapsed
    ? Array.from({ length: selection.rangeCount }, (_, k) => selection.getRangeAt(k)) : [];
  const range = document.createRange(), fixedness = new Map();
  const parent = el => el.parentElement || (el.parentNode && el.parentNode.host) || null;
  // Whether text stays put in the window as the page scrolls: in a fixed
  // box, or a sticky one that is stuck to the window, which is where its
  // own top (or bottom) says; a sticky box not stuck yet scrolls with the
  // page. One in a box that scrolls by itself sticks to that box, which
  // the page's scroll doesn't move, so it is the page's.
  const stuck = (el, style) => {
    for (let up = parent(el); up && up !== document.body && up !== document.documentElement; up = parent(up)) {
      if (up.nodeType === 1 && getComputedStyle(up).overflowY !== 'visible') return false;
    }
    const r = el.getBoundingClientRect(), top = parseFloat(style.top), bottom = parseFloat(style.bottom);
    return (!isNaN(top) && Math.abs(r.top - top) < 1) || (!isNaN(bottom) && Math.abs(H - r.bottom - bottom) < 1);
  };
  const fixed = el => {
    if (!el) return false;
    if (!fixedness.has(el)) {
      const style = getComputedStyle(el);
      fixedness.set(el, style.position === 'fixed' || (style.position === 'sticky' && stuck(el, style)) || fixed(parent(el)));
    }
    return fixedness.get(el);
  };
  const onScreen = r => r.width > 0 && r.bottom > 0 && r.top < H && r.right > 0 && r.left < W;
  // Where an element's content shows: the window, cut by each box around
  // it that clips what overflows it (a box that scrolls by itself); text
  // outside, scrolled out of such a box, isn't shown.
  const clips = new Map();
  const clipOf = el => {
    if (!el || el === document.body || el === document.documentElement) return [0, 0, W, H];
    if (!clips.has(el)) {
      const style = getComputedStyle(el);
      // A fixed box escapes every box around it, an absolute one those up to
      // the one it is placed in (a dropdown hanging out of its bar).
      let holder = parent(el);
      if (style.position === 'absolute') {
        while (holder && holder !== document.body && holder !== document.documentElement && holder.nodeType === 1
          && getComputedStyle(holder).position === 'static') holder = parent(holder);
      }
      let c = style.position === 'fixed' ? [0, 0, W, H] : clipOf(holder && holder.nodeType === 1 ? holder : null);
      if (style.overflowX !== 'visible' || style.overflowY !== 'visible') {
        const r = el.getBoundingClientRect();
        c = [Math.max(c[0], r.left), Math.max(c[1], r.top), Math.min(c[2], r.right), Math.min(c[3], r.bottom)];
      }
      clips.set(el, c);
    }
    return clips.get(el);
  };
  const clipped = (el, x, y) => {
    const [l, t, r, b] = clipOf(el);
    return x < l || x >= r || y < t || y >= b;
  };
  // The boxes floating over the page (a cookie banner, a sticky header),
  // found from the text in them: the painted box around such text, where
  // it is fixed or sticky. Text of the page under one isn't shown.
  const floating = new Map(), covers = [];
  const floats = el => {
    if (!el) return false;
    if (!floating.has(el)) floating.set(el, /^(fixed|sticky)$/.test(getComputedStyle(el).position) || floats(parent(el)));
    return floating.get(el);
  };
  const cover = el => {
    for (let box = el; box && floats(box); box = parent(box)) {
      if (!painted(box)) continue;
      // A dialog kept hidden until it is opened covers nothing.
      if (box.checkVisibility && !box.checkVisibility({ opacityProperty: true, visibilityProperty: true })) return;
      if (!covers.some(c => c.box === box)) {
        const r = box.getBoundingClientRect();
        if (onScreen(r)) covers.push({ box, l: r.left, t: r.top, r: r.right, b: r.bottom });
      }
      return;
    }
  };
  // Only the page's own text: which of two floating boxes is on top
  // isn't known, nor need be, as the hit test tells.
  const covered = (el, x, y) => !floats(el) && covers.some(c => x >= c.l && x < c.r && y >= c.t && y < c.b);
  const painted = el => {
    if (/^(img|video|canvas|iframe|svg|picture|object|embed)$/.test(el.localName)) return true;
    const style = getComputedStyle(el);
    const alpha = (style.backgroundColor.match(/[\d.]+/g) || []).map(Number)[3];
    return style.backgroundImage !== 'none' || alpha === undefined || alpha > 0;
  };
  // Whether the text in `el`, at x, y, is the first thing painted there.
  const seen = (el, node, x, y) => {
    // The topmost element alone answers most of the time, for a third of
    // the cost of all of them.
    const top = node.getRootNode().elementFromPoint(x, y);
    if (top && (top === el || el.contains(top) || top.contains(el))) return true;
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
    // An icon font's letters, which only it draws: shown by the pixels,
    // its fill put back to be seen (and what the page had there kept, for
    // SHOW_TEXT to put back). Words beside an icon are read as words.
    if (el.closest(ICONS) && ICON_TEXT.test(node.data)) return;
    if (ICON_FACE(style.fontFamily)) {
      const filled = window.__kbFilled || (window.__kbFilled = new Map());
      if (!filled.has(el)) filled.set(el, [el.style.getPropertyValue('-webkit-text-fill-color'), el.style.getPropertyPriority('-webkit-text-fill-color')]);
      el.style.setProperty('-webkit-text-fill-color', 'currentcolor', 'important');
      return;
    }
    const rgba = (style.color.match(/[\d.]+/g) || []).map(Number);
    if (style.visibility !== 'visible' || rgba.length < 3 || (rgba.length > 3 && rgba[3] < 0.5)) return;
    // Text kept for screen readers alone: squeezed into a pixel and
    // clipped, or see-through.
    const box = el.getBoundingClientRect();
    if ((box.width < 3 && box.height < 3) || /rect\(0/.test(style.clip) || /inset\(50%/.test(style.clipPath)) return;
    if (el.checkVisibility && !el.checkVisibility({ opacityProperty: true })) return;
    const key = rgba.slice(0, 3).join();
    const f = fixed(el), dx = f ? 0 : scrollX, dy = f ? 0 : scrollY, transform = style.textTransform;
    // Where the page collapses runs of white space, each run shows as one
    // space, or none at the end of a line: a newline in the source between
    // two words is a space between them, not a control character.
    const collapse = !/^(pre|pre-wrap|break-spaces)$/.test(style.whiteSpace);
    const data = node.data, n = data.length;
    const inSelection = chosen.filter(r => r.intersectsNode(node)).map(r => [
      r.startContainer === node ? r.startOffset : 0,
      r.endContainer === node ? r.endOffset : n,
    ]);
    const box1 = (from, to) => { range.setStart(node, from); range.setEnd(node, to); return range.getBoundingClientRect(); };
    // One letter's box. The first letter after a break at a soft hyphen has
    // two, the end of the line before and the letter where it is, and
    // their bounds would put it on the line before: the last with a width
    // is the letter.
    const glyph = at => {
      range.setStart(node, at);
      range.setEnd(node, at + width(at));
      const boxes = range.getClientRects();
      if (boxes.length < 2) return boxes[0] || range.getBoundingClientRect();
      for (let k = boxes.length - 1; k >= 0; k--) if (boxes[k].width > 0) return boxes[k];
      return boxes[boxes.length - 1];
    };
    const width = at => (data.codePointAt(at) > 0xffff ? 2 : 1);
    // The letter at `at`, or the first after it with a box: collapsed
    // white space has none, however long the run (an indented source
    // line), so such a run is passed over at once rather than a letter at
    // a time. A space that shows (between a link and the words after it)
    // is a letter like any other.
    const shown = /\S/g;
    const letter = at => {
      for (let k = 0; at < n && k < 64; k++) {
        const r = glyph(at);
        if (r.width > 0) return [at, r];
        if (collapse && /\s/.test(data[at])) {
          shown.lastIndex = at;
          const next = shown.exec(data);
          if (!next) return null;
          at = next.index;
        } else at += width(at);
      }
      return null;
    };
    const cased = ch => {
      const c = transform === 'uppercase' ? ch.toUpperCase() : transform === 'lowercase' ? ch.toLowerCase() : ch;
      return [...c].length === 1 ? c : ch;
    };
    let checked = false;
    const gone = (x, y) => clipped(el, x, y) || covered(el, x, y);
    const push = line => {
      if (!line.text) return;
      if (!index.has(key)) { index.set(key, colours.length); colours.push(rgba.slice(0, 3)); }
      const entry = [index.get(key), f ? 1 : 0, line.text, line.l + dx, line.t + dy, line.r + dx, line.b + dy];
      if (line.chosen.length) entry.push(line.chosen);
      lines.push(entry);
    };
    // One line's letters from `from` to `to`, a box each, as the page put
    // them: for a line that runs off the side of the window, or that
    // something covers in part (a cookie banner over a code block), when
    // each letter is looked for too.
    const byLetter = (from, to, look) => {
      let line = null;
      for (let i = from; i < to; ) {
        const w = width(i), ch = data.slice(i, i + w), r = glyph(i);
        const picked = inSelection.some(([a, b]) => i >= a && i + w <= b);
        i += w;
        if (!onScreen(r)) continue;
        if (look && !(collapse && /\s/.test(ch)) && gone(r.left + r.width / 2, r.top + r.height / 2)) {
          if (line) push(line);
          line = null;
          continue;
        }
        // Lower down, or back to the left: another line (the next column).
        if (line && (Math.abs(r.top - line.t) > r.height / 2 || r.left < line.r - r.width)) {
          push(line);
          line = null;
        }
        if (!line) line = { text: '', l: r.left, t: r.top, r: r.right, b: r.bottom, chosen: [], n: 0 };
        if (picked) {
          const last = line.chosen[line.chosen.length - 1];
          if (last && last[1] === line.n) last[1] += 1;
          else line.chosen.push([line.n, line.n + 1]);
        }
        line.n += 1;
        // A soft hyphen with a box is where the page broke the line.
        line.text += collapse && /\s/.test(ch) ? ' ' : ch === '\u00ad' ? '-' : cased(ch);
        line.r = Math.max(line.r, r.right);
        line.b = Math.max(line.b, r.bottom);
      }
      if (line) push(line);
    };
    // Line by line: where each ends is found by halving, as its letters
    // share a top and the next line's are lower, so that a line costs a
    // few boxes, not one a letter.
    let at = 0;
    while (at < n) {
      const first = letter(at);
      if (!first) break;
      const [s0, r0] = first;
      const sameLine = i => {
        const found = letter(i);
        return found && found[0] < n && Math.abs(found[1].top - r0.top) < Math.max(r0.height, 1) / 2;
      };
      // The last letter start on this line: lo is on it, hi isn't.
      let lo = s0, hi = n;
      while (hi - lo > 1) {
        let mid = (lo + hi) >> 1;
        if (data.charCodeAt(mid) >= 0xdc00 && data.charCodeAt(mid) <= 0xdfff) mid -= 1;
        if (mid <= lo) break;
        if (sameLine(mid)) lo = mid; else hi = mid;
      }
      const e = lo + width(lo);
      const r = box1(s0, e);
      at = e;
      if (r.top >= H) break;
      if (r.bottom <= 0 || r.width === 0) continue;
      if (!checked) {
        checked = true;
        if (!seen(el, node, r0.left + r0.width / 2, r0.top + r0.height / 2)) break;
      }
      // Taller than a line: the halving found a letter at this line's top
      // further on, in the next column of text set in columns. Letter by
      // letter, which breaks it where the page does.
      if (r.bottom - r.top > r0.height * 1.6 + 1) { byLetter(s0, e, true); continue; }
      if (r.left < 0 || r.right > W) { byLetter(s0, e, true); continue; }
      // Under a floating box, or clipped, at the line's start, middle and
      // end: none of it, all of it, or some, letter by letter.
      const middle = (r.top + r.bottom) / 2;
      const hidden = [r.left + 1, (r.left + r.right) / 2, r.right - 1].map(x => gone(x, middle));
      if (hidden.every(Boolean)) continue;
      if (hidden.some(Boolean)) { byLetter(s0, e, true); continue; }
      const line = { text: '', l: r.left, t: r.top, r: r.right, b: r.bottom, chosen: [], n: 0 };
      for (let i = s0; i < e; ) {
        const w = width(i);
        let ch = data.slice(i, i + w);
        const picked = inSelection.some(([a, b]) => i >= a && i + w <= b);
        i += w;
        if (collapse && /\s/.test(ch)) {
          if (line.text.endsWith(' ')) continue;
          ch = ' ';
        } else if (ch === '\u00ad') {
          // A soft hyphen shows only where the page broke the line at it.
          if (i < e) continue;
          ch = '-';
        } else ch = cased(ch);
        if (picked) {
          const last = line.chosen[line.chosen.length - 1];
          if (last && last[1] === line.n) last[1] += 1;
          else line.chosen.push([line.n, line.n + 1]);
        }
        line.n += 1;
        line.text += ch;
      }
      // A space the page put at the end of the line, where it wrapped, has
      // no width there: it isn't shown.
      if (collapse && line.text.endsWith(' ') && box1(e - 1, e).width === 0) {
        line.text = line.text.slice(0, -1);
        line.n -= 1;
        line.chosen = line.chosen.map(([a, b]) => [a, Math.min(b, line.n)]).filter(([a, b]) => b > a);
      }
      push(line);
    }
  };
  // The text in view first, and the floating boxes it is in, so that the
  // page's text under them is known before it is read.
  const texts = [];
  const walk = root => {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    for (let node; (node = walker.nextNode()); ) {
      if (node.nodeType === Node.TEXT_NODE) {
        if (node.parentElement && /\S/.test(node.data)) {
          range.selectNodeContents(node);
          if (onScreen(range.getBoundingClientRect())) {
            texts.push(node);
            if (floats(node.parentElement)) cover(node.parentElement);
          }
        }
      } else if (node.shadowRoot) { roots.push(node.shadowRoot); walk(node.shadowRoot); }
      if (performance.now() - began > BUDGET) throw 'complex';
    }
  };
  try {
    const root = document.body || document.documentElement;
    if (root) walk(root);
    for (const node of texts) {
      read(node);
      if (performance.now() - began > BUDGET) throw 'complex';
    }
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
  return JSON.stringify({ w: W, h: H, sx: scrollX, sy: scrollY, colours, lines });
})()"#
);

/// Shows the text `READ_TEXT` hid, for the page in pixels again.
pub const SHOW_TEXT: &str = r#"(() => {
  for (const root of window.__kbHidden || []) {
    root.adoptedStyleSheets = root.adoptedStyleSheets.filter(sheet => sheet !== window.__kbHideText);
  }
  for (const [el, [was, priority]] of window.__kbFilled || []) {
    if (el.style.getPropertyValue('-webkit-text-fill-color') !== 'currentcolor') continue;
    if (was) el.style.setProperty('-webkit-text-fill-color', was, priority);
    else el.style.removeProperty('-webkit-text-fill-color');
  }
  delete window.__kbFilled;
  delete window.__kbHideText;
  delete window.__kbHidden;
  delete window.__kbTooMuch;
})()"#;

/// Experimental: sets the page's type in a monospace face, a letter one
/// cell wide and a line one cell high, `cell` being a cell's size in CSS
/// pixels. Laid out so, the page breaks its lines where the terminal
/// would, a letter of the page falls in each cell, and lines of text are a
/// row apart, so scrolling by whole rows keeps them on the page under them.
/// Headings lose their size (a terminal has one), but keep their weight;
/// superscripts and subscripts sit on the line, as raised they would make
/// it taller than a row.
/// The size is measured, not assumed: the monospace face's advance at 100
/// pixels, scaled to the cell. Wide letters (CJK) are set two cells wide,
/// in an installed face scaled to fit. Icon fonts keep their face, so that
/// their icons are drawn, by the pixels. `PLAIN_TYPE` undoes it.
pub fn terminal_type(cell: (f64, f64)) -> String {
    TERMINAL_TYPE.replace("__W__", &cell.0.to_string()).replace("__H__", &cell.1.to_string())
}

/// `terminal_type`'s script, with the cell's width and height to fill in.
///
/// Lines a row high keep the lines of a paragraph a whole number of rows
/// apart, but the space between blocks is the page's own, so that each
/// paragraph would start part way into a row, each at a different part.
/// So blocks' margins and padding are rounded to whole rows too: padding
/// with its border, and the margin to what makes margin, border and
/// padding whole rows together, a pixel less, say, under a heading's
/// underline. Values the page gives in other units than pixels (`auto`, to
/// centre a box) are left alone. A picture or a table isn't lines of
/// text, so its height is made whole rows by its bottom margin; a table
/// row's height, and the space between rows, are rounded too. Each block is rounded once, its own
/// inline values kept to put back: read in one pass and written in
/// another, so the page lays itself out once, not once a block, and in
/// at most `BUDGET` milliseconds a time, the rest the next time the text
/// is read. When the cell changes size, or the page's style sheets change
/// (a page still loading), they are put back and rounded again.
const TERMINAL_TYPE: &str = concat!(
    r#"(() => {
  const ICONS = `"#,
    icons!(),
    r#"`;
  const ICON_FACE = "#,
    icon_face!(),
    r#";
  const ICON_TEXT = "#,
    icon_text!(),
    r#";
  const W = (__W__), H = (__H__), BUDGET = 15, began = performance.now();
  const FACE = 'ui-monospace, Menlo, "DejaVu Sans Mono", monospace';
  const WIDE_FACES = ['Hiragino Sans W3', 'HiraginoSans-W3', 'Hiragino Kaku Gothic ProN W3', 'HiraKakuProN-W3',
    'PingFang SC Regular', 'PingFangSC-Regular', 'Apple SD Gothic Neo Regular', 'AppleSDGothicNeo-Regular',
    'Noto Sans CJK JP Regular', 'NotoSansCJKjp-Regular', 'Noto Sans CJK SC Regular', 'WenQuanYi Micro Hei',
    'Droid Sans Fallback'].map(name => `local("${name}")`).join(', ');
  const WIDE_RANGE = 'U+1100-115F, U+2E80-A4CF, U+AC00-D7A3, U+F900-FAFF, U+FE30-FE4F, U+FF00-FF60, U+FFE0-FFE6, U+20000-3FFFD';
  const ctx = document.createElement('canvas').getContext('2d');
  ctx.font = `100px ${FACE}`;
  const advance = ctx.measureText('MMMMMMMMMM').width / 1000 || 0.6;
  // Wide letters (CJK) in a face of their own, sized to two cells: such
  // faces draw them an em wide, so they are scaled by what the em of one
  // installed here measures, once it has loaded.
  const wide = window.__kbWide || (window.__kbWide = { state: 'new' });
  if (wide.state === 'new') {
    wide.state = 'loading';
    new FontFace('kb-wide-measure', WIDE_FACES, { unicodeRange: WIDE_RANGE }).load().then(face => {
      document.fonts.add(face);
      ctx.font = '100px kb-wide-measure';
      const em = ctx.measureText('国国国国国国国国国国').width / 1000;
      wide.face = new FontFace('kb-wide', WIDE_FACES, { unicodeRange: WIDE_RANGE, sizeAdjust: `${(200 * advance / em).toFixed(2)}%` });
      document.fonts.add(wide.face);
      wide.state = 'ready';
    }, () => { wide.state = 'none'; });
  }
  // Icons keep their face and size, to be seen; a line a row high still.
  const css = `*:not(svg, svg *, math, math *) {
      line-height: ${H.toFixed(3)}px !important;
      letter-spacing: 0 !important;
      word-spacing: 0 !important; }
    *:not(svg, svg *, math, math *, ${ICONS}) {
      font-size: ${(W / advance).toFixed(3)}px !important;
      font-family: "kb-wide", ${FACE} !important;
      font-variant-ligatures: none !important;
      text-rendering: optimizeSpeed !important; }
    sup, sub { vertical-align: baseline !important; }`;
  if (!window.__kbType) window.__kbType = new CSSStyleSheet();
  if (window.__kbTypeCss !== css) { window.__kbType.replaceSync(css); window.__kbTypeCss = css; }
  // The document once (the reader lists it among the roots it hid text in).
  const roots = [...new Set([document, ...(window.__kbHidden || [])])];

  // What kb writes into elements' own styles, and what was there before:
  // put back only where it is still as kb left it, so that what the page
  // has set since (a panel's height, opened) stays.
  window.__kbUnsnap = () => {
    const snap = window.__kbSnap;
    if (!snap) return;
    for (const [el, props] of snap.written) {
      for (const [prop, [was, priority, wrote]] of props) {
        if (el.style.getPropertyValue(prop) !== wrote) continue;
        if (was) el.style.setProperty(prop, was, priority);
        else el.style.removeProperty(prop);
      }
    }
    snap.written.clear();
    snap.done = new WeakSet();
    snap.scanned = new WeakSet();
    snap.boxes.clear();
  };
  const sheets = roots.reduce((n, root) => n + root.styleSheets.length, 0);
  const snap = window.__kbSnap
    || (window.__kbSnap = { h: 0, sheets: 0, done: new WeakSet(), boxes: new Map(), written: new Map(), iconQueue: new Set(), scanned: new WeakSet(), prune: 256 });
  // What a page has taken out (a feed, a chat) isn't held on to: what kb
  // wrote there is dropped once the records have doubled since last time.
  if (snap.written.size > snap.prune) {
    for (const el of snap.written.keys()) if (!el.isConnected) snap.written.delete(el);
    for (const el of snap.iconQueue) if (!el.isConnected) snap.iconQueue.delete(el);
    snap.prune = Math.max(256, snap.written.size * 2);
  }
  // Put back and rounded again for another cell, or for style sheets come
  // or gone (a page loading): once their count has held for three
  // readings, as some pages add and drop a <style> as they run (on hover),
  // which would undo the rounding each time.
  if (snap.sheets !== sheets) snap.held = snap.held && snap.held[0] === sheets ? [sheets, snap.held[1] + 1] : [sheets, 1];
  if (snap.h !== H || (snap.sheets !== sheets && snap.held[1] >= 3)) {
    window.__kbUnsnap();
    snap.h = H;
    snap.sheets = sheets;
    snap.held = null;
    // What was put back is looked at again, icons too, without waiting.
    snap.iconAt = undefined;
  }
  const write = (el, prop, value) => {
    if (!snap.written.has(el)) snap.written.set(el, new Map());
    const props = snap.written.get(el);
    const was = props.get(prop) || [el.style.getPropertyValue(prop), el.style.getPropertyPriority(prop)];
    el.style.setProperty(prop, value, 'important');
    props.set(prop, [was[0], was[1], el.style.getPropertyValue(prop)]);
  };
  const fresh = [];
  for (const root of roots) {
    for (const el of root.querySelectorAll('*')) if (!snap.done.has(el)) fresh.push(el);
  }
  // Where the sheet went is kept apart from the reader's list, which it
  // empties when a page is too much to read, so that it can all be undone.
  const typed = window.__kbTyped || (window.__kbTyped = new Set());
  for (const root of roots) {
    typed.add(root);
    if (!root.adoptedStyleSheets.includes(window.__kbType)) root.adoptedStyleSheets = [...root.adoptedStyleSheets, window.__kbType];
  }

  // Icon fonts a class doesn't name, found by their face: the page's own,
  // read with kb's sheet off for a moment, and kept, with their size and
  // ligatures, so that their icons are drawn. Only the first face in the
  // list counts: a text font may name a symbols font after it. Turning the
  // sheet off restyles the whole page, so only elements whose text could
  // be an icon's are looked at (a name in lower case and underscores, as a
  // ligature's, or letters from the private use area), queued, and at most
  // every ICON_GAP.
  const ICON_GAP = 2000;
  // Text of its own, or, for an empty element, what its ::before or
  // ::after puts there (an icon class like `icon-home` draws so).
  const iconish = el => [...el.childNodes].some(n => n.nodeType === 3 && ICON_TEXT.test(n.data))
    || (!el.firstElementChild && !/\S/.test(el.textContent)
      && ['::before', '::after'].some(at => /[\ue000-\uf8ff]/.test(getComputedStyle(el, at).content)));
  // Each element is looked at once, in a third of the time: the rest is
  // for rounding, which must not wait on it.
  for (const el of fresh) {
    if (performance.now() - began > BUDGET / 3) break;
    if (snap.scanned.has(el)) continue;
    snap.scanned.add(el);
    if (el instanceof SVGElement || el.closest(ICONS) || !iconish(el)) continue;
    snap.iconQueue.add(el);
  }
  if (snap.iconQueue.size && !(performance.now() - (snap.iconAt || -Infinity) < ICON_GAP)) {
    snap.iconAt = performance.now();
    window.__kbType.disabled = true;
    const icons = [];
    for (const el of snap.iconQueue) {
      snap.iconQueue.delete(el);
      if (!el.isConnected) continue;
      const style = getComputedStyle(el);
      if (ICON_FACE(style.fontFamily)) icons.push([el, style.fontFamily, style.fontSize]);
    }
    window.__kbType.disabled = false;
    for (const [el, family, size] of icons) {
      write(el, 'font-family', family);
      write(el, 'font-size', size);
      write(el, 'font-variant-ligatures', 'normal');
      write(el, '-webkit-text-fill-color', 'currentcolor');
    }
  }

  // What isn't text, whose height isn't lines: a picture, a table.
  const BOX = /^(img|video|canvas|iframe|svg|object|embed|table)$/;
  const rows = v => Math.round(v / H) * H;
  const px = (style, side) => {
    const v = style.getPropertyValue(side);
    return v.endsWith('px') ? parseFloat(v) : null;
  };
  const writes = [];
  for (const el of fresh) {
    if (performance.now() - began > BUDGET) break;
    snap.done.add(el);
    const style = getComputedStyle(el);
    const box = BOX.test(el.localName) && !(el.parentElement instanceof SVGElement)
      && style.display !== 'none' && style.float === 'none' && !/^(absolute|fixed)$/.test(style.position);
    if ((el instanceof SVGElement && !box) || /^(inline|contents|none)$/.test(style.display) && !box) continue;
    // A table's rows, by a height the page gave one (a spacer row of a few
    // pixels) or the space between them. A row its content sizes is whole
    // rows already, and a height measured from its content would hold when
    // the content changes: a row left far too tall when the pane widens,
    // its cells centred in it, as a gap.
    if (el.localName === 'tr' && el.computedStyleMap) {
      const given = el.computedStyleMap().get('height');
      const height = given instanceof CSSUnitValue && given.unit === 'px' ? given.value : 0;
      if (height > 0 && Math.abs(rows(height) - height) > 0.01) writes.push([el, 'height', `${rows(height)}px`]);
    }
    if (/table/.test(style.display) && style.borderCollapse === 'separate') {
      const [x, y = x] = style.borderSpacing.split(' ').map(parseFloat);
      if (y && Math.abs(rows(y) - y) > 0.01) writes.push([el, 'border-spacing', `${x}px ${rows(y)}px`]);
    }
    if (box) snap.boxes.set(el, px(style, 'margin-bottom') ?? 0);
    // Only what the page gave as a length: `auto` (a footer pushed to the
    // bottom of its card) and percentages come out of getComputedStyle in
    // pixels too, but are left alone, or they would be frozen.
    const given = el.computedStyleMap ? el.computedStyleMap() : null;
    const length = side => {
      const v = px(style, side);
      if (v === null || !given) return v;
      const value = given.get(side);
      return value instanceof CSSUnitValue && value.unit === 'px' ? v : null;
    };
    for (const edge of ['top', 'bottom']) {
      const margin = length(`margin-${edge}`), padding = length(`padding-${edge}`);
      const border = px(style, `border-${edge}-width`) || 0;
      let inside = padding ?? px(style, `padding-${edge}`) ?? 0;
      if (padding !== null) {
        inside = Math.max(0, rows(padding + border) - border);
        if (Math.abs(inside - padding) > 0.01) writes.push([el, `padding-${edge}`, `${inside}px`]);
      }
      if (margin !== null && !(edge === 'bottom' && snap.boxes.has(el))) {
        let want = rows(margin + inside + border) - inside - border;
        // A border's pixel or two come out of the margin (a heading's
        // underline); more would pull the box over its neighbours, so the
        // margin goes on to the next row instead.
        while (margin >= 0 && want < -2) want += H;
        if (Math.abs(want - margin) > 0.01) writes.push([el, `margin-${edge}`, `${want}px`]);
      }
    }
  }
  for (const [el, prop, value] of writes) write(el, prop, value);
  // Then, laid out so, each box's bottom margin to what makes it whole
  // rows, every time, as a picture's height changes when it loads.
  const margins = [];
  for (const [el, margin] of snap.boxes) {
    if (!el.isConnected) {
      snap.boxes.delete(el);
      continue;
    }
    const height = el.getBoundingClientRect().height;
    if (height < H) continue;
    const want = Math.ceil((height + margin - 0.5) / H) * H - height;
    const now = el.style.marginBottom === '' ? margin : parseFloat(el.style.marginBottom);
    if (Math.abs(want - now) > 0.01) margins.push([el, want]);
  }
  for (const [el, want] of margins) write(el, 'margin-bottom', `${want}px`);
})()"#
);

/// The page's own type again, after `terminal_type`, and its own margins
/// and padding.
pub const PLAIN_TYPE: &str = r#"(() => {
  if (!window.__kbType) return;
  for (const root of new Set([document, ...(window.__kbHidden || []), ...(window.__kbTyped || [])])) {
    root.adoptedStyleSheets = root.adoptedStyleSheets.filter(sheet => sheet !== window.__kbType);
  }
  if (window.__kbUnsnap) window.__kbUnsnap();
  delete window.__kbTyped;
  delete window.__kbType;
  delete window.__kbTypeCss;
  delete window.__kbSnap;
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

/// A letter of the page's text in a cell, its colour, and whether it is
/// selected. `CONTINUED` is the right half of a wide letter in the cell
/// before it.
type Letter = Option<(char, [u8; 3], bool)>;

/// What the cell after a wide letter holds: the letter's right half.
const CONTINUED: char = '\0';

/// The page's text as `READ_TEXT` read it.
#[derive(Default, Debug, PartialEq)]
pub struct Text {
    /// The viewport it was read in, in CSS pixels.
    width: f64,
    height: f64,
    /// How far the page was scrolled when it was read: newer than the last
    /// frame's, and the only word of a scroll that changes no pixels (plain
    /// text on white, whose letters are hidden), for which Chrome sends no
    /// frame.
    scroll: Option<(f64, f64)>,
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
    /// The letters the page has selected, as runs from, to, counted in
    /// the letters of `text`.
    selected: Vec<(usize, usize)>,
}

impl Text {
    /// How far the page was scrolled when the text was read.
    pub fn scroll(&self) -> Option<(f64, f64)> {
        self.scroll
    }

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
                    selected: line[7]
                        .as_array()
                        .map(|runs| {
                            let at = |run: &serde_json::Value, i: usize| run[i].as_u64().unwrap_or(0) as usize;
                            runs.iter().map(|run| (at(run, 0), at(run, 1))).collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect();
        let scroll = value["sx"].as_f64().zip(value["sy"].as_f64());
        Some(Text { width: value["w"].as_f64()?, height: value["h"].as_f64()?, scroll, lines })
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
                    // Selected, the other way round, as a terminal shows
                    // its own selection.
                    (_, Some((ch, colour, selected))) => {
                        let Cell { fg, bg, .. } = self.fit(&pixels, area);
                        let bg = if distance(fg, colour) > distance(bg, colour) { fg } else { bg };
                        let (fg, bg) = if selected { (bg, colour) } else { (colour, bg) };
                        match ch {
                            ' ' => Cell::flat(bg),
                            _ => Cell { ch, fg, bg },
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
        // A wide letter and its right half are sent together: either
        // written alone would leave half of one, or overwrite it.
        let mut send = vec![false; count];
        for &at in &changed {
            send[at] = true;
            if cells[at].ch == CONTINUED && at > 0 {
                send[at - 1] = true;
            }
            if cell_width(cells[at].ch) == 2 && at + 1 < count {
                send[at + 1] = true;
            }
        }
        let changed: Vec<usize> = (0..count).filter(|&at| send[at]).collect();
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
            }
            // Going on a space after a line that reached the edge, or text
            // the page starts past it, would start beyond the last cell.
            .min(cols);
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
            let free = |cell: &Letter| !cell.is_some_and(|(letter, _, _)| letter != ' ');
            for (k, ch) in line.text.chars().enumerate() {
                let width = cell_width(ch);
                if width == 0 {
                    continue;
                }
                if at + width > stop {
                    break;
                }
                let selected = line.selected.iter().any(|&(from, to)| k >= from && k < to);
                let cells = &mut grid[row * cols + at..row * cols + at + width];
                if cells.iter().all(free) {
                    cells[0] = Some((if ch == '\u{a0}' { ' ' } else { ch }, line.colour, selected));
                    if width == 2 {
                        cells[1] = Some((CONTINUED, line.colour, selected));
                    }
                }
                at += width;
            }
            taken[row] = Some((start, at, right));
        }
        grid
    }

    /// Draw again the page's cells within `rect`, once something drawn over
    /// them (a box, a menu) is gone.
    /// A wide letter the box's edge cut in two is drawn whole, its other
    /// half too, unless that half is under a box still `drawn`: then the
    /// half in `rect` is a space, so as not to write over that box.
    pub fn repaint(&self, out: &mut Vec<u8>, rect: crate::ui::Rect, drawn: &[crate::ui::Rect]) {
        if self.shown.is_empty() {
            return;
        }
        let (cols, top) = (self.cols as usize, self.top as usize);
        let free = |at: usize| {
            let (row, col) = ((top + at / cols) as u16, (at % cols + 1) as u16);
            !drawn.iter().any(|r| row >= r.row && row < r.row + r.height && col >= r.col && col < r.col + r.width)
        };
        let mut cells = Vec::new();
        for row in rect.row..rect.row + rect.height {
            let Some(row) = row.checked_sub(self.top).filter(|row| *row < self.rows) else { continue };
            let (first, end) =
                (rect.col.saturating_sub(1) as usize, ((rect.col - 1 + rect.width).min(self.cols)) as usize);
            if first >= end {
                continue;
            }
            let at = |col: usize| row as usize * cols + col;
            if self.shown[at(first)].ch == CONTINUED && first > 0 && free(at(first - 1)) {
                cells.push(at(first - 1));
            }
            cells.extend((first..end).map(at));
            if cell_width(self.shown[at(end - 1)].ch) == 2 && end < cols && free(at(end)) {
                cells.push(at(end));
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
        for (k, &i) in at.iter().enumerate() {
            let mut cell = self.shown[i];
            // A wide letter and its right half go together. A half alone is
            // the edge of a box being uncovered cutting the letter: it is a
            // space, as the other half's cell is the box's, still drawn.
            if cell.ch == CONTINUED {
                if next == Some(i + 1) {
                    continue;
                }
                cell.ch = ' ';
            } else if cell_width(cell.ch) == 2 && at.get(k + 1) != Some(&(i + 1)) {
                cell.ch = ' ';
            }
            if next != Some(i) {
                let _ = write!(s, "\x1b[{};{}H", self.top as usize + i / width, i % width + 1);
            }
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
            let step = cell_width(cell.ch).max(1);
            next = (i % width + step < width).then_some(i + step);
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

/// How many cells `ch` takes, 0 if it isn't printed: a control character
/// (page text sent to the terminal as one could drive it), one of no width
/// that joins or reorders others, an emoji (whose width terminals don't
/// agree on), or one of the scripts written right to left, which a
/// terminal would print left to right a letter at a time, unjoined, and
/// are better left to the pixels. Wide letters (CJK) take two.
pub(crate) fn cell_width(ch: char) -> usize {
    let c = ch as u32;
    // Of no width: combining marks, joiners, direction marks, and the soft
    // hyphen, which shows only where the page breaks a line at it.
    let zero = matches!(
        c,
        0xAD | 0x300..=0x36F
            | 0x61C
            | 0x180E
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x206F
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xE0000..=0xE0FFF
    );
    let wide = matches!(
        c,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD
    );
    let emoji = matches!(c, 0x1F300..=0x1FAFF);
    let right_to_left = matches!(c, 0x590..=0x8FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFC);
    match () {
        _ if ch.is_control() || zero || emoji || right_to_left || ch == CONTINUED => 0,
        _ if wide => 2,
        _ => 1,
    }
}

fn distance(a: [u8; 3], b: [u8; 3]) -> i32 {
    (0..3).map(|i| (a[i] as i32 - b[i] as i32).pow(2)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_type_writes_whole_cells_as_numbers_a_script_can_use() {
        // `12.toFixed(3)` is a syntax error; `(12).toFixed(3)` isn't.
        let script = terminal_type((6.0, 12.0));
        assert!(script.contains("const W = (6), H = (12),"), "{script}");
    }

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
        page.repaint(&mut out, crate::ui::Rect { col: 2, row: 1, width: 2, height: 3 }, &[]);
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
        grid.chunks(cols)
            .map(|row| row.iter().map(|l| l.map_or('.', |(ch, _, _)| if ch == CONTINUED { '>' } else { ch })).collect())
            .collect()
    }

    #[test]
    fn reads_the_pages_text() {
        let text = text();
        assert_eq!((text.width, text.height, text.lines.len()), (40.0, 20.0, 2));
        assert_eq!(text.scroll(), None);
        let scrolled = Text::parse(r#"{"w":40,"h":20,"sx":0,"sy":4044.5,"colours":[],"lines":[]}"#).unwrap();
        assert_eq!(scrolled.scroll(), Some((0.0, 4044.5)));
        let red = Line {
            colour: [255, 0, 0],
            fixed: true,
            text: "ab".into(),
            rect: [20.0, 10.0, 40.0, 20.0],
            selected: Vec::new(),
        };
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
    fn wide_letters_take_two_cells_and_selected_ones_say_so() {
        let mut page = AsciiPage::new(Style::Letters);
        // "漢字a" from the left of a 6-cell row, its second and third
        // letters selected; then "漢字" with room for only one of them.
        let json = r#"{"w":60,"h":20,"colours":[[0,0,0]],
            "lines":[[0,0,"漢字a",0,0,50,10,[[1,3]]],[0,0,"漢字",30,10,60,20]]}"#;
        page.text = Text::parse(json);
        let grid = page.place(6, 2, (0.0, 0.0), (10.0, 10.0));
        assert_eq!(letters(&grid, 6), ["漢>字>a.", "...漢>."]);
        let selected: String = grid[..6].iter().map(|l| l.map_or('.', |(_, _, s)| if s { 's' } else { '-' })).collect();
        assert_eq!(selected, "--sss.");
    }

    #[test]
    fn a_wide_letter_is_sent_once_for_its_two_cells() {
        let mut page = AsciiPage::new(Style::Letters);
        page.text = Text::parse(r#"{"w":40,"h":10,"colours":[[0,0,0]],"lines":[[0,0,"字",0,0,20,10]]}"#);
        let mut out = Vec::new();
        page.draw_pixels(&mut out, frame(4, 1, |_, _| WHITE), 4, 1, 1, (0.0, 0.0));
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.matches('字').count(), 1, "{out:?}");
        assert!(!out.contains('\0'), "{out:?}");
        // A box gone from over one half draws the letter whole again; one
        // still over the other half keeps it: that half is a space.
        for col in [1, 2] {
            let mut again = Vec::new();
            page.repaint(&mut again, crate::ui::Rect { col, row: 1, width: 1, height: 1 }, &[]);
            let again = String::from_utf8(again).unwrap();
            assert!(again.contains("\x1b[1;1H") && again.matches('字').count() == 1, "{again:?}");
            let other = crate::ui::Rect { col: 3 - col, row: 1, width: 1, height: 1 };
            let mut kept = Vec::new();
            page.repaint(&mut kept, crate::ui::Rect { col, row: 1, width: 1, height: 1 }, &[other]);
            let kept = String::from_utf8(kept).unwrap();
            assert!(!kept.contains('字') && kept.contains(&format!("\x1b[1;{col}H")), "{kept:?}");
            assert!(!kept.contains(&format!("\x1b[1;{}H", 3 - col)), "{kept:?}");
        }
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
    fn a_line_that_reaches_the_edge_leaves_no_room_for_what_follows() {
        let mut page = AsciiPage::new(Style::Letters);
        // "abcd" fills the row's four cells; " link" follows it, a space on,
        // and so would start past the edge. Text past the edge does too.
        let json = r#"{"w":80,"h":10,"colours":[[0,0,0]],"lines":[
            [0,0,"abcd",0,0,40,10],[0,0,"link",42,0,60,10],[0,1,"far",65,0,80,10]]}"#;
        page.text = Text::parse(json);
        assert_eq!(letters(&page.place(4, 1, (0.0, 0.0), (10.0, 10.0)), 4), ["abcd"]);
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
        assert!(['a', 'é', '—'].iter().all(|&ch| cell_width(ch) == 1));
        assert!(['\x1b', '\u{9b}', '\u{202e}', '\0', '\u{ad}', '\u{2028}', '\u{e0067}', '\u{e0100}']
            .iter()
            .all(|&ch| cell_width(ch) == 0));
        assert!(['😀', '\u{301}', 'ذ', 'ש'].iter().all(|&ch| cell_width(ch) == 0));
        assert!(['漢', 'ん', '한', '，'].iter().all(|&ch| cell_width(ch) == 2));
    }
}
