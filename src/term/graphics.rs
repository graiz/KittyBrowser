//! Kitty graphics protocol: send frames to the terminal and place them.
//!
//! There are two ways to send a frame. Base64 PNG in chunks through the
//! pty is the one every kitty-graphics terminal accepts. Decoded pixels left
//! in a temporary file, for the terminal to read itself, is several times
//! quicker where the terminal allows it; `ask_about_files` finds out.
//! docs/ARCHITECTURE.md has the measurements.

/// Every frame reuses this id, so a new frame replaces the previous one.
const IMAGE_ID: u32 = 1;

/// Removes every image from the screen.
pub const DELETE: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// Where the page and the tiles over it sit relative to text. Below this
/// threshold an image is drawn under text and under any cell with a
/// background colour of its own, so a box of coloured text (the help) can
/// be drawn over the page and the page shows again once those cells are
/// blanked.
const PAGE_Z: i64 = -1_073_741_826;
const TILE_Z: i64 = -1_073_741_825;

/// Ids from here up are tiles; the whole-page image and queries are below.
const FIRST_TILE_ID: u32 = 10;
/// A tile is this many cells across and down, less at the right and bottom
/// edges: about square, and small enough that a caret or a spinner costs
/// one tile, large enough that a whole page is under a hundred.
const TILE_COLS: u16 = 12;
const TILE_ROWS: u16 = 6;

/// The protocol's limit on the payload of one escape.
const CHUNK: usize = 4096;

/// Append the escapes that draw `png_base64` at the cursor, scaled to fill
/// `cols` x `rows` cells. The cursor doesn't move.
pub fn put_png(out: &mut Vec<u8>, png_base64: &str, cols: u16, rows: u16) {
    let mut chunks = png_base64.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            let keys = format!("a=T,f=100,i={IMAGE_ID},p=1,z={PAGE_Z},q=2,C=1,c={cols},r={rows},m={more}");
            out.extend_from_slice(b"\x1b_G");
            out.extend_from_slice(keys.as_bytes());
            first = false;
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more}").as_bytes());
        }
        out.push(b';');
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// A decoded frame: rows of RGB or RGBA bytes, top to bottom.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub data: Vec<u8>,
}

pub fn decode_png(bytes: &[u8]) -> Option<Pixels> {
    let mut reader = png::Decoder::new(bytes).read_info().ok()?;
    let mut data = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data).ok()?;
    data.truncate(info.buffer_size());
    let alpha = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => false,
        (png::ColorType::Rgba, png::BitDepth::Eight) => true,
        _ => return None,
    };
    Some(Pixels { width: info.width, height: info.height, alpha, data })
}

pub fn decode_jpeg(bytes: &[u8]) -> Option<Pixels> {
    let mut decoder = zune_jpeg::JpegDecoder::new(bytes);
    let data = decoder.decode().ok()?;
    let (width, height) = decoder.dimensions()?;
    (data.len() == width * height * 3).then_some(Pixels {
        width: width as u32,
        height: height as u32,
        alpha: false,
        data,
    })
}

/// Where to put frame number `seq` for the terminal to read. The protocol
/// wants a temporary directory and this phrase in the name before it will
/// read, then delete, a file.
pub fn temp_path(seq: u64) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("tty-graphics-protocol-kb-{}-{seq}", std::process::id()))
}

