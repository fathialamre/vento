//! The Copy from screen overlay: one dimmed, borderless panel per screen,
//! on which the user drags a rectangle.
//!
//! Mouse-up hands the rectangle to the caller. Esc, a right click, a drag
//! under 4 × 4 pt, a screen change and another app taking focus cancel. The
//! panels join every Space, so switching Space leaves the overlay up.
//!
//! Everything here runs on the main thread. A selection lives in [`SESSION`]
//! from [`select_area`] until [`finish`], which takes it out (so `done` runs
//! once) and orders every panel out *before* calling `done`, so the overlay
//! is never in the capture that follows.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class,
    msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions,
    NSApplicationDidChangeScreenParametersNotification, NSApplicationDidResignActiveNotification,
    NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSCursor, NSEvent, NSPanel,
    NSRectFillUsingOperation, NSRunningApplication, NSScreen, NSScreenSaverWindowLevel,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString,
};
use vento_core::geometry::{Point, Rect};
use vento_core::screen_text::{Selection, selection_from_drag};

use super::screens::{activate_app, flip_rect, frame_contains, primary_height};

/// The dim over each screen: black at this alpha.
const DIM_ALPHA: f64 = 0.35;
/// The selection's border: white at this alpha, this many points wide.
const BORDER_ALPHA: f64 = 0.8;
const BORDER_WIDTH: f64 = 1.0;
/// `kVK_Escape`.
const ESC_KEY_CODE: u16 = 53;

/// What [`select_area`] calls once, with the selection or `None`.
type Done = Box<dyn FnOnce(Option<Selection>)>;

/// An overlay on screen, waiting for the user.
struct Session {
    /// Tells this session's panels and observer from those of an earlier
    /// one that AppKit may still send a queued event to.
    id: u64,
    panels: Vec<Retained<SelectPanel>>,
    done: Done,
    observer: Retained<SelectObserver>,
    /// The app that had focus before the overlay took it, given it back
    /// when the overlay goes.
    previous_app: Option<Retained<NSRunningApplication>>,
}

thread_local! {
    /// The overlay on screen, if any. Only the main thread ever fills it.
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    /// The id of the next session.
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

/// Global top-left-origin point of `view`, a point in the content view
/// (bottom-left origin) of a panel whose AppKit frame is `panel_frame`.
fn to_global(view: NSPoint, panel_frame: NSRect, primary_height: f64) -> Point {
    Point {
        x: panel_frame.origin.x + view.x,
        y: primary_height - (panel_frame.origin.y + view.y),
    }
}

/// Dims every screen and lets the user drag a rectangle on one of them;
/// `done` gets it, or `None` when cancelled. Main thread only.
pub(super) fn select_area(done: Done) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("select_area called off the main thread; cancelled");
        call(done, None);
        return;
    };
    let busy = SESSION.with(|slot| slot.try_borrow().map_or(true, |s| s.is_some()));
    if busy {
        log::warn!("an area is already being selected; the new selection is cancelled");
        call(done, None);
        return;
    }

    let id = NEXT_ID.with(|n| n.replace(n.get().wrapping_add(1)));
    let primary_h = primary_height(mtm);
    let panels: Vec<_> = NSScreen::screens(mtm)
        .iter()
        .filter_map(|screen| {
            let Some(display_id) = display_id(&screen) else {
                log::warn!("a screen has no display number; it is not dimmed");
                return None;
            };
            Some(SelectPanel::new(
                mtm,
                id,
                display_id,
                screen.frame(),
                primary_h,
            ))
        })
        .collect();
    if panels.is_empty() {
        log::warn!("no screen to select an area on; cancelled");
        call(done, None);
        return;
    }

    let me = NSRunningApplication::currentApplication().processIdentifier();
    let previous_app = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .filter(|app| app.processIdentifier() != me);
    // The panel under the pointer takes the keyboard, for Esc.
    let mouse = NSEvent::mouseLocation();
    let mouse = Point {
        x: mouse.x,
        y: primary_h - mouse.y,
    };
    let key = panels
        .iter()
        .position(|p| {
            let f = p.frame();
            frame_contains(
                flip_rect(
                    primary_h,
                    f.origin.x,
                    f.origin.y,
                    f.size.width,
                    f.size.height,
                ),
                mouse,
            )
        })
        .unwrap_or(0);
    let shown: Vec<_> = panels.iter().map(|p| p.retain()).collect();

    let session = Session {
        id,
        panels,
        done,
        observer: SelectObserver::install(mtm, id),
        previous_app,
    };
    // Stored before anything shows, so every event finds it.
    let stored = SESSION.with(|slot| match slot.try_borrow_mut() {
        Ok(mut slot) => {
            *slot = Some(session);
            None
        }
        Err(_) => Some(session),
    });
    if let Some(session) = stored {
        log::warn!("the selection could not be stored; cancelled");
        session.observer.remove();
        call(session.done, None);
        return;
    }

    for panel in &shown {
        panel.orderFrontRegardless();
    }
    shown[key].makeKeyWindow();
    // An agent app gets no keys until it is active.
    activate_app(mtm);
    // Cursor rects apply only once the pointer moves; show it at once.
    NSCursor::crosshairCursor().set();
}

