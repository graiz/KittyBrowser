//! Frames through the pty, sending only what changed.
//!
//! When the terminal can't read our files (kb over ssh), every byte of a
//! frame crosses the link, and Chrome's PNG of a whole page is about a
//! megabyte. Most frames differ from the last in a few places: a caret, a
//! typed word, a spinner. `Delta` keeps the frame on screen, finds the
//! rectangles of cells that changed, and lays each over the page as a small
//! image of raw pixels, zlib-compressed. Raw pixels compress better than
//! filtered PNG rows on page text, and level 1 on a few threads is quick
//! enough that even a scroll, which changes everything but the margins,
//! costs less this way than Chrome's PNG of the whole page. When nearly
//! everything changed, Chrome's PNG goes as it came.
//!
//! Each rectangle is a new image stacked above those before it, so it can
//! overlap them. A rectangle is deleted once later ones cover all of its
//! cells, and when too many build up they are folded into one.

use super::graphics::{self, Pixels};

/// The page, under every rectangle. Below `i32::MIN / 2`, like the page in
/// `graphics`, so that cells with a background of their own (the help) are
/// drawn over it.
const PAGE_Z: i64 = -2_000_000_000;

/// Image ids from here up are rectangles; the page is `PAGE_ID`.
const PAGE_ID: u32 = 1;
const FIRST_ID: u32 = 1_000_000;

/// The protocol's limit on the payload of one escape.
const CHUNK: usize = 4096;

/// More changed cells than this share of the page, and the whole page is
/// sent instead: Chrome has already compressed it. Measured over ssh, a
/// scroll sent as rectangles of 60% of the page beat the whole page by
/// about 10 ms; at 50% here it went whole, and lost that.
const WHOLE_SHARE: f64 = 0.9;

/// More rectangles than this on screen, and they are folded into one.
const MOST_RECTS: usize = 64;

/// zlib level for rectangles. On a page, 1 is four times quicker than 3
/// and a quarter larger; over a LAN the time matters more than the bytes
/// (levels 2 and 4 measured no better).
const LEVEL: u8 = 1;

/// Large rectangles are compressed in bands of about this many cells, on
/// up to this many threads.
const BAND_AREA: usize = 400;
const MOST_THREADS: usize = 8;

/// A block of cells: column, row, columns, rows, from the page's top left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Rect {
    fn right(&self) -> u16 {
        self.col + self.cols
    }

    fn bottom(&self) -> u16 {
        self.row + self.rows
    }

    fn area(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    fn union(&self, other: &Rect) -> Rect {
        let (col, row) = (self.col.min(other.col), self.row.min(other.row));
        let (right, bottom) = (self.right().max(other.right()), self.bottom().max(other.bottom()));
        Rect { col, row, cols: right - col, rows: bottom - row }
    }

    /// Whether the two touch or overlap, corners included.
    fn touches(&self, other: &Rect) -> bool {
        self.col <= other.right()
            && other.col <= self.right()
            && self.row <= other.bottom()
            && other.row <= self.bottom()
    }
}

/// What `Delta::draw` sent.
#[derive(PartialEq, Eq, Debug)]
pub enum Drawn {
    /// The whole page, as Chrome sent it.
    Whole,
    /// These rectangles over the page.
    Rects(Vec<Rect>),
}

/// A rectangle on screen: its image id, where it is, and how many of its
/// cells no later rectangle covers.
struct Shown {
    id: u32,
    rect: Rect,
    visible: usize,
}

#[derive(Default)]
pub struct Delta {
    /// The frame on screen, cut to the cells, if this drew it.
    frame: Option<Pixels>,
    /// The grid it was drawn on: columns, rows, cell size.
    grid: (u16, u16, (u32, u32)),
    rects: Vec<Shown>,
    /// The page last sent whole, while it is still being decoded.
    pending: Option<Pending>,
    /// A frame's worth of memory to decode the next frame into.
    spare: Vec<u8>,
    /// For each cell, row by row, the id of the topmost rectangle over it,
    /// or `PAGE_ID`.
    owner: Vec<u32>,
    next_id: u32,
}

