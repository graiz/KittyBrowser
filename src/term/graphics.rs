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
const DELETE: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// How many times kb has told the terminal to drop every image, so that an
/// image kept for later (the agent's cursor) knows to be sent again.
static WIPES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Append the escape that drops every image, their data too.
pub fn delete_all(out: &mut Vec<u8>) {
    WIPES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    out.extend_from_slice(DELETE);
}

/// How many times every image has been dropped.
pub fn wipes() -> u64 {
    WIPES.load(std::sync::atomic::Ordering::Relaxed)
}

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
    transmit(out, &format!("a=T,f=100,i={IMAGE_ID},p=1,z={PAGE_Z},q=2,C=1,c={cols},r={rows}"), png_base64);
}

/// Append `base64` as an image, in escapes of at most `CHUNK` bytes, the
/// first carrying `keys`.
pub(super) fn transmit(out: &mut Vec<u8>, keys: &str, base64: &str) {
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

/// A decoded frame: rows of RGB or RGBA bytes, top to bottom.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub data: Vec<u8>,
}

/// Decode a PNG into `data`, which is reused if it is large enough: a new
/// frame's worth of memory costs a page fault every 16 KB, a millisecond or
/// two a frame.
pub fn decode_png(bytes: &[u8], mut data: Vec<u8>) -> Option<Pixels> {
    // png 0.18 reads from anything that can seek, so a slice goes in a cursor.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Frames come from Chrome over a pipe and the logo is built in, neither
    // off a disk or a network: checking checksums is a tenth of the time
    // for nothing.
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

pub fn decode_jpeg(bytes: &[u8]) -> Option<Pixels> {
    let mut decoder = zune_jpeg::JpegDecoder::new(zune_jpeg::zune_core::bytestream::ZCursor::new(bytes));
    let data = decoder.decode().ok()?;
    let (width, height) = decoder.dimensions()?;
    (data.len() == width * height * 3).then_some(Pixels {
        width: width as u32,
        height: height as u32,
        alpha: false,
        data,
    })
}

/// Where frames wait for the terminal to read them. The protocol wants them
/// under the temporary directory, with `tty-graphics-protocol` in the path,
/// before it will read, then delete, a file. That directory is shared on
/// Linux, so frames go in a folder of their own made once per run: only the
/// user can open it, and its name can't be guessed, so no one else can read
/// a frame of a signed-in page or put a file or link where kb will write
/// one.
struct Frames {
    folder: Option<std::path::PathBuf>,
    next: u64,
}

impl Frames {
    const fn new() -> Frames {
        Frames { folder: None, next: 1 }
    }

    /// Save `bytes` in a new file for the terminal, and return its path.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
        let mut again = true;
        loop {
            let folder = match &self.folder {
                Some(folder) => folder,
                None => {
                    sweep(&std::env::temp_dir());
                    let prefix = format!("{FRAMES_PREFIX}{}-", std::process::id());
                    self.folder.insert(crate::files::private_temp_dir(&prefix)?)
                }
            };
            let path = folder.join(format!("tty-graphics-protocol-{}", self.next));
            self.next += 1;
            match crate::files::write_new(&path, bytes, 0o600) {
                // The folder went, taken by something clearing the
                // temporary directory: make another, once.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && std::mem::take(&mut again) => {
                    self.folder = None;
                }
                result => return result.map(|_| path),
            }
        }
    }

    /// Remove the folder and whatever the terminal left in it.
    fn remove(&mut self) {
        if let Some(folder) = self.folder.take() {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

/// What frame folders and files are named for: this phrase, then the
/// process id of the kb that wrote them, so the next kb can tell whose run
/// is over.
const FRAMES_PREFIX: &str = "tty-graphics-protocol-kb-";

/// Remove from `dir` the frames of kbs no longer running: the folders of
/// ones that were killed, or whose pane closed, before they could remove
/// their own, and the files earlier kbs wrote straight into it. Only the
/// user's own, and nothing a running kb is using: a kb in a container
/// sharing /tmp has process ids of its own, so a folder written to in the
/// last `SWEEP_AFTER` is left too.
fn sweep(dir: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let me = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(run_of) else { continue };
        // Of the entry itself, not of what a link names.
        let Ok(file) = entry.metadata() else { continue };
        let unchanged = file.modified().ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age >= SWEEP_AFTER);
        if file.uid() != me || !unchanged || running(pid) {
            continue;
        }
        let _ = match file.is_dir() {
            true => std::fs::remove_dir_all(entry.path()),
            false => std::fs::remove_file(entry.path()),
        };
    }
}