/// Ends session `id`, if it is still the one on screen: orders every panel
/// out, gives focus back, then calls `done` with `sel`. Any later call (a
/// second mouse-down, a late notification) finds no session and does
/// nothing.
fn finish(id: u64, sel: Option<Selection>) {
    // Off the main thread `SESSION` is another thread's, always empty.
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let taken = SESSION.with(|slot| {
        let mut slot = slot.try_borrow_mut().ok()?;
        if slot.as_ref().is_some_and(|s| s.id == id) {
            slot.take()
        } else {
            None
        }
    });
    let Some(session) = taken else {
        return;
    };
    // First, before `done`: the capture that follows must never see the
    // dim, whether or not vento's windows are left out of it.
    for panel in &session.panels {
        panel.orderOut(None);
    }
    session.observer.remove();
    give_focus_back(mtm, session.previous_app);
    // AppKit may still be inside one of these panels' (or the observer's)
    // event handling: the run loop's autorelease pool releases them once
    // that is over.
    for panel in session.panels {
        let _ = Retained::autorelease_ptr(panel);
    }
    let _ = Retained::autorelease_ptr(session.observer);
    call(session.done, sel);
}

/// Hands focus back to the app that had it before the overlay, unless
/// another app has taken it since.
fn give_focus_back(mtm: MainThreadMarker, previous: Option<Retained<NSRunningApplication>>) {
    let Some(previous) = previous else {
        return;
    };
    if NSApplication::sharedApplication(mtm).isActive()
        && !previous.isTerminated()
        && !previous.activateWithOptions(NSApplicationActivationOptions::empty())
    {
        log::debug!("could not give focus back after selecting an area");
    }
}

/// Calls `done`, never letting a panic unwind into AppKit.
fn call(done: Done, sel: Option<Selection>) {
    if catch_unwind(AssertUnwindSafe(|| done(sel))).is_err() {
        log::error!("area selection callback panicked");
    }
}

/// The `CGDirectDisplayID` of `screen`.
fn display_id(screen: &NSScreen) -> Option<u32> {
    let number = screen
        .deviceDescription()
        .objectForKey(&NSString::from_str("NSScreenNumber"))?;
    number
        .downcast::<NSNumber>()
        .ok()
        .map(|n| n.unsignedIntValue())
}

