//! Frames through the pty that send only what the terminal doesn't have.
//!
//! Over ssh the terminal can't read kb's files, so every pixel it shows
//! crosses the link. When the page scrolls, most of a new frame is the old
//! one moved up or down, and the terminal already holds those pixels. So the
//! screen is a stack of images: the last whole frame at the bottom, and
//! above it strips of what changed since. A placement can show any rectangle
//! of an image at any pixel, so to scroll, the placements are moved up or
//! down and cut to what is still on the page; then what the move didn't
//! account for, the newly exposed rows and whatever didn't move with the
//! page (a sticky header), goes on top as raw pixels, zlib-compressed. When
//! the page doesn't move (a typed letter) only that goes.
//!
//! Part of a page can scroll while the rest stays put (a fixed sidebar, a
//! scrolling box), so the frame is cut into columns a cell wide, each finds
//! how far it moved, and neighbours that moved alike are moved together.
//!
//! Images that scroll off are kept for a while, with their pixels here:
//! scrolled back, the page shows them again, and rows found in one are
//! placed from it instead of sent. Placements that leave the page or are
//! hidden are deleted, and images that haven't shown for longest beyond a
//! couple of pages' worth. When too much is new for strips to pay, or
//! placements pile up, the whole frame goes again as Chrome sent it. The
//! screen always ends exactly at Chrome's frame: whatever isn't known to be
//! on screen already, pixel for pixel, is sent.

use super::graphics::{crop, Pixels};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Ids from here up are these images, clear of the page, tiles and queries.
const FIRST_ID: u32 = 1000;
/// Images are stacked from here up, each above the last. Below
/// -1,073,741,824 an image is drawn under text and cell backgrounds, as the
/// page is (see graphics::PAGE_Z).
const BOTTOM_Z: i64 = -2_000_000_000;
/// More placements than this and the whole frame goes again.
const MAX_PLACEMENTS: usize = 96;
/// Changed rows this close together go in one strip: a strip's own cost
/// (headers, an escape) is worth about this many unchanged rows.
const GAP: u32 = 16;
/// Fewer than one in this many rows of columns changed, and nothing is
/// taken to have moved.
const STILL: usize = 32;
/// One column in this many votes on how far things moved.
const SAMPLE: usize = 4;
/// Images that no longer show are kept, up to this many pages' worth, to
/// be placed again when the page scrolls back to them.
const RETAIN: u64 = 2;
/// Rows of a held image are placed again only this many at a time or
/// more; fewer cost about as much to place as to send.
const MIN_RUN: u32 = 32;
/// The protocol's limit on the payload of one escape.
const CHUNK: usize = 4096;

/// A rectangle, in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    fn contains(&self, other: &Rect) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }
}

/// Part of an image shown on the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Placement {
    image: u32,
    id: u32,
    z: i64,
    /// What of the image shows.
    from: Rect,
    /// Where on the page, the same size.
    x: u32,
    y: u32,
}

impl Placement {
    fn on_page(&self) -> Rect {
        Rect { x: self.x, y: self.y, width: self.from.width, height: self.from.height }
    }

    /// What of this shows in `region` of the next frame, where what was at
    /// row `y + shift` is now at row `y`, on a page `height` pixels tall.
    fn moved(&self, region: &Region, height: u32) -> Option<Placement> {
        let (shift, page) = (region.shift, self.on_page());
        let left = page.x.max(region.x.start);
        let right = (page.x + page.width).min(region.x.end);
        let top = (page.y as i64).max(shift).max(0);
        let bottom = ((page.y + page.height) as i64).min(shift + height as i64).min(height as i64);
        if left >= right || top >= bottom {
            return None;
        }
        let (top, bottom) = (top as u32, bottom as u32);
        Some(Placement {
            from: Rect {
                x: self.from.x + left - page.x,
                y: self.from.y + top - page.y,
                width: right - left,
                height: bottom - top,
            },
            x: left,
            y: (top as i64 - shift) as u32,
            ..*self
        })
    }
}

/// Columns `x` of the page, which moved up by `shift` pixels (down if
/// negative): row `y` of the new frame is row `y + shift` of the old.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Region {
    x: std::ops::Range<u32>,
    shift: i64,
}

/// What `Layers::draw` sent.
#[derive(PartialEq, Eq, Debug)]
pub enum Sent {
    /// The whole frame, as Chrome sent it.
    Whole(Why),
    /// The placements moved by the largest `shift` of any part of the page,
    /// and this many strips laid over them.
    Strips { shift: i64, strips: usize },
}

/// Why the whole frame was sent.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Why {
    /// Nothing this drew is on the screen.
    First,
    /// Half the page or more changed.
    Changed,
    /// Strips would have left too many placements.
    Placements,
}

/// The frame on screen and a hash of each cell-wide column of each of its
/// rows, column by column.
struct Shown {
    pixels: Pixels,
    hashes: Vec<u64>,
}

#[derive(Default)]
pub struct Layers {
    shown: Option<Shown>,
    /// Bottom to top.
    placements: Vec<Placement>,
    next_image: u32,
    next_placement: u32,
    next_z: i64,
    /// A frame's worth of memory to decode the next frame into: a new one
    /// costs a page fault every 16 KB, a millisecond or two a frame.
    spare: Vec<u8>,
    /// The images the terminal holds, oldest first, with their pixels.
    held: Vec<Held>,
    /// Draws so far, to know which held image showed least recently.
    clock: u64,
}

/// An image the terminal holds, placed or not, and what is in it. Once a
/// strip has scrolled off the page it is kept for a while: scrolled back,
/// the page shows it again, and it can be placed instead of sent.
struct Held {
    id: u32,
    /// The page column of its left edge (pages move only up and down).
    x: u32,
    pixels: Pixels,
    /// The draw it last showed in.
    used: u64,
    /// Where each row that occurs once in it is, by a hash of its columns
    /// `.0` to `.0 + .1`: made when first looked in, kept for next time.
    rows: Vec<(u32, u32, Rows)>,
}

/// Rows by their hash, `None` for one that occurs more than once.
type Rows = HashMap<u64, Option<u32>, BuildHasherDefault<Identity>>;

