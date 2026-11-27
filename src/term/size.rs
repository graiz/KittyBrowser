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
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::*;
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } == 0 {
            return None;
        }
        Some(Window {
            cols: (info.srWindow.Right - info.srWindow.Left + 1) as u16,
            rows: (info.srWindow.Bottom - info.srWindow.Top + 1) as u16,
            width_px: 0,
            height_px: 0,
        })
    }
    #[cfg(unix)]
    {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } < 0 || size.ws_col == 0 || size.ws_row == 0 {
            return None;
        }
        Some(Window { cols: size.ws_col, rows: size.ws_row, width_px: size.ws_xpixel, height_px: size.ws_ypixel })
    }
}
