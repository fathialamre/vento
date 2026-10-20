//! App notifications vento listens for: screen changes, so the Pocket island
//! can move with its screen, and termination, so pending work is finished.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{AllocAnyThread, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplicationDidChangeScreenParametersNotification, NSApplicationWillTerminateNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};

/// Set by the screen-parameters notification, cleared by [`screens_changed`].
static SCREENS_CHANGED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Runs when the app is about to terminate (main thread).
    static ON_TERMINATE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `AppObserver` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "VentoAppObserver"]
    pub(super) struct AppObserver;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for AppObserver {}

    impl AppObserver {
        #[unsafe(method(screensChanged:))]
        fn screens_did_change(&self, _note: &NSNotification) {
            SCREENS_CHANGED.store(true, Ordering::Relaxed);
        }

        #[unsafe(method(willTerminate:))]
        fn will_terminate(&self, _note: &NSNotification) {
            ON_TERMINATE.with(|slot| {
                let Ok(slot) = slot.try_borrow() else {
                    return;
                };
                if let Some(f) = slot.as_ref()
                    && catch_unwind(AssertUnwindSafe(f)).is_err()
                {
                    log::error!("terminate handler panicked");
                }
            });
        }
    }
);

impl AppObserver {
    /// Creates the observer and registers it for both notifications. The
    /// notification center holds it weakly: keep it alive.
    pub(super) fn install() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer.
        let observer: Retained<Self> = unsafe { msg_send![super(this), init] };
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: both selectors are methods of `AppObserver` that take one
        // NSNotification and return nothing.
        unsafe {
            center.addObserver_selector_name_object(
                &observer,
                sel!(screensChanged:),
                Some(NSApplicationDidChangeScreenParametersNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &observer,
                sel!(willTerminate:),
                Some(NSApplicationWillTerminateNotification),
                None,
            );
        }
        observer
    }
}

/// Whether the screens changed since the last call.
pub(super) fn screens_changed() -> bool {
    SCREENS_CHANGED.swap(false, Ordering::Relaxed)
}

/// Runs `f` when the app is about to terminate, in place of any earlier one.
pub(super) fn on_terminate(f: Box<dyn Fn()>) {
    ON_TERMINATE.with(|slot| *slot.borrow_mut() = Some(f));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn notifications_set_the_flag_and_run_the_handler() {
        let _observer = AppObserver::install();
        let center = NSNotificationCenter::defaultCenter();
        assert!(!screens_changed());
        // SAFETY: plain notification posts, delivered on this thread.
        unsafe {
            center.postNotificationName_object(
                NSApplicationDidChangeScreenParametersNotification,
                None,
            );
        }
        assert!(screens_changed());
        // Read once, then clear.
        assert!(!screens_changed());

        let ran = Rc::new(Cell::new(0));
        let seen = ran.clone();
        on_terminate(Box::new(move || seen.set(seen.get() + 1)));
        unsafe {
            center.postNotificationName_object(NSApplicationWillTerminateNotification, None);
        }
        assert_eq!(ran.get(), 1);
    }
}