impl Delta {
    /// The screen was cleared of images: the next draw sends the whole page.
    pub fn forget(&mut self) {
        self.frame = None;
        self.pending = None;
        self.rects.clear();
    }

    /// Append the escapes that bring the screen to Chrome's frame
    /// `png_base64`, filling `cols` x `rows` cells of `cell` pixels with
    /// its top at screen row `top`. Returns `None`, having sent nothing, if
    /// the frame can't be decoded or doesn't line up with the cells; the
    /// caller then draws it some other way and calls `forget`.
    ///
    /// When the whole page goes, the rest of its decoding is left for
    /// `finish`, to be done while the terminal reads it.
    pub fn draw(
        &mut self,
        out: &mut Vec<u8>,
        png_base64: &str,
        cols: u16,
        rows: u16,
        cell: (u32, u32),
        top: u16,
    ) -> Option<Drawn> {
        self.finish();
        let bytes = crate::base64::decode(png_base64)?;
        let reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
        let info = reader.info();
        let (width, height) = (info.width, info.height);
        let rgb =
            (info.color_type, info.bit_depth, info.interlaced) == (png::ColorType::Rgb, png::BitDepth::Eight, false);
        let size = Pixels { width, height, alpha: false, data: Vec::new() };
        if !rgb || !graphics::Tiles::fits(&size, cols, rows, cell) {
            return None;
        }
        let old = self.frame.take().filter(|_| self.grid == (cols, rows, cell));
        self.grid = (cols, rows, cell);
        let (frame_w, frame_h) = (cols as usize * cell.0 as usize, rows as usize * cell.1 as usize);
        let mut pending = Pending {
            reader,
            // The buffer of the frame before last, if there is one: a new
            // one costs a page fault every 16 KB, a millisecond or two a frame.
            data: {
                let mut data = std::mem::take(&mut self.spare);
                data.resize(frame_w * frame_h * 3, 0);
                data
            },
            line: 0,
            stride: frame_w * 3,
            scratch: vec![0; width as usize * 3],
        };
        let Some(old) = old else {
            self.whole(out, png_base64, cols, rows, top);
            self.pending = Some(pending);
            return Some(Drawn::Whole);
        };

        // Decode a row of cells at a time, marking the cells that differ, and
        // give up on rectangles as soon as too many have.
        let cells = cols as usize * rows as usize;
        let enough = (cells as f64 * WHOLE_SHARE) as usize;
        let mut changed = vec![false; cells];
        let mut count = 0;
        let stride = frame_w * 3;
        let (cell_w, cell_h) = (cell.0 as usize * 3, cell.1 as usize);
        for row in 0..rows as usize {
            for _ in 0..cell_h {
                let y = pending.line;
                pending.next_line()?;
                let (a, b) = (&old.data[y * stride..][..stride], &pending.data[y * stride..][..stride]);
                if a == b {
                    continue;
                }
                let marks = &mut changed[row * cols as usize..][..cols as usize];
                for (col, mark) in marks.iter_mut().enumerate() {
                    if !*mark && a[col * cell_w..][..cell_w] != b[col * cell_w..][..cell_w] {
                        *mark = true;
                        count += 1;
                    }
                }
            }
            if count > enough {
                self.whole(out, png_base64, cols, rows, top);
                self.pending = Some(pending);
                self.spare = old.data;
                return Some(Drawn::Whole);
            }
        }
        self.spare = old.data;
        let new = Pixels { width: frame_w as u32, height: frame_h as u32, alpha: false, data: pending.data };
        let changed = rects(&changed, cols, rows);
        let area: usize = changed.iter().map(Rect::area).sum();
        if area > enough {
            self.whole(out, png_base64, cols, rows, top);
            self.frame = Some(new);
            return Some(Drawn::Whole);
        }
        self.put_rects(out, &new, &changed, top);
        let mut sent = changed;
        if self.rects.len() > MOST_RECTS {
            // Fold them into one: everything they cover, sent again.
            let all = self.rects.iter().map(|shown| shown.rect).reduce(|a, b| a.union(&b))?;
            if all.area() > enough {
                self.whole(out, png_base64, cols, rows, top);
                self.frame = Some(new);
                return Some(Drawn::Whole);
            }
            self.put_rects(out, &new, &[all], top);
            sent.push(all);
        }
        self.frame = Some(new);
        Some(Drawn::Rects(sent))
    }