define_class!(
    // SAFETY:
    // - NSPanel has no subclassing requirements beyond calling its
    //   designated initializer, which `SelectPanel::new` does.
    // - `SelectPanel` does not implement `Drop`.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoSelectPanel"]
    struct SelectPanel;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SelectPanel {}

    impl SelectPanel {
        /// A borderless window cannot become key unless it says so; the
        /// overlay must, for Esc.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

impl SelectPanel {
    /// A clear panel over the full `frame` (AppKit coordinates) of screen
    /// `display_id`, above everything, holding a [`SelectView`].
    fn new(
        mtm: MainThreadMarker,
        session: u64,
        display_id: u32,
        frame: NSRect,
        primary_h: f64,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `initWithContentRect:styleMask:backing:defer:` is
        // NSWindow's designated initializer, sent with the argument types it
        // takes.
        let panel: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        // SAFETY: the panel is owned by the session, not by AppKit, so it
        // must not release itself when closed.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(NSScreenSaverWindowLevel);
        panel.setHidesOnDeactivate(false);
        // Gone the moment it is ordered out, not after a fade.
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        // Not left to the initializer, which may move a window off the
        // menu bar.
        panel.setFrame_display(frame, false);
        let view = SelectView::new(
            mtm,
            ViewIvars {
                session,
                display_id,
                frame,
                primary_h,
                start: Cell::new(None),
                end: Cell::new(None),
            },
        );
        panel.setContentView(Some(&view));
        panel.makeFirstResponder(Some(&view));
        panel
    }
}

/// State behind one panel's view.
struct ViewIvars {
    /// The [`Session`] the view belongs to.
    session: u64,
    display_id: u32,
    /// The screen's full frame, AppKit coordinates (bottom-left origin).
    frame: NSRect,
    /// The flip axis between AppKit and global coordinates.
    primary_h: f64,
    /// Where the drag started and where it is now, in view coordinates.
    start: Cell<Option<NSPoint>>,
    end: Cell<Option<NSPoint>>,
}

impl ViewIvars {
    /// The screen's full frame in global coordinates.
    fn display(&self) -> Rect {
        let f = self.frame;
        flip_rect(
            self.primary_h,
            f.origin.x,
            f.origin.y,
            f.size.width,
            f.size.height,
        )
    }

    /// The dragged rectangle, global and clamped to this screen; `None`
    /// before a drag or while it is too small.
    fn selection(&self) -> Option<Rect> {
        let start = to_global(self.start.get()?, self.frame, self.primary_h);
        let end = to_global(self.end.get()?, self.frame, self.primary_h);
        selection_from_drag(start, end, self.display())
    }

    /// Global `r` in view coordinates.
    fn in_view(&self, r: Rect) -> NSRect {
        let f = flip_rect(self.primary_h, r.x, r.y, r.w, r.h);
        NSRect::new(
            NSPoint::new(f.x - self.frame.origin.x, f.y - self.frame.origin.y),
            NSSize::new(f.w, f.h),
        )
    }

    /// Whether this view's session is the one on screen.
    fn is_current(&self) -> bool {
        SESSION.with(|slot| {
            slot.try_borrow()
                .is_ok_and(|s| s.as_ref().is_some_and(|s| s.id == self.session))
        })
    }
}

define_class!(
    // SAFETY:
    // - NSView has no subclassing requirements beyond calling its designated
    //   initializer, which `SelectView::new` does.
    // - `SelectView` does not implement `Drop`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoSelectView"]
    #[ivars = ViewIvars]
    struct SelectView;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SelectView {}

    // None of these methods can panic: `finish` runs `done` under
    // `catch_unwind`, and the session is only borrowed with `try_borrow`.
    impl SelectView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        /// A drag may start on a screen whose panel is not key.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::crosshairCursor());
        }

        /// Cursor rects only work in the key panel; the tracking area set up
        /// in `new` (active while vento is) brings the crosshair to the
        /// others.
        #[unsafe(method(cursorUpdate:))]
        fn cursor_update(&self, _event: &NSEvent) {
            NSCursor::crosshairCursor().set();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let ivars = self.ivars();
            if ivars.start.get().is_some() || !ivars.is_current() {
                return;
            }
            let at = self.point_of(event);
            ivars.start.set(Some(at));
            ivars.end.set(Some(at));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let ivars = self.ivars();
            if ivars.start.get().is_none() || !ivars.is_current() {
                return;
            }
            ivars.end.set(Some(self.point_of(event)));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let ivars = self.ivars();
            if ivars.start.get().is_none() {
                return;
            }
            ivars.end.set(Some(self.point_of(event)));
            let sel = ivars.selection().map(|rect| Selection {
                display_id: ivars.display_id,
                display: ivars.display(),
                rect,
            });
            finish(ivars.session, sel);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            finish(self.ivars().session, None);
        }

        /// Esc cancels; every other key is swallowed, without a beep.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == ESC_KEY_CODE {
                finish(self.ivars().session, None);
            }
        }
    }
);

