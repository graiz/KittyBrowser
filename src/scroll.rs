//! Turning the wheel ticks a terminal reports into scrolls for the page.

use std::time::{Duration, Instant};

/// Ticks further apart than this are separate scrolls.
const PAUSE: Duration = Duration::from_millis(300);

/// How long before its first tick a scroll is taken to have begun, in
/// milliseconds.
const START: f64 = 100.0;

/// The least the glide sends at once, in pixels: less isn't seen, and
/// would only cost Chrome a frame.
const MIN_STEP: f64 = 0.5;

/// How far short of `MIN_STEP` a step may still go: `next_due` says when
/// one comes due, and a wake at that moment, a hair early by rounding,
/// should find it.
const SLACK: f64 = 0.01;

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
        if step.abs() < MIN_STEP - SLACK {
            return None;
        }
        self.sent += step;
        Some(step)
    }

    /// Whether ticks now belong to the scroll under way.
    fn moving(&self, now: Instant) -> bool {
        self.start.is_some() && self.ms(now) - self.last.0 < PAUSE.as_secs_f64() * 1000.0
    }

    /// How long from `now` until `due` will have a step to send, or `None`
    /// if it won't until the next tick. On a slow drag the page is owed
    /// half a pixel only every 10 or 20 ms; asking `due` more often than
    /// that would find nothing, and a loop that woke to ask would spin.
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        self.start?;
        let behind = self.target(now) - self.sent;
        if behind.abs() >= MIN_STEP - SLACK {
            return Some(Duration::ZERO);
        }
        let speed = self.speed();
        // The page stops a lead past the last tick; if that is less than
        // a step on from where it was sent, there is nothing more.
        let end = self.last.1 + self.lead.copysign(speed);
        if speed == 0.0 || (end - self.sent).abs() < MIN_STEP - SLACK {
            return None;
        }
        // The target moves on at `speed` until it reaches `end`: when it
        // is a step past what was sent.
        let at = self.last.0 + (self.sent + MIN_STEP.copysign(speed) - self.last.1) / speed;
        Some(Duration::from_secs_f64((at - self.ms(now)).max(0.0) / 1000.0))
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

    /// Run the glide as the app does: woken by each tick, and otherwise
    /// only when `next_due` says, no sooner than 8 ms after the last step.
    /// Returns what was sent and how many wakes found nothing to send.
    fn run_like_the_app(ticks: &[(u64, i32)], row: f64, until_ms: u64) -> (Vec<f64>, usize) {
        let mut glide = Glide::default();
        let start = Instant::now();
        // In tenths of a millisecond, to be finer than the glide.
        let at = |tenths: u64| start + Duration::from_micros(tenths * 100);
        let (mut sent, mut idle) = (Vec::new(), 0);
        let (mut now, mut glided, mut ticks) = (0u64, 0u64, ticks.iter().peekable());
        while now <= until_ms * 10 {
            let mut ticked = false;
            while let Some(&&(_, n)) = ticks.peek().filter(|(ms, _)| ms * 10 <= now) {
                glide.tick(n, row, at(now));
                ticks.next();
                ticked = true;
            }
            if ticked || now - glided >= 80 {
                match glide.due(at(now)) {
                    Some(step) => {
                        sent.push(step);
                        glided = now;
                    }
                    None if !ticked => idle += 1,
                    None => {}
                }
            }
            let tick = ticks.peek().map(|(ms, _)| ms * 10);
            let due = glide.next_due(at(now)).map(|due| {
                let due = due.as_micros().div_ceil(100) as u64;
                now + due.max((glided + 80).saturating_sub(now)).max(1)
            });
            now = match (tick, due) {
                (Some(tick), Some(due)) => tick.min(due),
                (tick, due) => match tick.or(due) {
                    Some(next) => next,
                    None => break,
                },
            };
        }
        (sent, idle)
    }

    #[test]
    fn a_slow_drag_wakes_only_to_send() {
        // A tick of 7 pixels every 280 ms: half a pixel is owed only every
        // 20 ms, so most 8 ms steps would find nothing to send.
        let ticks: Vec<(u64, i32)> = (0..10).map(|n| (n * 280, 1)).collect();
        let (sent, idle) = run_like_the_app(&ticks, 7.0, 4000);
        assert!(idle <= 2, "{idle} wakes sent nothing; sent {sent:?}");
        // Still all of it: ten rows and the glide's one past them.
        assert!((sent.iter().sum::<f64>() - 7.0 * 11.0).abs() < 0.5, "{sent:?}");
        // And steadily, a little at a time, with no wait of more than
        // half a pixel's worth before each step.
        assert!(sent[2..].iter().all(|step| step.abs() <= 1.0), "{sent:?}");
    }

    #[test]
    fn a_fast_drag_still_steps_every_8_ms() {
        // A tick of 14 pixels every 30 ms: a step is owed at every chance.
        let ticks: Vec<(u64, i32)> = (0..20).map(|n| (n * 30, 1)).collect();
        let (sent, idle) = run_like_the_app(&ticks, 14.0, 1500);
        assert_eq!(idle, 0, "{sent:?}");
        assert!((sent.iter().sum::<f64>() - 14.0 * 21.0).abs() < 0.5, "{sent:?}");
        // About 600 ms of drag at one step each 8 ms, and no more.
        assert!((60..=90).contains(&sent.len()), "{} steps", sent.len());
    }

    #[test]
    fn nothing_is_due_once_the_glide_is_over() {
        let mut glide = Glide::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(glide.next_due(at(0)), None);
        // A lone tick goes at once, and leaves nothing to glide.
        glide.tick(1, 14.0, at(0));
        assert_eq!(glide.next_due(at(0)), Some(Duration::ZERO));
        glide.due(at(0));
        assert_eq!(glide.next_due(at(0)), None);
        // A second tick sets a speed: the next half pixel is due in about
        // 0.5 / (14 / 100) ms, and after the row past it, nothing.
        glide.tick(1, 14.0, at(100));
        glide.due(at(100));
        let next = glide.next_due(at(100)).unwrap();
        assert!((3..=4).contains(&next.as_millis()), "{next:?}");
        while glide.due(at(500)).is_some() {}
        assert_eq!(glide.next_due(at(500)), None);
    }

    #[test]
    fn a_pause_starts_a_new_scroll() {
        let steps = glide(&[(0, 1), (96, 1), (1000, 1)], 14.0, 160);
        // Two rows and the glide's one past them, then the new scroll's
        // first tick, which has no speed to glide on with.
        assert!((steps.iter().sum::<f64>() - 14.0 * 4.0).abs() < 0.5, "{steps:?}");
    }
}
