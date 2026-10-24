//! Dragging items out of the Pocket: what a drag takes, one session per
//! press, its preview, and what a drop removes.

use std::cell::Cell;

use log::debug;
use slint::platform::{PointerEventButton, WindowEvent};
use vento_core::geometry::{Point, Rect};
use vento_platform::RgbaImage;

use super::geometry::{contains, window_pos};
#[cfg(target_os = "linux")]
use crate::drag;

/// The items a drag takes, in Pocket order (`all`): every item for "Drag
/// all" (`pressed` is `None`); the whole selection when the pressed item is
/// selected; otherwise the pressed item alone. Missing items are left out.
pub(super) fn drag_ids(
    pressed: Option<u64>,
    all: &[u64],
    selected: &[u64],
    missing: &[u64],
) -> Vec<u64> {
    let wanted = |id: u64| match pressed {
        None => true,
        Some(p) if selected.contains(&p) => selected.contains(&id),
        Some(p) => id == p,
    };
    all.iter()
        .copied()
        .filter(|&id| wanted(id) && !missing.contains(&id))
        .collect()
}

/// Where a drag out ended, in the logical top-left global coordinates the
/// island uses. drag 2.1.1 (macOS) flips AppKit's bottom-left screen point
/// against `CGDisplayPixelsHigh` of the main display, which is the primary
/// screen's height in points (982 on this Mac, whose panel is 1964 pixels
/// high) and so the same flip the platform layer uses; only the widening
/// is left to do.
pub(super) fn drop_point(pos: &drag::CursorPosition) -> Point {
    Point {
        x: f64::from(pos.x),
        y: f64::from(pos.y),
    }
}

/// Whether a drag that ended with `result` at `at` removes what it took:
/// only a drop, only with the setting on, and never a drop back on the
/// island (`island`, its expanded rect), which the island takes as nothing.
pub(super) fn removes_after_drop(
    result: drag::DragResult,
    remove_after_drop: bool,
    at: Point,
    island: Option<Rect>,
) -> bool {
    let on_island = island.is_some_and(|r| contains(&r, at));
    matches!(result, drag::DragResult::Dropped) && remove_after_drop && !on_island
}

/// The press target of "Drag all"; any other target is a cell's index.
pub(super) const ALL: u32 = u32::MAX;
/// How far, in logical pixels, a press moves before it becomes a drag.
const DRAG_DISTANCE: f32 = 4.0;

/// Turns presses into clicks and drag sessions, at most one session per
/// press: Slint keeps reporting moves after a session took over the mouse
/// (it never sees that release), so a move alone must not start another.
#[derive(Default)]
pub(super) struct PressLatch {
    /// What is pressed: set on press, cleared on release or when the press
    /// starts a drag.
    token: Cell<Option<u32>>,
    /// The press moved [`DRAG_DISTANCE`] or more, so its release is no click.
    far: Cell<bool>,
    /// A drag session is running; the drag callback ends it.
    running: Cell<bool>,
}

impl PressLatch {
    /// A new press. A session still marked running is stale (its drag
    /// callback never came, or the mouse could not be pressed again during
    /// it): it is forgotten, so this press can drag.
    pub(super) fn press(&self, target: u32) {
        if self.running.replace(false) {
            debug!("stale drag session; resetting");
        }
        self.token.set(Some(target));
        self.far.set(false);
    }

    /// The target to drag, once per press, when the press has moved far
    /// enough and no session is running.
    pub(super) fn moved(&self, dx: f32, dy: f32) -> Option<u32> {
        if dx.hypot(dy) < DRAG_DISTANCE {
            return None;
        }
        self.far.set(true);
        if self.running.get() {
            return None;
        }
        let target = self.token.take()?;
        self.running.set(true);
        Some(target)
    }

    /// The target clicked, if the press ends without having moved far.
    pub(super) fn release(&self) -> Option<u32> {
        let target = self.token.take();
        if self.far.replace(false) {
            None
        } else {
            target
        }
    }

