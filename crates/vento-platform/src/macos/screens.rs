//! Screen geometry and app activation.

use std::cell::{Cell, OnceCell};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{AllocAnyThread, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationDidChangeScreenParametersNotification, NSScreen};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowOwnerPID,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use vento_core::geometry::{Point, Rect};

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

thread_local! {
    /// The primary screen's height as last read; `None` until it is read,
    /// and again after the screens change. The notification arrives on the
    /// main thread, where the height is read. Kept apart from the flag in
    /// `notify`, which the Pocket consumes.
    static PRIMARY_HEIGHT: Cell<Option<f64>> = const { Cell::new(None) };
    /// Forgets that height when the screens change; kept for good.
    static SCREEN_WATCH: OnceCell<Retained<ScreenWatch>> = const { OnceCell::new() };
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `ScreenWatch` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "VentoScreenWatch"]
    struct ScreenWatch;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ScreenWatch {}

    impl ScreenWatch {
        #[unsafe(method(screensChanged:))]
        fn screens_did_change(&self, _note: &NSNotification) {
            // `try_with`: a panic must not unwind into AppKit.
            let _ = PRIMARY_HEIGHT.try_with(|cell| cell.set(None));
        }
    }
);

impl ScreenWatch {
    /// Creates the watch and registers it for screen changes. The
    /// notification center holds it weakly: keep it alive.
    fn install() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer.
        let watch: Retained<Self> = unsafe { msg_send![super(this), init] };
        // SAFETY: `screensChanged:` is a method of `ScreenWatch` that takes
        // one NSNotification and returns nothing.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &watch,
                sel!(screensChanged:),
                Some(NSApplicationDidChangeScreenParametersNotification),
                None,
            );
        }
        watch
    }
}

/// Height of the primary screen (the first in `NSScreen.screens`), read
/// again only after the screens change: the input poll asks on every tick.
pub(super) fn primary_height(mtm: MainThreadMarker) -> f64 {
    cached_primary_height(|| {
        NSScreen::screens(mtm)
            .firstObject()
            .map(|s| s.frame().size.height)
            .unwrap_or(0.0)
    })
}

/// The cached primary height, from `read` when none is cached or the
/// screens changed since.
fn cached_primary_height(read: impl FnOnce() -> f64) -> f64 {
    SCREEN_WATCH.with(|watch| {
        watch.get_or_init(ScreenWatch::install);
    });
    if let Some(height) = PRIMARY_HEIGHT.with(Cell::get) {
        return height;
    }
    let height = read();
    PRIMARY_HEIGHT.with(|cell| cell.set(Some(height)));
    height
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

/// The backing scale of the screen whose full frame (menu bar included)
/// contains `p`, in global logical top-left-origin coordinates; the primary
/// screen's when none does.
pub(super) fn scale_factor_at(p: Point) -> f64 {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("scale_factor_at() called off the main thread; returning 1.0");
        return 1.0;
    };
    let primary_h = primary_height(mtm);
    let all = NSScreen::screens(mtm);
    let hit = all.iter().find(|s| {
        let f = s.frame();
        frame_contains(
            flip_rect(
                primary_h,
                f.origin.x,
                f.origin.y,
                f.size.width,
                f.size.height,
            ),
            p,
        )
    });
    hit.or_else(|| all.firstObject())
        .map_or(1.0, |s| s.backingScaleFactor())
}

/// Whether `p` is inside `r` (left and top edges included).
pub(super) fn frame_contains(r: Rect, p: Point) -> bool {
    p.x >= r.x && p.x < r.x + r.w && p.y >= r.y && p.y < r.y + r.h
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
        slot: None,
        tray_corner: None,
    })
}

/// Whether a window at `layer`, of the process `pid`, is an app's normal
/// window, not vento's (`ours`) nor the menu bar, the Dock, a panel or the
/// desktop, which all sit at other layers.
fn counts_as_front(layer: i64, pid: i64, ours: i64) -> bool {
    layer == 0 && pid != ours
}

