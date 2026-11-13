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
    /// Which drew the images on screen, if any.
    on_screen: Option<Drawer>,
}

/// How frames reach the terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// In files, which the terminal reads itself.
    Files,
    /// Through the pty, only what moved or changed. This needs placements
    /// with source rectangles, offsets within cells, z and `o=z`.
    Layers,
    /// Through the pty, whole PNG frames only (`KB_TRANSFER=png`): for a
    /// terminal with no more of the kitty protocol than that.
    Png,
}

/// The ways of drawing the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drawer {
    Tiles,
    Layers,
    /// One image, scaled to fit the cells.
    Whole,
}

impl PageImage {
    /// The screen was cleared of images: the next draw starts again.
    pub fn forget(&mut self) {
        self.tiles.forget();
        self.layers.forget();
        self.on_screen = None;
    }

    /// The terminal reported an error for image `id`. True if the page was
    /// drawn with it, and has been forgotten: the caller should clear the
    /// screen of images and draw the frame again.
    pub fn lost(&mut self, id: u32) -> bool {
        let lost = self.layers.lost(id);
        if lost {
            self.forget();
        }
        lost
    }

    /// Hand the screen to `drawer`. Each knows only its own images, so when
    /// another drew what is there, all of it goes first: left, it could
    /// cover what the new one draws.
    fn hand_to(&mut self, out: &mut Vec<u8>, drawer: Drawer) {
        if self.on_screen != Some(drawer) {
            out.extend_from_slice(graphics::DELETE);
            self.forget();
            self.on_screen = Some(drawer);
        }
    }

    /// Remove the last frame's file, should the terminal not have.
    pub fn close(&mut self) {
        if let Some(path) = self.last_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Append the escapes that draw `png`, one of Chrome's frames, over
    /// `cols` x `rows` cells of `cell` pixels from screen row `top`.
    /// `scaled` says the frame is a fixed viewport, scaled to fit. Returns
    /// what was sent, to log.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        out: &mut Vec<u8>,
        png: &str,
        route: Route,
        scaled: bool,
        cols: u16,
        rows: u16,
        cell: (u32, u32),
        top: u16,
    ) -> String {
        let mut pixels = match route {
            Route::Files => crate::base64::decode(png).and_then(|bytes| graphics::decode_png(&bytes, Vec::new())),
            Route::Layers | Route::Png => None,
        };
        // Only what changed, when the frame lines up with the cells: tiles
        // by file, or layers through the pty. Otherwise (a fixed viewport,
        // a frame from before a resize) one image scaled to fit.
        let lined_up = !scaled && graphics::png_size(png).is_some_and(|size| graphics::fits(size, cols, rows, cell));
        let drawer = match &pixels {
            Some(_) if lined_up => Drawer::Tiles,
            None if lined_up && route == Route::Layers => Drawer::Layers,
            _ => Drawer::Whole,
        };
        self.hand_to(out, drawer);
        let drawn = match drawer {
            Drawer::Tiles => pixels
                .take()
                .and_then(|pixels| self.tiles.draw(out, pixels, cols, rows, cell, top))
                .map(|sent| format!("{sent:?}")),
            Drawer::Layers => self.layers.draw(out, png, cols, rows, cell, top).map(|sent| format!("{sent:?}")),
            Drawer::Whole => None,
        };
        if let Some(drawn) = drawn {
            return drawn;
        }
        // Tiles couldn't write a file, or layers decode the frame: the
        // screen is in no known state, so it is cleared for the frame whole.
        self.hand_to(out, Drawer::Whole);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::layers::tests::{encode, frame, Screen};

    // 4 columns and 10 rows of 4x4 pixel cells, from screen row 2.
    const COLS: u16 = 4;
    const ROWS: u16 = 10;
    const CELL: (u32, u32) = (4, 4);

    /// A frame `width` pixels wide whose rows differ, `scrolled` down.
    fn page(width: u32, scrolled: u32) -> graphics::Pixels {
        frame(width, 40, |x, y| ((y + scrolled) * 7 % 251 + x) as u8)
    }

    /// Draw `new` and check the screen then shows it, and only it. Returns
    /// the escapes sent.
    fn shows(image: &mut PageImage, screen: &mut Screen, new: &graphics::Pixels, route: Route) -> String {
        let mut out = Vec::new();
        let drawn = image.draw(&mut out, &encode(new), route, false, COLS, ROWS, CELL, 2);
        screen.feed(&out, CELL);
        assert!(screen.page(2, CELL, 16, 40) == new.data, "after {drawn}");
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_frame_drawn_whole_goes_when_layers_take_over() {
        // A frame a column narrow, as one rendered before a resize, goes
        // whole between two that layers draw.
        let (mut image, mut screen) = (PageImage::default(), Screen::default());
        shows(&mut image, &mut screen, &page(16, 0), Route::Layers);
        let mut out = Vec::new();
        image.draw(&mut out, &encode(&page(12, 5)), Route::Layers, false, COLS, ROWS, CELL, 2);
        screen.feed(&out, CELL);
        shows(&mut image, &mut screen, &page(16, 10), Route::Layers);
    }

    #[test]
    fn tiles_take_over_from_layers() {
        // The terminal's answer that it reads files comes after layers
        // drew the first frame.
        let (mut image, mut screen) = (PageImage::default(), Screen::default());
        shows(&mut image, &mut screen, &page(16, 0), Route::Layers);
        shows(&mut image, &mut screen, &page(16, 3), Route::Files);
        shows(&mut image, &mut screen, &page(16, 6), Route::Files);
    }

    #[test]
    fn png_frames_only_when_asked() {
        // Each frame whole, as Chrome sent it, scaled to the cells: no
        // placements, offsets or compressed pixels.
        let (mut image, mut screen) = (PageImage::default(), Screen::default());
        for scrolled in [0, 3, 6] {
            let text = shows(&mut image, &mut screen, &page(16, scrolled), Route::Png);
            assert!(text.contains("\x1b[2;1H\x1b_Ga=T,f=100,i=1,p=1,"), "{text:?}");
            assert!(text.contains(",c=4,r=10,"), "{text:?}");
            for key in ["a=p", "o=z", "X=", "Y="] {
                assert!(!text.contains(key), "{text:?}");
            }
        }
    }
}
