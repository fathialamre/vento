//! Where the island's shapes sit on its screen, and which of them takes the
//! pointer.

use slint::LogicalPosition;
use slint::platform::WindowEvent;
use vento_core::geometry::{Point, Rect};
use vento_core::pocket_zone::{IslandInputs, Presence, ZONE_MARGIN};
use vento_core::settings::IslandPlace;
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
/// Height of the Clipboard view, the Pocket list and the Screenshots view,
/// above the bar.
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
/// Height of the card a new screenshot shows below the menu bar: its
/// picture and actions, with no bar.
pub(super) const CARD_H: f64 = 158.0;

/// Room the virtual island keeps free at each end of the top edge, for
/// whatever lives in the corners.
const SIDE_KEEP: f64 = 180.0;
/// Gap between the screen's top and the virtual pill, or between a top bar
/// and it.
const VIRTUAL_GAP: f64 = 6.0;
const VIRTUAL_GAP_UNDER_BAR: f64 = 4.0;
/// The lip the virtual island rests as over a bar of the front window.
const LIP_W: f64 = 84.0;
const LIP_H: f64 = 6.0;
/// How far the push zone reaches past the pill on each side, and how deep
/// it is: a thin strip along the screen's top edge.
const PUSH_PAD: f64 = 40.0;
const PUSH_DEPTH: f64 = 2.0;
/// How far the front window's top may be from the bar's bottom and still
/// count as a bar under the island.
const BAR_TOLERANCE: f64 = 8.0;

/// The news pill: its height, and its one button's and "+N" chip's, and
/// how far the button sits in from the pill's right end.
const NEWS_H: f64 = 34.0;
const NEWS_BUTTON_H: f64 = 26.0;
const NEWS_CHIP_H: f64 = 20.0;
const NEWS_BUTTON_INSET: f64 = 5.0;

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
    /// The screenshots' grid, as tall as the Clipboard view.
    Screenshots,
    /// Only where to drop, with no bar: the island in the corner by the
    /// taskbar's clock while something is dragged.
    Drop,
    /// The card of a screenshot just saved, with no bar.
    Card,
}

/// What the virtual island needs to know besides its home: where on the top
/// edge it sits, and whether the front window has a bar under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VirtualSpot {
    pub(super) place: IslandPlace,
    /// The front window's own bar lies along the island's place, so the
    /// island rests as a lip instead of a pill.
    pub(super) bar_under: bool,
}

/// The island's rects for one home, in logical top-left global coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Frames {
    /// The island lives in the corner by the taskbar's clock and grows up
    /// from there.
    pub(super) corner: bool,
    /// The virtual island: a pill at the top that rests visible, opens when
    /// pushed against the screen's top edge and is not tied to a notch.
    pub(super) virtual_island: bool,
    /// Its resting form is the lip rather than the pill.
    pub(super) lip: bool,
    /// Where pushing the pointer against the top edge opens it; [`NOWHERE`]
    /// outside the virtual island.
    pub(super) push: Rect,
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
        virtual_island: false,
        lip: false,
        push: NOWHERE,
        notch_style,
        notch_w,
        top_h,
        collapsed,
        zone,
        expanded,
    }
}

/// The bottom of the bar the virtual island hangs from: the top of the
/// taskbar's work area on Windows, else the menu bar's bottom; the screen's
/// top when there is no bar.
pub(super) fn bar_bottom(home: &IslandHome) -> f64 {
    match home.tray_corner {
        Some(corner) => corner.work.y,
        None => home.screen.y + home.menu_bar_h,
    }
}

/// The x the virtual island is centred on: the screen's middle, or
/// [`SIDE_KEEP`] in from its left or right end. On a screen too small to
/// keep both corners free, every place is the middle.
pub(super) fn virtual_mid_x(home: &IslandHome, place: IslandPlace) -> f64 {
    let screen = home.screen;
    let w = ISLAND_W.min(screen.w - 2.0 * SCREEN_MARGIN);
    let centre = screen.x + screen.w / 2.0;
    let left = screen.x + SIDE_KEEP + w / 2.0;
    let right = screen.x + screen.w - SIDE_KEEP - w / 2.0;
    if left > centre {
        return centre;
    }
    match place {
        IslandPlace::Left => left,
        IslandPlace::Centre => centre,
        IslandPlace::Right => right,
    }
}