/// How long frames sit unchanged before the sweep takes them.
const SWEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The process id in a frame folder or file's name, if it is one.
fn run_of(name: &str) -> Option<i32> {
    let pid: i32 = name.strip_prefix(FRAMES_PREFIX)?.split('-').next()?.parse().ok()?;
    (pid > 0).then_some(pid)
}

/// Whether process `pid` is running, someone else's included.
fn running(pid: i32) -> bool {
    pid as u32 == std::process::id()
        || unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

static FRAMES: std::sync::Mutex<Frames> = std::sync::Mutex::new(Frames::new());

/// Save a frame's pixels for the terminal to read (see `Frames`), and
/// return the file's path.
pub fn write_frame(bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    FRAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).write(bytes)
}

/// Remove the frames' folder, as kb leaves the terminal.
pub fn remove_frames() {
    // Not waited for: leave runs from the panic hook too, and a panic while
    // writing a frame holds the lock. Then the next kb's sweep removes them.
    match FRAMES.try_lock() {
        Ok(mut frames) => frames.remove(),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner().remove(),
        Err(std::sync::TryLockError::WouldBlock) => {}
    }
}

/// Remove the frames' folder once it is empty: tests draw at the same time,
/// and one finishing mustn't take the folder from under another.
#[cfg(test)]
pub fn tidy_frames() {
    let mut frames = FRAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if frames.folder.as_ref().is_some_and(|folder| std::fs::remove_dir(folder).is_ok()) {
        frames.folder = None;
    }
}

/// Append the escape that draws the pixels saved at `path` (see `write_frame`)
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

