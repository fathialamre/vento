//! Where the island's shapes sit on its screen, and which of them takes the
//! pointer.

use slint::LogicalPosition;
use slint::platform::WindowEvent;
use vento_core::geometry::{Point, Rect};
use vento_core::pocket_zone::{IslandInputs, Presence, ZONE_MARGIN};
use vento_platform::{IslandHome, TrayCorner};

/// Width of each wing beside the notch.
const WING_W: f64 = 44.0;
/// The collapsed pill on screens without a notch.
const PILL_W: f64 = 120.0;
const PILL_H: f64 = 28.0;
/// Gap between the menu bar and the pill.
const PILL_GAP: f64 = 4.0;
/// The expanded island is the notch plus this, but never narrower than
/// [`ISLAND_W`]: every view has the same width, so switching between them
/// only changes the height.
const EXPANDED_EXTRA_W: f64 = 96.0;
const ISLAND_W: f64 = 560.0;
/// Room the expanded island leaves free on each side of its screen.
const SCREEN_MARGIN: f64 = 20.0;
/// Height of the bar under every view: the switch between the views (with
/// the clipboard history on) and the shown view's own controls.
const BAR_H: f64 = 44.0;
/// Height of the Clipboard view and of the Pocket list, above the bar.
const LIST_H: f64 = 408.0;
/// One kind's group in the Pocket view, as `pocket.slint` draws it: its
/// header, its row of tiles and the gap under it; and the room above the
/// first.
pub(super) const GROUP_H: f64 = 92.0;
const GROUPS_TOP: f64 = 8.0;
/// Groups in sight at once; more scroll.
pub(super) const GROUPS_SHOWN: usize = 3;
/// Top height when neither the menu bar nor a notch gives one.
const FALLBACK_TOP_H: f64 = 32.0;
/// Room the island in the corner by the taskbar's clock leaves between it
/// and the taskbar, and between it and the screen's edge.
const CORNER_GAP: f64 = 12.0;
/// Height of the drop box the island in that corner shows during a drag.
const DROP_H: f64 = 150.0;

/// A shape no pointer is ever in: it has a negative size.
pub(super) const NOWHERE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: -1.0,
    h: -1.0,
};

/// What the expanded island shows below the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    /// The Pocket's items grouped by kind, `groups` kinds of them (none for
    /// an empty Pocket, which shows where to drop).
    Kinds { groups: usize },
    /// The list of the Pocket's items.
    List,
    /// The clipboard history's list.
    Clipboard,
    /// Only where to drop, with no bar: the island in the corner by the
    /// taskbar's clock while something is dragged.
    Drop,
}

/// The island's rects for one home, in logical top-left global coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Frames {
    /// The island lives in the corner by the taskbar's clock and grows up
    /// from there.
    pub(super) corner: bool,
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

/// The height the island takes at the top of the screen: the menu bar, or
/// the notch if taller (the menu bar reports 0 when it auto-hides).
pub(super) fn top_height(home: &IslandHome) -> f64 {
    let h = home.menu_bar_h.max(home.notch.map_or(0.0, |n| n.h));
    if h > 0.0 { h } else { FALLBACK_TOP_H }
}

/// Every shape is centred on the screen, as the window is; the notch sits in
/// the middle of its screen (to within half a point). With room kept for it
/// in the menu bar (a slot), the island fills that room as it would sit
/// around a notch, and every shape is centred on the slot. Expanded, the
/// island is as wide in every view (up to the screen's width less a
/// margin), so the pointer stays over it as it switches; its height is the
/// view's above the bottom bar: the Pocket's groups (up to
/// [`GROUPS_SHOWN`]; more scroll), or the taller lists.
pub(super) fn frames(home: &IslandHome, panel: Panel) -> Frames {
    if let Some(corner) = home.tray_corner {
        return corner_frames(home.screen, corner, panel);
    }
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
    let w = (notch_w + EXPANDED_EXTRA_W)
        .max(ISLAND_W)
        .min(screen.w - 2.0 * SCREEN_MARGIN);
    let expanded = Rect {
        x: mid - w / 2.0,
        y: screen.y,
        w,
        h: top_h + panel_h(panel),
    };
    Frames {
        corner: false,
        notch_style,
        notch_w,
        top_h,
        collapsed,
        zone,
        expanded,
    }
}

