//! Per-window overrides of `NSWindow` methods for the windows vento floats
//! above the menu bar.
//!
//! - Never key nor main, so showing the window never takes keystrokes (or ⌘Q)
//!   away from the frontmost app.
//! - Never constrained to the screen's visible area. winit's `is_zoomed`
//!   briefly gives the window a title bar, and AppKit then pushes it below the
//!   menu bar (65 pt down on a notched MacBook).
//! - Clicking its view never activates vento nor orders the window front:
//!   the view delays window ordering for every mouse-down (as a drag source
//!   does) and then prevents it, so the frontmost app keeps its keystrokes.
//!
//! Changing the window's class (an isa swizzle) would break the key-value
//! observing that winit installs on every window, because KVO has already
//! swapped the isa for its own notifying subclass. Instead, each method is
//! replaced once on the window's class (as `-class` reports it, so behind KVO).
//! The replacement acts only for windows marked with an associated object and
//! defers to the original implementation for every other window.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use objc2::MainThreadMarker;
use objc2::ffi;
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSView, NSWindow};
use objc2_foundation::NSRect;
use raw_window_handle::RawWindowHandle;

use crate::PlatformError;

/// Its address marks a window that never becomes key or main.
static NON_KEY: u8 = 0;
/// Its address marks a window that keeps the frame it is given.
static FREE_FRAME: u8 = 0;
/// Its address marks a view whose clicks never activate the app.
static NO_ACTIVATE: u8 = 0;

type BoolGetter = unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool;
type Constrain = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, NSRect, *mut AnyObject) -> NSRect;
type EventGetter = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;
type EventHandler = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

/// The implementations replaced, by class address and selector. It is global,
/// like the class methods it mirrors.
static ORIGINALS: LazyLock<Mutex<HashMap<(usize, Sel), Imp>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn originals() -> MutexGuard<'static, HashMap<(usize, Sel), Imp>> {
    ORIGINALS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn key_ptr(key: &'static u8) -> *const c_void {
    std::ptr::from_ref(key).cast()
}

fn is_marked(object: *mut AnyObject, key: &'static u8) -> bool {
    // SAFETY: `object` is the receiver of a message, so it is a live object.
    !unsafe { ffi::objc_getAssociatedObject(object, key_ptr(key)) }.is_null()
}

fn mark(object: &AnyObject, key: &'static u8) {
    let ptr = std::ptr::from_ref(object).cast_mut();
    // SAFETY: the value is the object itself, stored without retaining it,
    // so the mark lives exactly as long as the object.
    unsafe { ffi::objc_setAssociatedObject(ptr, key_ptr(key), ptr, ffi::OBJC_ASSOCIATION_ASSIGN) };
}

/// The replaced implementation of `sel` for `object`: the one saved for its
/// class, or for the nearest superclass that was patched.
fn original(object: *mut AnyObject, sel: Sel) -> Option<Imp> {
    // SAFETY: `object` is the receiver of a message, so it is a live object.
    let mut class: Option<&AnyClass> = unsafe { ffi::object_getClass(object).as_ref() };
    let map = originals();
    while let Some(c) = class {
        if let Some(imp) = map.get(&(std::ptr::from_ref(c) as usize, sel)) {
            return Some(*imp);
        }
        class = c.superclass();
    }
    None
}

unsafe extern "C-unwind" fn refuse_if_marked(this: *mut AnyObject, sel: Sel) -> Bool {
    if is_marked(this, &NON_KEY) {
        return Bool::NO;
    }
    match original(this, sel) {
        // SAFETY: the saved implementation has this signature (see `patch`).
        Some(imp) => unsafe { std::mem::transmute::<Imp, BoolGetter>(imp)(this, sel) },
        None => Bool::YES,
    }
}

unsafe extern "C-unwind" fn keep_frame_if_marked(
    this: *mut AnyObject,
    sel: Sel,
    rect: NSRect,
    screen: *mut AnyObject,
) -> NSRect {
    if is_marked(this, &FREE_FRAME) {
        return rect;
    }
    match original(this, sel) {
        // SAFETY: the saved implementation has this signature (see `patch`).
        Some(imp) => unsafe { std::mem::transmute::<Imp, Constrain>(imp)(this, sel, rect, screen) },
        None => rect,
    }
}