/// Whether the front window has a bar under the island: its frame's top is
/// within [`BAR_TOLERANCE`] of the bar's bottom or of the screen's top (a
/// fullscreen video, game or slide show under a menu bar, GNOME's top bar or
/// a top taskbar) and it spans the pill's x range around `mid_x`.
pub(super) fn bar_under(front: Option<Rect>, home: &IslandHome, mid_x: f64) -> bool {
    front.is_some_and(|f| {
        ((f.y - bar_bottom(home)).abs() <= BAR_TOLERANCE
            || (f.y - home.screen.y).abs() <= BAR_TOLERANCE)
            && f.x <= mid_x - PILL_W / 2.0
            && f.x + f.w >= mid_x + PILL_W / 2.0
    })
}

/// The virtual island: the pill [`VIRTUAL_GAP`] under the screen's top
/// (less under a top bar), or the lip hanging from the bar's lower edge
/// over a window's bar. It is centred on `spot.place`; expanded, it is
/// [`ISLAND_W`] wide as in every mode. Like the notch island, its window
/// starts at the bar's edge, with the gap as its top height, so the lip and
/// the pill both lie in it. Pushing against
/// the top edge near the pill opens it, so the push zone is a thin strip
/// along the screen's top, and the zone for a drag is the pill and the lip
/// grown by [`ZONE_MARGIN`] on the left, right and bottom.
pub(super) fn virtual_frames(home: &IslandHome, spot: VirtualSpot, panel: Panel) -> Frames {
    let screen = home.screen;
    let bar = bar_bottom(home);
    let mid = virtual_mid_x(home, spot.place);
    let gap = if (bar - screen.y).abs() < 0.5 {
        VIRTUAL_GAP
    } else {
        VIRTUAL_GAP_UNDER_BAR
    };
    let pill = Rect {
        x: mid - PILL_W / 2.0,
        y: bar + gap,
        w: PILL_W,
        h: PILL_H,
    };
    let lip = Rect {
        x: mid - LIP_W / 2.0,
        y: bar,
        w: LIP_W,
        h: LIP_H,
    };
    let collapsed = if spot.bar_under { lip } else { pill };
    let rest = union(pill, lip);
    let zone = Rect {
        x: rest.x - ZONE_MARGIN,
        y: rest.y,
        w: rest.w + 2.0 * ZONE_MARGIN,
        h: rest.h + ZONE_MARGIN,
    };
    let w = ISLAND_W.min(screen.w - 2.0 * SCREEN_MARGIN);
    Frames {
        corner: false,
        virtual_island: true,
        lip: spot.bar_under,
        push: Rect {
            x: pill.x - PUSH_PAD,
            y: screen.y,
            w: PILL_W + 2.0 * PUSH_PAD,
            h: PUSH_DEPTH,
        },
        notch_style: false,
        notch_w: 0.0,
        top_h: gap,
        collapsed,
        zone,
        expanded: Rect {
            x: mid - w / 2.0,
            y: bar,
            w,
            h: gap + panel_h(panel),
        },
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
        virtual_island: false,
        lip: false,
        push: NOWHERE,
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
        Panel::List | Panel::Clipboard | Panel::Screenshots => LIST_H,
        Panel::Drop => return DROP_H,
        Panel::Card => return CARD_H,
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
        rest_visible: f.virtual_island,
        push: f.virtual_island.then_some(f.push),
    }
}

/// Where the news pill and what on it takes clicks lie, in logical
/// top-left global coordinates. The pill itself takes none: clicks on its
/// text fall through to what is below.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct NewsRects {
    pub pill: Rect,
    pub button: Option<Rect>,
    pub chip: Option<Rect>,
}

