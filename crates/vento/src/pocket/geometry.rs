//! Where the island's shapes sit on its screen, and which of them takes the
//! pointer.

use slint::LogicalPosition;
use slint::platform::WindowEvent;
use vento_core::geometry::{Point, Rect};
use vento_core::pocket_zone::{IslandInputs, Presence, ZONE_MARGIN};
use vento_platform::IslandHome;

/// Width of each wing beside the notch.
const WING_W: f64 = 44.0;
/// The collapsed pill on screens without a notch.
const PILL_W: f64 = 120.0;
const PILL_H: f64 = 28.0;
/// Gap between the menu bar and the pill.
const PILL_GAP: f64 = 4.0;
/// The expanded island is the notch plus this, but never narrower than
/// [`EXPANDED_MIN_W`], and wide enough for its cells (see [`panel_w`]).
const EXPANDED_EXTRA_W: f64 = 96.0;
const EXPANDED_MIN_W: f64 = 420.0;
/// Room the expanded island leaves free on each side of its screen.
const SCREEN_MARGIN: f64 = 20.0;
/// The panel's layout, as `pocket.slint` draws it: side padding, one cell
/// per step, the "+N" tile, and the gap and column of "Drag all".
const PAD: f64 = 14.0;
const CELL_STEP: f64 = 68.0;
const OVERFLOW_W: f64 = 48.0;
const SIDE_GAP: f64 = 10.0;
const SIDE_W: f64 = 52.0;
/// Height of the Pocket view's item panel below the menu bar.
const PANEL_H: f64 = 112.0;
/// Height of the bar under either view that switches between them.
const SWITCH_H: f64 = 40.0;
/// Height of the Clipboard view's list: six 36 pt rows and 8 pt above and
/// below.
const CLIP_PANEL_H: f64 = 232.0;
/// Top height when neither the menu bar nor a notch gives one.
const FALLBACK_TOP_H: f64 = 32.0;
/// Cells shown in the expanded island; the rest are summed up as "+N".
pub(super) const MAX_CELLS: usize = 8;

/// What the expanded island shows below the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    /// The Pocket's cells, with the bottom switch under them when the
    /// clipboard history is on.
    Pocket { switch: bool },
    /// The clipboard history's list, always with the bottom switch.
    Clipboard,
}

/// The island's rects for one home, in logical top-left global coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Frames {
    pub(super) notch_style: bool,
    pub(super) notch_w: f64,
    pub(super) top_h: f64,
    /// The drawn shape while collapsed: wings beside the notch, or the pill.
    pub(super) collapsed: Rect,
    /// `collapsed` grown by [`ZONE_MARGIN`] on the left, right and bottom.
    pub(super) zone: Rect,
    /// The drawn shape while expanded, and the window's frame.
    pub(super) expanded: Rect,
}

/// The width the panel needs to show `count` items without scrolling: the
/// first [`MAX_CELLS`] cells, the "+N" tile when there are more, and the
/// "Drag all" column.
fn panel_w(count: usize) -> f64 {
    let cells = count.min(MAX_CELLS) as f64 * CELL_STEP;
    let overflow = if count > MAX_CELLS { OVERFLOW_W } else { 0.0 };
    PAD + cells + overflow + SIDE_GAP + SIDE_W + PAD
}

/// The height the island takes at the top of the screen: the menu bar, or
/// the notch if taller (the menu bar reports 0 when it auto-hides).
pub(super) fn top_height(home: &IslandHome) -> f64 {
    let h = home.menu_bar_h.max(home.notch.map_or(0.0, |n| n.h));
    if h > 0.0 { h } else { FALLBACK_TOP_H }
}

