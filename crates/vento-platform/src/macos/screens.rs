//! Screen geometry and app activation.

use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSApplication, NSScreen};
use objc2_foundation::NSObjectProtocol;
use vento_core::geometry::Rect;

use crate::IslandHome;

/// Convert a bottom-left-origin AppKit rect to top-left-origin logical
/// coordinates, using the primary screen's height as the flip axis.
pub(super) fn flip_rect(primary_h: f64, x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        x,
        y: primary_h - (y + h),
        w,
        h,
    }
}

/// Height of the primary screen (the first in `NSScreen.screens`).
pub(super) fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm)
        .firstObject()
        .map(|s| s.frame().size.height)
        .unwrap_or(0.0)
}

/// Each screen's work area (`visibleFrame`: without the menu bar and Dock),
/// flipped against the primary screen's full frame height.
pub(super) fn screens() -> Vec<Rect> {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("screens() called off the main thread; returning none");
        return Vec::new();
    };
    let primary_h = primary_height(mtm);
    NSScreen::screens(mtm)
        .iter()
        .map(|s| {
            let f = s.visibleFrame();
            flip_rect(
                primary_h,
                f.origin.x,
                f.origin.y,
                f.size.width,
                f.size.height,
            )
        })
        .collect()
}

/// The notch between the two menu-bar halves of `screen`, `top_inset` high.
///
/// All rects are already in top-left-origin coordinates. The aux areas hug
/// the screen's top corners, so only their widths matter. `None` when the
/// screen has no top inset or either aux area is empty.
pub(super) fn notch_rect(
    screen: Rect,
    left_aux: Rect,
    right_aux: Rect,
    top_inset: f64,
) -> Option<Rect> {
    let empty = |r: Rect| r.w <= 0.0 || r.h <= 0.0;
    if top_inset <= 0.0 || empty(left_aux) || empty(right_aux) {
        return None;
    }
    let w = screen.w - left_aux.w - right_aux.w;
    (w > 0.0).then_some(Rect {
        x: screen.x + left_aux.w,
        y: screen.y,
        w,
        h: top_inset,
    })
}

/// The first notched screen (or else the primary one), its notch and its
/// menu-bar height, in top-left-origin logical coordinates.
pub(super) fn pocket_home() -> Option<IslandHome> {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("pocket_home() called off the main thread; returning none");
        return None;
    };
    let all = NSScreen::screens(mtm);
    let screen = all
        .iter()
        .find(|s| s.safeAreaInsets().top > 0.0)
        .or_else(|| all.firstObject())?;
    let primary_h = primary_height(mtm);
    let flip = |r: objc2_foundation::NSRect| {
        flip_rect(
            primary_h,
            r.origin.x,
            r.origin.y,
            r.size.width,
            r.size.height,
        )
    };

    let frame = screen.frame();
    let visible = screen.visibleFrame();
    let menu_bar_h =
        (frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height);
    let screen_rect = flip(frame);
    let notch = notch_rect(
        screen_rect,
        flip(screen.auxiliaryTopLeftArea()),
        flip(screen.auxiliaryTopRightArea()),
        screen.safeAreaInsets().top,
    );
    Some(IslandHome {
        screen: screen_rect,
        notch,
        menu_bar_h,
    })
}

/// Bring the app to the front so a modal panel can take focus.
///
/// `activate` only exists on macOS 14+; macOS 13 needs the older call.
pub(super) fn activate_app(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if app.respondsToSelector(sel!(activate)) {
        app.activate();
    } else {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_screen_stays_at_origin() {
        let r = flip_rect(900.0, 0.0, 0.0, 1440.0, 900.0);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 0.0, 1440.0, 900.0));
    }

    #[test]
    fn screen_above_primary_gets_negative_y() {
        // AppKit: a 1080-high screen sitting directly above a 900-high primary.
        let r = flip_rect(900.0, 0.0, 900.0, 1920.0, 1080.0);
        assert_eq!(r.y, -1080.0);
    }

    #[test]
    fn primary_work_area_starts_below_the_menu_bar() {
        // AppKit visibleFrame of a 900-high primary with a 25 px menu bar
        // and a 70 px Dock at the bottom.
        let r = flip_rect(900.0, 0.0, 70.0, 1440.0, 805.0);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 25.0, 1440.0, 805.0));
    }

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn notch_rect_between_aux_areas() {
        let screen = r(0.0, 0.0, 1512.0, 982.0);
        let left = r(0.0, 0.0, 662.0, 38.0);
        let right = r(850.0, 0.0, 662.0, 38.0);
        let n = notch_rect(screen, left, right, 38.0).expect("notch");
        assert_eq!((n.x, n.y, n.w, n.h), (662.0, 0.0, 188.0, 38.0));
    }

    #[test]
    fn no_notch_when_inset_zero() {
        let screen = r(0.0, 0.0, 1512.0, 982.0);
        let left = r(0.0, 0.0, 662.0, 38.0);
        let right = r(850.0, 0.0, 662.0, 38.0);
        assert!(notch_rect(screen, left, right, 0.0).is_none());
    }

    #[test]
    fn no_notch_when_aux_empty() {
        let screen = r(0.0, 0.0, 1440.0, 900.0);
        let empty = r(0.0, 0.0, 0.0, 0.0);
        assert!(notch_rect(screen, empty, empty, 38.0).is_none());
    }

    #[test]
    fn screen_below_primary_gets_y_past_primary_height() {
        let r = flip_rect(900.0, 0.0, -1080.0, 1920.0, 1080.0);
        assert_eq!(r.y, 900.0);
    }
}
