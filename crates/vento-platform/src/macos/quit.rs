//! Quitting from the app menu (⌘Q, or its Quit item): handed to vento's own
//! quit path, which asks about unsaved PDF edits first, instead of ending
//! vento at once. The system ending vento (log out, restart, shut down)
//! still ends it straight away.
//!
//! Slint gives every window its own app menu, whose Quit sends
//! `terminate:`; AppKit then asks the app delegate
//! `applicationShouldTerminate:`. winit's delegate has no such method, so
//! one is added to its class once the delegate exists.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};

/// The Apple event that asks an app to quit (`'quit'`).
const QUIT_EVENT: u32 = u32::from_be_bytes(*b"quit");
/// The parameter the system adds to it when the session ends (`'why?'`):
/// log out, restart or shut down.
const QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

thread_local! {
    /// What a Quit from the app menu runs instead of terminating.
    static ON_QUIT: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    /// Whether the delegate has been given its method.
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` in place of terminating when the app menu's Quit is used, in
/// place of any earlier `f`. Main thread, after the event loop exists.
pub(super) fn on_quit_request(f: Box<dyn Fn()>) {
    ON_QUIT.with(|slot| *slot.borrow_mut() = Some(Rc::from(f)));
    if !INSTALLED.get() {
        INSTALLED.set(install());
    }
}

/// Adds `applicationShouldTerminate:` to the app delegate's class. False
/// when it cannot: Quit then ends vento at once, as before.
fn install() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("on_quit_request called off the main thread; ignored");
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        log::warn!("no app delegate: Quit in the app menu will not ask about edits");
        return false;
    };
    let object: &AnyObject = delegate.as_ref();
    let class: &AnyClass = object.class();
    // SAFETY: `should_terminate` has the signature the type encoding names
    // (an `NSUInteger` return; self, `_cmd` and one object argument), which
    // is the signature AppKit calls `applicationShouldTerminate:` with.
    // Adding a method to a class that lacks it is allowed at any time.
    let added = unsafe {
        let imp: Imp = std::mem::transmute::<ShouldTerminate, Imp>(should_terminate);
        objc2::ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            sel!(applicationShouldTerminate:),
            imp,
            c"Q@:@".as_ptr(),
        )
    };
    if !added.as_bool() {
        log::warn!("the app delegate already decides about quitting; left as it is");
        return false;
    }
    // AppKit may note what a delegate answers to when it is set.
    app.setDelegate(Some(&delegate));
    log::debug!("Quit in the app menu goes through vento's quit");
    true
}

/// `applicationShouldTerminate:`'s signature.
type ShouldTerminate =
    extern "C-unwind" fn(&AnyObject, Sel, &NSApplication) -> NSApplicationTerminateReply;

/// AppKit asks whether to terminate: not when the user quit from the app
/// menu (vento's own quit runs instead, and may ask about edits); yes when
/// the system ends the session or nothing has been set. Never unwinds into
/// AppKit.
extern "C-unwind" fn should_terminate(
    _this: &AnyObject,
    _cmd: Sel,
    _app: &NSApplication,
) -> NSApplicationTerminateReply {
    if session_ends() {
        return NSApplicationTerminateReply::TerminateNow;
    }
    let Some(quit) = ON_QUIT.with(|slot| slot.borrow().clone()) else {
        return NSApplicationTerminateReply::TerminateNow;
    };
    if catch_unwind(AssertUnwindSafe(|| quit())).is_err() {
        log::error!("quit handler panicked");
    }
    NSApplicationTerminateReply::TerminateCancel
}

/// Whether the terminate under way comes from the system ending the
/// session: the quit Apple event being handled carries a reason.
fn session_ends() -> bool {
    let manager = NSAppleEventManager::sharedAppleEventManager();
    let Some(event) = manager.currentAppleEvent() else {
        return false;
    };
    // Called by message: the typed methods need another crate's feature.
    // SAFETY: `eventID` takes nothing and returns a four-char code (a
    // `UInt32`); `paramDescriptorForKeyword:` takes one and returns a
    // descriptor or nil.
    let (event_id, reason): (u32, Option<Retained<NSAppleEventDescriptor>>) = unsafe {
        (
            msg_send![&*event, eventID],
            msg_send![&*event, paramDescriptorForKeyword: QUIT_REASON],
        )
    };
    is_session_end(event_id, reason.is_some())
}

/// The decision behind [`session_ends`]: a quit event with a reason.
fn is_session_end(event_id: u32, has_reason: bool) -> bool {
    event_id == QUIT_EVENT && has_reason
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logout_quit_ends_vento_and_any_other_quit_asks() {
        assert!(
            is_session_end(QUIT_EVENT, true),
            "log out, restart, shut down"
        );
        assert!(!is_session_end(QUIT_EVENT, false), "a script's quit");
        assert!(!is_session_end(u32::from_be_bytes(*b"odoc"), true));
        assert_eq!(QUIT_EVENT, 0x7175_6974);
        assert_eq!(QUIT_REASON, 0x7768_793f);
    }

    #[test]
    fn no_apple_event_is_not_a_session_end() {
        // Tests run outside any Apple event, like a menu Quit.
        assert!(!session_ends());
    }
}
