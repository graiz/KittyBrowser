//! Turning the wheel ticks a terminal reports into scrolls for the page.

use std::time::{Duration, Instant};

/// Ticks further apart than this are separate scrolls.
const PAUSE: Duration = Duration::from_millis(300);

/// How long before its first tick a scroll is taken to have begun, in
/// milliseconds.
const START: f64 = 100.0;

/// Keeps a scroll on one axis, as browsers do for a trackpad. Fingers
/// drawn down a trackpad drift a little sideways, and terminals report
/// that drift as ticks left and right among the ticks down; sent on, they
/// would shake anything on the page that scrolls across. Once a scroll has
/// gone down, it goes across only when it is mostly across. Until it has,
/// ticks across go at once, so a short scroll across isn't lost; the price
/// is that drift reported before a scroll's first tick down goes across.
#[derive(Default)]
pub struct Rails {
    /// Ticks so far in this scroll, across and down, and when the last came.
    across: i32,
    down: i32,
    last: Option<Instant>,
}

impl Rails {
    /// The part of these ticks (right, down) to scroll by.
    pub fn filter(&mut self, dx: i32, dy: i32, now: Instant) -> (i32, i32) {
        if self.last.is_none_or(|last| now.duration_since(last) > PAUSE) {
            (self.across, self.down) = (0, 0);
        }
        self.last = Some(now);
        self.across += dx.abs();
        self.down += dy.abs();
        if self.down == 0 || self.across > 2 * self.down {
            (dx, 0)
        } else {
            (0, dy)
        }
    }
}

/// Spreads a scroll's ticks over time. A terminal reports a finger on a
/// trackpad as a tick each time it moves a row, so sent as they come the
/// page would jump a row at a time, a few times a second, while the finger
/// moves smoothly. Instead the page goes to each tick and glides on from
/// it at the speed the ticks have been coming, for at most a row, so that
/// at a steady speed it reaches each row just as the next tick comes and
/// never jumps. When the finger stops, the page goes on to the end of that
/// row. A scroll's first ticks have no speed yet, so the page goes to them
/// and no further: a mouse wheel's notch, several ticks at once, scrolls
/// just its rows. Replaying a trackpad's ticks on a news site, this kept the page
/// 40 to 60 ms behind the finger, against 85 sent as they came.
#[derive(Default)]
pub struct Glide {
    /// The last two places the finger passed, in pixels from where this
    /// scroll began, and when, in milliseconds since `start`.
    before: (f64, f64),
    last: (f64, f64),
    start: Option<Instant>,
    /// How far past the last tick the page may glide: a row, or nothing.
    lead: f64,
    /// How far the page has been sent so far in this scroll.
    sent: f64,
}

impl Glide {
    fn ms(&self, now: Instant) -> f64 {
        self.start.map_or(0.0, |start| now.duration_since(start).as_secs_f64() * 1000.0)
    }

    /// `ticks` (down is positive) of `row` pixels each, come together.
    pub fn tick(&mut self, ticks: i32, row: f64, now: Instant) {
        let pixels = ticks as f64 * row;
        let going = self.last.1 - self.before.1;
        if !self.moving(now) {
            // The finger set off a moment before its first tick: as long
            // before as ticks of a slow scroll come apart.
            *self = Glide { start: Some(now), last: (-START, 0.0), ..Glide::default() };
        } else if going * pixels < 0.0 {
            // Turned back: from where the page is, which may be past the
            // last tick, not from the tick.
            self.last = (self.ms(now) - START, self.sent);
        }
        let first = self.last.0 < -START / 2.0;
        self.before = self.last;
        self.last = (self.ms(now), self.last.1 + pixels);
        self.lead = if first { 0.0 } else { row };
    }

    /// The finger's speed over its last ticks, in pixels a millisecond.
    fn speed(&self) -> f64 {
        (self.last.1 - self.before.1) / (self.last.0 - self.before.0).max(4.0)
    }

    /// Where the page should be now, from the start of this scroll.
    fn target(&self, now: Instant) -> f64 {
        let on = self.speed() * (self.ms(now) - self.last.0);
        self.last.1 + on.clamp(-self.lead, self.lead)
    }

    /// The scroll to send now, in pixels, if the page has fallen behind.
    pub fn due(&mut self, now: Instant) -> Option<f64> {
        self.start?;
        let step = self.target(now) - self.sent;
        if step.abs() < 0.5 {
            return None;
        }
        self.sent += step;
        Some(step)
    }

