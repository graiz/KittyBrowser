//! The logo shown in the middle of a black pane until the first page is
//! drawn, with a light travelling round its ring, as Internet Explorer's
//! logo spun while it loaded.

use crate::term::graphics::{self, Pixels};
use std::time::{Duration, Instant};

const LOGO: &[u8] = include_bytes!("../assets/logo.png");

/// The image id the logo is drawn under.
const ID: u32 = 3;
/// Frames in one trip round the ring.
const FRAMES: usize = 48;
/// The time between frames, and how long to animate before holding still:
/// with no page asked for, the logo stays as a home screen and shouldn't
/// cost anything once the user has seen it.
pub const FRAME_GAP: Duration = Duration::from_millis(60);
const ANIMATE_FOR: Duration = Duration::from_secs(12);

/// The logo is drawn from assets/logo.svg, whose viewBox starts here and is
/// this wide; logo.png is rendered from it with
/// `rsvg-convert -w 570 logo.svg | magick - PNG32:logo.png`.
const SVG_ORIGIN: (f64, f64) = (15.0, 60.0);
const SVG_WIDTH: f64 = 380.0;
/// The ring in the SVG: centre, half widths and tilt in degrees.
const RING: (f64, f64, f64, f64, f64) = (197.0, 212.0, 152.0, 38.0, -15.0);
/// The sphere the ring goes behind: centre and radius.
const SPHERE: (f64, f64, f64) = (205.0, 225.0, 100.0);
/// The pane's background while the logo shows: the logo's own black.
const BLACK: &str = "\x1b[0;48;2;0;0;0m";

pub struct Splash {
    frames: Vec<Pixels>,
    next: usize,
    started: Instant,
    last: Option<Instant>,
}

impl Splash {
    pub fn new() -> Option<Splash> {
        let logo = graphics::decode_png(LOGO).filter(|logo| logo.alpha)?;
        let frames = (0..FRAMES).map(|n| frame(&logo, n)).collect();
        Some(Splash { frames, next: 0, started: Instant::now(), last: None })
    }

    /// Whether it is time for the next frame.
    pub fn due(&self) -> bool {
        match self.last {
            None => true,
            Some(_) if self.started.elapsed() > ANIMATE_FOR => false,
            Some(last) => last.elapsed() >= FRAME_GAP,
        }
    }

    /// How long until the next frame is due, if one will be.
    pub fn wait(&self) -> Option<Duration> {
        match self.last {
            None => Some(Duration::ZERO),
            Some(_) if self.started.elapsed() > ANIMATE_FOR => None,
            Some(last) => Some(FRAME_GAP.saturating_sub(last.elapsed())),
        }
    }

    /// Append the escapes that draw the next frame in the middle of a
    /// `cols` x `rows` area of `cell`-pixel cells whose top row is `top`.
    /// Needs a terminal that reads files.
    pub fn draw(&mut self, out: &mut Vec<u8>, cols: u16, rows: u16, cell: (u32, u32), top: u16) {
        let still = self.last.is_some() && self.started.elapsed() > ANIMATE_FOR;
        let pixels = &self.frames[if still { FRAMES / 4 } else { self.next }];
        self.next = (self.next + 1) % FRAMES;
        self.last = Some(Instant::now());

        // Two fifths of the smaller side, and never larger than the logo
        // itself shown at twice its size.
        let space = (cols as f64 * cell.0 as f64).min(rows as f64 * cell.1 as f64);
        let size = (space * 0.4).min(pixels.width as f64 * 2.0);
        let width = ((size / cell.0 as f64).round() as u16).clamp(1, cols);
        let height = ((size * pixels.height as f64 / pixels.width as f64 / cell.1 as f64).round() as u16).clamp(1, rows);
        let (col, row) = ((cols - width) / 2 + 1, top + (rows - height) / 2);

        // Paint the pane black around the logo.
        let blank = " ".repeat(cols as usize);
        for line in top..top + rows {
            out.extend_from_slice(format!("\x1b[{line};1H{BLACK}{blank}").as_bytes());
        }
        out.extend_from_slice(b"\x1b[0m");

        let path = graphics::next_path();
        if std::fs::write(&path, &pixels.data).is_ok() {
            out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
            graphics::put_image_file(out, &path, pixels, ID, width, height);
        }
    }

