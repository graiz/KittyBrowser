//! Page engines: turn a URL into pixels and take input back.
//!
//! Everything else in kb talks to the `Engine` trait, so backends can be
//! swapped. Planned backends are described in docs/ARCHITECTURE.md.

#![allow(dead_code)]

/// One rendered frame of the page, as tightly packed RGBA.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Input forwarded from the terminal to the page, in page pixels.
pub enum Input {
    Click { x: u32, y: u32 },
    Scroll { dy: i32 },
    Key(char),
}

pub trait Engine {
    fn navigate(&mut self, url: &str) -> Result<(), String>;
    /// Set the viewport in device pixels plus the display scale factor.
    fn resize(&mut self, width: u32, height: u32, scale: f32);
    fn input(&mut self, input: Input);
    /// The newest frame, if the page changed since the last call.
    fn frame(&mut self) -> Option<Frame>;
}