/// The news pill `pill_w` wide, with its button `button_w` wide and its
/// "+N" chip `chip_w` wide at `chip_x` from the pill's left (as the window
/// lays them out; `None` for none). The pill is [`NEWS_H`] high with its
/// top at the resting pill's, also while the island rests as the lip, and
/// centred on the island; the button sits [`NEWS_BUTTON_INSET`] in from its
/// right end. Both are centred in its height.
pub(super) fn news_rects(
    f: &Frames,
    pill_w: f64,
    button_w: Option<f64>,
    chip_w: Option<f64>,
    chip_x: Option<f64>,
) -> NewsRects {
    let mid = f.expanded.x + f.expanded.w / 2.0;
    let pill = Rect {
        x: mid - pill_w / 2.0,
        y: f.expanded.y + f.top_h,
        w: pill_w,
        h: NEWS_H,
    };
    let button = button_w.map(|w| Rect {
        x: pill.x + pill.w - NEWS_BUTTON_INSET - w,
        y: pill.y + (NEWS_H - NEWS_BUTTON_H) / 2.0,
        w,
        h: NEWS_BUTTON_H,
    });
    let chip = chip_w.zip(chip_x).map(|(w, x)| Rect {
        x: pill.x + x,
        y: pill.y + (NEWS_H - NEWS_CHIP_H) / 2.0,
        w,
        h: NEWS_CHIP_H,
    });
    NewsRects { pill, button, chip }
}

/// Where the window should take the pointer in this presence; everywhere
/// else, clicks fall through to what is below. The resting virtual island
/// takes none, except on the button and the chip of the news it shows.
pub(super) fn interactive_rects(
    f: &Frames,
    presence: Presence,
    news: Option<&NewsRects>,
) -> Vec<Rect> {
    match presence {
        Presence::Hidden => Vec::new(),
        Presence::Collapsed if f.virtual_island => news
            .map(|n| n.button.into_iter().chain(n.chip).collect())
            .unwrap_or_default(),
        Presence::Collapsed if f.corner => Vec::new(),
        Presence::Collapsed => vec![f.collapsed],
        Presence::Expanded => vec![hold(f)],
    }
}

/// Whether `p` is inside `r`, edges included.
pub(super) fn contains(r: &Rect, p: Point) -> bool {
    p.x >= r.x && p.x <= r.x + r.w && p.y >= r.y && p.y <= r.y + r.h
}