/// The frame of the frontmost normal window of another app, in top-left
/// global points: `kCGWindowBounds` is already in those. Reads no window
/// contents, so needs no Screen Recording permission. Any thread.
pub(super) fn front_window_frame() -> Option<Rect> {
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID)?;
    // SAFETY: the window list is an array of dictionaries keyed by strings.
    let list =
        unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(list) };
    // SAFETY: the keys are immutable constants of CoreGraphics.
    let (layer_key, pid_key, alpha_key, bounds_key) = unsafe {
        (
            kCGWindowLayer,
            kCGWindowOwnerPID,
            kCGWindowAlpha,
            kCGWindowBounds,
        )
    };
    let ours = i64::from(std::process::id());
    let frame = |window: &CFDictionary<CFString, CFType>| -> Option<Rect> {
        let number = |key| window.get(key)?.downcast::<CFNumber>().ok();
        let layer = number(layer_key)?.as_i64()?;
        let pid = number(pid_key)?.as_i64()?;
        // Some apps keep invisible windows on screen.
        let alpha = number(alpha_key).and_then(|a| a.as_f64()).unwrap_or(1.0);
        if !counts_as_front(layer, pid, ours) || alpha <= 0.0 {
            return None;
        }
        let bounds = window.get(bounds_key)?.downcast::<CFDictionary>().ok()?;
        let mut r = CGRect::default();
        // SAFETY: `bounds` is the dictionary form of a rect; `r` is a live
        // local.
        let read = unsafe { CGRectMakeWithDictionaryRepresentation(Some(&bounds), &mut r) };
        (read && r.size.width > 0.0 && r.size.height > 0.0).then_some(Rect {
            x: r.origin.x,
            y: r.origin.y,
            w: r.size.width,
            h: r.size.height,
        })
    };
    // Front to back: the first window that counts is the front one.
    list.iter().find_map(|w| frame(&w))
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
    fn only_other_apps_normal_windows_are_front_windows() {
        assert!(counts_as_front(0, 501, 77));
        assert!(!counts_as_front(0, 77, 77), "vento's own window");
        assert!(!counts_as_front(25, 501, 77), "the menu bar or a panel");
        assert!(!counts_as_front(-2147483623, 501, 77), "the desktop");
    }

    #[test]
    fn the_front_window_has_a_size_when_there_is_one() {
        if let Some(f) = front_window_frame() {
            assert!(f.w > 0.0 && f.h > 0.0);
        }
    }

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
    fn frame_contains_includes_top_left_and_excludes_bottom_right() {
        let r = Rect {
            x: -100.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        assert!(frame_contains(r, Point { x: -100.0, y: 0.0 }));
        assert!(frame_contains(r, Point { x: -1.0, y: 10.0 }));
        assert!(!frame_contains(r, Point { x: 0.0, y: 10.0 }));
        assert!(!frame_contains(r, Point { x: -50.0, y: 50.0 }));
    }

    #[test]
    fn primary_height_is_read_again_after_a_screen_change() {
        let read = |h: f64| move || h;
        // The cache is per thread, and a post is delivered on the thread
        // that posts: other tests posting screen changes leave this one be.
        assert_eq!(cached_primary_height(read(900.0)), 900.0);
        assert_eq!(cached_primary_height(read(1.0)), 900.0, "cached");
        // SAFETY: a plain notification post, delivered on this thread.
        unsafe {
            NSNotificationCenter::defaultCenter().postNotificationName_object(
                NSApplicationDidChangeScreenParametersNotification,
                None,
            );
        }
        assert_eq!(cached_primary_height(read(1080.0)), 1080.0);
        assert_eq!(cached_primary_height(read(1.0)), 1080.0);
    }

    #[test]
    fn screen_below_primary_gets_y_past_primary_height() {
        let r = flip_rect(900.0, 0.0, -1080.0, 1920.0, 1080.0);
        assert_eq!(r.y, 900.0);
    }
}