unsafe extern "C-unwind" fn delay_if_marked(
    this: *mut AnyObject,
    sel: Sel,
    event: *mut AnyObject,
) -> Bool {
    if is_marked(this, &NO_ACTIVATE) {
        return Bool::YES;
    }
    match original(this, sel) {
        // SAFETY: the saved implementation has this signature (see `patch`).
        Some(imp) => unsafe { std::mem::transmute::<Imp, EventGetter>(imp)(this, sel, event) },
        None => Bool::NO,
    }
}

/// Prevents the ordering (and activation) that was delayed for this
/// mouse-down, then lets the view handle it as before.
unsafe extern "C-unwind" fn mouse_down_without_ordering(
    this: *mut AnyObject,
    sel: Sel,
    event: *mut AnyObject,
) {
    if is_marked(this, &NO_ACTIVATE)
        && let Some(mtm) = MainThreadMarker::new()
    {
        NSApplication::sharedApplication(mtm).preventWindowOrdering();
    }
    if let Some(imp) = original(this, sel) {
        // SAFETY: the saved implementation has this signature (see `patch`).
        unsafe { std::mem::transmute::<Imp, EventHandler>(imp)(this, sel, event) }
    }
}

/// Whether `imp` is one of vento's replacements.
fn is_replacement(imp: Imp) -> bool {
    // SAFETY: only the addresses are compared; nothing is called.
    let ours: [Imp; 4] = unsafe {
        [
            std::mem::transmute::<BoolGetter, Imp>(refuse_if_marked),
            std::mem::transmute::<Constrain, Imp>(keep_frame_if_marked),
            std::mem::transmute::<EventGetter, Imp>(delay_if_marked),
            std::mem::transmute::<EventHandler, Imp>(mouse_down_without_ordering),
        ]
    };
    ours.iter().any(|&o| std::ptr::fn_addr_eq(o, imp))
}

/// Replaces `sel` on `class` with `replacement`, once. The caller passes a
/// replacement with the same signature as the method it replaces.
fn patch(class: &AnyClass, sel: Sel, replacement: Imp) -> Result<(), PlatformError> {
    let key = (std::ptr::from_ref(class) as usize, sel);
    let mut map = originals();
    if map.contains_key(&key) {
        return Ok(());
    }
    // SAFETY: plain runtime lookups on a registered class.
    let method = unsafe { ffi::class_getInstanceMethod(class, sel) };
    if method.is_null() {
        return Err(PlatformError::Failed(format!(
            "{} does not implement {}",
            class.name().to_string_lossy(),
            sel.name().to_string_lossy()
        )));
    }
    // SAFETY: `method` is a valid method of `class`.
    let (imp, types) = unsafe {
        (
            ffi::method_getImplementation(method),
            ffi::method_getTypeEncoding(method),
        )
    };
    let imp = imp.ok_or_else(|| PlatformError::Failed("method has no implementation".into()))?;
    if is_replacement(imp) {
        // Inherited from a class already patched: that replacement already
        // acts for this class, and recording it as the original would make
        // it call itself.
        return Ok(());
    }
    map.insert(key, imp);
    // SAFETY: the replacement keeps the signature and type encoding of the
    // method it replaces (or inherits from).
    unsafe {
        ffi::class_replaceMethod(
            std::ptr::from_ref(class).cast_mut(),
            sel,
            replacement,
            types,
        )
    };
    Ok(())
}

/// The class methods are patched on: `-class`, which, unlike the isa, is the
/// class KVO hides behind.
fn class_of(object: &AnyObject) -> &'static AnyClass {
    unsafe { msg_send![object, class] }
}

/// Marks `object` as never key nor main, patching its class first.
fn refuse_key(object: &AnyObject) -> Result<(), PlatformError> {
    let class = class_of(object);
    // SAFETY: both selectors take no arguments and return BOOL.
    let replacement = unsafe { std::mem::transmute::<BoolGetter, Imp>(refuse_if_marked) };
    patch(class, sel!(canBecomeKeyWindow), replacement)?;
    patch(class, sel!(canBecomeMainWindow), replacement)?;
    mark(object, &NON_KEY);
    Ok(())
}

/// Marks `object` as keeping whatever frame it is given, patching its class
/// first.
fn free_frame(object: &AnyObject) -> Result<(), PlatformError> {
    // SAFETY: `constrainFrameRect:toScreen:` takes an NSRect and an NSScreen
    // and returns an NSRect.
    let replacement = unsafe { std::mem::transmute::<Constrain, Imp>(keep_frame_if_marked) };
    patch(
        class_of(object),
        sel!(constrainFrameRect:toScreen:),
        replacement,
    )?;
    mark(object, &FREE_FRAME);
    Ok(())
}