impl Layers {
    /// Whether the screen holds a page this drew.
    pub fn active(&self) -> bool {
        self.shown.is_some()
    }

    /// The terminal reported an error for image `id`. If it is one of
    /// these, it was most likely dropped, and the screen is in no known
    /// state: this forgets it all and returns true, and the caller should
    /// clear the screen of images and draw the frame again.
    pub fn lost(&mut self, id: u32) -> bool {
        let ours = self.held.iter().any(|held| held.id == id);
        if ours {
            self.forget();
        }
        ours
    }

    /// The screen was cleared of images: the next draw sends the whole frame.
    pub fn forget(&mut self) {
        self.shown = None;
        self.placements.clear();
        self.held.clear();
    }

    /// Append the escapes that bring the screen to `png_base64`, one of
    /// Chrome's frames, drawn at `top` (a screen row) over `cols` x `rows`
    /// cells of `cell` pixels. `None`, having appended nothing, if the frame
    /// can't be decoded or doesn't line up with the cells (see
    /// `Tiles::fits`); the caller then draws it some other way, and calls
    /// `forget` once it has cleared the screen.
    pub fn draw(
        &mut self,
        out: &mut Vec<u8>,
        png_base64: &str,
        cols: u16,
        rows: u16,
        cell: (u32, u32),
        top: u16,
    ) -> Option<Sent> {
        let new = decode(&crate::base64::decode(png_base64)?, std::mem::take(&mut self.spare))?;
        if !super::graphics::Tiles::fits(&new, cols, rows, cell) {
            return None;
        }
        let bytes = if new.alpha { 4 } else { 3 };
        let new = crop(new, cols as u32 * cell.0, rows as u32 * cell.1, bytes)?;
        let hashes = column_hashes(&new, cell.0);
        let page = Page { top, cell, height: new.height };

        let old = self.shown.take().filter(|old| {
            (old.pixels.width, old.pixels.height, old.pixels.alpha) == (new.width, new.height, new.alpha)
        });
        let sent = match &old {
            None => Sent::Whole(Why::First),
            Some(old) => {
                let regions = find_regions(&old.hashes, &hashes, new.height as usize, cell.0, new.width);
                let mut strips = Vec::new();
                for region in &regions {
                    strips.extend(changed(&old.pixels, &new, region));
                }
                let strips = merge(strips);
                match self.patch(out, &new, &regions, &strips, page) {
                    Err(why) => Sent::Whole(why),
                    Ok(()) => {
                        let shift = regions.iter().map(|r| r.shift).max_by_key(|s| s.abs()).unwrap_or(0);
                        Sent::Strips { shift, strips: strips.len() }
                    }
                }
            }
        };
        if let Sent::Whole(_) = sent {
            self.whole(out, png_base64, &new, page);
        }
        self.shown = Some(Shown { pixels: new, hashes });
        if let Some(old) = old {
            self.spare = old.pixels.data;
        }
        Some(sent)
    }

    /// Replace everything with the frame Chrome sent, which may be a pixel
    /// or two larger than `new` (its cropped copy): only `new`'s part shows.
    fn whole(&mut self, out: &mut Vec<u8>, png_base64: &str, new: &Pixels, page: Page) {
        self.placements.clear();
        for held in self.held.drain(..) {
            delete_image(out, held.id);
        }
        self.next_z = BOTTOM_Z;
        let placement = self.new_placement(Rect { x: 0, y: 0, width: new.width, height: new.height });
        page.cursor(out, &placement);
        let keys = format!(
            "a=T,f=100,i={},p={},w={},h={},z={},C=1,q=2",
            placement.image, placement.id, new.width, new.height, placement.z
        );
        transmit(out, &keys, png_base64);
        self.placements.push(placement);
        let pixels = Pixels { data: new.data.clone(), ..*new };
        self.held.push(Held { id: placement.image, x: 0, pixels, used: self.clock, rows: Vec::new() });
    }