/// The island in the corner by the taskbar's clock: as wide as on top, with
/// [`CORNER_GAP`] between it and the taskbar and the screen's edge, growing
/// up from there. It draws nothing collapsed: its collapsed shape is the
/// band under it down to the screen's bottom edge, the taskbar's whole
/// height included, where resting the pointer opens it but which takes no
/// clicks. A pointer headed for the taskbar overshoots any thinner strip
/// before a sample sees it rest. A drag anywhere on its screen brings it
/// out, so the drop zone is the whole screen.
fn corner_frames(screen: Rect, corner: TrayCorner, panel: Panel) -> Frames {
    let work = corner.work;
    let w = ISLAND_W.min(work.w - 2.0 * CORNER_GAP);
    let x = if corner.left {
        work.x + CORNER_GAP
    } else {
        work.x + work.w - CORNER_GAP - w
    };
    let bottom = work.y + work.h - CORNER_GAP;
    let h = panel_h(panel);
    Frames {
        corner: true,
        notch_style: false,
        notch_w: 0.0,
        top_h: 0.0,
        collapsed: Rect {
            x,
            y: bottom,
            w,
            h: (screen.y + screen.h - bottom).max(CORNER_GAP),
        },
        zone: screen,
        expanded: Rect {
            x,
            y: bottom - h,
            w,
            h,
        },
    }
}

/// Height of what the expanded island draws below the menu bar for
/// `panel`, the bottom bar included.
fn panel_h(panel: Panel) -> f64 {
    let view = match panel {
        Panel::Kinds { groups } => GROUPS_TOP + groups.clamp(1, GROUPS_SHOWN) as f64 * GROUP_H,
        Panel::List | Panel::Clipboard => LIST_H,
        Panel::Drop => return DROP_H,
    };
    view + BAR_H
}