/// Marks `view` as never activating the app when clicked, patching its
/// class first.
fn refuse_activation(view: &AnyObject) -> Result<(), PlatformError> {
    let class = class_of(view);
    // SAFETY: `shouldDelayWindowOrderingForEvent:` takes an NSEvent and
    // returns BOOL; `mouseDown:` takes an NSEvent and returns nothing.
    let (delay, down) = unsafe {
        (
            std::mem::transmute::<EventGetter, Imp>(delay_if_marked),
            std::mem::transmute::<EventHandler, Imp>(mouse_down_without_ordering),
        )
    };
    patch(class, sel!(shouldDelayWindowOrderingForEvent:), delay)?;
    patch(class, sel!(mouseDown:), down)?;
    mark(view, &NO_ACTIVATE);
    Ok(())
}

/// The window behind an AppKit `handle`, on the main thread.
pub(super) fn window_of(
    handle: RawWindowHandle,
    what: &str,
) -> Result<objc2::rc::Retained<NSWindow>, PlatformError> {
    let RawWindowHandle::AppKit(h) = handle else {
        return Err(PlatformError::Unsupported);
    };
    if MainThreadMarker::new().is_none() {
        log::warn!("{what} called off the main thread; ignored");
        return Err(PlatformError::Failed("not on the main thread".into()));
    }
    // SAFETY: an AppKit window handle's `ns_view` points to a live NSView
    // for as long as the window exists, and we are on the main thread.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    view.window()
        .ok_or_else(|| PlatformError::Failed("view is not in a window".into()))
}

/// Keep `window` exactly where it is placed, menu bar or not.
pub(super) fn keep_frame(window: &NSWindow) -> Result<(), PlatformError> {
    free_frame(window)
}