/// Whether the window should take the pointer at `cursor`, with `news`
/// showing.
pub(super) fn hits(
    f: &Frames,
    presence: Presence,
    news: Option<&NewsRects>,
    cursor: Point,
) -> bool {
    interactive_rects(f, presence, news)
        .iter()
        .any(|r| contains(r, cursor))
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
        for panel in [
            Panel::List,
            Panel::Clipboard,
            Panel::Screenshots,
            Panel::Drop,
        ] {
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
        assert!(interactive_rects(&f, Presence::Collapsed, None).is_empty());
        assert!(!hits(&f, Presence::Collapsed, None, low));
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
        assert!(interactive_rects(&f, Presence::Hidden, None).is_empty());
        assert_eq!(
            interactive_rects(&f, Presence::Collapsed, None),
            vec![f.collapsed]
        );
        assert_eq!(
            interactive_rects(&f, Presence::Expanded, None),
            vec![union(f.collapsed, f.expanded)]
        );
        let wing = Point { x: 630.0, y: 10.0 };
        // Inside the window, under the collapsed wings: where toolbars sit.
        let below = Point { x: 700.0, y: 100.0 };
        assert!(hits(&f, Presence::Collapsed, None, wing));
        assert!(!hits(&f, Presence::Collapsed, None, below));
        assert!(hits(&f, Presence::Expanded, None, below));
        assert!(!hits(&f, Presence::Hidden, None, wing));
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
            Panel::Screenshots,
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
    fn card_panel_has_no_bar() {
        assert_eq!(panel_h(Panel::Card), CARD_H);
        // As wide as every other view.
        let card = frames(&notched(200.0, 37.0), Panel::Card);
        let kinds = frames(&notched(200.0, 37.0), TWO);
        assert_eq!(card.expanded.w, kinds.expanded.w);
        assert_eq!(card.expanded.h, card.top_h + CARD_H);
    }

    #[test]
    fn screenshots_panel_is_as_tall_as_clipboard() {
        let home = notched(188.0, 38.0);
        assert_eq!(
            frames(&home, Panel::Screenshots).expanded,
            frames(&home, Panel::Clipboard).expanded
        );
        assert_eq!(panel_h(Panel::Screenshots), LIST_H + BAR_H);
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
                assert!(!hits(&f, presence, None, at), "{at:?} takes clicks");
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

    fn spot(place: IslandPlace, bar_under: bool) -> VirtualSpot {
        VirtualSpot { place, bar_under }
    }

    #[test]
    fn virtual_pill_sits_at_the_top_centre_of_a_windows_screen() {
        let f = virtual_frames(&by_the_clock(false), spot(IslandPlace::Centre, false), TWO);
        assert!(f.virtual_island && !f.corner && !f.lip);
        assert_eq!(f.collapsed, rect(580.0, 6.0, 120.0, 28.0));
        assert_eq!(f.push, rect(540.0, 0.0, 200.0, 2.0));
        assert_eq!(
            f.expanded,
            rect(360.0, 0.0, 560.0, 6.0 + 8.0 + 2.0 * 92.0 + 44.0)
        );
        assert_eq!(f.top_h, 6.0);
    }

    #[test]
    fn the_virtual_window_holds_the_lip_and_the_pill() {
        for home in [by_the_clock(false), plain(24.0)] {
            for bar in [false, true] {
                let f = virtual_frames(&home, spot(IslandPlace::Centre, bar), TWO);
                let window = window_frame(&f, f.expanded.h - f.top_h);
                let lip = virtual_frames(&home, spot(IslandPlace::Centre, true), TWO).collapsed;
                let pill = virtual_frames(&home, spot(IslandPlace::Centre, false), TWO).collapsed;
                for shape in [lip, pill] {
                    assert_eq!(union(window, shape), window, "{shape:?} outside {window:?}");
                }
                assert_eq!(window, f.expanded);
            }
        }
        let f = virtual_frames(&plain(24.0), spot(IslandPlace::Centre, false), TWO);
        assert_eq!(f.expanded.y, 24.0);
        assert_eq!(f.top_h, 4.0);
    }

    #[test]
    fn over_a_bar_it_rests_as_a_lip_on_the_edge() {
        let f = virtual_frames(&by_the_clock(false), spot(IslandPlace::Centre, true), TWO);
        assert!(f.lip);
        assert_eq!(f.collapsed, rect(598.0, 0.0, 84.0, 6.0));
    }

    #[test]
    fn under_a_menu_bar_the_lip_hangs_from_the_bar() {
        let f = virtual_frames(&plain(24.0), spot(IslandPlace::Centre, true), TWO);
        assert_eq!(f.collapsed, rect(678.0, 24.0, 84.0, 6.0));
        let f = virtual_frames(&plain(24.0), spot(IslandPlace::Centre, false), TWO);
        assert_eq!(f.collapsed, rect(660.0, 28.0, 120.0, 28.0));
        assert_eq!(f.push.y, 0.0);
    }

    #[test]
    fn left_and_right_keep_the_corners_free() {
        let home = by_the_clock(false);
        assert_eq!(virtual_mid_x(&home, IslandPlace::Left), 180.0 + 280.0);
        assert_eq!(
            virtual_mid_x(&home, IslandPlace::Right),
            1280.0 - 180.0 - 280.0
        );
    }

    #[test]
    fn small_screen_places_everything_at_centre() {
        let home = IslandHome {
            screen: rect(0.0, 0.0, 900.0, 600.0),
            ..plain(0.0)
        };
        for p in IslandPlace::ALL {
            assert_eq!(virtual_mid_x(&home, p), 450.0);
        }
        let f = virtual_frames(&home, spot(IslandPlace::Left, false), TWO);
        assert!(f.expanded.x >= 0.0 && f.expanded.x + f.expanded.w <= 900.0);
    }

    #[test]
    fn virtual_frames_follow_the_screen_origin() {
        let home = IslandHome {
            screen: rect(-1920.0, -200.0, 1920.0, 1080.0),
            ..plain(0.0)
        };
        let f = virtual_frames(&home, spot(IslandPlace::Centre, false), TWO);
        assert_eq!(f.collapsed, rect(-1020.0, -194.0, 120.0, 28.0));
        assert_eq!(f.push.y, -200.0);
    }

    #[test]
    fn a_resting_virtual_island_takes_no_clicks() {
        let f = virtual_frames(&by_the_clock(false), spot(IslandPlace::Centre, false), TWO);
        assert!(interactive_rects(&f, Presence::Collapsed, None).is_empty());
        assert_eq!(
            interactive_rects(&f, Presence::Expanded, None),
            vec![hold(&f)]
        );
    }

    /// A news pill 300 wide with a 60-wide button and a 28-wide chip 200 in.
    fn news_with(f: &Frames, button: bool, chip: bool) -> NewsRects {
        news_rects(
            f,
            300.0,
            button.then_some(60.0),
            chip.then_some(28.0),
            chip.then_some(200.0),
        )
    }

    #[test]
    fn only_the_button_and_chip_take_clicks() {
        let f = virtual_frames(&by_the_clock(false), spot(IslandPlace::Centre, false), TWO);
        let news = news_with(&f, true, true);
        // 34 high at the resting pill's top, the button 5 in from its right
        // end and the chip where Slint put it, both centred in its height.
        assert_eq!(news.pill, rect(490.0, 6.0, 300.0, 34.0));
        assert_eq!(news.button, Some(rect(725.0, 10.0, 60.0, 26.0)));
        assert_eq!(news.chip, Some(rect(690.0, 13.0, 28.0, 20.0)));
        assert_eq!(
            interactive_rects(&f, Presence::Collapsed, Some(&news)),
            vec![news.button.unwrap(), news.chip.unwrap()]
        );
        // The pill's text lets clicks through to the tab bar below.
        let text = Point { x: 550.0, y: 23.0 };
        assert!(contains(&news.pill, text));
        assert!(!hits(&f, Presence::Collapsed, Some(&news), text));
        let button = Point { x: 750.0, y: 23.0 };
        assert!(hits(&f, Presence::Collapsed, Some(&news), button));
        let chip = Point { x: 700.0, y: 23.0 };
        assert!(hits(&f, Presence::Collapsed, Some(&news), chip));
        // Hidden, nothing does.
        assert!(interactive_rects(&f, Presence::Hidden, Some(&news)).is_empty());
    }

    #[test]
    fn a_news_pill_without_a_button_takes_no_clicks() {
        let f = virtual_frames(&by_the_clock(false), spot(IslandPlace::Centre, false), TWO);
        let news = news_with(&f, false, false);
        assert_eq!((news.button, news.chip), (None, None));
        assert!(interactive_rects(&f, Presence::Collapsed, Some(&news)).is_empty());
        let middle = Point { x: 640.0, y: 23.0 };
        assert!(!hits(&f, Presence::Collapsed, Some(&news), middle));
        // A chip alone takes its own clicks only.
        let news = news_with(&f, false, true);
        assert_eq!(
            interactive_rects(&f, Presence::Collapsed, Some(&news)),
            vec![news.chip.unwrap()]
        );
        // A chip's width without its x is no chip.
        let news = news_rects(&f, 300.0, None, Some(28.0), None);
        assert_eq!(news.chip, None);
    }

    #[test]
    fn the_news_pill_is_centred_on_the_island() {
        let home = plain(24.0);
        for place in IslandPlace::ALL {
            let mid = virtual_mid_x(&home, place);
            for bar in [false, true] {
                let f = virtual_frames(&home, spot(place, bar), TWO);
                let news = news_with(&f, true, false);
                // Under the menu bar by the gap, over the lip too.
                assert_eq!(news.pill, rect(mid - 150.0, 28.0, 300.0, 34.0));
            }
        }
    }

    #[test]
    fn an_open_island_still_takes_the_whole_hold() {
        let f = virtual_frames(&by_the_clock(false), spot(IslandPlace::Centre, false), TWO);
        let news = news_with(&f, true, true);
        assert_eq!(
            interactive_rects(&f, Presence::Expanded, Some(&news)),
            vec![hold(&f)]
        );
        let text = Point { x: 550.0, y: 23.0 };
        assert!(hits(&f, Presence::Expanded, Some(&news), text));
        // Without news, as before.
        assert_eq!(
            interactive_rects(&f, Presence::Expanded, None),
            vec![hold(&f)]
        );
        assert!(interactive_rects(&f, Presence::Collapsed, None).is_empty());
    }

    #[test]
    fn bar_under_ignores_none_and_off_span_windows() {
        let home = by_the_clock(false);
        assert!(!bar_under(None, &home, 640.0));
        assert!(bar_under(Some(rect(0.0, 0.0, 1280.0, 752.0)), &home, 640.0));
        assert!(bar_under(
            Some(rect(400.0, 7.0, 600.0, 400.0)),
            &home,
            640.0
        ));
        assert!(!bar_under(
            Some(rect(400.0, 9.0, 600.0, 400.0)),
            &home,
            640.0
        ));
        assert!(!bar_under(Some(rect(0.0, 0.0, 600.0, 752.0)), &home, 640.0));
    }

    #[test]
    fn fullscreen_front_window_counts_as_bar_under() {
        let home = by_the_clock(false);
        assert!(bar_under(Some(home.screen), &home, 640.0));
    }

    #[test]
    fn fullscreen_under_a_top_bar_counts_as_bar_under() {
        // A Mac menu bar: fullscreen video, a game or slides.
        let home = plain(24.0);
        assert!(bar_under(Some(home.screen), &home, 720.0));
        // GNOME's 32-px top bar.
        let home = plain(32.0);
        assert!(bar_under(Some(home.screen), &home, 720.0));
        // A normal window starting at the bar's bottom still counts.
        let home = plain(24.0);
        assert!(bar_under(
            Some(rect(200.0, 24.0, 1000.0, 600.0)),
            &home,
            720.0
        ));
        // Between the screen's top and the bar's bottom, past the tolerance
        // of both, it does not.
        assert!(!bar_under(
            Some(rect(200.0, 12.0, 1000.0, 600.0)),
            &home,
            720.0
        ));
        // A fullscreen window off the pill's span does not either.
        assert!(!bar_under(Some(rect(0.0, 0.0, 600.0, 900.0)), &home, 720.0));
    }

    #[test]
    fn under_a_top_taskbar_the_island_hangs_from_the_work_area() {
        // Windows with the taskbar along the top: the work area starts at 48.
        let home = IslandHome {
            tray_corner: Some(TrayCorner {
                work: rect(0.0, 48.0, 1280.0, 752.0),
                left: false,
            }),
            screen: rect(0.0, 0.0, 1280.0, 800.0),
            ..plain(0.0)
        };
        assert_eq!(bar_bottom(&home), 48.0);
        let f = virtual_frames(&home, spot(IslandPlace::Centre, false), TWO);
        assert_eq!(f.collapsed, rect(580.0, 52.0, 120.0, 28.0));
        let f = virtual_frames(&home, spot(IslandPlace::Centre, true), TWO);
        assert_eq!(f.collapsed, rect(598.0, 48.0, 84.0, 6.0));
    }

    #[test]
    fn a_bar_a_hair_off_the_screen_top_keeps_the_full_gap() {
        // A scaled screen can put the bar a fraction off the screen's top.
        let home = plain(0.2);
        let f = virtual_frames(&home, spot(IslandPlace::Centre, false), TWO);
        assert_eq!(f.collapsed.y, 0.2 + 6.0);
    }
}