    /// Move the placements as `regions` say and lay `strips` of `new` over
    /// them. Fails, having appended nothing, if what held images don't have
    /// is too much of the page to be worth sending in strips, or it would
    /// leave too many placements.
    fn patch(
        &mut self,
        out: &mut Vec<u8>,
        new: &Pixels,
        regions: &[Region],
        strips: &[Rect],
        page: Page,
    ) -> Result<(), Why> {
        let mut reused = Vec::new();
        let mut send = Vec::new();
        for strip in strips {
            self.reuse(new, *strip, &mut reused, &mut send);
        }
        if area(&send) * 2 >= new.width as u64 * new.height as u64 {
            return Err(Why::Changed);
        }
        // Compressing is most of the work, so big strips are cut into
        // pieces compressed side by side.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(MAX_THREADS);
        let pieces = split(&send, threads);

        // Where everything goes, bottom to top: each placement's part in
        // each region, if any shows and no strip covers it (the first part
        // keeps the placement's id), then the held images placed again,
        // then the pieces sent.
        let mut next = Vec::with_capacity(self.placements.len() + reused.len() + pieces.len());
        for placement in std::mem::take(&mut self.placements) {
            let parts = regions
                .iter()
                .filter_map(|region| placement.moved(region, page.height))
                .filter(|part| !strips.iter().any(|strip| strip.contains(&part.on_page())));
            for (n, part) in parts.enumerate() {
                next.push(if n == 0 { part } else { Placement { id: self.next_id(), ..part } });
            }
            self.placements.push(placement);
        }
        for &(image, from, x, y) in &reused {
            self.next_z += 1;
            next.push(Placement { image, id: self.next_id(), z: self.next_z, from, x, y });
        }
        let first_sent = next.len();
        for piece in &pieces {
            let mut placement = self.new_placement(*piece);
            (placement.x, placement.y) = (piece.x, piece.y);
            next.push(placement);
        }
        let shows = visible(&next, new.width, page);
        if shows.iter().filter(|&&shows| shows).count() > MAX_PLACEMENTS {
            return Err(Why::Placements);
        }

        // A placement that went is deleted, but not its image, which
        // `tidy` keeps or deletes.
        let before: HashMap<(u32, u32), Placement> =
            self.placements.iter().map(|placement| ((placement.image, placement.id), *placement)).collect();
        let after: std::collections::HashSet<(u32, u32)> =
            next.iter().zip(&shows).filter(|(_, &shows)| shows).map(|(p, _)| (p.image, p.id)).collect();
        for placement in &self.placements {
            if !after.contains(&(placement.image, placement.id)) {
                out.extend_from_slice(
                    format!("\x1b_Ga=d,d=i,i={},p={},q=2\x1b\\", placement.image, placement.id).as_bytes(),
                );
            }
        }
        for (placement, _) in next[..first_sent].iter().zip(&shows).filter(|(_, &shows)| shows) {
            if before.get(&(placement.image, placement.id)) != Some(placement) {
                page.place(out, placement);
            }
        }

        let cuts: Vec<Pixels> = pieces.iter().map(|piece| cut(new, *piece)).collect();
        let compressed: Vec<Vec<u8>> = std::thread::scope(|scope| {
            let work: Vec<_> = cuts.iter().map(|cut| scope.spawn(move || compress(&cut.data))).collect();
            work.into_iter().map(|done| done.join().unwrap_or_default()).collect()
        });
        for ((placement, data), pixels) in next[first_sent..].iter().zip(compressed).zip(cuts) {
            page.cursor(out, placement);
            let keys = format!(
                "a=T,f={},s={},v={},o=z,i={},p={},X={},Y={},z={},C=1,q=2",
                if new.alpha { 32 } else { 24 },
                pixels.width,
                pixels.height,
                placement.image,
                placement.id,
                placement.x % page.cell.0,
                placement.y % page.cell.1,
                placement.z
            );
            transmit(out, &keys, &crate::base64::encode(&data));
            self.held.push(Held { id: placement.image, x: placement.x, pixels, used: self.clock, rows: Vec::new() });
        }
        self.placements = next.into_iter().zip(shows).filter(|(_, shows)| *shows).map(|(p, _)| p).collect();
        self.tidy(out, new.width as u64 * new.height as u64);
        Ok(())
    }

    /// Note which images show, and delete those that haven't for longest
    /// beyond `RETAIN` pages' worth of pixels.
    fn tidy(&mut self, out: &mut Vec<u8>, page: u64) {
        self.clock += 1;
        for held in &mut self.held {
            if self.placements.iter().any(|p| p.image == held.id) {
                held.used = self.clock;
            }
        }
        let size = |held: &Held| held.pixels.width as u64 * held.pixels.height as u64;
        let mut kept: u64 = self.held.iter().filter(|h| h.used < self.clock).map(size).sum();
        while kept > RETAIN * page {
            let unused = self.held.iter().enumerate().filter(|(_, h)| h.used < self.clock);
            let Some((oldest, _)) = unused.min_by_key(|(_, h)| h.used) else { break };
            let held = self.held.remove(oldest);
            kept -= size(&held);
            delete_image(out, held.id);
        }
    }

    /// Split `strip` of `new` into what held images already have, pushed
    /// to `reused` as the image, the part of it, and where it goes on the
    /// page, and the rest, pushed to `send`.
    fn reuse(&mut self, new: &Pixels, strip: Rect, reused: &mut Vec<(u32, Rect, u32, u32)>, send: &mut Vec<Rect>) {
        if strip.height < MIN_RUN {
            send.push(strip);
            return;
        }
        let line = |pixels, x, y| line(pixels, x, y, strip.width);
        let mut done = vec![false; strip.height as usize];
        let hashes: Vec<u64> = (0..strip.height).map(|y| row_hash(line(new, strip.x, strip.y + y))).collect();
        for held in self.held.iter_mut().rev() {
            if held.pixels.alpha != new.alpha || strip.x < held.x || strip.x + strip.width > held.x + held.pixels.width
            {
                continue;
            }
            if done.iter().all(|&d| d) {
                break;
            }
            let from = strip.x - held.x;
            let index = match held.rows.iter().position(|(x, width, _)| (*x, *width) == (from, strip.width)) {
                Some(index) => index,
                None => {
                    let mut rows = Rows::default();
                    for y in 0..held.pixels.height {
                        let hash = row_hash(line(&held.pixels, from, y));
                        rows.entry(hash).and_modify(|row| *row = None).or_insert(Some(y));
                    }
                    // A strip of a sidebar and one of the page beside it
                    // take turns: each keeps its index.
                    if held.rows.len() == 2 {
                        held.rows.remove(0);
                    }
                    held.rows.push((from, strip.width, rows));
                    held.rows.len() - 1
                }
            };
            let rows = &held.rows[index].2;
            let same = |y: u32, k: u32| line(new, strip.x, strip.y + y) == line(&held.pixels, from, k);
            let mut y = 0;
            while y < strip.height {
                let found = match rows.get(&hashes[y as usize]) {
                    Some(Some(k)) if !done[y as usize] && same(y, *k) => *k,
                    _ => {
                        y += 1;
                        continue;
                    }
                };
                // Grow the match up and down over rows not yet found.
                let (mut top, mut k_top) = (y, found);
                while top > 0 && k_top > 0 && !done[top as usize - 1] && same(top - 1, k_top - 1) {
                    (top, k_top) = (top - 1, k_top - 1);
                }
                let mut bottom = y + 1;
                while bottom < strip.height
                    && k_top + (bottom - top) < held.pixels.height
                    && !done[bottom as usize]
                    && same(bottom, k_top + bottom - top)
                {
                    bottom += 1;
                }
                if bottom - top >= MIN_RUN {
                    done[top as usize..bottom as usize].fill(true);
                    let part = Rect { x: from, y: k_top, width: strip.width, height: bottom - top };
                    reused.push((held.id, part, strip.x, strip.y + top));
                }
                y = bottom;
            }
        }
        // What no image had is sent, a run of rows at a time.
        let mut y = 0;
        while y < strip.height {
            if done[y as usize] {
                y += 1;
                continue;
            }
            let start = y;
            while y < strip.height && !done[y as usize] {
                y += 1;
            }
            send.push(Rect { y: strip.y + start, height: y - start, ..strip });
        }
    }

