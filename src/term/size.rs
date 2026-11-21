//! Pane size in cells and pixels.
//!
//! A new Rex pane reports 80x24 for a moment before
//! it is laid out, so the app polls `window` rather than trusting the first
//! reading.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    pub cols: u16,
    pub rows: u16,
    /// Zero when the terminal doesn't fill in the pixel fields.
    pub width_px: u16,
    pub height_px: u16,
}

pub fn window() -> Option<Window> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } < 0 || size.ws_col == 0 || size.ws_row == 0 {
        return None;
    }
    Some(Window { cols: size.ws_col, rows: size.ws_row, width_px: size.ws_xpixel, height_px: size.ws_ypixel })
}