/// Whether a frame of `size` pixels lines up with `cols` x `rows` cells of
/// `cell` pixels, as `Tiles` and `layers::Layers` need: it must cover them
/// exactly, give or take the pixel or two of rounding a viewport picks up,
/// which is cropped.
pub fn fits(size: (u32, u32), cols: u16, rows: u16, cell: (u32, u32)) -> bool {
    let covers = |have: u32, cells: u16, size: u32| (0..=2).contains(&(have as i64 - cells as i64 * size as i64));
    covers(size.0, cols, cell.0) && covers(size.1, rows, cell.1)
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
    /// The screen was cleared of images: the next draw sends the whole page.
    pub fn forget(&mut self) {
        self.shown = None;
        self.overlaid.clear();
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
            let path = write_frame(&new.data).ok()?;
            out.extend_from_slice(format!("\x1b[{top};1H").as_bytes());
            put_raw_file(out, &path, (new.width, new.height), new.alpha, IMAGE_ID, PAGE_Z, cols, rows);
            self.last.push(path);
            Drawn::Whole
        } else {
            for &(id, col, row, tile_cols, tile_rows) in &changed {
                let (x, y) = (col as u32 * cell.0, row as u32 * cell.1);
                let tile = cut(&new, x, y, tile_cols as u32 * cell.0, tile_rows as u32 * cell.1);
                let path = write_frame(&tile.data).ok()?;
                out.extend_from_slice(format!("\x1b[{};{}H", top + row, col + 1).as_bytes());
                let size = (tile.width, tile.height);
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

/// The pixels of the `width` x `height` rectangle of `pixels` whose top
/// left is at `x`, `y`.
pub(super) fn cut(pixels: &Pixels, x: u32, y: u32, width: u32, height: u32) -> Pixels {
    let bytes = if pixels.alpha { 4 } else { 3 };
    let stride = pixels.width as usize * bytes;
    let mut data = Vec::with_capacity(width as usize * height as usize * bytes);
    for row in y as usize..(y + height) as usize {
        data.extend_from_slice(&pixels.data[row * stride + x as usize * bytes..][..width as usize * bytes]);
    }
    Pixels { width, height, alpha: pixels.alpha, data }
}

/// Cut a frame down to `width` x `height`, dropping the right and bottom
/// edges. `None` if it is smaller than that.
pub(super) fn crop(mut pixels: Pixels, width: u32, height: u32, bytes: usize) -> Option<Pixels> {
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
/// on another machine can't see our files and says no. Returns the file
/// asked about, to remove should the terminal not.
pub fn ask_about_files() -> Option<std::path::PathBuf> {
    let path = write_frame(&[0, 0, 0]).ok()?;
    let name = crate::base64::encode(path.as_os_str().as_encoded_bytes());
    super::write(format!("\x1b_Ga=q,i={FILE_QUERY_ID},f=24,s=1,v=1,t=t;{name}\x1b\\").as_bytes());
    Some(path)
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
    fn decodes_png_and_jpeg() {
        let png = decode_png(include_bytes!("../../assets/logo.png"), Vec::new()).expect("the logo decodes");
        assert_eq!((png.width, png.height), (570, 435));
        assert_eq!(png.data.len(), 570 * 435 * if png.alpha { 4 } else { 3 });
        if let Ok(path) = std::env::var("KB_TEST_JPEG") {
            let jpeg = decode_jpeg(&std::fs::read(path).unwrap()).expect("the jpeg decodes");
            assert_eq!(jpeg.data.len() as u32, jpeg.width * jpeg.height * 3);
            println!("jpeg {}x{}", jpeg.width, jpeg.height);
        }
    }

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
        assert!(fits((60, 56), cols, rows, cell));
        assert!(fits((61, 57), cols, rows, cell));
        assert!(!fits((59, 56), cols, rows, cell));
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
        tidy_frames();
    }

    #[test]
    fn frames_are_private_and_go_with_their_folder() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path| path.metadata().unwrap().permissions().mode() & 0o777;
        let mut frames = Frames::new();
        let first = frames.write(b"first").unwrap();
        let second = frames.write(b"second").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        assert_eq!(mode(&first), 0o600);
        let folder = first.parent().unwrap();
        assert_eq!(mode(folder), 0o700);
        // Where the terminal will read them: under the temporary directory,
        // with the protocol's phrase in the name.
        assert!(folder.starts_with(std::env::temp_dir()));
        assert!(first.to_str().unwrap().contains("tty-graphics-protocol"));
        // Another run's folder is another name.
        let mut other = Frames::new();
        assert_ne!(other.write(b"").unwrap().parent(), Some(folder));
        other.remove();
        // Something clearing the temporary directory takes the folder: the
        // next frame goes in a new one.
        std::fs::remove_dir_all(folder).unwrap();
        let third = frames.write(b"third").unwrap();
        assert_ne!(third.parent(), Some(folder));
        assert_eq!(std::fs::read(&third).unwrap(), b"third");
        frames.remove();
        assert!(!first.exists() && !second.exists() && !third.exists());
        assert!(!folder.exists());
    }

    #[test]
    fn leaving_while_a_frame_is_written_doesnt_wait() {
        // As when the panic hook runs inside Frames::write.
        let held = FRAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_frames();
        drop(held);
    }

    #[test]
    fn frames_of_runs_that_are_over_are_swept() {
        assert_eq!(run_of("tty-graphics-protocol-kb-123-aZ09xy"), Some(123));
        assert_eq!(run_of("tty-graphics-protocol-kb-123-7"), Some(123));
        assert_eq!(run_of("tty-graphics-protocol-kb-0-7"), None);
        assert_eq!(run_of("tty-graphics-protocol-kb-99999999999-7"), None);
        assert_eq!(run_of("tty-graphics-protocol-other-1"), None);
        assert_eq!(run_of("kb-history"), None);

        let dir = std::env::temp_dir().join(format!("kb-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let gone = child.id();
        child.wait().unwrap();
        let mine = std::process::id();
        let killed = dir.join(format!("tty-graphics-protocol-kb-{gone}-AbC123"));
        std::fs::create_dir(&killed).unwrap();
        std::fs::write(killed.join("tty-graphics-protocol-4"), "frame").unwrap();
        let old = dir.join(format!("tty-graphics-protocol-kb-{gone}-17"));
        std::fs::write(&old, "frame").unwrap();
        let live = dir.join(format!("tty-graphics-protocol-kb-{mine}-XyZ789"));
        std::fs::create_dir(&live).unwrap();
        let other = dir.join("tty-graphics-protocol-other");
        std::fs::write(&other, "").unwrap();
        // Written to just now, perhaps by a kb in a container.
        let fresh = dir.join(format!("tty-graphics-protocol-kb-{gone}-FrEsH1"));
        std::fs::create_dir(&fresh).unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for path in [&killed, &old, &live, &other] {
            std::fs::File::open(path).unwrap().set_modified(hour_ago).unwrap();
        }
        sweep(&dir);
        assert!(!killed.exists() && !old.exists());
        assert!(live.exists() && other.exists() && fresh.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_image_sends_nothing() {
        assert_eq!(put(""), "");
    }
}
