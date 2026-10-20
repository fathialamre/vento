//! Pocket island presence and shake detection: pure logic, driven by the shell.

use std::time::{Duration, Instant};

use crate::geometry::{Point, Rect};

/// How far the drop zone extends past the collapsed island on the left, right
/// and bottom, in logical pixels.
pub const ZONE_MARGIN: f64 = 60.0;
/// How long the cursor must rest on the collapsed island before it expands,
/// so a pointer passing over the menu bar doesn't open it.
pub const HOVER_DWELL: Duration = Duration::from_millis(150);
/// How long the expand conditions must stay false before the island leaves
/// `Expanded`, so a cursor wobbling at the edge doesn't flicker it.
pub const COLLAPSE_DELAY: Duration = Duration::from_millis(400);
/// Horizontal direction reversals that make a shake.
pub const SHAKE_REVERSALS: usize = 4;
/// Minimum horizontal travel, in logical pixels, of the leg a reversal ends.
pub const SHAKE_MIN_LEG: f64 = 40.0;
/// All reversals of a shake must fall within this span.
pub const SHAKE_WINDOW: Duration = Duration::from_millis(600);

/// How much of the island is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Hidden,
    Collapsed,
    Expanded,
}

/// Everything [`IslandState::update`] needs to decide the presence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IslandInputs {
    pub has_items: bool,
    pub dragging: bool,
    pub cursor: Point,
    /// The collapsed shape: resting on it for [`HOVER_DWELL`] expands an
    /// island with items.
    pub hover: Rect,
    /// The collapsed rect grown by [`ZONE_MARGIN`] on the left, right and
    /// bottom: a drag inside it expands the island. Only used while dragging.
    pub zone: Rect,
    /// While expanded, the cursor inside this (the panel and the hover
    /// shape) keeps the island open.
    pub hold: Rect,
    /// The shortcut or the Pocket action asked for the island to stay open.
    pub pinned_open: bool,
}

/// Decides whether the island is hidden, collapsed or expanded.
#[derive(Clone, Copy, Debug)]
pub struct IslandState {
    presence: Presence,
    /// When the expand conditions first went false while `Expanded`.
    leaving_since: Option<Instant>,
    /// Since when the cursor has rested on the hover shape.
    hover_since: Option<Instant>,
}

impl Default for IslandState {
    fn default() -> Self {
        Self::new()
    }
}

impl IslandState {
    pub fn new() -> Self {
        Self {
            presence: Presence::Hidden,
            leaving_since: None,
            hover_since: None,
        }
    }

    /// The presence decided by the last [`update`](Self::update).
    pub fn presence(&self) -> Presence {
        self.presence
    }

    /// Applies one round of inputs and returns the resulting presence.
    pub fn update(&mut self, i: &IslandInputs, now: Instant) -> Presence {
        let dwelt = if i.has_items && contains(&i.hover, i.cursor) {
            let since = *self.hover_since.get_or_insert(now);
            now.saturating_duration_since(since) >= HOVER_DWELL
        } else {
            self.hover_since = None;
            false
        };
        let dragged_in = i.dragging && contains(&i.zone, i.cursor);
        let held = self.presence == Presence::Expanded
            && (i.dragging || i.has_items)
            && contains(&i.hold, i.cursor);
        let expand = i.pinned_open || dragged_in || dwelt || held;
        let resting = resting(i.has_items);

        if expand {
            self.presence = Presence::Expanded;
            self.leaving_since = None;
        } else if self.presence == Presence::Expanded {
            let since = *self.leaving_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= COLLAPSE_DELAY {
                self.presence = resting;
                self.leaving_since = None;
            }
        } else {
            self.presence = resting;
        }
        self.presence
    }

    /// Drops straight to the resting presence (collapsed with items, hidden
    /// without), with no collapse delay; for when vento is paused.
    pub fn settle(&mut self, has_items: bool) -> Presence {
        *self = Self {
            presence: resting(has_items),
            ..Self::new()
        };
        self.presence
    }
}

fn resting(has_items: bool) -> Presence {
    if has_items {
        Presence::Collapsed
    } else {
        Presence::Hidden
    }
}

/// Whether `p` is inside `r`, edges included.
fn contains(r: &Rect, p: Point) -> bool {
    p.x >= r.x && p.x <= r.x + r.w && p.y >= r.y && p.y <= r.y + r.h
}