    /// A new image of `size`, all of it placed at the page's top left, on
    /// top of everything.
    fn new_placement(&mut self, size: Rect) -> Placement {
        // Ids wrap long before they could reach a live image's again.
        self.next_image =
            if (FIRST_ID..FIRST_ID + 1_000_000).contains(&self.next_image) { self.next_image + 1 } else { FIRST_ID };
        self.next_z = self.next_z.max(BOTTOM_Z) + 1;
        Placement {
            image: self.next_image,
            id: self.next_id(),
            z: self.next_z,
            from: Rect { x: 0, y: 0, ..size },
            x: 0,
            y: 0,
        }
    }

    fn next_id(&mut self) -> u32 {
        self.next_placement = self.next_placement % 1_000_000 + 1;
        self.next_placement
    }
}

/// Which of `placements`, bottom to top on a page `width` pixels wide,
/// can be seen: those with a cell that the placements above them don't
/// cover. A cell counts as covered only by one placement that covers all
/// of it, which errs towards keeping a placement.
fn visible(placements: &[Placement], width: u32, page: Page) -> Vec<bool> {
    let (cols, rows) = (width.div_ceil(page.cell.0) as usize, page.height.div_ceil(page.cell.1) as usize);
    let mut covered = vec![false; cols * rows];
    let mut order: Vec<usize> = (0..placements.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((placements[i].z, i)));
    let mut shows = vec![false; placements.len()];
    for i in order {
        let rect = placements[i].on_page();
        let touched = (
            rect.x / page.cell.0,
            rect.y / page.cell.1,
            (rect.x + rect.width).div_ceil(page.cell.0),
            (rect.y + rect.height).div_ceil(page.cell.1),
        );
        let inside = (
            rect.x.div_ceil(page.cell.0),
            rect.y.div_ceil(page.cell.1),
            (rect.x + rect.width) / page.cell.0,
            (rect.y + rect.height) / page.cell.1,
        );
        let cells = |(left, top, right, bottom): (u32, u32, u32, u32)| {
            (top as usize..bottom as usize)
                .flat_map(move |row| (left as usize..right as usize).map(move |col| row * cols + col))
        };
        shows[i] = cells(touched).any(|at| !covered[at]);
        for at in cells(inside) {
            covered[at] = true;
        }
    }
    shows
}

/// Where the page is on the screen.
#[derive(Clone, Copy)]
struct Page {
    /// The screen row of its top.
    top: u16,
    cell: (u32, u32),
    /// In pixels.
    height: u32,
}

impl Page {
    /// Move the cursor to the cell that holds `placement`'s top left.
    fn cursor(&self, out: &mut Vec<u8>, placement: &Placement) {
        let (line, column) = (self.top as u32 + placement.y / self.cell.1, 1 + placement.x / self.cell.0);
        out.extend_from_slice(format!("\x1b[{line};{column}H").as_bytes());
    }

    /// Place an image the terminal has, or move a placement of it. An
    /// image that has been gone a while may have been dropped (a terminal
    /// keeps only so much), so the terminal is asked to say if it has: see
    /// `Layers::lost`.
    fn place(&self, out: &mut Vec<u8>, placement: &Placement) {
        self.cursor(out, placement);
        let from = placement.from;
        out.extend_from_slice(
            format!(
                "\x1b_Ga=p,i={},p={},x={},y={},w={},h={},X={},Y={},z={},C=1,q=1\x1b\\",
                placement.image,
                placement.id,
                from.x,
                from.y,
                from.width,
                from.height,
                placement.x % self.cell.0,
                placement.y % self.cell.1,
                placement.z
            )
            .as_bytes(),
        );
    }
}

fn delete_image(out: &mut Vec<u8>, id: u32) {
    out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").as_bytes());
}

/// Append `base64` as an image, in escapes of at most `CHUNK` bytes, the
/// first carrying `keys`.
fn transmit(out: &mut Vec<u8>, keys: &str, base64: &str) {
    let mut chunks = base64.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            out.extend_from_slice(format!("\x1b_G{keys},m={more}").as_bytes());
            first = false;
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more}").as_bytes());
        }
        out.push(b';');
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// How hard zlib works on strips: 3 makes them about as small as Chrome's
/// own PNGs, in a third of the time 6 takes; PNG's row filters only made
/// them larger.
const LEVEL: u8 = 3;

/// Decode a PNG into `data`, which is reused if it is large enough.
fn decode(bytes: &[u8], mut data: Vec<u8>) -> Option<Pixels> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Frames come from Chrome over a pipe, not off a disk or a network:
    // checking their checksums is a tenth of the time for nothing.
    decoder.ignore_checksums(true);
    let mut reader = decoder.read_info().ok()?;
    data.resize(reader.output_buffer_size()?, 0);
    let info = reader.next_frame(&mut data).ok()?;
    data.truncate(info.buffer_size());
    let alpha = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => false,
        (png::ColorType::Rgba, png::BitDepth::Eight) => true,
        _ => return None,
    };
    Some(Pixels { width: info.width, height: info.height, alpha, data })
}

/// At most this many strips are compressed at once.
const MAX_THREADS: usize = 4;
/// A strip is cut in pieces of no fewer than this many pixels.
const PIECE: u64 = 128 * 1024;

/// `strips`, the larger cut across into up to `threads` pieces, to be
/// compressed in parallel.
fn split(strips: &[Rect], threads: usize) -> Vec<Rect> {
    let mut pieces = Vec::new();
    for strip in strips {
        let count = (strip.width as u64 * strip.height as u64 / PIECE).clamp(1, threads as u64) as u32;
        let count = count.min(strip.height);
        for n in 0..count {
            let (top, bottom) = (strip.height * n / count, strip.height * (n + 1) / count);
            pieces.push(Rect { y: strip.y + top, height: bottom - top, ..*strip });
        }
    }
    pieces
}

/// The pixels of `rect`, zlib-compressed for the terminal.
fn compress(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, LEVEL)
}