    /// The drag session is over (or never started).
    pub(super) fn ended(&self) {
        self.running.set(false);
    }

    /// Whether a drag session is running.
    pub(super) fn running(&self) -> bool {
        self.running.get()
    }
}

/// What the island's window (its frame `window`) is told when a drag out
/// ends at `at`: the release Slint never saw, since the drag session took
/// the mouse (without it, the pressed cell keeps the pointer grabbed), and
/// that the pointer left, unless it is back over the island (`over_island`).
pub(super) fn drag_end_events(window: &Rect, at: Point, over_island: bool) -> Vec<WindowEvent> {
    let mut events = vec![WindowEvent::PointerReleased {
        position: window_pos(window, at),
        button: PointerEventButton::Left,
    }];
    if !over_island {
        events.push(WindowEvent::PointerExited);
    }
    events
}

/// `icon` as PNG bytes for a drag preview, tagged with the resolution that
/// makes AppKit size it at `points` (an icon rendered at 2× is 144 dpi).
/// `None` if its buffer doesn't match its size.
pub(super) fn preview_png(icon: &RgbaImage, points: u32) -> Option<Vec<u8>> {
    let len = icon.width as usize * icon.height as usize * 4;
    if len == 0 || icon.pixels.len() != len || points == 0 {
        return None;
    }
    // 72 dpi is one pixel per point: 2834.6 pixels per metre.
    let ppm = (72.0 / 0.0254 * f64::from(icon.width) / f64::from(points)).round() as u32;
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, icon.width, icon.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: ppm,
        yppu: ppm,
        unit: png::Unit::Meter,
    }));
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&icon.pixels).ok()?;
    writer.finish().ok()?;
    Some(bytes)
}