/// Spots a quick side-to-side shake of the cursor during a drag.
#[derive(Clone, Debug)]
pub struct ShakeDetector {
    /// Where the current leg started: the first sample, then each turning point.
    pivot: Option<f64>,
    /// Furthest x reached on the current leg.
    extreme: f64,
    /// Direction of the current leg: 1 right, -1 left, 0 not yet moving.
    dir: i8,
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
            extreme: 0.0,
            dir: 0,
            reversals: Vec::new(),
            fired: false,
        }
    }

    /// Feeds one cursor x position; returns true once, when a shake completes.
    pub fn feed(&mut self, x: f64, now: Instant) -> bool {
        let Some(pivot) = self.pivot else {
            self.pivot = Some(x);
            self.extreme = x;
            return false;
        };
        if self.dir == 0 {
            if x != pivot {
                self.dir = if x > pivot { 1 } else { -1 };
                self.extreme = x;
            }
            return false;
        }
        let dir = f64::from(self.dir);
        if x * dir >= self.extreme * dir {
            self.extreme = x;
            return false;
        }
        // Moving back against the leg: it is a reversal only if the leg that
        // just ended was long enough; smaller back-steps are jitter.
        if (self.extreme - pivot).abs() < SHAKE_MIN_LEG {
            return false;
        }
        self.pivot = Some(self.extreme);
        self.extreme = x;
        self.dir = -self.dir;
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

    /// Forgets everything and re-arms; call on mouse-up.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLLAPSED: Rect = Rect {
        x: 500.0,
        y: 0.0,
        w: 200.0,
        h: 32.0,
    };
    const ZONE: Rect = Rect {
        x: 440.0,
        y: 0.0,
        w: 320.0,
        h: 92.0,
    };
    const HOLD: Rect = Rect {
        x: 390.0,
        y: 0.0,
        w: 420.0,
        h: 144.0,
    };
    const INSIDE: Point = Point { x: 600.0, y: 10.0 };
    const OUTSIDE: Point = Point { x: 100.0, y: 400.0 };
    /// In the drop zone, under the collapsed wings: where toolbars sit.
    const UNDER_WINGS: Point = Point { x: 600.0, y: 60.0 };
    /// In the drop zone, beside the collapsed wings.
    const BESIDE_WINGS: Point = Point { x: 460.0, y: 10.0 };

    fn inputs(has_items: bool, dragging: bool, cursor: Point, pinned_open: bool) -> IslandInputs {
        IslandInputs {
            has_items,
            dragging,
            cursor,
            hover: COLLAPSED,
            zone: ZONE,
            hold: HOLD,
            pinned_open,
        }
    }

    /// Rests on the collapsed island long enough to expand it.
    fn hover_open(s: &mut IslandState, t0: Instant) {
        s.update(&inputs(true, false, INSIDE, false), t0);
        assert_eq!(
            s.update(&inputs(true, false, INSIDE, false), t0 + HOVER_DWELL),
            Presence::Expanded
        );
    }

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn empty_pocket_is_hidden_until_a_drag_enters_the_zone() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        assert_eq!(s.presence(), Presence::Hidden);
        assert_eq!(
            s.update(&inputs(false, false, INSIDE, false), t0),
            Presence::Hidden
        );
        assert_eq!(
            s.update(&inputs(false, true, OUTSIDE, false), ms(t0, 10)),
            Presence::Hidden
        );
        assert_eq!(
            s.update(&inputs(false, true, INSIDE, false), ms(t0, 20)),
            Presence::Expanded
        );
    }

    #[test]
    fn items_show_collapsed_and_expand_after_a_hover_dwell() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        assert_eq!(
            s.update(&inputs(true, false, OUTSIDE, false), t0),
            Presence::Collapsed
        );
        let on = inputs(true, false, INSIDE, false);
        assert_eq!(s.update(&on, ms(t0, 10)), Presence::Collapsed);
        assert_eq!(s.update(&on, ms(t0, 159)), Presence::Collapsed);
        assert_eq!(s.update(&on, ms(t0, 160)), Presence::Expanded);
    }

    #[test]
    fn a_short_hover_does_not_expand() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        let on = inputs(true, false, INSIDE, false);
        let off = inputs(true, false, OUTSIDE, false);
        s.update(&on, t0);
        assert_eq!(s.update(&on, ms(t0, 100)), Presence::Collapsed);
        // Leaving restarts the dwell.
        assert_eq!(s.update(&off, ms(t0, 120)), Presence::Collapsed);
        assert_eq!(s.update(&on, ms(t0, 200)), Presence::Collapsed);
        assert_eq!(s.update(&on, ms(t0, 349)), Presence::Collapsed);
        assert_eq!(s.update(&on, ms(t0, 350)), Presence::Expanded);
    }

    #[test]
    fn hovering_beside_or_under_the_wings_does_not_expand() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        for (k, at) in [UNDER_WINGS, BESIDE_WINGS].into_iter().enumerate() {
            for step in 0..10 {
                let now = ms(t0, 1000 * k as u64 + 100 * step);
                assert_eq!(
                    s.update(&inputs(true, false, at, false), now),
                    Presence::Collapsed
                );
            }
        }
    }

    #[test]
    fn a_drag_in_the_zone_expands_at_once() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        assert_eq!(
            s.update(&inputs(true, true, UNDER_WINGS, false), t0),
            Presence::Expanded
        );
        let mut s = IslandState::new();
        assert_eq!(
            s.update(&inputs(true, true, BESIDE_WINGS, false), t0),
            Presence::Expanded
        );
    }

    #[test]
    fn zone_edges_are_inclusive() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        let corner = Point { x: 440.0, y: 92.0 };
        assert_eq!(
            s.update(&inputs(true, true, corner, false), t0),
            Presence::Expanded
        );
    }

    #[test]
    fn the_hold_area_keeps_an_expanded_island_open() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        hover_open(&mut s, t0);
        // On the panel, below the wings: still open long after the delay.
        let panel = inputs(true, false, Point { x: 400.0, y: 140.0 }, false);
        assert_eq!(s.update(&panel, ms(t0, 1000)), Presence::Expanded);
        assert_eq!(s.update(&panel, ms(t0, 2000)), Presence::Expanded);
    }

    #[test]
    fn expanded_collapses_only_after_delay() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        s.update(&inputs(true, true, INSIDE, false), t0);
        let away = inputs(true, false, OUTSIDE, false);
        assert_eq!(s.update(&away, ms(t0, 100)), Presence::Expanded);
        assert_eq!(s.update(&away, ms(t0, 499)), Presence::Expanded);
        assert_eq!(s.update(&away, ms(t0, 501)), Presence::Collapsed);
    }

    #[test]
    fn collapse_countdown_runs_from_the_first_false_sample() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        s.update(&inputs(true, true, INSIDE, false), t0);
        let away = inputs(true, false, OUTSIDE, false);
        s.update(&away, ms(t0, 100));
        assert_eq!(s.update(&away, ms(t0, 499)), Presence::Expanded);
        assert_eq!(s.update(&away, ms(t0, 501)), Presence::Collapsed);
    }

    #[test]
    fn coming_back_cancels_the_countdown() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        s.update(&inputs(true, true, INSIDE, false), t0);
        let away = inputs(true, false, OUTSIDE, false);
        s.update(&away, ms(t0, 100));
        // Back on the island before the delay ran out: it stays open.
        s.update(&inputs(true, false, INSIDE, false), ms(t0, 300));
        // Away again: a fresh 400 ms starts at 350, not at 100.
        s.update(&away, ms(t0, 350));
        assert_eq!(s.update(&away, ms(t0, 600)), Presence::Expanded);
        assert_eq!(s.update(&away, ms(t0, 751)), Presence::Collapsed);
    }

    #[test]
    fn empty_pocket_hides_after_delay() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        s.update(&inputs(false, true, INSIDE, false), t0);
        let away = inputs(false, false, OUTSIDE, false);
        assert_eq!(s.update(&away, ms(t0, 100)), Presence::Expanded);
        assert_eq!(s.update(&away, ms(t0, 501)), Presence::Hidden);
    }

    #[test]
    fn settle_drops_to_rest_at_once() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        hover_open(&mut s, t0);
        assert_eq!(s.settle(true), Presence::Collapsed);
        assert_eq!(s.presence(), Presence::Collapsed);
        assert_eq!(s.settle(false), Presence::Hidden);
        // The dwell starts over after a settle.
        let on = inputs(true, false, INSIDE, false);
        assert_eq!(s.update(&on, ms(t0, 1000)), Presence::Collapsed);
        assert_eq!(s.update(&on, ms(t0, 1150)), Presence::Expanded);
    }

    #[test]
    fn pinned_open_expands_even_when_empty() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        assert_eq!(
            s.update(&inputs(false, false, OUTSIDE, true), t0),
            Presence::Expanded
        );
    }

    /// Feeds x positions 50 ms apart starting at `t0`; returns how many times
    /// the detector fired.
    fn shake(d: &mut ShakeDetector, t0: Instant, step_ms: u64, xs: &[f64]) -> usize {
        xs.iter()
            .enumerate()
            .filter(|(k, x)| d.feed(**x, ms(t0, step_ms * *k as u64)))
            .count()
    }

    const FOUR_REVERSALS: [f64; 6] = [0.0, 50.0, 0.0, 50.0, 0.0, 50.0];

    #[test]
    fn shake_fires_on_four_quick_reversals() {
        let mut d = ShakeDetector::new();
        assert_eq!(shake(&mut d, Instant::now(), 50, &FOUR_REVERSALS), 1);
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