/// The pixels of `rect` of `pixels`.
fn cut(pixels: &Pixels, rect: Rect) -> Pixels {
    let bytes = if pixels.alpha { 4 } else { 3 };
    let stride = pixels.width as usize * bytes;
    let mut data = Vec::with_capacity(rect.width as usize * rect.height as usize * bytes);
    for y in rect.y..rect.y + rect.height {
        data.extend_from_slice(
            &pixels.data[y as usize * stride + rect.x as usize * bytes..][..rect.width as usize * bytes],
        );
    }
    Pixels { width: rect.width, height: rect.height, alpha: pixels.alpha, data }
}

/// `width` pixels of row `y` of `pixels`, from column `x`.
fn line(pixels: &Pixels, x: u32, y: u32, width: u32) -> &[u8] {
    let bytes = if pixels.alpha { 4 } else { 3 };
    &pixels.data[(y as usize * pixels.width as usize + x as usize) * bytes..][..width as usize * bytes]
}

/// A hash of a row of pixels.
fn row_hash(row: &[u8]) -> u64 {
    let mut words = row.chunks_exact(8);
    let mut hash = row.len() as u64;
    for word in &mut words {
        hash = mix(hash, u64::from_le_bytes(word.try_into().unwrap()));
    }
    for &byte in words.remainder() {
        hash = mix(hash, byte as u64);
    }
    hash
}

/// The hashes in `find_regions` are already well mixed: hashing them again
/// would only cost time.
#[derive(Default)]
struct Identity(u64);

impl Hasher for Identity {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = mix(self.0, byte as u64);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

fn mix(hash: u64, word: u64) -> u64 {
    (hash.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95)
}

/// A hash of each `width`-pixel column of each row of `pixels`: all of the
/// first column's rows, top to bottom, then the next column's.
fn column_hashes(pixels: &Pixels, width: u32) -> Vec<u64> {
    let bytes = if pixels.alpha { 4 } else { 3 };
    let (stride, column) = (pixels.width as usize * bytes, width.max(1) as usize * bytes);
    let height = pixels.height as usize;
    let columns = stride.div_ceil(column);
    let mut hashes = vec![0; columns * height];
    for (y, row) in pixels.data.chunks_exact(stride).enumerate() {
        for (c, part) in row.chunks(column).enumerate() {
            let mut words = part.chunks_exact(8);
            let mut hash = part.len() as u64;
            for word in &mut words {
                hash = mix(hash, u64::from_le_bytes(word.try_into().unwrap()));
            }
            for &byte in words.remainder() {
                hash = mix(hash, byte as u64);
            }
            hashes[c * height + y] = hash;
        }
    }
    hashes
}

/// How far each part of the page moved between two frames, given their
/// `column_hashes` for columns `column` pixels wide on a page `width` by
/// `height`. Each row of a column that occurs once in that column of the
/// old frame (not blank space or a rule) votes for how far it moved; then
/// each column takes, of the likeliest shifts and no shift, the one that
/// leaves it the fewest rows to send.
fn find_regions(old: &[u64], new: &[u64], height: usize, column: u32, width: u32) -> Vec<Region> {
    // Little changed (a caret, a typed word): nothing moved worth finding,
    // and looking costs more than the rest of a frame's work.
    if old.iter().zip(new).filter(|(a, b)| a != b).count() * STILL < old.len() {
        return vec![Region { x: 0..width, shift: 0 }];
    }
    let columns = old.len() / height.max(1);
    // Where each row that occurs once in its column of the old frame was,
    // for every fourth column: enough to find a part that moved a few cells
    // wide, in a quarter of the time.
    let sampled = || (0..columns).step_by(SAMPLE).flat_map(|c| (0..height).map(move |y| (c, y)));
    let key = |c: usize, hash: u64| hash.wrapping_add(c as u64 * 0x9e37_79b9_7f4a_7c15);
    let mut rows: HashMap<u64, Option<usize>, BuildHasherDefault<Identity>> = HashMap::default();
    rows.reserve(old.len() / SAMPLE + height);
    for (c, y) in sampled() {
        rows.entry(key(c, old[c * height + y])).and_modify(|row| *row = None).or_insert(Some(y));
    }
    let mut votes: HashMap<i64, usize> = HashMap::new();
    for (c, y) in sampled() {
        if let Some(Some(from)) = rows.get(&key(c, new[c * height + y])) {
            *votes.entry(*from as i64 - y as i64).or_default() += 1;
        }
    }
    let mut likely: Vec<(i64, usize)> = votes.into_iter().filter(|&(shift, count)| shift != 0 && count >= 2).collect();
    likely.sort_by_key(|&(shift, count)| (std::cmp::Reverse(count), shift.abs()));
    let mut shifts = vec![0];
    shifts.extend(likely.iter().take(4).map(|&(shift, _)| shift));

    let mut regions: Vec<Region> = Vec::new();
    for c in 0..columns {
        let (old, new) = (&old[c * height..][..height], &new[c * height..][..height]);
        // Rows that come into view are counted at an eighth: they go in
        // the same strip as the next column's, so cost little more.
        let unmatched = |shift: i64| {
            let exposed = shift.unsigned_abs().min(height as u64) as usize;
            let rows = (0..height).filter(|&y| (0..height as i64).contains(&(y as i64 + shift)));
            rows.filter(|&y| old[(y as i64 + shift) as usize] != new[y]).count() + exposed / 8
        };
        // Ties go to the column to the left, so regions stay whole.
        let previous = regions.last().map(|r| r.shift);
        let shift = shifts
            .iter()
            .map(|&shift| (unmatched(shift), previous != Some(shift), shift))
            .min()
            .map_or(0, |(_, _, shift)| shift);
        let x = c as u32 * column..((c as u32 + 1) * column).min(width);
        match regions.last_mut() {
            Some(region) if region.shift == shift => region.x.end = x.end,
            _ => regions.push(Region { x, shift }),
        }
    }
    regions
}

/// The rectangles of `new` in `region` that differ from `old` moved as the
/// region says, top to bottom. Rows with nothing in `old` to come from
/// differ.
fn changed(old: &Pixels, new: &Pixels, region: &Region) -> Vec<Rect> {
    let bytes = if new.alpha { 4 } else { 3 };
    let stride = new.width as usize * bytes;
    let (left, right) = (region.x.start as usize * bytes, region.x.end as usize * bytes);
    let mut strips: Vec<Rect> = Vec::new();
    // The strip being grown: first row, last row, and the columns it spans.
    let mut open: Option<(u32, u32, u32, u32)> = None;
    for y in 0..new.height {
        let row = &new.data[y as usize * stride..][left..right];
        let from = y as i64 + region.shift;
        let span = if (0..old.height as i64).contains(&from) {
            let before = &old.data[from as usize * stride..][left..right];
            // Most rows are the same, and comparing them whole is quickest.
            (row != before).then(|| row.iter().zip(before).position(|(a, b)| a != b).unwrap_or(0)).map(|first| {
                let last =
                    row.len() - 1 - row.iter().rev().zip(before.iter().rev()).position(|(a, b)| a != b).unwrap_or(0);
                region.x.start + (first / bytes) as u32..region.x.start + (last / bytes) as u32 + 1
            })
        } else {
            Some(region.x.clone())
        };
        let Some(span) = span else { continue };
        open = match open {
            Some((start, end, left, right)) if y - end <= GAP => {
                Some((start, y, left.min(span.start), right.max(span.end)))
            }
            other => {
                if let Some((start, end, left, right)) = other {
                    strips.push(Rect { x: left, y: start, width: right - left, height: end + 1 - start });
                }
                Some((y, y, span.start, span.end))
            }
        };
    }
    if let Some((start, end, left, right)) = open {
        strips.push(Rect { x: left, y: start, width: right - left, height: end + 1 - start });
    }
    strips
}

/// What a strip costs beyond its pixels (an escape, a PNG's headers, a
/// placement for the terminal to keep), in pixels.
const STRIP_COST: u64 = 4096;

/// Join strips that cost less together than apart, as many small ones
/// along the edge of a region that moved tend to.
fn merge(mut strips: Vec<Rect>) -> Vec<Rect> {
    let union = |a: &Rect, b: &Rect| {
        let (x, y) = (a.x.min(b.x), a.y.min(b.y));
        let right = (a.x + a.width).max(b.x + b.width);
        let bottom = (a.y + a.height).max(b.y + b.height);
        Rect { x, y, width: right - x, height: bottom - y }
    };
    let size = |r: &Rect| r.width as u64 * r.height as u64;
    'again: loop {
        for i in 0..strips.len() {
            for j in i + 1..strips.len() {
                let both = union(&strips[i], &strips[j]);
                if size(&both) <= size(&strips[i]) + size(&strips[j]) + STRIP_COST {
                    strips[i] = both;
                    strips.swap_remove(j);
                    continue 'again;
                }
            }
        }
        return strips;
    }
}