/// A plain grey square, for a preview when the icon can't be rendered (the
/// drag needs some image).
pub(super) fn placeholder_icon(px: u32) -> RgbaImage {
    RgbaImage {
        width: px,
        height: px,
        pixels: [0x80, 0x80, 0x88, 0xc0].repeat(px as usize * px as usize),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn drag_ids_use_selection_when_pressed_cell_is_selected() {
        let all = [1, 2, 3, 4];
        // The pressed cell is selected: the whole selection, in Pocket order.
        assert_eq!(drag_ids(Some(3), &all, &[3, 1], &[]), vec![1, 3]);
        // The pressed cell is not selected: that cell alone.
        assert_eq!(drag_ids(Some(2), &all, &[3, 1], &[]), vec![2]);
        // "Drag all" takes everything, selected or not.
        assert_eq!(drag_ids(None, &all, &[3], &[]), vec![1, 2, 3, 4]);
        // An id the Pocket no longer has is not dragged.
        assert!(drag_ids(Some(9), &all, &[], &[]).is_empty());
    }

    #[test]
    fn drag_ids_skip_missing() {
        let all = [1, 2, 3];
        assert_eq!(drag_ids(Some(1), &all, &[1, 2], &[2]), vec![1]);
        assert_eq!(drag_ids(None, &all, &[], &[1, 3]), vec![2]);
        // Everything selected is missing: nothing to drag.
        assert!(drag_ids(Some(2), &all, &[2, 3], &[2, 3]).is_empty());
    }

    #[test]
    fn drop_inside_island_is_ignored() {
        let island = rect(546.0, 0.0, 420.0, 150.0);
        // The drag crate already reports logical top-left global points.
        let inside = drop_point(&drag::CursorPosition { x: 700, y: 40 });
        assert_eq!(inside, Point { x: 700.0, y: 40.0 });
        let outside = drop_point(&drag::CursorPosition { x: 700, y: 400 });
        let left_screen = drop_point(&drag::CursorPosition { x: -900, y: 20 });
        assert_eq!(left_screen, Point { x: -900.0, y: 20.0 });

        use drag::DragResult::{Cancel, Dropped};
        assert!(!removes_after_drop(Dropped, true, inside, Some(island)));
        let corner = Point { x: 546.0, y: 150.0 };
        assert!(!removes_after_drop(Dropped, true, corner, Some(island)));
        assert!(removes_after_drop(Dropped, true, outside, Some(island)));
        assert!(removes_after_drop(Dropped, true, left_screen, Some(island)));
        assert!(removes_after_drop(Dropped, true, inside, None));
        assert!(!removes_after_drop(Cancel, true, outside, Some(island)));
        assert!(!removes_after_drop(Dropped, false, outside, Some(island)));
    }

    #[test]
    fn one_drag_per_press() {
        let latch = PressLatch::default();
        let mut starts = 0;
        latch.press(2);
        for (dx, dy) in [(1.0, 1.0), (4.0, 0.0), (9.0, 3.0), (20.0, 5.0)] {
            if let Some(target) = latch.moved(dx, dy) {
                assert_eq!(target, 2);
                starts += 1;
                // The session ends at once; later moves of this press
                // still start nothing.
                latch.ended();
            }
        }
        assert_eq!(latch.release(), None, "a drag is not a click");
        assert_eq!(starts, 1);
        // Stale moves after the press start nothing either.
        assert_eq!(latch.moved(30.0, 0.0), None);

        // A new press drags again; its moves start nothing more while that
        // session runs.
        latch.press(ALL);
        assert_eq!(latch.moved(0.0, 5.0), Some(ALL));
        assert_eq!(latch.moved(8.0, 0.0), None);
        latch.ended();
        assert_eq!(latch.moved(9.0, 0.0), None);
    }

    #[test]
    fn a_press_resets_a_stale_session() {
        let latch = PressLatch::default();
        latch.press(1);
        assert_eq!(latch.moved(9.0, 0.0), Some(1));
        assert!(latch.running());
        // The drag callback never came; the next press still drags.
        latch.release();
        latch.press(2);
        assert!(!latch.running());
        assert_eq!(latch.moved(9.0, 0.0), Some(2));
        assert!(latch.running());
        latch.ended();
        assert!(!latch.running());
    }

    #[test]
    fn a_drag_end_releases_the_grab() {
        let window = rect(546.0, 0.0, 420.0, 150.0);
        let at = Point { x: 700.0, y: 400.0 };
        assert_eq!(
            drag_end_events(&window, at, false),
            vec![
                WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(154.0, 400.0),
                    button: PointerEventButton::Left,
                },
                WindowEvent::PointerExited,
            ]
        );
        // Dropped back on the island: the pointer is still over it.
        let on = Point { x: 600.0, y: 60.0 };
        assert_eq!(
            drag_end_events(&window, on, true),
            vec![WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(54.0, 60.0),
                button: PointerEventButton::Left,
            }]
        );
    }

    #[test]
    fn a_short_press_is_a_click() {
        let latch = PressLatch::default();
        latch.press(3);
        assert_eq!(latch.moved(2.0, 2.0), None);
        assert_eq!(latch.release(), Some(3));
        // Released twice (up, then a cancel): one click.
        assert_eq!(latch.release(), None);
        assert_eq!(latch.release(), None);
    }

    #[test]
    fn preview_is_a_png_of_the_icon_at_its_point_size() {
        let icon = RgbaImage {
            width: 80,
            height: 80,
            pixels: vec![200; 80 * 80 * 4],
        };
        let bytes = preview_png(&icon, 40).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!((info.width, info.height), (80, 80));
        // 144 dpi, so AppKit sizes the 80-pixel image at 40 points.
        let dims = info.pixel_dims.unwrap();
        assert_eq!((dims.xppu, dims.yppu), (5669, 5669));
        assert!(
            preview_png(
                &RgbaImage {
                    width: 2,
                    height: 2,
                    pixels: vec![0; 3]
                },
                40
            )
            .is_none()
        );
    }
}
