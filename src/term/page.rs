//! The page's image: changed tiles by file, moved layers through the pty,
//! or one image scaled to fit, whichever suits the terminal and the frame.

use super::graphics::{self, Tiles};
use super::layers::Layers;

#[derive(Default)]
pub struct PageImage {
    /// The page as tiles, when it is drawn that way.
    tiles: Tiles,
    /// The page through the pty, moved as it scrolls and only what changed
    /// sent, when it is drawn that way (the terminal can't read files, as
    /// over ssh).
    layers: Layers,
    /// The last frame written whole to a file.
    last_file: Option<std::path::PathBuf>,
}

impl PageImage {
    /// The screen was cleared of images: the next draw starts again.
    pub fn forget(&mut self) {
        self.tiles.forget();
        self.layers.forget();
    }

    /// The terminal reported an error for image `id`. True if the page was
    /// drawn with it, and has been forgotten: the caller should clear the
    /// screen of images and draw the frame again.
    pub fn lost(&mut self, id: u32) -> bool {
        self.layers.lost(id)
    }

    /// Remove the last frame's file, should the terminal not have.
    pub fn close(&mut self) {
        if let Some(path) = self.last_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Append the escapes that draw `png`, one of Chrome's frames, over
    /// `cols` x `rows` cells of `cell` pixels from screen row `top`.
    /// `by_file` says the terminal reads files; `scaled`, that the frame is
    /// a fixed viewport, scaled to fit. Returns what was sent, to log.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        out: &mut Vec<u8>,
        png: &str,
        by_file: bool,
        scaled: bool,
        cols: u16,
        rows: u16,
        cell: (u32, u32),
        top: u16,
    ) -> String {
        let pixels = match by_file {
            true => crate::base64::decode(png).and_then(|bytes| graphics::decode_png(&bytes, Vec::new())),
            false => None,
        };
        // Only what changed, when the frame lines up with the cells;
        // otherwise (a fixed viewport, or a terminal that won't read
        // files) one image scaled to fit.
        let mut drawn = None;
        let pixels = match pixels {
            Some(pixels) if !scaled && graphics::fits((pixels.width, pixels.height), cols, rows, cell) => {
                if !self.tiles.active() {
                    out.extend_from_slice(graphics::DELETE);
                }
                match self.tiles.draw(out, pixels, cols, rows, cell, top) {
                    Some(sent) => drawn = Some(format!("{sent:?}")),
                    None => out.extend_from_slice(graphics::DELETE),
                }
                None
            }
            other => other,
        };
        // Through the pty, only what changed, when the frame lines up.
        if pixels.is_none() && !by_file && !scaled {
            match self.layers.draw(out, png, cols, rows, cell, top) {
                Some(sent) => drawn = Some(format!("{sent:?}")),
                None if self.layers.active() => {
                    out.extend_from_slice(graphics::DELETE);
                    self.layers.forget();
                }
                None => {}
            }
        } else if self.layers.active() {
            out.extend_from_slice(graphics::DELETE);
            self.layers.forget();
        }
        if let Some(drawn) = drawn {
            return drawn;
        }
        if self.tiles.active() {
            out.extend_from_slice(graphics::DELETE);
            self.tiles.forget();
        }
        out.extend_from_slice(format!("\x1b[{top};1H").as_bytes());
        let path = graphics::next_path();
        match pixels.filter(|pixels| std::fs::write(&path, &pixels.data).is_ok()) {
            Some(pixels) => {
                graphics::put_file(out, &path, &pixels, cols, rows);
                // The terminal deletes each file once read; this covers one
                // it didn't get to.
                if let Some(old) = self.last_file.replace(path) {
                    let _ = std::fs::remove_file(old);
                }
                "the whole frame, by file".to_owned()
            }
            None => {
                graphics::put_png(out, png, cols, rows);
                "the whole frame, through the pty".to_owned()
            }
        }
    }
}
