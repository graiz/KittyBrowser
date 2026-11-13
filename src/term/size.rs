//! Pane size in cells and pixels.
//!
//! A new Rex pane reports 80x24 for a moment before
//! it is laid out, so the app polls `window` rather than trusting the first
//! reading.

use std::process::Command;

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

/// Cell size in device pixels as Rex reports it for this pane, when running
/// in Rex.
pub fn rex_cell() -> Option<(u32, u32)> {
    let session = std::env::var("REX_SESSION").ok()?;
    let block = std::env::var("REX_BLOCK").ok()?;
    let output = Command::new("rex").args(["block", "call", "-s", &session, "-b", &block, "size"]).output().ok()?;
    let size: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let width = size["cell_width_px"].as_u64().filter(|w| *w > 0)?;
    let height = size["cell_height_px"].as_u64().filter(|h| *h > 0)?;
    Some((width as u32, height as u32))
}