    /// Append the escapes that remove the logo and its black, from the row
    /// `top` down. The page is drawn under cells with a colour of their own,
    /// so they must go before it can show.
    pub fn remove(out: &mut Vec<u8>, top: u16) {
        out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={ID},q=2\x1b\\\x1b[0m\x1b[{top};1H\x1b[J").as_bytes());
    }
}

/// Where the light is at `turn` (0 to 1 round the ring), in SVG units, and
/// whether that part of the ring is hidden behind the sphere.
fn on_ring(turn: f64) -> (f64, f64, bool) {
    let (cx, cy, a, b, tilt) = RING;
    let angle = turn * std::f64::consts::TAU;
    let (x, y) = (a * angle.cos(), b * angle.sin());
    let tilt = tilt.to_radians();
    let (px, py) = (cx + x * tilt.cos() - y * tilt.sin(), cy + x * tilt.sin() + y * tilt.cos());
    // The far half of the ring is the upper one; where it crosses the
    // sphere it is behind it.
    let (sx, sy, r) = SPHERE;
    let hidden = angle.sin() < 0.0 && (px - sx).hypot(py - sy) < r * 0.92;
    (px, py, hidden)
}

/// The logo with the light `n` steps round the ring, trailing a short tail.
fn frame(logo: &Pixels, n: usize) -> Pixels {
    let mut pixels = Pixels { width: logo.width, height: logo.height, alpha: true, data: logo.data.clone() };
    for (step, strength) in [(0.0, 1.0), (0.012, 0.55), (0.024, 0.3), (0.036, 0.15)] {
        let turn = n as f64 / FRAMES as f64 - step;
        let (x, y, hidden) = on_ring(turn.rem_euclid(1.0));
        let strength = if hidden { strength * 0.12 } else { strength };
        let scale = logo.width as f64 / SVG_WIDTH;
        glow(&mut pixels, (x - SVG_ORIGIN.0) * scale, (y - SVG_ORIGIN.1) * scale, scale, strength);
    }
    pixels
}

/// Add a soft white-violet light at (`x`, `y`), sized for a logo drawn at
/// `scale` pixels per SVG unit.
fn glow(pixels: &mut Pixels, x: f64, y: f64, scale: f64, strength: f64) {
    let radius = 16.0 * scale;
    let (width, height) = (pixels.width as i64, pixels.height as i64);
    let reach = (radius * 2.5) as i64;
    for py in (y as i64 - reach).max(0)..(y as i64 + reach).min(height) {
        for px in (x as i64 - reach).max(0)..(x as i64 + reach).min(width) {
            let d = ((px as f64 - x).powi(2) + (py as f64 - y).powi(2)) / (radius * radius);
            let light = strength * ((-d * 2.0).exp() + 0.6 * (-d * 8.0).exp());
            if light < 0.004 {
                continue;
            }
            let at = (py * width + px) as usize * 4;
            let rgba = &mut pixels.data[at..at + 4];
            for (channel, tint) in rgba[..3].iter_mut().zip([235.0, 220.0, 255.0]) {
                *channel = (*channel as f64 + tint * light).min(255.0) as u8;
            }
            rgba[3] = rgba[3].max((light * 255.0).min(255.0) as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_decodes_and_the_light_moves() {
        let splash = Splash::new().expect("the logo is an RGBA PNG");
        assert_eq!(splash.frames.len(), FRAMES);
        assert_ne!(splash.frames[0].data, splash.frames[FRAMES / 2].data);
        // The near half of the ring is in front, the far half partly behind.
        assert!(!on_ring(0.25).2);
        assert!(on_ring(0.75).2);
    }
}