/// The window's frame while it is `window_h` tall below the menu bar (as
/// tall as the island, or taller a moment as it shrinks): it keeps the
/// expanded island's top, or its bottom by the clock.
pub(super) fn window_frame(f: &Frames, window_h: f64) -> Rect {
    let e = f.expanded;
    let h = f.top_h + window_h;
    let y = if f.corner { e.y + e.h - h } else { e.y };
    Rect {
        x: e.x,
        y,
        w: e.w,
        h,
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
        Presence::Collapsed => (!f.corner).then_some(f.collapsed),
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

    /// The Pocket view with two kinds of items.
    const TWO: Panel = Panel::Kinds { groups: 2 };

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn notched(notch_w: f64, menu_bar_h: f64) -> IslandHome {
        IslandHome {
            screen: rect(0.0, 0.0, 1512.0, 982.0),
            notch: Some(rect((1512.0 - notch_w) / 2.0, 0.0, notch_w, 32.0)),
            menu_bar_h,
            slot: None,
            tray_corner: None,
        }
    }

    fn plain(menu_bar_h: f64) -> IslandHome {
        IslandHome {
            screen: rect(0.0, 0.0, 1440.0, 900.0),
            notch: None,
            menu_bar_h,
            slot: None,
            tray_corner: None,
        }
    }

    /// Windows: a 1280 × 800 screen with a 48-high taskbar along the
    /// bottom, its clock at the right end unless `left`.
    fn by_the_clock(left: bool) -> IslandHome {
        IslandHome {
            tray_corner: Some(TrayCorner {
                work: rect(0.0, 0.0, 1280.0, 752.0),
                left,
            }),
            screen: rect(0.0, 0.0, 1280.0, 800.0),
            ..plain(0.0)
        }
    }

    #[test]
    fn by_the_clock_the_island_grows_up_from_the_corner() {
        let f = frames(&by_the_clock(false), TWO);
        assert!(f.corner && !f.notch_style);
        // 12 from the taskbar and the edge; 8 + 2 * 92 + 44 high.
        assert_eq!(f.expanded, rect(708.0, 504.0, 560.0, 236.0));
        // Every view keeps its bottom and its sides: only its top moves.
        for panel in [Panel::List, Panel::Clipboard, Panel::Drop] {
            let e = frames(&by_the_clock(false), panel).expanded;
            assert_eq!((e.x, e.w, e.y + e.h), (708.0, 560.0, 740.0));
        }
        assert_eq!(frames(&by_the_clock(false), Panel::Drop).expanded.h, 150.0);
        // The clock at the left end: the bottom-left corner.
        assert_eq!(frames(&by_the_clock(true), TWO).expanded.x, 12.0);
    }

    #[test]
    fn no_pointer_is_ever_nowhere() {
        for p in [(0.0, 0.0), (-1.0, -1.0), (-0.5, -0.5), (900.0, 799.0)] {
            assert!(!contains(&NOWHERE, Point { x: p.0, y: p.1 }));
        }
    }

    #[test]
    fn by_the_clock_a_rest_on_the_taskbar_or_a_drag_opens_it() {
        use std::time::{Duration, Instant};
        use vento_core::pocket_zone::IslandState;

        let f = frames(&by_the_clock(false), TWO);
        // The band under the island, down to the screen's bottom edge.
        assert_eq!(f.collapsed, rect(708.0, 740.0, 560.0, 60.0));
        // Collapsed, the window takes no clicks, even on the band.
        let low = Point { x: 900.0, y: 799.0 };
        assert_eq!(interactive_rect(&f, Presence::Collapsed), None);
        assert!(!hits(&f, Presence::Collapsed, low));
        let t0 = Instant::now();
        let at = |s: &mut IslandState, p: Point, dragging: bool, ms: u64| {
            let inputs = island_inputs(&f, true, dragging, p, false);
            s.update(&inputs, t0 + Duration::from_millis(ms))
        };
        // On the taskbar away from the island: nothing.
        let mut s = IslandState::new();
        let apps = Point { x: 400.0, y: 776.0 };
        assert_eq!(at(&mut s, apps, false, 0), Presence::Collapsed);
        assert_eq!(at(&mut s, apps, false, 500), Presence::Collapsed);
        // Resting anywhere on the taskbar under the island, even at its
        // bottom edge where a pointer thrown down ends up, opens it, and
        // moving up into it keeps it open.
        assert_eq!(at(&mut s, low, false, 1000), Presence::Collapsed);
        assert_eq!(at(&mut s, low, false, 1150), Presence::Expanded);
        let inside = Point { x: 900.0, y: 600.0 };
        assert_eq!(at(&mut s, inside, false, 2000), Presence::Expanded);
        // A drag anywhere on the screen brings it out.
        let mut s = IslandState::new();
        let far = Point { x: 40.0, y: 30.0 };
        assert_eq!(at(&mut s, far, true, 0), Presence::Expanded);
    }

    #[test]
    fn a_slot_in_the_top_bar_holds_the_island() {
        // GNOME: a 96-wide slot right of the clock in a 32-high top bar.
        let home = IslandHome {
            slot: Some(rect(1060.0, 0.0, 96.0, 32.0)),
            ..plain(32.0)
        };
        let f = frames(&home, TWO);
        assert!(f.notch_style);
        assert_eq!(f.notch_w, 8.0);
        assert_eq!(f.collapsed, rect(1060.0, 0.0, 96.0, 32.0));
        // Centred on the slot, not on the screen.
        assert_eq!(f.expanded.x + f.expanded.w / 2.0, 1108.0);
        assert_eq!(f.expanded.y, 0.0);
    }

    #[test]
    fn island_frame_on_notch_screen() {
        let f = frames(&notched(188.0, 38.0), TWO);
        assert!(f.notch_style);
        assert_eq!(f.collapsed, rect(618.0, 0.0, 276.0, 38.0));
        // Grown by ZONE_MARGIN on the left, right and bottom, not the top.
        assert_eq!(f.zone, rect(558.0, 0.0, 396.0, 98.0));
    }

    #[test]
    fn island_frame_without_notch() {
        let f = frames(&plain(24.0), TWO);
        assert!(!f.notch_style);
        assert_eq!(f.collapsed, rect(660.0, 28.0, 120.0, 28.0));
        // Off-origin screens keep the pill centred on their own screen.
        let mut home = plain(24.0);
        home.screen.x = -1440.0;
        home.screen.y = -300.0;
        assert_eq!(
            frames(&home, TWO).collapsed,
            rect(-780.0, -272.0, 120.0, 28.0)
        );
    }

    #[test]
    fn expanded_frame_is_560_wide_unless_the_notch_needs_more() {
        // 38 + 8 + 2 * 92 + 44 below the top.
        assert_eq!(
            frames(&notched(188.0, 38.0), TWO).expanded,
            rect(476.0, 0.0, 560.0, 274.0)
        );
        assert_eq!(frames(&plain(24.0), TWO).expanded.w, 560.0);
        assert_eq!(frames(&notched(500.0, 38.0), TWO).expanded.w, 596.0);
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
        let f = frames(&notched(188.0, 38.0), TWO);
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
    fn every_view_is_as_wide_and_only_its_height_changes() {
        let home = notched(188.0, 38.0);
        let views = [
            Panel::Kinds { groups: 0 },
            Panel::Kinds { groups: 7 },
            Panel::List,
            Panel::Clipboard,
        ];
        let first = frames(&home, views[0]).expanded;
        for v in views {
            let f = frames(&home, v).expanded;
            assert_eq!((f.x, f.y, f.w), (first.x, first.y, first.w));
        }
        // The lists: 408 and the bar.
        assert_eq!(frames(&home, Panel::List).expanded.h, 38.0 + 408.0 + 44.0);
        assert_eq!(
            frames(&home, Panel::Clipboard).expanded,
            frames(&home, Panel::List).expanded
        );
        // The collapsed shape doesn't change with the view.
        assert_eq!(
            frames(&home, Panel::Clipboard).collapsed,
            frames(&home, TWO).collapsed
        );
        // Never wider than the screen less 20 pt a side.
        let mut small = plain(24.0);
        small.screen.w = 500.0;
        assert_eq!(
            frames(&small, Panel::List).expanded,
            rect(20.0, 0.0, 460.0, 24.0 + 408.0 + 44.0)
        );
    }

    #[test]
    fn the_pocket_is_as_tall_as_its_groups_up_to_three() {
        let home = notched(188.0, 38.0);
        let h = |groups| frames(&home, Panel::Kinds { groups }).expanded.h;
        // Empty: room for one group, where the drop hint goes.
        assert_eq!(h(0), 38.0 + 8.0 + 92.0 + 44.0);
        assert_eq!(h(1), h(0));
        assert_eq!(h(2), h(1) + 92.0);
        assert_eq!(h(3), 38.0 + 8.0 + 3.0 * 92.0 + 44.0);
        // More scroll.
        assert_eq!(h(7), h(3));
        assert!(h(3) < frames(&home, Panel::List).expanded.h);
    }

    #[test]
    fn hovering_under_the_wings_never_opens_the_island() {
        use std::time::{Duration, Instant};
        use vento_core::pocket_zone::IslandState;

        let f = frames(&notched(188.0, 38.0), TWO);
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
