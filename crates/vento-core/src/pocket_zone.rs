//! Pocket island presence: pure logic, driven by the shell.

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
}