    /// Whether ticks now belong to the scroll under way.
    fn moving(&self, now: Instant) -> bool {
        self.start.is_some() && self.ms(now) - self.last.0 < PAUSE.as_secs_f64() * 1000.0
    }

    /// Whether the page still has some way to go, so `due` is worth asking.
    pub fn gliding(&self, now: Instant) -> bool {
        let speed = self.speed().abs();
        let gliding_until = self.last.0 + if speed > 0.0 { self.lead / speed } else { 0.0 };
        self.start.is_some() && (self.ms(now) < gliding_until || (self.target(now) - self.sent).abs() >= 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scroll_down_drops_the_drift_across() {
        let mut rails = Rails::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(rails.filter(0, 1, at(0)), (0, 1));
        assert_eq!(rails.filter(-1, 0, at(10)), (0, 0));
        assert_eq!(rails.filter(1, 1, at(20)), (0, 1));
        assert_eq!(rails.filter(-1, 0, at(30)), (0, 0));
        // After a pause a scroll across goes across from its first tick,
        // even one tick alone, and drops the drift down.
        assert_eq!(rails.filter(1, 0, at(500)), (1, 0));
        assert_eq!(rails.filter(1, 0, at(1000)), (1, 0));
        assert_eq!(rails.filter(1, 0, at(1010)), (1, 0));
        assert_eq!(rails.filter(1, 1, at(1020)), (1, 0));
    }

    /// What the glide sends, step by step (every `GLIDE_STEP`, 8 ms, as
    /// the app asks) for `steps` steps, given `ticks` as (ms, ticks) of
    /// `row` pixels.
    fn glide(ticks: &[(u64, i32)], row: f64, steps: u64) -> Vec<f64> {
        let mut glide = Glide::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut sent = Vec::new();
        for step in 0..steps {
            let now = step * 8;
            for &(_, n) in ticks.iter().filter(|(when, _)| (now.saturating_sub(7)..=now).contains(when)) {
                glide.tick(n, row, at(now));
            }
            sent.extend(glide.due(at(now)));
        }
        sent
    }

    #[test]
    fn ticks_become_a_steady_glide() {
        // A tick of 14 pixels every 96 ms: after the first two, the page
        // moves the same little way each step, with no jump when a tick
        // comes.
        let ticks: Vec<(u64, i32)> = (0..5).map(|n| (n * 96, 1)).collect();
        let steps = glide(&ticks, 14.0, 80);
        let steady = &steps[14..46];
        assert!(steady.iter().all(|&step| (step - 14.0 * 8.0 / 96.0).abs() < 0.1), "{steps:?}");
        // The finger stopped after five rows; the page goes on to the end
        // of the sixth, and no further.
        assert!((steps.iter().sum::<f64>() - 14.0 * 6.0).abs() < 0.5, "{steps:?}");
    }

    #[test]
    fn a_notch_or_a_lone_tick_scrolls_just_its_rows() {
        // A mouse wheel's notch: three ticks at once.
        assert_eq!(glide(&[(0, 3)], 14.0, 60), vec![42.0]);
        // One tick alone.
        assert_eq!(glide(&[(0, -1)], 14.0, 60), vec![-14.0]);
        // A wheel turned on, notch after notch: each notch's rows, and at
        // most one more.
        let sent: f64 = glide(&[(0, 3), (80, 3), (160, 3)], 14.0, 60).iter().sum();
        assert!((126.0..=140.0).contains(&sent), "{sent}");
    }

    #[test]
    fn turning_back_starts_from_where_the_page_is() {
        // Down at a steady speed, then back up: the page doesn't leap back
        // past the last tick down in one frame.
        let ticks = [(0, 1), (96, 1), (192, 1), (288, -1), (384, -1)];
        let steps = glide(&ticks, 14.0, 80);
        assert!(steps.iter().all(|step| step.abs() <= 14.0), "{steps:?}");
    }

    #[test]
    fn a_pause_starts_a_new_scroll() {
        let steps = glide(&[(0, 1), (96, 1), (1000, 1)], 14.0, 160);
        // Two rows and the glide's one past them, then the new scroll's
        // first tick, which has no speed to glide on with.
        assert!((steps.iter().sum::<f64>() - 14.0 * 4.0).abs() < 0.5, "{steps:?}");
    }
}
