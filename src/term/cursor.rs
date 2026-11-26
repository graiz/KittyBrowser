//! The agent's cursor: an orange pointer drawn over the page while an agent
//! drives kb, as the address field turns orange. It drifts to where the
//! agent clicks, hovers or types, wiggles there slowly between commands,
//! and fades away when the address field goes back. It is
//! an image kb places in the terminal, not part of the page, so the page,
//! its screenshots and its HTML never see it.

use super::graphics::{self, Pixels};
use std::time::{Duration, Instant};

/// The image id the cursor is drawn under.
const ID: u32 = 5;
/// Above the page and what is laid over it, below text, so a box drawn
/// over the page still covers it.
const Z: i64 = -1;
/// How long a drift to a new point takes: slow, as a hand moves.
const DRIFT: Duration = Duration::from_millis(1500);
/// How far it wiggles while it waits, across and down, in cells' heights
/// (about 8 and 5 points), and how slowly: a loop that takes this many
/// seconds.
const WIGGLE: (f64, f64) = (0.45, 0.6);
const WIGGLE_PERIOD: f64 = 8.0;
/// How tall the pointer is, in cells' heights.
const SIZE: f64 = 1.6;
/// How long it takes to fade away.
pub const FADE: Duration = Duration::from_millis(1000);
/// The time between frames while it moves or floats.
pub const FRAME_GAP: Duration = Duration::from_millis(33);

/// The pointer's outline, as an arrow with its tip at (0, 0), in units of
/// a twelfth of its width; its height is 19 of them.
const ARROW: [(f64, f64); 7] =
    [(0.0, 0.0), (0.0, 16.0), (4.0, 12.5), (6.8, 19.0), (9.3, 18.0), (6.6, 11.6), (11.5, 11.6)];
const ORANGE: [u8; 3] = [230, 126, 34];
const EDGE: [u8; 3] = [255, 255, 255];

/// The pointer, `height` pixels tall: orange, edged in white so it shows on
/// any page, smoothed at its edges.
pub fn arrow(height: u32) -> Pixels {
    let unit = height as f64 / 19.0;
    let edge = (unit * 0.9).max(1.0);
    let width = (12.0 * unit + edge * 2.0).ceil() as u32;
    let height = height + (edge * 2.0).ceil() as u32;
    let points: Vec<(f64, f64)> = ARROW.iter().map(|(x, y)| (x * unit + edge, y * unit + edge)).collect();
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for py in 0..height {
        for px in 0..width {
            // Four samples a side, for smooth edges.
            let (mut fill, mut rim) = (0.0, 0.0);
            for sy in 0..4 {
                for sx in 0..4 {
                    let at = (px as f64 + (sx as f64 + 0.5) / 4.0, py as f64 + (sy as f64 + 0.5) / 4.0);
                    if inside(&points, at) {
                        fill += 1.0 / 16.0;
                    } else if distance(&points, at) <= edge {
                        rim += 1.0 / 16.0;
                    }
                }
            }
            let alpha = fill + rim;
            let color = |c: usize| {
                if alpha > 0.0 {
                    ((ORANGE[c] as f64 * fill + EDGE[c] as f64 * rim) / alpha) as u8
                } else {
                    0
                }
            };
            data.extend_from_slice(&[color(0), color(1), color(2), (alpha * 255.0).round() as u8]);
        }
    }
    Pixels { width, height, alpha: true, data }
}

/// Whether `at` is inside the polygon `points`.
fn inside(points: &[(f64, f64)], at: (f64, f64)) -> bool {
    let mut inside = false;
    for (n, &(x1, y1)) in points.iter().enumerate() {
        let (x2, y2) = points[(n + 1) % points.len()];
        if (y1 > at.1) != (y2 > at.1) && at.0 < x1 + (at.1 - y1) / (y2 - y1) * (x2 - x1) {
            inside = !inside;
        }
    }
    inside
}