/// A `temp_path` no other frame or tile has used.
pub fn next_path() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    temp_path(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Append the escape that draws the pixels saved at `path` (see `temp_path`)
/// at the cursor, scaled to fill `cols` x `rows` cells. The terminal reads
/// the file itself and deletes it, so nothing but its name crosses the pty.
pub fn put_file(out: &mut Vec<u8>, path: &std::path::Path, pixels: &Pixels, cols: u16, rows: u16) {
    put_raw_file(out, path, (pixels.width, pixels.height), pixels.alpha, IMAGE_ID, PAGE_Z, cols, rows);
}

/// Append the escape that draws the pixels saved at `path` at the cursor,
/// as image `id`, filling `cols` x `rows` cells, above the page.
pub fn put_image_file(out: &mut Vec<u8>, path: &std::path::Path, pixels: &Pixels, id: u32, cols: u16, rows: u16) {
    put_raw_file(out, path, (pixels.width, pixels.height), pixels.alpha, id, 0, cols, rows);
}

#[allow(clippy::too_many_arguments)]
fn put_raw_file(
    out: &mut Vec<u8>,
    path: &std::path::Path,
    size: (u32, u32),
    alpha: bool,
    id: u32,
    z: i64,
    cols: u16,
    rows: u16,
) {
    let format = if alpha { 32 } else { 24 };
    let name = crate::base64::encode(path.as_os_str().as_encoded_bytes());
    let (width, height) = size;
    out.extend_from_slice(
        format!(
            "\x1b_Ga=T,f={format},s={width},v={height},t=t,i={id},p=1,z={z},q=2,C=1,c={cols},r={rows};{name}\x1b\\"
        )
        .as_bytes(),
    );
}

/// What `Tiles::draw` sent.
#[derive(PartialEq, Eq, Debug)]
pub enum Drawn {
    /// The whole page, as one image.
    Whole,
    /// This many tiles, laid over the page where it changed.
    Tiles(usize),
}

/// Draws the page so that a small change costs little. The page is one
/// image; where a later frame differs, the differing blocks of cells are
/// sent as small images laid over it. When too much differs for that to
/// pay, the whole page is sent again and the tiles are removed. Needs a
/// terminal that reads frames from files.
#[derive(Default)]
pub struct Tiles {
    /// The frame on screen, if this drew it.
    shown: Option<Pixels>,
    /// The ids of the tiles lying over the page.
    overlaid: Vec<u32>,
    /// The last draw's files, in case the terminal didn't delete them.
    last: Vec<std::path::PathBuf>,
}

impl Tiles {
    /// Whether the screen holds a page this drew.
    pub fn active(&self) -> bool {
        self.shown.is_some()
    }

    /// The screen was cleared of images: the next draw sends the whole page.
    pub fn forget(&mut self) {
        self.shown = None;
        self.overlaid.clear();
    }

    /// Whether a frame lines up with `cols` x `rows` cells of `cell`
    /// pixels: it must cover them exactly, give or take the pixel or two of
    /// rounding a viewport picks up, which is cropped.
    pub fn fits(pixels: &Pixels, cols: u16, rows: u16, cell: (u32, u32)) -> bool {
        let covers = |have: u32, cells: u16, size: u32| (0..=2).contains(&(have as i64 - cells as i64 * size as i64));
        covers(pixels.width, cols, cell.0) && covers(pixels.height, rows, cell.1)
    }

    /// Append the escapes that bring the screen to `new`, which must fit
    /// (see `fits`). `top` is the screen row of the page's first row.
    /// Returns what was sent, or `None` if a file couldn't be written, when
    /// the screen is in no known state.
    pub fn draw(
        &mut self,
        out: &mut Vec<u8>,
        new: Pixels,
        cols: u16,
        rows: u16,
        cell: (u32, u32),
        top: u16,
    ) -> Option<Drawn> {
        for path in self.last.drain(..) {
            let _ = std::fs::remove_file(path);
        }
        let bytes = if new.alpha { 4 } else { 3 };
        let (cell_w, cell_h) = (cell.0 as usize, cell.1 as usize);
        let new = crop(new, cols as u32 * cell.0, rows as u32 * cell.1, bytes)?;
        let old =
            self.shown.take().filter(|old| (old.width, old.height, old.alpha) == (new.width, new.height, new.alpha));
        let stride = new.width as usize * bytes;

        // (id, column, row, columns, rows) of each tile that differs.
        let mut changed = Vec::new();
        let mut total = 0;
        for row in (0..rows).step_by(TILE_ROWS as usize) {
            for col in (0..cols).step_by(TILE_COLS as usize) {
                let id = FIRST_TILE_ID + total;
                total += 1;
                let Some(old) = &old else { continue };
                let (tile_cols, tile_rows) = (TILE_COLS.min(cols - col), TILE_ROWS.min(rows - row));
                let (x, width) = (col as usize * cell_w * bytes, tile_cols as usize * cell_w * bytes);
                let lines = row as usize * cell_h..(row + tile_rows) as usize * cell_h;
                let same = lines.clone().all(|y| {
                    let (a, b) = (y * stride + x, y * stride + x + width);
                    old.data[a..b] == new.data[a..b]
                });
                if !same {
                    changed.push((id, col, row, tile_cols, tile_rows));
                }
            }
        }

        let drawn = if old.is_none() || changed.len() * 3 > total as usize {
            // Too much for tiles to pay: the page again, and no tiles.
            for id in self.overlaid.drain(..) {
                out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").as_bytes());
            }
            let path = next_path();
            std::fs::write(&path, &new.data).ok()?;
            out.extend_from_slice(format!("\x1b[{top};1H").as_bytes());
            put_raw_file(out, &path, (new.width, new.height), new.alpha, IMAGE_ID, PAGE_Z, cols, rows);
            self.last.push(path);
            Drawn::Whole
        } else {
            for &(id, col, row, tile_cols, tile_rows) in &changed {
                let (x, width) = (col as usize * cell_w * bytes, tile_cols as usize * cell_w * bytes);
                let lines = row as usize * cell_h..(row + tile_rows) as usize * cell_h;
                let mut data = Vec::with_capacity(width * lines.len());
                for y in lines.clone() {
                    data.extend_from_slice(&new.data[y * stride + x..][..width]);
                }
                let path = next_path();
                std::fs::write(&path, data).ok()?;
                out.extend_from_slice(format!("\x1b[{};{}H", top + row, col + 1).as_bytes());
                let size = ((width / bytes) as u32, lines.len() as u32);
                put_raw_file(out, &path, size, new.alpha, id, TILE_Z, tile_cols, tile_rows);
                self.last.push(path);
                if !self.overlaid.contains(&id) {
                    self.overlaid.push(id);
                }
            }
            Drawn::Tiles(changed.len())
        };
        self.shown = Some(new);
        Some(drawn)
    }
}

/// Cut a frame down to `width` x `height`, dropping the right and bottom
/// edges. `None` if it is smaller than that.
fn crop(mut pixels: Pixels, width: u32, height: u32, bytes: usize) -> Option<Pixels> {
    if pixels.width < width
        || pixels.height < height
        || pixels.data.len() != pixels.width as usize * pixels.height as usize * bytes
    {
        return None;
    }
    if pixels.width > width {
        let (from, to) = (pixels.width as usize * bytes, width as usize * bytes);
        for y in 0..height as usize {
            pixels.data.copy_within(y * from..y * from + to, y * to);
        }
    }
    pixels.data.truncate(width as usize * height as usize * bytes);
    (pixels.width, pixels.height) = (width, height);
    Some(pixels)
}

/// The image id of the query `ask_about_files` sends.
pub const FILE_QUERY_ID: u32 = 2;

/// Ask the terminal whether it will read pixels from a temporary file,
/// which is far quicker than sending them through the pty. The answer
/// arrives as an `input::Event::Graphics` with `FILE_QUERY_ID`. A terminal
/// on another machine can't see our files and says no.
pub fn ask_about_files() {
    let path = temp_path(0);
    if std::fs::write(&path, [0, 0, 0]).is_ok() {
        let name = crate::base64::encode(path.as_os_str().as_encoded_bytes());
        super::write(format!("\x1b_Ga=q,i={FILE_QUERY_ID},f=24,s=1,v=1,t=t;{name}\x1b\\").as_bytes());
    }
}

/// Width and height of a base64 PNG, read from its header. For diagnostics.
pub fn png_size(png_base64: &str) -> Option<(u32, u32)> {
    // The size sits in bytes 16..24, inside the first 32 base64 characters.
    let header = crate::base64::decode(png_base64.get(..32)?)?;
    let field = |at: usize| u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    Some((field(16), field(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_png_size() {
        // The first 24 bytes of a 1000x800 PNG.
        assert_eq!(png_size("iVBORw0KGgoAAAANSUhEUgAAA+gAAAMg"), Some((1000, 800)));
        assert_eq!(png_size("short"), None);
    }

    fn put(data: &str) -> String {
        let mut out = Vec::new();
        put_png(&mut out, data, 80, 23);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn small_image_is_one_escape() {
        assert_eq!(put("QUJD"), "\x1b_Ga=T,f=100,i=1,p=1,z=-1073741826,q=2,C=1,c=80,r=23,m=0;QUJD\x1b\\");
    }

    #[test]
    fn large_image_is_chunked() {
        let data = "A".repeat(CHUNK * 2 + 8);
        let out = put(&data);
        let escapes: Vec<&str> = out.split_terminator("\x1b\\").collect();
        assert_eq!(escapes.len(), 3);
        assert!(escapes[0].starts_with("\x1b_Ga=T,") && escapes[0].contains(",m=1;"));
        assert_eq!(escapes[1], format!("\x1b_Gm=1;{}", "A".repeat(CHUNK)));
        assert_eq!(escapes[2], "\x1b_Gm=0;AAAAAAAA");
    }

    fn solid(width: u32, height: u32, value: u8) -> Pixels {
        Pixels { width, height, alpha: false, data: vec![value; (width * height * 3) as usize] }
    }

    #[test]
    fn tiles_send_only_what_changed() {
        // 30x14 cells of 2x4 pixels: 3 tiles across, 3 down.
        let (cols, rows, cell) = (30, 14, (2, 4));
        let mut tiles = Tiles::default();
        let mut out = Vec::new();
        assert!(Tiles::fits(&solid(60, 56, 0), cols, rows, cell));
        assert!(Tiles::fits(&solid(61, 57, 0), cols, rows, cell));
        assert!(!Tiles::fits(&solid(59, 56, 0), cols, rows, cell));
        // The first frame goes whole, a pixel too wide and cropped.
        assert_eq!(tiles.draw(&mut out, solid(61, 56, 0), cols, rows, cell, 2), Some(Drawn::Whole));
        let text = String::from_utf8(std::mem::take(&mut out)).unwrap();
        assert!(text.starts_with("\x1b[2;1H\x1b_Ga=T,f=24,s=60,v=56,t=t,i=1,p=1,z=-1073741826,"), "{text:?}");
        assert_eq!(tiles.draw(&mut out, solid(60, 56, 0), cols, rows, cell, 2), Some(Drawn::Tiles(0)));
        assert!(out.is_empty());

        // One pixel in the last cell: only the bottom right tile goes.
        let mut changed = solid(60, 56, 0);
        *changed.data.last_mut().unwrap() = 1;
        assert_eq!(tiles.draw(&mut out, changed, cols, rows, cell, 2), Some(Drawn::Tiles(1)));
        let text = String::from_utf8(std::mem::take(&mut out)).unwrap();
        // Tile 9 of 9, at column 25 and row 2 + 12, 6 cells by 2, on top.
        assert!(text.starts_with("\x1b[14;25H\x1b_Ga=T,f=24,s=12,v=8,t=t,i=18,p=1,z=-1073741825,"), "{text:?}");
        assert!(text.contains(",c=6,r=2;"), "{text:?}");

        // Everything changes: the page again, and the tile is removed.
        assert_eq!(tiles.draw(&mut out, solid(60, 56, 9), cols, rows, cell, 2), Some(Drawn::Whole));
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b_Ga=d,d=I,i=18,q=2\x1b\\\x1b[2;1H\x1b_Ga=T,"), "{text:?}");
        for path in tiles.last.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn empty_image_sends_nothing() {
        assert_eq!(put(""), "");
    }
}
