//! Spots a quick shake of the pointer, in any direction, during a drag.

use std::time::{Duration, Instant};

use crate::geometry::Point;

/// Reversals of direction that make a shake.
pub const SHAKE_REVERSALS: usize = 4;
/// Minimum travel, in logical pixels, of the leg a reversal ends.
pub const SHAKE_MIN_LEG: f64 = 40.0;
/// How far, in logical pixels, the pointer must come back along the leg for
/// it to count as a reversal, so drifting sideways while moving on does not.
pub const SHAKE_MIN_BACK: f64 = 8.0;
/// All reversals of a shake must fall within this span.
pub const SHAKE_WINDOW: Duration = Duration::from_millis(600);
/// How closely a move must point back along the leg to be a reversal (the
/// cosine of the angle to the leg, about 135° or more), and forward along it
/// to extend it. Moves in between, once long enough, start a new leg: the
/// pointer turned a corner.
const BACK_COS: f64 = -0.7;
const FORWARD_COS: f64 = 0.7;

/// Watches the pointer for a quick back-and-forth: left-right, up-down,
/// diagonal, or anything between.
#[derive(Clone, Debug)]
pub struct ShakeDetector {
    /// Where the current leg started: the first position, then each
    /// turning point.
    pivot: Option<Point>,
    /// The furthest point reached on the current leg.
    extreme: Point,
    reversals: Vec<Instant>,
    fired: bool,
}

impl Default for ShakeDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ShakeDetector {
    pub fn new() -> Self {
        Self {
            pivot: None,
            extreme: Point { x: 0.0, y: 0.0 },
            reversals: Vec::new(),
            fired: false,
        }
    }

    /// Feeds one pointer position; returns true once, when a shake completes.
    pub fn feed(&mut self, p: Point, now: Instant) -> bool {
        let Some(pivot) = self.pivot else {
            self.pivot = Some(p);
            self.extreme = p;
            return false;
        };
        let (mx, my) = (p.x - self.extreme.x, p.y - self.extreme.y);
        let moved = mx.hypot(my);
        if moved < SHAKE_MIN_BACK {
            return false;
        }
        let (lx, ly) = (self.extreme.x - pivot.x, self.extreme.y - pivot.y);
        let leg = lx.hypot(ly);
        if leg == 0.0 {
            // The first move sets the leg's direction.
            self.extreme = p;
            return false;
        }
        let cos = (mx * lx + my * ly) / (moved * leg);
        if cos >= FORWARD_COS {
            self.extreme = p;
            return false;
        }
        if cos > BACK_COS {
            // Turned a corner: a new leg, but no reversal.
            if moved >= SHAKE_MIN_LEG {
                self.pivot = Some(self.extreme);
                self.extreme = p;
            }
            return false;
        }
        // Coming back along the leg: a reversal only if the leg that just
        // ended was long enough; smaller back-steps are jitter.
        if leg < SHAKE_MIN_LEG {
            return false;
        }
        self.pivot = Some(self.extreme);
        self.extreme = p;
        if self.fired {
            return false;
        }
        self.reversals.push(now);
        self.reversals
            .retain(|t| now.saturating_duration_since(*t) <= SHAKE_WINDOW);
        if self.reversals.len() >= SHAKE_REVERSALS {
            self.fired = true;
            return true;
        }
        false
    }

    /// Forgets everything and re-arms; call when the drag ends.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    /// Feeds `points` `step_ms` apart starting at `t0`; returns how many
    /// times the detector fired.
    fn shake_at(d: &mut ShakeDetector, t0: Instant, step_ms: u64, points: &[Point]) -> usize {
        points
            .iter()
            .enumerate()
            .filter(|(k, p)| d.feed(**p, ms(t0, step_ms * *k as u64)))
            .count()
    }

    /// [`shake_at`] along the x axis.
    fn shake(d: &mut ShakeDetector, t0: Instant, step_ms: u64, xs: &[f64]) -> usize {
        let points: Vec<Point> = xs.iter().map(|&x| Point { x, y: 0.0 }).collect();
        shake_at(d, t0, step_ms, &points)
    }

    const FOUR_REVERSALS: [f64; 6] = [0.0, 50.0, 0.0, 50.0, 0.0, 50.0];

    #[test]
    fn shake_fires_on_four_quick_reversals() {
        let mut d = ShakeDetector::new();
        assert_eq!(shake(&mut d, Instant::now(), 50, &FOUR_REVERSALS), 1);
    }

    #[test]
    fn shakes_up_and_down_or_diagonally_count_too() {
        let up_down: Vec<Point> = FOUR_REVERSALS
            .iter()
            .map(|&y| Point { x: 300.0, y })
            .collect();
        assert_eq!(
            shake_at(&mut ShakeDetector::new(), Instant::now(), 50, &up_down),
            1
        );
        let diagonal: Vec<Point> = FOUR_REVERSALS
            .iter()
            .map(|&v| Point { x: v, y: -v })
            .collect();
        assert_eq!(
            shake_at(&mut ShakeDetector::new(), Instant::now(), 50, &diagonal),
            1
        );
    }

    #[test]
    fn three_reversals_do_not_shake() {
        let mut d = ShakeDetector::new();
        assert_eq!(shake(&mut d, Instant::now(), 50, &FOUR_REVERSALS[..5]), 0);
    }

    #[test]
    fn small_wiggles_do_not_shake() {
        let mut d = ShakeDetector::new();
        let xs = [0.0, 20.0, 0.0, 20.0, 0.0, 20.0, 0.0, 20.0, 0.0, 20.0];
        assert_eq!(shake(&mut d, Instant::now(), 50, &xs), 0);
    }

    #[test]
    fn slow_reversals_do_not_shake() {
        // Four reversals, but they span 900 ms (sampled 300 ms apart).
        let mut d = ShakeDetector::new();
        assert_eq!(shake(&mut d, Instant::now(), 300, &FOUR_REVERSALS), 0);
    }

    #[test]
    fn jitter_within_a_leg_is_not_a_reversal() {
        let mut d = ShakeDetector::new();
        // Each leg stutters back 5 px on the way; still only 4 real reversals.
        let xs = [
            0.0, 30.0, 25.0, 50.0, 0.0, 20.0, 15.0, 50.0, 0.0, 50.0, 0.0, 50.0,
        ];
        assert_eq!(shake(&mut d, Instant::now(), 20, &xs), 1);
    }

    #[test]
    fn a_long_drag_that_turns_corners_is_no_shake() {
        // Right, down, left, up, right: a square path, never coming back
        // along a leg.
        let path = [
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ]
        .map(|(x, y)| Point { x, y });
        let mut d = ShakeDetector::new();
        assert_eq!(shake_at(&mut d, Instant::now(), 30, &path), 0);
    }

    #[test]
    fn shake_fires_once_until_reset() {
        let t0 = Instant::now();
        let mut d = ShakeDetector::new();
        assert_eq!(shake(&mut d, t0, 50, &FOUR_REVERSALS), 1);
        let more = [0.0, 50.0, 0.0, 50.0, 0.0, 50.0];
        assert_eq!(shake(&mut d, ms(t0, 1000), 50, &more), 0);
        d.reset();
        assert_eq!(shake(&mut d, ms(t0, 2000), 50, &FOUR_REVERSALS), 1);
    }
}