fn area(rects: &[Rect]) -> u64 {
    rects.iter().map(|r| r.width as u64 * r.height as u64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_png(pixels: &Pixels, rect: Rect) -> Option<Vec<u8>> {
        let bytes = if pixels.alpha { 4 } else { 3 };
        let stride = pixels.width as usize * bytes;
        let mut data = Vec::with_capacity(rect.width as usize * rect.height as usize * bytes);
        for y in rect.y..rect.y + rect.height {
            data.extend_from_slice(
                &pixels.data[y as usize * stride + rect.x as usize * bytes..][..rect.width as usize * bytes],
            );
        }
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, rect.width, rect.height);
        encoder.set_color(if pixels.alpha { png::ColorType::Rgba } else { png::ColorType::Rgb });
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&data).ok()?;
        writer.finish().ok()?;
        Some(png)
    }

    /// A frame `width` pixels wide whose pixel at `x`, `y` is `at(x, y)`.
    fn frame(width: u32, height: u32, at: impl Fn(u32, u32) -> u8) -> Pixels {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend([at(x, y); 3]);
            }
        }
        Pixels { width, height, alpha: false, data }
    }

    /// A page of distinct rows, 8 pixels wide, `scrolled` pixels down.
    fn page(scrolled: u32) -> Pixels {
        frame(8, 40, |_, y| ((y + scrolled) * 7 % 251) as u8)
    }

    fn whole_page(shift: i64) -> Vec<Region> {
        vec![Region { x: 0..8, shift }]
    }

    #[test]
    fn finds_how_far_the_page_moved() {
        let at = |scrolled| column_hashes(&page(scrolled), 4);
        assert_eq!(find_regions(&at(10), &at(10), 40, 4, 8), whole_page(0));
        assert_eq!(find_regions(&at(10), &at(17), 40, 4, 8), whole_page(7));
        assert_eq!(find_regions(&at(17), &at(10), 40, 4, 8), whole_page(-7));
        // Nothing in common: no shift.
        let blank = column_hashes(&frame(8, 40, |_, _| 255), 4);
        assert_eq!(find_regions(&at(0), &blank, 40, 4, 8), whole_page(0));
    }

    #[test]
    fn finds_a_part_that_moved() {
        // A sidebar 4 pixels wide that stays put, and a column beside it
        // that scrolls 5 pixels, 12 wide, with an edge of 2 that is a
        // column of its own.
        let at = |scrolled| {
            column_hashes(
                &frame(18, 40, |x, y| if x < 4 { y as u8 } else { ((y + scrolled) * 7 % 251) as u8 + x as u8 }),
                4,
            )
        };
        assert_eq!(
            find_regions(&at(3), &at(8), 40, 4, 18),
            vec![Region { x: 0..4, shift: 0 }, Region { x: 4..18, shift: 5 }]
        );
    }

    #[test]
    fn sends_the_exposed_rows_and_what_did_not_move() {
        let (old, mut new) = (page(10), page(17));
        assert_eq!(changed(&old, &new, &whole_page(7)[0]), vec![Rect { x: 0, y: 33, width: 8, height: 7 }]);
        assert_eq!(changed(&new, &old, &whole_page(-7)[0]), vec![Rect { x: 0, y: 0, width: 8, height: 7 }]);
        // A sticky header, two rows tall, stays put; one pixel changes in
        // row 20, close enough to the exposed rows to go with them.
        for y in 0..2 {
            new.data[y * 24..(y + 1) * 24].copy_from_slice(&old.data[y * 24..(y + 1) * 24]);
        }
        new.data[20 * 24 + 3 * 3] ^= 1;
        assert_eq!(
            changed(&old, &new, &whole_page(7)[0]),
            vec![Rect { x: 0, y: 0, width: 8, height: 2 }, Rect { x: 0, y: 20, width: 8, height: 20 }]
        );
    }

    #[test]
    fn sends_one_changed_pixel() {
        let (old, mut new) = (page(10), page(10));
        new.data[20 * 24 + 3 * 3 + 2] ^= 1;
        assert_eq!(changed(&old, &new, &whole_page(0)[0]), vec![Rect { x: 3, y: 20, width: 1, height: 1 }]);
        assert_eq!(changed(&old, &new, &Region { x: 4..8, shift: 0 }), vec![]);
        assert!(changed(&old, &old, &whole_page(0)[0]).is_empty());
    }

    #[test]
    fn placements_move_and_are_cut() {
        let placement =
            Placement { image: 1, id: 1, z: 0, from: Rect { x: 2, y: 4, width: 6, height: 30 }, x: 2, y: 10 };
        let moved = |x: std::ops::Range<u32>, shift| placement.moved(&Region { x, shift }, 40);
        assert_eq!(moved(0..8, 0), Some(placement));
        // Up 15: its top 5 rows go off the page.
        assert_eq!(
            moved(0..8, 15),
            Some(Placement { from: Rect { x: 2, y: 9, width: 6, height: 25 }, x: 2, y: 0, ..placement })
        );
        // Down 5 in columns 4 to 6 only.
        assert_eq!(
            moved(4..6, -5),
            Some(Placement { from: Rect { x: 4, y: 4, width: 2, height: 25 }, x: 4, y: 15, ..placement })
        );
        assert_eq!(moved(0..2, 0), None);
        assert_eq!(moved(0..8, 40), None);
    }

    #[test]
    fn draws_whole_then_strips() {
        let encode = |pixels: &Pixels| {
            crate::base64::encode(
                &encode_png(pixels, Rect { x: 0, y: 0, width: pixels.width, height: pixels.height }).unwrap(),
            )
        };
        // 2 columns, 10 rows of 4x4 pixel cells.
        let (cols, rows, cell) = (2, 10, (4, 4));
        let mut layers = Layers::default();
        let mut out = Vec::new();
        assert_eq!(layers.draw(&mut out, &encode(&page(10)), cols, rows, cell, 2), Some(Sent::Whole(Why::First)));
        let text = String::from_utf8(std::mem::take(&mut out)).unwrap();
        assert!(
            text.starts_with("\x1b[2;1H\x1b_Ga=T,f=100,i=1000,p=1,w=8,h=40,z=-1999999999,C=1,q=2,m=0;"),
            "{text:?}"
        );

        // Scrolled 6 pixels: the frame moves up a cell and a half, and the
        // last 6 rows come in from row 34, which is row 8 of the cells and 2
        // pixels down.
        assert_eq!(
            layers.draw(&mut out, &encode(&page(16)), cols, rows, cell, 2),
            Some(Sent::Strips { shift: 6, strips: 1 })
        );
        let text = String::from_utf8(std::mem::take(&mut out)).unwrap();
        assert!(
            text.starts_with("\x1b[2;1H\x1b_Ga=p,i=1000,p=1,x=0,y=6,w=8,h=34,X=0,Y=0,z=-1999999999,C=1,q=1\x1b\\"),
            "{text:?}"
        );
        assert!(
            text.contains("\x1b[10;1H\x1b_Ga=T,f=24,s=8,v=6,o=z,i=1001,p=2,X=0,Y=2,z=-1999999998,C=1,q=2,"),
            "{text:?}"
        );

        // Back up 3: the first image moves down 3, the strip too, losing
        // its last 3 rows off the bottom, and the top 3 rows are sent.
        assert_eq!(
            layers.draw(&mut out, &encode(&page(13)), cols, rows, cell, 2),
            Some(Sent::Strips { shift: -3, strips: 1 })
        );
        let text = String::from_utf8(std::mem::take(&mut out)).unwrap();
        assert!(text.contains("\x1b[2;1H\x1b_Ga=p,i=1000,p=1,x=0,y=6,w=8,h=34,X=0,Y=3,"), "{text:?}");
        assert!(text.contains("\x1b[11;1H\x1b_Ga=p,i=1001,p=2,x=0,y=0,w=8,h=3,X=0,Y=1,"), "{text:?}");
        assert!(text.contains("\x1b[2;1H\x1b_Ga=T,f=24,s=8,v=3,o=z,i=1002,p=3,X=0,Y=0,"), "{text:?}");

        // Nothing changes: nothing is sent.
        assert_eq!(
            layers.draw(&mut out, &encode(&page(13)), cols, rows, cell, 2),
            Some(Sent::Strips { shift: 0, strips: 0 })
        );
        assert!(out.is_empty());
    }

    /// Just enough of a kitty-graphics terminal to see what `Layers` puts
    /// on the screen: images, placements of parts of them at pixels within
    /// cells, stacked by z, and deletions.
    #[derive(Default)]
    struct Screen {
        cursor: (u32, u32),
        images: HashMap<u32, Pixels>,
        /// Image, placement id, z, order placed, part of the image, and its
        /// top left on the screen in pixels.
        placements: Vec<(u32, u32, i64, usize, Rect, u32, u32)>,
        partial: Option<(HashMap<String, String>, String)>,
        placed: usize,
    }

    impl Screen {
        fn feed(&mut self, mut bytes: &[u8], cell: (u32, u32)) {
            while !bytes.is_empty() {
                if let Some(rest) = bytes.strip_prefix(b"\x1b[") {
                    let end = rest.iter().position(|&b| b == b'H').unwrap();
                    let (row, col) = std::str::from_utf8(&rest[..end]).unwrap().split_once(';').unwrap();
                    self.cursor = (row.parse().unwrap(), col.parse().unwrap());
                    bytes = &rest[end + 1..];
                } else {
                    let rest = bytes.strip_prefix(b"\x1b_G").expect("only cursor moves and graphics");
                    let end = rest.windows(2).position(|w| w == b"\x1b\\").unwrap();
                    self.escape(std::str::from_utf8(&rest[..end]).unwrap(), cell);
                    bytes = &rest[end + 2..];
                }
            }
        }

        fn escape(&mut self, text: &str, cell: (u32, u32)) {
            let (keys, payload) = text.split_once(';').unwrap_or((text, ""));
            let keys: HashMap<String, String> =
                keys.split(',').filter_map(|pair| pair.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
            let (mut keys, mut data) = self.partial.take().unwrap_or_else(|| (keys.clone(), String::new()));
            data.push_str(payload);
            if text.contains("m=1") {
                self.partial = Some((keys, data));
                return;
            }
            keys.remove("m");
            let number = |key: &str| keys.get(key).map(|v| v.parse::<i64>().unwrap());
            let id = number("i").unwrap() as u32;
            match keys["a"].as_str() {
                "d" => {
                    match number("p") {
                        Some(p) => self.placements.retain(|q| (q.0, q.1) != (id, p as u32)),
                        None => self.placements.retain(|q| q.0 != id),
                    }
                    if keys["d"] == "I" {
                        assert!(self.images.remove(&id).is_some(), "deleted image {id}, which isn't there");
                    }
                }
                action => {
                    if action == "T" {
                        let bytes = crate::base64::decode(&data).unwrap();
                        let pixels = match number("f") {
                            Some(100) => decode(&bytes, Vec::new()).unwrap(),
                            Some(24) => Pixels {
                                width: number("s").unwrap() as u32,
                                height: number("v").unwrap() as u32,
                                alpha: false,
                                data: miniz_oxide::inflate::decompress_to_vec_zlib(&bytes).unwrap(),
                            },
                            other => panic!("format {other:?}"),
                        };
                        self.images.insert(id, pixels);
                    }
                    let image = self.images.get(&id).unwrap_or_else(|| panic!("placed image {id}, which isn't there"));
                    let p = number("p").unwrap() as u32;
                    let (x, y) = (number("x").unwrap_or(0) as u32, number("y").unwrap_or(0) as u32);
                    let width = number("w").map_or(image.width - x, |w| w as u32);
                    let height = number("h").map_or(image.height - y, |h| h as u32);
                    let (dx, dy) = (number("X").unwrap_or(0) as u32, number("Y").unwrap_or(0) as u32);
                    assert!(dx < cell.0 && dy < cell.1, "an offset outside its cell");
                    self.placements.retain(|q| (q.0, q.1) != (id, p));
                    self.placed += 1;
                    let at = ((self.cursor.1 - 1) * cell.0 + dx, (self.cursor.0 - 1) * cell.1 + dy);
                    let from = Rect { x, y, width, height };
                    self.placements.push((id, p, number("z").unwrap(), self.placed, from, at.0, at.1));
                }
            }
        }

        /// The pixels of the screen from row `top`, `width` by `height`.
        fn page(&self, top: u32, cell: (u32, u32), width: u32, height: u32) -> Vec<u8> {
            let mut page = vec![0; width as usize * height as usize * 3];
            let mut order: Vec<_> = self.placements.iter().collect();
            order.sort_by_key(|q| (q.2, q.3));
            for &&(id, _, _, _, from, x, y) in &order {
                let image = &self.images[&id];
                for row in 0..from.height {
                    let Some(to) = (y + row).checked_sub((top - 1) * cell.1).filter(|&to| to < height) else {
                        continue;
                    };
                    let source = line(image, from.x, from.y + row, from.width);
                    let at = (to as usize * width as usize + x as usize) * 3;
                    let fits = (width - x).min(from.width) as usize * 3;
                    page[at..at + fits].copy_from_slice(&source[..fits]);
                }
            }
            page
        }
    }

    #[test]
    fn the_screen_ends_as_the_frame() {
        // 4 columns and 30 rows of 4x4 cells: a sidebar a cell wide that
        // stays put, and a page beside it that scrolls under a sticky
        // header 6 pixels tall.
        let (cols, rows, cell) = (4u16, 30u16, (4u32, 4u32));
        let (width, height) = (16, 120);
        let frame = |scrolled: u32, caret: bool| {
            frame(width, height, |x, y| match (x, y) {
                (0..4, _) => (y * 5 % 256) as u8,
                (_, 0..6) => 200 + x as u8,
                (10, 50..54) if caret => 0,
                _ => ((y + scrolled) * 13 % 256 + x * 3) as u8,
            })
        };
        let encode =
            |pixels: &Pixels| crate::base64::encode(&encode_png(pixels, Rect { x: 0, y: 0, width, height }).unwrap());
        let mut layers = Layers::default();
        let mut screen = Screen::default();
        let mut scrolled = 0;
        // Down by more than a run of held rows, back up over what was seen,
        // a caret on and off, and a jump too far for anything to be kept.
        let steps = [(0, false), (40, false), (40, false), (7, false), (-47, false), (-40, true), (0, false)];
        for (n, (step, caret)) in steps.into_iter().chain([(5, false), (200, false), (-3, true)]).enumerate() {
            scrolled = (scrolled as i64 + step) as u32;
            let new = frame(scrolled, caret);
            let mut out = Vec::new();
            let sent = layers.draw(&mut out, &encode(&new), cols, rows, cell, 2).unwrap();
            screen.feed(&out, cell);
            assert!(screen.page(2, cell, width, height) == new.data, "scrolled to {scrolled} and sent {sent:?}");
            if n == 4 {
                // Back up over rows seen before: placed again, not sent.
                assert!(!String::from_utf8_lossy(&out).contains("a=T"), "sent rows the terminal had: {sent:?}");
            }
        }
    }
}