    /// Finish decoding the page last sent whole, so the next frame can be
    /// compared with it. Call it once the frame is written; `draw` does it
    /// if this wasn't called.
    pub fn finish(&mut self) {
        let Some(mut pending) = self.pending.take() else { return };
        let (cols, rows, cell) = self.grid;
        let (frame_w, frame_h) = (cols as u32 * cell.0, rows as u32 * cell.1);
        while pending.line < frame_h as usize {
            if pending.next_line().is_none() {
                // A damaged frame: the next one goes whole.
                return;
            }
        }
        self.frame = Some(Pixels { width: frame_w, height: frame_h, alpha: false, data: pending.data });
    }

    /// The whole page, as Chrome sent it, with the rectangles removed.
    fn whole(&mut self, out: &mut Vec<u8>, png_base64: &str, cols: u16, rows: u16, top: u16) {
        for shown in self.rects.drain(..) {
            out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", shown.id).as_bytes());
        }
        self.owner = vec![PAGE_ID; cols as usize * rows as usize];
        self.next_id = FIRST_ID;
        out.extend_from_slice(format!("\x1b[{top};1H").as_bytes());
        let keys = format!("a=T,f=100,i={PAGE_ID},p=1,z={PAGE_Z},q=2,C=1,c={cols},r={rows}");
        put_chunks(out, &keys, png_base64.as_bytes());
    }

    /// Lay `rects` of `frame` over the page, above every rectangle before
    /// them, and delete those they leave with nothing showing. Large
    /// rectangles are cut into bands that are compressed side by side.
    fn put_rects(&mut self, out: &mut Vec<u8>, frame: &Pixels, rects: &[Rect], top: u16) {
        let (cols, _, cell) = self.grid;
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(MOST_THREADS);
        let total: usize = rects.iter().map(Rect::area).sum();
        let band_area = total.div_ceil(threads).max(BAND_AREA);
        let mut pieces = Vec::new();
        for rect in rects {
            let band_rows = (band_area / rect.cols as usize).clamp(1, rect.rows as usize) as u16;
            for row in (rect.row..rect.bottom()).step_by(band_rows as usize) {
                pieces.push(Rect { row, rows: band_rows.min(rect.bottom() - row), ..*rect });
            }
        }
        let pack = |rect: &Rect| -> String {
            let (x, width) = (rect.col as usize * cell.0 as usize, rect.cols as usize * cell.0 as usize);
            let (y, height) = (rect.row as usize * cell.1 as usize, rect.rows as usize * cell.1 as usize);
            let stride = frame.width as usize * 3;
            let mut data = Vec::with_capacity(width * height * 3);
            for line in y..y + height {
                data.extend_from_slice(&frame.data[line * stride + x * 3..][..width * 3]);
            }
            crate::base64::encode(&miniz_oxide::deflate::compress_to_vec_zlib(&data, LEVEL))
        };
        let packed: Vec<String> = if pieces.len() <= 1 {
            pieces.iter().map(pack).collect()
        } else {
            let per_thread = pieces.len().div_ceil(threads);
            std::thread::scope(|scope| {
                let handles: Vec<_> = pieces
                    .chunks(per_thread)
                    .map(|chunk| scope.spawn(move || chunk.iter().map(pack).collect::<Vec<_>>()))
                    .collect();
                handles.into_iter().flat_map(|handle| handle.join().expect("compressing a rectangle")).collect()
            })
        };

        for (rect, base64) in pieces.iter().zip(packed) {
            let id = self.next_id;
            self.next_id += 1;
            let z = PAGE_Z + 1 + (id - FIRST_ID) as i64;
            let (width, height) = (rect.cols as u32 * cell.0, rect.rows as u32 * cell.1);
            out.extend_from_slice(format!("\x1b[{};{}H", top + rect.row, rect.col + 1).as_bytes());
            let keys =
                format!("a=T,f=24,o=z,s={width},v={height},i={id},p=1,z={z},q=2,C=1,c={},r={}", rect.cols, rect.rows);
            put_chunks(out, &keys, base64.as_bytes());

            for row in rect.row..rect.bottom() {
                for col in rect.col..rect.right() {
                    let at = row as usize * cols as usize + col as usize;
                    let under = std::mem::replace(&mut self.owner[at], id);
                    if let Some(shown) = self.rects.iter_mut().find(|shown| shown.id == under) {
                        shown.visible -= 1;
                    }
                }
            }
            self.rects.retain(|shown| {
                if shown.visible == 0 {
                    out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", shown.id).as_bytes());
                }
                shown.visible > 0
            });
            self.rects.push(Shown { id, rect: *rect, visible: rect.area() });
        }
    }
}