impl SelectView {
    fn new(mtm: MainThreadMarker, ivars: ViewIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        let owner: &AnyObject = view.as_ref();
        // SAFETY: the owner is the view, which keeps the area (and so
        // outlives it); there is no user info.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                // Not `ActiveAlways`: AppKit does not support it for cursor
                // updates. The app is active while the overlay is up.
                NSTrackingAreaOptions::CursorUpdate
                    | NSTrackingAreaOptions::ActiveInActiveApp
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(owner),
                None,
            )
        };
        view.addTrackingArea(&area);
        view
    }

    /// Where `event` happened, in this view's coordinates.
    fn point_of(&self, event: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }

    /// The dim over the whole view, and the clear, bordered selection.
    fn draw(&self) {
        // `Copy` replaces what was drawn before instead of piling up on it.
        NSColor::blackColor()
            .colorWithAlphaComponent(DIM_ALPHA)
            .setFill();
        NSRectFillUsingOperation(self.bounds(), NSCompositingOperation::Copy);
        let Some(sel) = self.ivars().selection() else {
            return;
        };
        let r = self.ivars().in_view(sel);
        NSColor::clearColor().setFill();
        NSRectFillUsingOperation(r, NSCompositingOperation::Copy);
        // Inset by half the line, so the whole line sits inside the hole.
        let half = BORDER_WIDTH / 2.0;
        let border = NSBezierPath::bezierPathWithRect(NSRect::new(
            NSPoint::new(r.origin.x + half, r.origin.y + half),
            NSSize::new(r.size.width - BORDER_WIDTH, r.size.height - BORDER_WIDTH),
        ));
        border.setLineWidth(BORDER_WIDTH);
        NSColor::whiteColor()
            .colorWithAlphaComponent(BORDER_ALPHA)
            .setStroke();
        border.stroke();
    }
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `SelectObserver` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoSelectObserver"]
    #[ivars = u64]
    struct SelectObserver;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SelectObserver {}

    impl SelectObserver {
        /// The screens changed (the panels no longer fit them) or another
        /// app took focus (Esc no longer reaches the overlay): cancel.
        #[unsafe(method(cancel:))]
        fn cancel(&self, _note: &NSNotification) {
            finish(*self.ivars(), None);
        }
    }
);

impl SelectObserver {
    /// An observer that cancels session `session`, registered for both
    /// notifications. The center holds it weakly; the session keeps it.
    fn install(mtm: MainThreadMarker, session: u64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(session);
        // SAFETY: `init` is NSObject's designated initializer.
        let observer: Retained<Self> = unsafe { msg_send![super(this), init] };
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: `cancel:` is a method of `SelectObserver` that takes one
        // NSNotification and returns nothing.
        unsafe {
            for name in [
                NSApplicationDidChangeScreenParametersNotification,
                NSApplicationDidResignActiveNotification,
            ] {
                center.addObserver_selector_name_object(&observer, sel!(cancel:), Some(name), None);
            }
        }
        observer
    }

    fn remove(&self) {
        // SAFETY: `self` is a registered (or never registered) observer;
        // removing it is always allowed.
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(self) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_points_become_global() {
        // A 1920 × 1080 screen right of a 900 pt tall primary, its top 180 pt
        // above the primary's top.
        let frame = NSRect::new(NSPoint::new(1440.0, 0.0), NSSize::new(1920.0, 1080.0));
        let got = to_global(NSPoint::new(100.0, 1000.0), frame, 900.0);
        assert_eq!(
            got,
            Point {
                x: 1540.0,
                y: -100.0
            }
        );
    }
}