/// Every shape is centred on the screen, as the window is; the notch sits in
/// the middle of its screen (to within half a point). With room kept for it
/// in the menu bar (a slot), the island fills that room as it would sit
/// around a notch, and every shape is centred on the slot. Showing the Pocket, the
/// expanded island widens to fit `count` items, up to the screen's width less
/// a margin; the row scrolls if that is still too narrow. Showing the
/// Clipboard, it keeps the width of an empty Pocket and is taller.
pub(super) fn frames(home: &IslandHome, count: usize, panel: Panel) -> Frames {
    let screen = home.screen;
    let top_h = top_height(home);
    let (mid, notch_w, notch_style) = match home.slot {
        Some(slot) => (
            slot.x + slot.w / 2.0,
            (slot.w - 2.0 * WING_W).max(0.0),
            true,
        ),
        None => (
            screen.x + screen.w / 2.0,
            home.notch.map_or(0.0, |n| n.w),
            home.notch.is_some(),
        ),
    };
    let collapsed = if notch_style {
        let w = notch_w + 2.0 * WING_W;
        Rect {
            x: mid - w / 2.0,
            y: screen.y,
            w,
            h: top_h,
        }
    } else {
        Rect {
            x: mid - PILL_W / 2.0,
            y: screen.y + top_h + PILL_GAP,
            w: PILL_W,
            h: PILL_H,
        }
    };
    let zone = Rect {
        x: collapsed.x - ZONE_MARGIN,
        y: collapsed.y,
        w: collapsed.w + 2.0 * ZONE_MARGIN,
        h: collapsed.h + ZONE_MARGIN,
    };
    let (fit, panel_h) = match panel {
        Panel::Pocket { switch } => (
            panel_w(count),
            PANEL_H + if switch { SWITCH_H } else { 0.0 },
        ),
        Panel::Clipboard => (0.0, CLIP_PANEL_H + SWITCH_H),
    };
    let w = (notch_w + EXPANDED_EXTRA_W)
        .max(EXPANDED_MIN_W)
        .max(fit)
        .min(screen.w - 2.0 * SCREEN_MARGIN);
    let expanded = Rect {
        x: mid - w / 2.0,
        y: screen.y,
        w,
        h: top_h + panel_h,
    };
    Frames {
        notch_style,
        notch_w,
        top_h,
        collapsed,
        zone,
        expanded,
    }
}

/// The smallest rect holding both.
pub(super) fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        w: (a.x + a.w).max(b.x + b.w) - x,
        h: (a.y + a.h).max(b.y + b.h) - y,
    }
}

/// What keeps an expanded island open: the panel and the collapsed shape
/// (so moving onto the cells keeps it open).
pub(super) fn hold(f: &Frames) -> Rect {
    union(f.collapsed, f.expanded)
}

/// The presence inputs for one sample: hovering the collapsed shape opens
/// the island, the wider zone only counts during a drag (ruling P14).
pub(super) fn island_inputs(
    f: &Frames,
    has_items: bool,
    dragging: bool,
    cursor: Point,
    pinned_open: bool,
) -> IslandInputs {
    IslandInputs {
        has_items,
        dragging,
        cursor,
        hover: f.collapsed,
        zone: f.zone,
        hold: hold(f),
        pinned_open,
    }
}

/// Where the window should take the pointer in this presence; everywhere
/// else, clicks fall through to what is below.
pub(super) fn interactive_rect(f: &Frames, presence: Presence) -> Option<Rect> {
    match presence {
        Presence::Hidden => None,
        Presence::Collapsed => Some(f.collapsed),
        Presence::Expanded => Some(hold(f)),
    }
}

/// Whether `p` is inside `r`, edges included.
pub(super) fn contains(r: &Rect, p: Point) -> bool {
    p.x >= r.x && p.x <= r.x + r.w && p.y >= r.y && p.y <= r.y + r.h
}

/// Whether the window should take the pointer at `cursor`.
pub(super) fn hits(f: &Frames, presence: Presence, cursor: Point) -> bool {
    interactive_rect(f, presence).is_some_and(|r| contains(&r, cursor))
}

/// The global point `p` in the coordinates of the window whose frame is
/// `window`.
pub(super) fn window_pos(window: &Rect, p: Point) -> LogicalPosition {
    LogicalPosition::new((p.x - window.x) as f32, (p.y - window.y) as f32)
}