/// Make the window behind `handle` refuse key and main status, and clicks
/// on its view never activate vento.
pub(super) fn make_non_key(handle: RawWindowHandle) -> Result<(), PlatformError> {
    let window = window_of(handle, "make_non_key")?;
    refuse_key(&window)?;
    if window.isKeyWindow() {
        window.resignKeyWindow();
    }
    let RawWindowHandle::AppKit(h) = handle else {
        return Err(PlatformError::Unsupported);
    };
    // SAFETY: as in `window_of`, which succeeded on the main thread.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    refuse_activation(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::ClassType;
    use objc2::rc::Retained;
    use objc2::runtime::{ClassBuilder, NSObject};
    use objc2_foundation::{NSPoint, NSSize};

    extern "C-unwind" fn yes(_this: &AnyObject, _sel: Sel) -> Bool {
        Bool::YES
    }

    /// Stands in for AppKit pushing a window below the menu bar.
    extern "C-unwind" fn push_down(
        _this: &AnyObject,
        _sel: Sel,
        rect: NSRect,
        _screen: *mut AnyObject,
    ) -> NSRect {
        NSRect::new(NSPoint::new(rect.origin.x, rect.origin.y - 65.0), rect.size)
    }

    /// A stand-in for a window class that can become key and main and is
    /// constrained to the screen.
    fn window_like_class() -> &'static AnyClass {
        // Tests run in parallel: register the class exactly once.
        static CLASS: std::sync::OnceLock<&'static AnyClass> = std::sync::OnceLock::new();
        CLASS.get_or_init(register_window_like)
    }

    fn register_window_like() -> &'static AnyClass {
        let mut b = ClassBuilder::new(c"VentoTestWindowLike", NSObject::class()).unwrap();
        unsafe {
            b.add_method(
                sel!(canBecomeKeyWindow),
                yes as extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(canBecomeMainWindow),
                yes as extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(constrainFrameRect:toScreen:),
                push_down as extern "C-unwind" fn(_, _, _, _) -> _,
            );
        }
        b.register()
    }

    fn new_window_like() -> Retained<AnyObject> {
        unsafe { msg_send![window_like_class(), new] }
    }

    fn answers(object: &AnyObject) -> (bool, bool) {
        let key: Bool = unsafe { msg_send![object, canBecomeKeyWindow] };
        let main: Bool = unsafe { msg_send![object, canBecomeMainWindow] };
        (key.as_bool(), main.as_bool())
    }

    fn constrained(object: &AnyObject) -> NSRect {
        let rect = NSRect::new(NSPoint::new(546.0, 837.0), NSSize::new(420.0, 145.0));
        let screen: *mut AnyObject = std::ptr::null_mut();
        unsafe { msg_send![object, constrainFrameRect: rect, toScreen: screen] }
    }

    #[test]
    fn only_marked_objects_refuse_key_and_main() {
        let island = new_window_like();
        let other = new_window_like();
        refuse_key(&island).unwrap();
        // Marking a second time patches nothing twice.
        refuse_key(&island).unwrap();
        assert_eq!(answers(&island), (false, false));
        assert_eq!(answers(&other), (true, true));
        assert!(std::ptr::eq(island.class(), window_like_class()));
    }

    #[test]
    fn only_marked_objects_keep_their_frame() {
        let island = new_window_like();
        let other = new_window_like();
        free_frame(&island).unwrap();
        assert_eq!(constrained(&island).origin.y, 837.0);
        assert_eq!(constrained(&other).origin.y, 772.0);
    }

    static MOUSE_DOWNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    extern "C-unwind" fn no_delay(_this: &AnyObject, _sel: Sel, _event: *mut AnyObject) -> Bool {
        Bool::NO
    }

    extern "C-unwind" fn count_mouse_down(_this: &AnyObject, _sel: Sel, _event: *mut AnyObject) {
        MOUSE_DOWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// A stand-in for winit's view: orders its window front at once and
    /// handles mouse-downs.
    fn view_like_class() -> &'static AnyClass {
        static CLASS: std::sync::OnceLock<&'static AnyClass> = std::sync::OnceLock::new();
        CLASS.get_or_init(|| {
            let mut b = ClassBuilder::new(c"VentoTestViewLike", NSObject::class()).unwrap();
            unsafe {
                b.add_method(
                    sel!(shouldDelayWindowOrderingForEvent:),
                    no_delay as extern "C-unwind" fn(_, _, _) -> _,
                );
                b.add_method(
                    sel!(mouseDown:),
                    count_mouse_down as extern "C-unwind" fn(_, _, _),
                );
            }
            b.register()
        })
    }

    #[test]
    fn only_marked_views_delay_window_ordering_and_still_get_mouse_downs() {
        let island: Retained<AnyObject> = unsafe { msg_send![view_like_class(), new] };
        let other: Retained<AnyObject> = unsafe { msg_send![view_like_class(), new] };
        refuse_activation(&island).unwrap();
        refuse_activation(&island).unwrap();
        let event: *mut AnyObject = std::ptr::null_mut();
        let delays = |view: &AnyObject| -> bool {
            let b: Bool = unsafe { msg_send![view, shouldDelayWindowOrderingForEvent: event] };
            b.as_bool()
        };
        assert!(delays(&island));
        assert!(!delays(&other));
        // Off the main thread nothing is prevented, but the view's own
        // handler always runs, once.
        let before = MOUSE_DOWNS.load(std::sync::atomic::Ordering::SeqCst);
        let _: () = unsafe { msg_send![&*island, mouseDown: event] };
        let _: () = unsafe { msg_send![&*other, mouseDown: event] };
        assert_eq!(
            MOUSE_DOWNS.load(std::sync::atomic::Ordering::SeqCst),
            before + 2
        );
    }

    #[test]
    fn a_subclass_of_a_patched_class_is_not_patched_again() {
        // The parent is patched first (any window of it was marked).
        refuse_key(&new_window_like()).unwrap();
        static SUB: std::sync::OnceLock<&'static AnyClass> = std::sync::OnceLock::new();
        let sub = *SUB.get_or_init(|| {
            ClassBuilder::new(c"VentoTestWindowLikeSub", window_like_class())
                .unwrap()
                .register()
        });
        let island: Retained<AnyObject> = unsafe { msg_send![sub, new] };
        let other: Retained<AnyObject> = unsafe { msg_send![sub, new] };
        // The subclass inherits vento's replacement: recording that as its
        // original would make every unmarked window call itself forever.
        refuse_key(&island).unwrap();
        assert!(
            !originals()
                .contains_key(&(std::ptr::from_ref(sub) as usize, sel!(canBecomeKeyWindow)))
        );
        assert_eq!(answers(&island), (false, false));
        assert_eq!(answers(&other), (true, true));
    }

    #[test]
    fn classes_without_the_methods_are_refused() {
        let object: Retained<AnyObject> = unsafe { msg_send![NSObject::class(), new] };
        assert!(refuse_key(&object).is_err());
    }
}
