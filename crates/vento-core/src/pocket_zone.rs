//! Pocket island presence: pure logic, driven by the shell.

use std::time::{Duration, Instant};

use crate::geometry::{Point, Rect};

/// How far the drop zone extends past the collapsed island on the left, right
/// and bottom, in logical pixels.
pub const ZONE_MARGIN: f64 = 60.0;
/// How long the cursor must rest on the collapsed island before it expands,
/// so a pointer passing over the menu bar doesn't open it.
pub const HOVER_DWELL: Duration = Duration::from_millis(150);
/// How long the cursor must rest on the push rect at the screen's top edge
/// before a virtual island opens.
pub const PUSH_DWELL: Duration = Duration::from_millis(300);
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
    /// The island rests `Collapsed` even with no items (the virtual island).
    pub rest_visible: bool,
    /// When `Some`, the island opens by resting the cursor in this rect for
    /// [`PUSH_DWELL`], with or without items, and the `hover` dwell is off.
    pub push: Option<Rect>,
}

/// Decides whether the island is hidden, collapsed or expanded.
#[derive(Clone, Copy, Debug)]
pub struct IslandState {
    presence: Presence,
    /// When the expand conditions first went false while `Expanded`.
    leaving_since: Option<Instant>,
    /// Since when the cursor has rested on the hover shape.
    hover_since: Option<Instant>,
    /// Since when the cursor has rested on the push rect.
    push_since: Option<Instant>,
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
            push_since: None,
        }
    }

    /// The presence decided by the last [`update`](Self::update).
    pub fn presence(&self) -> Presence {
        self.presence
    }

    /// Applies one round of inputs and returns the resulting presence.
    pub fn update(&mut self, i: &IslandInputs, now: Instant) -> Presence {
        let dwelt = if i.push.is_some() {
            self.hover_since = None;
            false
        } else if i.has_items && contains(&i.hover, i.cursor) {
            let since = *self.hover_since.get_or_insert(now);
            now.saturating_duration_since(since) >= HOVER_DWELL
        } else {
            self.hover_since = None;
            false
        };
        let pushed = match i.push {
            Some(r) if !i.dragging && contains(&r, i.cursor) => {
                let since = *self.push_since.get_or_insert(now);
                now.saturating_duration_since(since) >= PUSH_DWELL
            }
            _ => {
                self.push_since = None;
                false
            }
        };
        let dragged_in = i.dragging && contains(&i.zone, i.cursor);
        let held = self.presence == Presence::Expanded
            && (i.dragging || i.has_items || i.push.is_some())
            && contains(&i.hold, i.cursor);
        let expand = i.pinned_open || dragged_in || dwelt || pushed || held;
        let resting = resting(i.has_items || i.rest_visible);

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

    /// How far the current push has filled, `0.0..=1.0`; `0.0` when no push
    /// is under way or once the island is expanded.
    pub fn push_progress(&self, now: Instant) -> f32 {
        match self.push_since {
            Some(since) if self.presence != Presence::Expanded => {
                let elapsed = now.saturating_duration_since(since);
                (elapsed.as_secs_f32() / PUSH_DWELL.as_secs_f32()).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    /// Drops straight to the resting presence (collapsed when `visible`,
    /// hidden without), with no collapse delay; for when vento is paused.
    /// Callers pass `has_items || rest_visible`.
    pub fn settle(&mut self, visible: bool) -> Presence {
        *self = Self {
            presence: resting(visible),
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
            rest_visible: false,
            push: None,
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

    const EDGE: Rect = Rect {
        x: 540.0,
        y: 0.0,
        w: 200.0,
        h: 2.0,
    };
    const ON_EDGE: Point = Point { x: 640.0, y: 1.0 };

    fn virt(has_items: bool, dragging: bool, cursor: Point) -> IslandInputs {
        IslandInputs {
            rest_visible: true,
            push: Some(EDGE),
            ..inputs(has_items, dragging, cursor, false)
        }
    }

    #[test]
    fn a_virtual_island_rests_collapsed_even_when_empty() {
        let mut s = IslandState::new();
        assert_eq!(
            s.update(&virt(false, false, OUTSIDE), Instant::now()),
            Presence::Collapsed
        );
        assert_eq!(s.settle(true), Presence::Collapsed);
    }

    #[test]
    fn hovering_the_virtual_island_does_not_open_it() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        for step in 0..20 {
            assert_eq!(
                s.update(&virt(true, false, INSIDE), ms(t0, 50 * step)),
                Presence::Collapsed
            );
        }
    }

    #[test]
    fn a_push_opens_after_the_dwell_with_or_without_items() {
        for has_items in [false, true] {
            let t0 = Instant::now();
            let mut s = IslandState::new();
            s.update(&virt(has_items, false, ON_EDGE), t0);
            assert_eq!(
                s.update(&virt(has_items, false, ON_EDGE), ms(t0, 299)),
                Presence::Collapsed
            );
            assert!((s.push_progress(ms(t0, 150)) - 0.5).abs() < 0.01);
            assert_eq!(
                s.update(&virt(has_items, false, ON_EDGE), ms(t0, 300)),
                Presence::Expanded
            );
            assert_eq!(s.push_progress(ms(t0, 300)), 0.0);
        }
    }

    #[test]
    fn passing_the_edge_restarts_the_push() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        s.update(&virt(true, false, ON_EDGE), t0);
        s.update(&virt(true, false, OUTSIDE), ms(t0, 200));
        s.update(&virt(true, false, ON_EDGE), ms(t0, 250));
        assert_eq!(
            s.update(&virt(true, false, ON_EDGE), ms(t0, 549)),
            Presence::Collapsed
        );
        assert_eq!(
            s.update(&virt(true, false, ON_EDGE), ms(t0, 550)),
            Presence::Expanded
        );
    }

    #[test]
    fn a_drag_at_the_edge_opens_at_once_not_by_push() {
        let mut s = IslandState::new();
        let at_edge_in_zone = Point { x: 600.0, y: 1.0 };
        assert_eq!(
            s.update(&virt(false, true, at_edge_in_zone), Instant::now()),
            Presence::Expanded
        );
        assert_eq!(s.push_progress(Instant::now()), 0.0);
    }

    #[test]
    fn an_empty_virtual_island_opened_by_push_stays_open_on_its_panel() {
        let t0 = Instant::now();
        let mut s = IslandState::new();
        s.update(&virt(false, false, ON_EDGE), t0);
        s.update(&virt(false, false, ON_EDGE), ms(t0, 300));
        let panel = virt(false, false, Point { x: 400.0, y: 140.0 });
        assert_eq!(s.update(&panel, ms(t0, 2000)), Presence::Expanded);
    }
}