/// The hover the window is told about on a tick, since AppKit sends no
/// mouse moves to an app that isn't active: a move while the cursor is over
/// the expanded window (`window`, `None` unless expanded) and moved since
/// the last tick (`last`), an exit once it left. `was_in` is whether the
/// pointer was over it; returns the event and whether it is now.
pub(super) fn hover_event(
    window: Option<&Rect>,
    cursor: Point,
    last: Option<Point>,
    was_in: bool,
) -> (Option<WindowEvent>, bool) {
    match window {
        Some(w) if contains(w, cursor) => {
            let moved = !was_in || last != Some(cursor);
            let event = moved.then(|| WindowEvent::PointerMoved {
                position: window_pos(w, cursor),
            });
            (event, true)
        }
        _ if was_in => (Some(WindowEvent::PointerExited), false),
        _ => (None, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Pocket view without the switch, as before the clipboard history.
    const OFF: Panel = Panel::Pocket { switch: false };

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn notched(notch_w: f64, menu_bar_h: f64) -> IslandHome {
        IslandHome {
            screen: rect(0.0, 0.0, 1512.0, 982.0),
            notch: Some(rect((1512.0 - notch_w) / 2.0, 0.0, notch_w, 32.0)),
            menu_bar_h,
            slot: None,
        }
    }

    fn plain(menu_bar_h: f64) -> IslandHome {
        IslandHome {
            screen: rect(0.0, 0.0, 1440.0, 900.0),
            notch: None,
            menu_bar_h,
            slot: None,
        }
    }

    #[test]
    fn a_slot_in_the_top_bar_holds_the_island() {
        // GNOME: a 96-wide slot right of the clock in a 32-high top bar.
        let home = IslandHome {
            slot: Some(rect(1060.0, 0.0, 96.0, 32.0)),
            ..plain(32.0)
        };
        let f = frames(&home, 2, OFF);
        assert!(f.notch_style);
        assert_eq!(f.notch_w, 8.0);
        assert_eq!(f.collapsed, rect(1060.0, 0.0, 96.0, 32.0));
        // Centred on the slot, not on the screen.
        assert_eq!(f.expanded.x + f.expanded.w / 2.0, 1108.0);
        assert_eq!(f.expanded.y, 0.0);
    }

    #[test]
    fn island_frame_on_notch_screen() {
        let f = frames(&notched(188.0, 38.0), 2, OFF);
        assert!(f.notch_style);
        assert_eq!(f.collapsed, rect(618.0, 0.0, 276.0, 38.0));
        // Grown by ZONE_MARGIN on the left, right and bottom, not the top.
        assert_eq!(f.zone, rect(558.0, 0.0, 396.0, 98.0));
    }

    #[test]
    fn island_frame_without_notch() {
        let f = frames(&plain(24.0), 2, OFF);
        assert!(!f.notch_style);
        assert_eq!(f.collapsed, rect(660.0, 28.0, 120.0, 28.0));
        // Off-origin screens keep the pill centred on their own screen.
        let mut home = plain(24.0);
        home.screen.x = -1440.0;
        home.screen.y = -300.0;
        assert_eq!(
            frames(&home, 2, OFF).collapsed,
            rect(-780.0, -272.0, 120.0, 28.0)
        );
    }

    #[test]
    fn expanded_frame_min_width_420() {
        assert_eq!(
            frames(&notched(188.0, 38.0), 2, OFF).expanded,
            rect(546.0, 0.0, 420.0, 150.0)
        );
        assert_eq!(frames(&plain(24.0), 2, OFF).expanded.w, 420.0);
        assert_eq!(frames(&notched(360.0, 38.0), 2, OFF).expanded.w, 456.0);
    }

    #[test]
    fn top_height_survives_an_auto_hidden_menu_bar() {
        assert_eq!(top_height(&notched(185.0, 33.0)), 33.0);
        // Auto-hidden menu bar: the notch still sets the height.
        assert_eq!(top_height(&notched(185.0, 0.0)), 32.0);
        assert_eq!(top_height(&plain(0.0)), FALLBACK_TOP_H);
    }

    #[test]
    fn only_the_drawn_shape_takes_the_pointer() {
        let f = frames(&notched(188.0, 38.0), 2, OFF);
        assert_eq!(interactive_rect(&f, Presence::Hidden), None);
        assert_eq!(interactive_rect(&f, Presence::Collapsed), Some(f.collapsed));
        assert_eq!(
            interactive_rect(&f, Presence::Expanded),
            Some(union(f.collapsed, f.expanded))
        );
        let wing = Point { x: 630.0, y: 10.0 };
        // Inside the window, under the collapsed wings: where toolbars sit.
        let below = Point { x: 700.0, y: 100.0 };
        assert!(hits(&f, Presence::Collapsed, wing));
        assert!(!hits(&f, Presence::Collapsed, below));
        assert!(hits(&f, Presence::Expanded, below));
        assert!(!hits(&f, Presence::Hidden, wing));
    }

    #[test]
    fn hover_is_replayed_only_over_the_expanded_window() {
        let w = rect(546.0, 0.0, 420.0, 150.0);
        let at = Point { x: 600.0, y: 60.0 };
        let moved = |x: f32, y: f32| {
            Some(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, y),
            })
        };
        // Entering, then moving: a move each time.
        assert_eq!(
            hover_event(Some(&w), at, None, false),
            (moved(54.0, 60.0), true)
        );
        let next = Point { x: 610.0, y: 61.0 };
        assert_eq!(
            hover_event(Some(&w), next, Some(at), true),
            (moved(64.0, 61.0), true)
        );
        // Resting: nothing to tell.
        assert_eq!(hover_event(Some(&w), next, Some(next), true), (None, true));
        // Leaving the window, or the island collapsing: one exit.
        let away = Point { x: 100.0, y: 400.0 };
        assert_eq!(
            hover_event(Some(&w), away, Some(next), true),
            (Some(WindowEvent::PointerExited), false)
        );
        assert_eq!(
            hover_event(Some(&w), away, Some(away), false),
            (None, false)
        );
        assert_eq!(
            hover_event(None, at, Some(at), true),
            (Some(WindowEvent::PointerExited), false)
        );
        assert_eq!(hover_event(None, at, Some(at), false), (None, false));
    }

    #[test]
    fn expanded_widens_to_fit_every_cell() {
        let home = notched(188.0, 38.0);
        // Up to four cells fit the minimum width.
        assert_eq!(frames(&home, 4, OFF).expanded.w, 420.0);
        // Five cells and the "Drag all" column: 14 + 5 * 68 + 10 + 52 + 14.
        assert_eq!(
            frames(&home, 5, OFF).expanded,
            rect(541.0, 0.0, 430.0, 150.0)
        );
        assert_eq!(frames(&home, 8, OFF).expanded.w, 634.0);
        // More than eight: eight cells and the "+N" tile.
        assert_eq!(frames(&home, 9, OFF).expanded.w, 682.0);
        assert_eq!(frames(&home, 40, OFF).expanded.w, 682.0);
        // Never wider than the screen less 20 pt a side.
        let mut small = plain(24.0);
        small.screen.w = 600.0;
        let f = frames(&small, 9, OFF);
        assert_eq!(f.expanded, rect(20.0, 0.0, 560.0, 136.0));
        // The collapsed shape doesn't change with the count.
        assert_eq!(
            frames(&home, 9, OFF).collapsed,
            frames(&home, 1, OFF).collapsed
        );
    }

    #[test]
    fn clipboard_panel_is_taller_and_fixed_width() {
        let home = notched(188.0, 38.0);
        let clip = frames(&home, 7, Panel::Clipboard).expanded;
        assert_eq!(clip.h, 38.0 + 232.0 + 40.0);
        // As wide as the Pocket view with nothing in it, whatever the count.
        let pocket = frames(&home, 0, Panel::Pocket { switch: true }).expanded;
        assert_eq!(clip.w, pocket.w);
        assert_eq!(clip.x, pocket.x);
        // Capped by the screen like the Pocket view.
        let mut small = plain(24.0);
        small.screen.w = 400.0;
        assert_eq!(frames(&small, 0, Panel::Clipboard).expanded.w, 360.0);
    }

    #[test]
    fn switch_adds_bar_height_to_pocket() {
        let home = notched(188.0, 38.0);
        let without = frames(&home, 3, Panel::Pocket { switch: false }).expanded;
        let with = frames(&home, 3, Panel::Pocket { switch: true }).expanded;
        assert_eq!(with.h, without.h + 40.0);
        assert_eq!(with.w, without.w);
    }

    #[test]
    fn hovering_under_the_wings_never_opens_the_island() {
        use std::time::{Duration, Instant};
        use vento_core::pocket_zone::IslandState;

        let f = frames(&notched(188.0, 38.0), 2, OFF);
        // In the drop zone, under the wings and beside them: menus and
        // toolbars of the app below.
        let under = Point { x: 700.0, y: 80.0 };
        let beside = Point { x: 590.0, y: 20.0 };
        let t0 = Instant::now();
        let mut s = IslandState::new();
        for at in [under, beside] {
            for k in 0..20 {
                let now = t0 + Duration::from_millis(100 * k);
                let presence = s.update(&island_inputs(&f, true, false, at, false), now);
                assert_eq!(presence, Presence::Collapsed);
                assert!(!hits(&f, presence, at), "{at:?} takes clicks");
            }
        }
        // Resting on the wings opens it.
        let wing = Point { x: 630.0, y: 10.0 };
        s.update(&island_inputs(&f, true, false, wing, false), t0);
        let presence = s.update(
            &island_inputs(&f, true, false, wing, false),
            t0 + Duration::from_millis(200),
        );
        assert_eq!(presence, Presence::Expanded);
    }
}