/// How far `at` is from the polygon's outline.
fn distance(points: &[(f64, f64)], at: (f64, f64)) -> f64 {
    let mut nearest = f64::MAX;
    for (n, &(x1, y1)) in points.iter().enumerate() {
        let (x2, y2) = points[(n + 1) % points.len()];
        let (dx, dy) = (x2 - x1, y2 - y1);
        let t = (((at.0 - x1) * dx + (at.1 - y1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        nearest = nearest.min(((at.0 - x1 - t * dx).powi(2) + (at.1 - y1 - t * dy).powi(2)).sqrt());
    }
    nearest
}

/// Where the cursor is, and where it is going: in pixels from the pane's
/// top left, the point being the pointer's tip.
pub struct AgentCursor {
    from: (f64, f64),
    to: (f64, f64),
    /// When the latest drift began.
    moved: Instant,
    /// The height and the opacity, in tenths, the image was sent at, once
    /// the terminal has it, and how many times every image had been dropped
    /// then: once that changes, the terminal hasn't got it any more.
    sent: Option<(u32, u32, u64)>,
    /// Whether it is on screen.
    shown: bool,
    last_frame: Option<Instant>,
}

impl AgentCursor {
    pub fn new(at: (f64, f64)) -> AgentCursor {
        AgentCursor { from: at, to: at, moved: Instant::now(), sent: None, shown: false, last_frame: None }
    }

    /// Drift from where it is now to `to`.
    pub fn aim(&mut self, to: (f64, f64)) {
        let now = Instant::now();
        self.from = self.position(now, (0.0, 0.0));
        self.to = to;
        self.moved = now;
    }

    /// Where the tip is at `now`, eased along its drift, and once there,
    /// wiggling slowly within `wiggle` (pixels across and down) of it.
    pub fn position(&self, now: Instant, wiggle: (f64, f64)) -> (f64, f64) {
        let t = (now.saturating_duration_since(self.moved).as_secs_f64() / DRIFT.as_secs_f64()).min(1.0);
        // Fast at first, slowing as it arrives.
        let eased = 1.0 - (1.0 - t).powi(3);
        let x = self.from.0 + (self.to.0 - self.from.0) * eased;
        let y = self.from.1 + (self.to.1 - self.from.1) * eased;
        if t < 1.0 {
            return (x, y);
        }
        // A slow figure of eight, from the point and back to it, easing in
        // so it doesn't jump on arrival.
        let waiting = now.saturating_duration_since(self.moved).saturating_sub(DRIFT).as_secs_f64();
        let angle = waiting * std::f64::consts::TAU / WIGGLE_PERIOD;
        let grow = (waiting / 2.0).min(1.0);
        (x + grow * wiggle.0 * angle.sin(), y + grow * wiggle.1 * (2.0 * angle).sin() / 2.0)
    }

    /// Whether a frame is due.
    pub fn due(&self) -> bool {
        let gap = if super::graphics() == super::Graphics::Sixel { Duration::from_millis(150) } else { FRAME_GAP };
        self.last_frame.is_none_or(|at| at.elapsed() >= gap)
    }

    pub fn painted(&mut self) {
        self.last_frame = Some(Instant::now());
    }
    /// Append the escapes that place the cursor for this moment, in a pane
    /// of `cell`-pixel cells, at `opacity` (1 for solid), sending its image
    /// first if the terminal hasn't got it so.
    pub fn draw(&mut self, out: &mut Vec<u8>, cell: (u32, u32), opacity: f64) {
        let (cell_w, cell_h) = (cell.0.max(1) as f64, cell.1.max(1) as f64);
        let height = (cell_h * SIZE).round().max(8.0) as u32;
        let tenths = (opacity.clamp(0.0, 1.0) * 10.0).round() as u32;
        if self.sent != Some((height, tenths, graphics::wipes())) {
            let mut pixels = arrow(height);
            for alpha in pixels.data.iter_mut().skip(3).step_by(4) {
                *alpha = (*alpha as u32 * tenths / 10) as u8;
            }
            let keys = format!("a=t,f=32,s={},v={},i={ID},q=2", pixels.width, pixels.height);
            graphics::transmit(out, &keys, &crate::base64::encode(&pixels.data));
            self.sent = Some((height, tenths, graphics::wipes()));
        }
        let (x, y) = self.position(Instant::now(), (cell_h * WIGGLE.0, cell_h * WIGGLE.1));
        let (x, y) = (x.max(0.0), y.max(0.0));
        let (col, row) = ((x / cell_w) as u32, (y / cell_h) as u32);
        let (dx, dy) = ((x - col as f64 * cell_w) as u32, (y - row as f64 * cell_h) as u32);
        out.extend_from_slice(
            format!("\x1b[{};{}H\x1b_Ga=p,i={ID},p=1,X={dx},Y={dy},z={Z},C=1,q=2\x1b\\", row + 1, col + 1).as_bytes(),
        );
        self.shown = true;
        self.last_frame = Some(Instant::now());
    }

    /// Append the escape that takes the cursor off the screen.
    pub fn hide(&mut self, out: &mut Vec<u8>) {
        if self.shown {
            out.extend_from_slice(format!("\x1b_Ga=d,d=i,i={ID},q=2\x1b\\").as_bytes());
            self.shown = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arrow_is_orange_inside_and_clear_around() {
        let arrow = arrow(38);
        let pixel = |x: u32, y: u32| {
            let at = ((y * arrow.width + x) * 4) as usize;
            arrow.data[at..at + 4].to_vec()
        };
        assert_eq!(arrow.data.len(), (arrow.width * arrow.height * 4) as usize);
        // Well inside, near the left edge a third of the way down: orange.
        assert_eq!(pixel(arrow.width / 6, arrow.height / 3), [230, 126, 34, 255]);
        // The top right corner, beside the tip: clear.
        assert_eq!(pixel(arrow.width - 1, 0)[3], 0);
    }

    #[test]
    fn it_drifts_then_floats() {
        let mut cursor = AgentCursor::new((0.0, 0.0));
        cursor.aim((100.0, 50.0));
        let start = cursor.moved;
        let still = (0.0, 0.0);
        assert_eq!(cursor.position(start, still), (0.0, 0.0));
        let halfway = cursor.position(start + DRIFT / 2, still);
        // Eased: more than half way in half the time.
        assert!(halfway.0 > 50.0 && halfway.0 < 100.0, "{halfway:?}");
        assert_eq!(cursor.position(start + DRIFT, (8.0, 4.0)), (100.0, 50.0));
        // Arrived, it wiggles within its bounds, and comes back round.
        let wiggle = (8.0, 4.0);
        for tenth in 1..40 {
            let at = cursor.position(start + DRIFT + Duration::from_millis(100 * tenth), wiggle);
            assert!((at.0 - 100.0).abs() <= 8.0 && (at.1 - 50.0).abs() <= 2.0, "{at:?}");
        }
        let quarter = cursor.position(start + DRIFT + Duration::from_secs_f64(WIGGLE_PERIOD * 1.25), wiggle);
        assert!((quarter.0 - 108.0).abs() < 0.01, "{quarter:?}");
        let round = cursor.position(start + DRIFT + Duration::from_secs_f64(WIGGLE_PERIOD * 2.0), wiggle);
        assert!((round.0 - 100.0).abs() < 0.01 && (round.1 - 50.0).abs() < 0.01, "{round:?}");
    }

    #[test]
    fn it_is_placed_by_cell_and_offset() {
        let mut cursor = AgentCursor::new((35.0, 70.0));
        let mut out = Vec::new();
        cursor.draw(&mut out, (16, 32), 1.0);
        let drawn = String::from_utf8(out).unwrap();
        // Sent once, then placed in row 3, column 3, 3 and 6 pixels in.
        assert!(drawn.contains("a=t,f=32"), "{drawn}");
        assert!(drawn.contains("\x1b[3;3H\x1b_Ga=p,i=5,p=1,X=3,Y=6,"), "{drawn}");
        let mut again = Vec::new();
        cursor.draw(&mut again, (16, 32), 1.0);
        assert!(!String::from_utf8(again).unwrap().contains("a=t"));
        // Fading, it is sent again, fainter.
        let mut faint = Vec::new();
        cursor.draw(&mut faint, (16, 32), 0.5);
        assert!(String::from_utf8(faint).unwrap().contains("a=t"));
        let mut gone = Vec::new();
        cursor.hide(&mut gone);
        assert!(String::from_utf8(gone).unwrap().contains("a=d,d=i,i=5"));
    }
}