/// Append one image as escapes of at most `CHUNK` bytes of base64 each.
fn put_chunks(out: &mut Vec<u8>, keys: &str, base64: &[u8]) {
    let mut chunks = base64.chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        out.extend_from_slice(b"\x1b_G");
        if first {
            out.extend_from_slice(keys.as_bytes());
            out.push(b',');
            first = false;
        }
        out.extend_from_slice(format!("m={more};").as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// A frame being decoded, a line at a time, into pixels cut to the cells.
struct Pending {
    reader: png::Reader<std::io::Cursor<Vec<u8>>>,
    data: Vec<u8>,
    /// The next line to decode.
    line: usize,
    /// The bytes of a line in `data`.
    stride: usize,
    /// A whole line, for frames wider than the cells.
    scratch: Vec<u8>,
}

impl Pending {
    /// Decode the next line into `data`. `None` if the frame is damaged.
    fn next_line(&mut self) -> Option<()> {
        let line = &mut self.data[self.line * self.stride..][..self.stride];
        if self.scratch.len() == self.stride {
            self.reader.read_row(line).ok()??;
        } else {
            self.reader.read_row(&mut self.scratch).ok()??;
            line.copy_from_slice(&self.scratch[..self.stride]);
        }
        self.line += 1;
        Some(())
    }
}

/// The changed cells as rectangles: each group of touching cells becomes
/// its bounding box, and boxes that touch are joined, until none do.
fn rects(changed: &[bool], cols: u16, rows: u16) -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    for row in 0..rows {
        let line = &changed[row as usize * cols as usize..][..cols as usize];
        let mut col = 0;
        while col < cols {
            if !line[col as usize] {
                col += 1;
                continue;
            }
            let start = col;
            while col < cols && line[col as usize] {
                col += 1;
            }
            found.push(Rect { col: start, row, cols: col - start, rows: 1 });
        }
    }
    // Join until nothing touches. Few rectangles in the cases that matter;
    // many only when much changed, and then the page goes whole anyway.
    loop {
        let mut joined = false;
        let mut i = 0;
        while i < found.len() {
            let mut j = i + 1;
            while j < found.len() {
                if found[i].touches(&found[j]) {
                    found[i] = found[i].union(&found[j]);
                    found.swap_remove(j);
                    joined = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
        if !joined {
            return found;
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Just enough of a kitty-graphics terminal to check what `Delta`
    /// sends: images put at the cursor, stacked by z, and deleted by id.
    #[derive(Default)]
    pub struct Screen {
        cursor: (u16, u16),
        images: HashMap<u32, (i64, (u16, u16), Pixels)>,
        /// The keys and payload of an image still arriving in chunks.
        partial: Option<(HashMap<String, String>, Vec<u8>)>,
    }

    impl Screen {
        pub fn feed(&mut self, mut bytes: &[u8]) {
            while !bytes.is_empty() {
                if let Some(rest) = bytes.strip_prefix(b"\x1b[") {
                    let end = rest.iter().position(|&b| b == b'H').unwrap();
                    let text = std::str::from_utf8(&rest[..end]).unwrap();
                    let (row, col) = text.split_once(';').unwrap();
                    self.cursor = (row.parse().unwrap(), col.parse().unwrap());
                    bytes = &rest[end + 1..];
                } else if let Some(rest) = bytes.strip_prefix(b"\x1b_G") {
                    let end = rest.windows(2).position(|w| w == b"\x1b\\").unwrap();
                    self.escape(std::str::from_utf8(&rest[..end]).unwrap());
                    bytes = &rest[end + 2..];
                } else {
                    panic!("unexpected bytes {:?}", String::from_utf8_lossy(&bytes[..bytes.len().min(20)]));
                }
            }
        }

        fn escape(&mut self, text: &str) {
            let (keys, payload) = text.split_once(';').unwrap_or((text, ""));
            let keys: HashMap<String, String> = keys
                .split(',')
                .map(|pair| pair.split_once('=').unwrap())
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            if keys.get("a").map(String::as_str) == Some("d") {
                assert_eq!(keys["d"], "I");
                assert!(self.images.remove(&keys["i"].parse().unwrap()).is_some(), "deleted a missing image");
                return;
            }
            let (mut first, mut data) = self.partial.take().unwrap_or_else(|| (keys.clone(), Vec::new()));
            data.extend_from_slice(payload.as_bytes());
            if keys["m"] == "1" {
                self.partial = Some((first, data));
                return;
            }
            first.remove("m");
            let bytes = crate::base64::decode(std::str::from_utf8(&data).unwrap()).unwrap();
            let pixels = match first["f"].as_str() {
                "100" => graphics::decode_png(&bytes).unwrap(),
                "24" => {
                    assert_eq!(first["o"], "z");
                    let data = miniz_oxide::inflate::decompress_to_vec_zlib(&bytes).unwrap();
                    let (width, height) = (first["s"].parse().unwrap(), first["v"].parse().unwrap());
                    assert_eq!(data.len(), (width * height * 3) as usize);
                    Pixels { width, height, alpha: false, data }
                }
                other => panic!("format {other}"),
            };
            let id = first["i"].parse().unwrap();
            assert!(!self.images.contains_key(&id) || id == PAGE_ID, "image {id} sent twice");
            self.images.insert(id, (first["z"].parse().unwrap(), self.cursor, pixels));
        }

        /// The page as shown: `cols` x `rows` cells from screen row `top`.
        pub fn page(&self, cols: u16, rows: u16, cell: (u32, u32), top: u16) -> Vec<u8> {
            let (width, height) = (cols as usize * cell.0 as usize, rows as usize * cell.1 as usize);
            let mut canvas = vec![0; width * height * 3];
            let mut images: Vec<_> = self.images.values().collect();
            images.sort_by_key(|(z, _, _)| *z);
            let zs: Vec<i64> = images.iter().map(|(z, _, _)| *z).collect();
            assert!(zs.windows(2).all(|w| w[0] < w[1]), "two images share a z");
            for (_, (row, col), pixels) in images {
                let (x0, y0) = ((*col - 1) as usize * cell.0 as usize, (*row - top) as usize * cell.1 as usize);
                let (w, h) = (pixels.width as usize, pixels.height as usize);
                // The page may be a pixel or two larger than the cells.
                for y in 0..h.min(height - y0) {
                    let line = &pixels.data[y * w * 3..][..w.min(width - x0) * 3];
                    canvas[(y0 + y) * width * 3 + x0 * 3..][..line.len()].copy_from_slice(line);
                }
            }
            canvas
        }

        pub fn count(&self) -> usize {
            self.images.len()
        }
    }

    fn grid(cols: u16, rows: u16, cells: &[(u16, u16)]) -> Vec<bool> {
        let mut changed = vec![false; cols as usize * rows as usize];
        for &(col, row) in cells {
            changed[row as usize * cols as usize + col as usize] = true;
        }
        changed
    }

    #[test]
    fn touching_cells_make_one_rect() {
        let changed = grid(10, 6, &[(1, 1), (2, 1), (3, 2), (8, 5)]);
        let mut found = rects(&changed, 10, 6);
        found.sort_by_key(|r| (r.row, r.col));
        assert_eq!(found, vec![Rect { col: 1, row: 1, cols: 3, rows: 2 }, Rect { col: 8, row: 5, cols: 1, rows: 1 }]);
    }

    #[test]
    fn nothing_changed_is_no_rects() {
        assert!(rects(&grid(4, 4, &[]), 4, 4).is_empty());
    }

    fn png(width: u32, height: u32, data: &[u8]) -> String {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        crate::base64::encode(&out)
    }

    /// Draw each frame, then check that the screen shows it exactly.
    pub fn check(frames: &[(u32, u32, Vec<u8>)], cols: u16, rows: u16, cell: (u32, u32)) -> Vec<Drawn> {
        let mut delta = Delta::default();
        let mut screen = Screen::default();
        let mut drawn = Vec::new();
        for (n, (width, height, data)) in frames.iter().enumerate() {
            let mut out = Vec::new();
            drawn.push(delta.draw(&mut out, &png(*width, *height, data), cols, rows, cell, 2).unwrap());
            delta.finish();
            screen.feed(&out);
            let (w, h) = (cols as usize * cell.0 as usize, rows as usize * cell.1 as usize);
            let expected: Vec<u8> =
                data.chunks_exact(*width as usize * 3).take(h).flat_map(|line| &line[..w * 3]).copied().collect();
            assert!(screen.page(cols, rows, cell, 2) == expected, "frame {n} isn't what the screen shows");
            assert!(screen.count() <= MOST_RECTS + 2, "{} images on screen", screen.count());
        }
        drawn
    }

    /// A frame of `cols` x `rows` cells of `cell` pixels, plus `extra`
    /// pixels right and below, in a pattern set by `seed`.
    fn frame(cols: u16, rows: u16, cell: (u32, u32), extra: u32, seed: u32) -> (u32, u32, Vec<u8>) {
        let (width, height) = (cols as u32 * cell.0 + extra, rows as u32 * cell.1 + extra);
        let data = (0..width * height * 3).map(|i| ((i / 7).wrapping_mul(seed | 1) >> 3) as u8).collect();
        (width, height, data)
    }

    fn paint(frame: &mut (u32, u32, Vec<u8>), x: u32, y: u32, w: u32, h: u32, value: u8) {
        let width = frame.0;
        for line in y..y + h {
            let at = ((line * width + x) * 3) as usize;
            frame.2[at..at + (w * 3) as usize].fill(value);
        }
    }

    #[test]
    fn screen_matches_every_frame() {
        let (cols, rows, cell) = (20, 10, (4, 6));
        let first = frame(cols, rows, cell, 1, 1);
        let mut frames = vec![first.clone()];
        // A caret: part of one cell, then the same cell back.
        let mut caret = first.clone();
        paint(&mut caret, 9, 13, 1, 5, 255);
        frames.push(caret);
        frames.push(first.clone());
        // Two places at once, one across a cell boundary.
        let mut two = first.clone();
        paint(&mut two, 2, 2, 3, 3, 0);
        paint(&mut two, 60, 40, 9, 9, 7);
        frames.push(two.clone());
        // Nothing changed.
        frames.push(two);
        // Typing: a run of small changes along a line, more than the
        // rectangles allowed on screen, so they are folded.
        let mut typed = first.clone();
        for i in 0..MOST_RECTS as u32 + 5 {
            paint(&mut typed, (i * 3) % 76, 30 + (i * 3) / 76 * 6, 3, 4, i as u8);
            frames.push(typed.clone());
        }
        // Everything: the page goes whole.
        frames.push(frame(cols, rows, cell, 1, 5));
        let drawn = check(&frames, cols, rows, cell);
        assert_eq!(drawn[0], Drawn::Whole);
        assert_eq!(drawn[1], Drawn::Rects(vec![Rect { col: 2, row: 2, cols: 1, rows: 1 }]));
        assert_eq!(drawn[4], Drawn::Rects(vec![]));
        assert_eq!(*drawn.last().unwrap(), Drawn::Whole);
    }

    #[test]
    fn frames_that_dont_fit_are_refused() {
        let (width, height, data) = frame(10, 5, (4, 6), 3, 1);
        let mut out = Vec::new();
        assert_eq!(Delta::default().draw(&mut out, &png(width, height, &data), 10, 5, (4, 6), 2), None);
        assert!(out.is_empty());
    }
}
