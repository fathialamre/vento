//! Sharing the dragged items: through one service's own window (AirDrop,
//! Messages, Mail, Notes) with `NSSharingService`, or through the system's
//! share popover with `NSSharingServicePicker`.
//!
//! Everything here runs on the main thread. One share at a time: what a
//! share needs kept alive (AppKit holds its delegates only weakly) lives in
//! [`ShareState`] from [`share`] or [`picker`] until the delegate hears how
//! it ended, and a call meanwhile is refused.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSSharingService, NSSharingServiceDelegate,
    NSSharingServiceNameComposeEmail, NSSharingServiceNameComposeMessage,
    NSSharingServiceNameSendViaAirDrop, NSSharingServicePicker, NSSharingServicePickerDelegate,
    NSStatusWindowLevel, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSError, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize, NSString,
};
use vento_core::geometry::Point;
use vento_core::share::{ShareOutcome, ShareTarget};

use super::clipboard::file_url;
use super::screens::{flip_rect, primary_height};
use crate::PlatformError;

/// The Notes share extension, which has no AppKit name constant.
const NOTES_SERVICE: &str = "com.apple.Notes.SharingExtension";
/// `NSUserCancelledError`: what a window's Cancel reports, in
/// `NSCocoaErrorDomain`.
const USER_CANCELLED: isize = 3072;
/// The targets [`targets`] looks for, in the order it lists them.
const SERVICES: [ShareTarget; 4] = [
    ShareTarget::AirDrop,
    ShareTarget::Messages,
    ShareTarget::Mail,
    ShareTarget::Notes,
];

/// What [`share`] calls once, with how the share ended.
type Done = Box<dyn Fn(ShareOutcome)>;

/// The share in flight, if any: a service's window or the share popover.
#[derive(Default)]
pub(super) struct ShareState {
    /// The service whose window is on screen, and its delegate. Emptied by
    /// the delegate once the outcome is delivered.
    sheet: Rc<RefCell<Option<SheetParts>>>,
    /// The share popover on screen, emptied by its delegate when it closes.
    picker: Rc<RefCell<Option<PickerParts>>>,
}

impl ShareState {
    /// Whether a service's window or the popover is still open.
    fn busy(&self) -> bool {
        self.sheet.borrow().is_some() || self.picker.borrow().is_some()
    }
}

/// A service at work: AppKit holds its delegate only weakly, and the service
/// is kept as well, so nothing the outcome depends on goes away early.
struct SheetParts {
    service: Retained<NSSharingService>,
    delegate: Retained<ShareDelegate>,
}

/// The share popover: AppKit holds the picker's delegate only weakly, and
/// the popover points at the anchor panel.
struct PickerParts {
    panel: Retained<NSPanel>,
    picker: Retained<NSSharingServicePicker>,
    delegate: Retained<PickerDelegate>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `ShareDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoShareDelegate"]
    #[ivars = RefCell<Option<Done>>]
    struct ShareDelegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ShareDelegate {}

    // SAFETY: the signatures match the protocol's. Neither method can panic:
    // the closure `share` gives runs the caller's under `catch_unwind`. The
    // items AppKit passes are `NSItemProvider`s by then, not the URLs, and
    // are not looked at.
    unsafe impl NSSharingServiceDelegate for ShareDelegate {
        /// The service took the items. AirDrop also says so after its
        /// Cancel.
        #[unsafe(method(sharingService:didShareItems:))]
        fn did_share(&self, _service: &NSSharingService, _items: &NSArray) {
            self.finish(ShareOutcome::Sent);
        }

        /// The person cancelled, or the service could not take the items.
        #[unsafe(method(sharingService:didFailToShareItems:error:))]
        fn did_fail(&self, _service: &NSSharingService, _items: &NSArray, error: &NSError) {
            self.finish(outcome_of(error));
        }
    }
);

impl ShareDelegate {
    /// A delegate that hands the outcome to `on_done`, once.
    fn new(on_done: Done, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Some(on_done)));
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Calls the closure with `outcome`, taking it out first, so a later
    /// callback finds nothing to call.
    fn finish(&self, outcome: ShareOutcome) {
        let on_done = self
            .ivars()
            .try_borrow_mut()
            .ok()
            .and_then(|mut d| d.take());
        if let Some(on_done) = on_done {
            log::debug!("share ended: {outcome:?}");
            on_done(outcome);
        }
    }
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `PickerDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoSharePickerDelegate"]
    #[ivars = Weak<RefCell<Option<PickerParts>>>]
    struct PickerDelegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for PickerDelegate {}

    // SAFETY: the signature matches the protocol's. It cannot panic: the
    // state is only borrowed with `try_borrow_mut`.
    unsafe impl NSSharingServicePickerDelegate for PickerDelegate {
        /// The popover closed: `service` is the one picked, which then runs
        /// on its own, or `None` when the popover was dismissed. Either way
        /// the anchor goes and the next share may start.
        #[unsafe(method(sharingServicePicker:didChooseSharingService:))]
        fn did_choose(&self, _picker: &NSSharingServicePicker, service: Option<&NSSharingService>) {
            log::debug!("share popover closed: chose {}", service.is_some());
            let Some(parts) = take(self.ivars()) else {
                return;
            };
            parts.panel.close();
            // AppKit is still inside the picker's call to this delegate: the
            // run loop's autorelease pool releases them once that is over.
            let _ = Retained::autorelease_ptr(parts.picker);
            let _ = Retained::autorelease_ptr(parts.delegate);
            let _ = Retained::autorelease_ptr(parts.panel);
        }
    }
);

impl PickerDelegate {
    /// A delegate that empties `slot` when the popover closes.
    fn new(slot: Weak<RefCell<Option<PickerParts>>>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(slot);
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Empties `slot`, if it still exists and is not borrowed.
fn take<T>(slot: &Weak<RefCell<Option<T>>>) -> Option<T> {
    let slot = slot.upgrade()?;
    let mut held = slot.try_borrow_mut().ok()?;
    held.take()
}

/// The error a call gets while a share is in flight.
fn in_flight() -> PlatformError {
    PlatformError::Failed("another share is still open".into())
}

/// The built-in service behind `target`, or `None` for More or when this
/// Mac has none. Main thread only (`_mtm`).
fn service(target: ShareTarget, _mtm: MainThreadMarker) -> Option<Retained<NSSharingService>> {
    let name = match target {
        // SAFETY: an AppKit constant, immutable and set before main runs.
        ShareTarget::AirDrop => unsafe { NSSharingServiceNameSendViaAirDrop }.retain(),
        // SAFETY: as for AirDrop.
        ShareTarget::Messages => unsafe { NSSharingServiceNameComposeMessage }.retain(),
        // SAFETY: as for AirDrop.
        ShareTarget::Mail => unsafe { NSSharingServiceNameComposeEmail }.retain(),
        ShareTarget::Notes => NSString::from_str(NOTES_SERVICE),
        ShareTarget::More => return None,
    };
    NSSharingService::sharingServiceNamed(&name)
}

/// `paths` as file URLs, in the array a service or the picker takes.
/// Nothing to share is an error: a service given no items may never report
/// back.
fn items(paths: &[PathBuf]) -> Result<Retained<NSArray>, PlatformError> {
    if paths.is_empty() {
        return Err(PlatformError::Failed("nothing to share".into()));
    }
    let urls = paths
        .iter()
        .map(|p| file_url(p).map(|url| Retained::into_super(Retained::into_super(url))))
        .collect::<Result<Vec<Retained<AnyObject>>, _>>()?;
    Ok(NSArray::from_retained_slice(&urls))
}

/// How a failed share ended: the window's Cancel (`NSUserCancelledError` in
/// `NSCocoaErrorDomain`), or a failure the error's own text explains.
fn outcome_of(error: &NSError) -> ShareOutcome {
    if error.code() == USER_CANCELLED && error.domain().to_string() == "NSCocoaErrorDomain" {
        ShareOutcome::Cancelled
    } else {
        ShareOutcome::Failed(error.localizedDescription().to_string())
    }
}

/// The targets among AirDrop, Messages, Mail and Notes whose service this
/// Mac has, in that order. A service that exists is enough: asked with no
/// items, `canPerformWithItems` is false for every one of them.
pub(super) fn targets(mtm: MainThreadMarker) -> Vec<ShareTarget> {
    SERVICES
        .into_iter()
        .filter(|t| service(*t, mtm).is_some())
        .collect()
}

/// Whether `target`'s service takes all of `paths`: always for More, never
/// when the service is missing or a path is not valid UTF-8.
pub(super) fn can_share(target: ShareTarget, paths: &[PathBuf], mtm: MainThreadMarker) -> bool {
    if target == ShareTarget::More {
        return true;
    }
    let (Some(service), Ok(items)) = (service(target, mtm), items(paths)) else {
        return false;
    };
    // SAFETY: the items are NSURLs, which a service takes.
    unsafe { service.canPerformWithItems(Some(&items)) }
}

/// Hands `paths` to `target`'s service, which shows its own window, and
/// returns. `activate` runs once the share is sure to start, just before
/// the hand-off. `on_done` is called once, later, on the main thread, with
/// how the share ended; `state` is emptied right after, so the next share
/// may start. More is an error (it is [`picker`]), as is a call while
/// another share is in flight.
pub(super) fn share(
    state: &ShareState,
    target: ShareTarget,
    paths: &[PathBuf],
    on_done: Done,
    activate: impl FnOnce(),
    mtm: MainThreadMarker,
) -> Result<(), PlatformError> {
    if target == ShareTarget::More {
        return Err(PlatformError::Failed("More is the picker".into()));
    }
    if state.busy() {
        return Err(in_flight());
    }
    let items = items(paths)?;
    let Some(service) = service(target, mtm) else {
        return Err(PlatformError::Failed(format!(
            "this Mac has no {} service",
            target.key()
        )));
    };
    let slot = Rc::downgrade(&state.sheet);
    let delegate = ShareDelegate::new(
        Box::new(move |outcome| {
            if catch_unwind(AssertUnwindSafe(|| on_done(outcome))).is_err() {
                log::error!("share callback panicked");
            }
            if let Some(sheet) = take(&slot) {
                // AppKit is still inside the service's call to this
                // delegate: the run loop's autorelease pool releases both
                // once that is over.
                let _ = Retained::autorelease_ptr(sheet.delegate);
                let _ = Retained::autorelease_ptr(sheet.service);
            }
        }),
        mtm,
    );
    service.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    *state.sheet.borrow_mut() = Some(SheetParts {
        service: service.clone(),
        delegate,
    });
    activate();
    log::debug!("sharing {} items with {}", paths.len(), target.key());
    // SAFETY: the items are NSURLs, which a service takes.
    unsafe { service.performWithItems(&items) };
    Ok(())
}

/// Shows the system's share popover for `paths`, pointing at `at` (global
/// logical point, top-left origin), and returns. The popover closes itself
/// and takes its anchor with it; a call while it or a share is open is an
/// error. Needs no activation: the anchor never takes focus.
pub(super) fn picker(
    state: &ShareState,
    paths: &[PathBuf],
    at: Point,
    mtm: MainThreadMarker,
) -> Result<(), PlatformError> {
    if state.busy() {
        return Err(in_flight());
    }
    let items = items(paths)?;
    let panel = anchor(at, mtm);
    let Some(view) = panel.contentView() else {
        return Err(PlatformError::Failed("the share anchor has no view".into()));
    };
    // SAFETY: the items are NSURLs, which the picker takes.
    let picker =
        unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
    let delegate = PickerDelegate::new(Rc::downgrade(&state.picker), mtm);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    *state.picker.borrow_mut() = Some(PickerParts {
        panel: panel.clone(),
        picker: picker.clone(),
        delegate,
    });
    panel.orderFrontRegardless();
    log::debug!("share popover for {} items", paths.len());
    picker.showRelativeToRect_ofView_preferredEdge(view.bounds(), &view, NSRectEdge::MinY);
    Ok(())
}

/// A clear, borderless 1×1 panel at `at` (global logical, top-left origin),
/// above other apps' windows on every Space, for the popover to point at.
/// Non-activating, so showing it leaves focus where it is.
fn anchor(at: Point, mtm: MainThreadMarker) -> Retained<NSPanel> {
    // Flipping top-left → bottom-left uses the same axis as the reverse.
    let f = flip_rect(primary_height(mtm), at.x, at.y, 1.0, 1.0);
    let frame = NSRect::new(NSPoint::new(f.x, f.y), NSSize::new(f.w, f.h));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::Borderless,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the panel is owned by the share state, not by AppKit, so it
    // must not release itself when closed.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setLevel(NSStatusWindowLevel);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    // Not left to the initializer, which may move a window off the menu bar.
    panel.setFrame_display(frame, false);
    panel
}

#[cfg(test)]
mod tests {
    use objc2_foundation::{NSDictionary, NSLocalizedDescriptionKey};

    use super::*;

    /// An error in `domain` with `code`, described as `description`.
    fn error(domain: &str, code: isize, description: &str) -> Retained<NSError> {
        // SAFETY: an immutable Foundation constant.
        let key = unsafe { NSLocalizedDescriptionKey };
        let text = NSString::from_str(description);
        let info = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[text.as_ref()]);
        // SAFETY: the user info maps strings to objects, as NSError expects.
        unsafe {
            NSError::errorWithDomain_code_userInfo(&NSString::from_str(domain), code, Some(&info))
        }
    }

    #[test]
    fn service_for_more_is_none() {
        // SAFETY: More has no service, so `service` returns before any
        // AppKit call; no main thread is needed.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        assert!(service(ShareTarget::More, mtm).is_none());
    }

    #[test]
    fn cancel_error_is_cancelled() {
        let cancel = error("NSCocoaErrorDomain", 3072, "The operation was cancelled.");
        assert_eq!(outcome_of(&cancel), ShareOutcome::Cancelled);
        let other = error("NSCocoaErrorDomain", 4, "The file is gone.");
        assert_eq!(
            outcome_of(&other),
            ShareOutcome::Failed("The file is gone.".into())
        );
        let elsewhere = error("SHKErrorDomain", 3072, "Could not send.");
        assert_eq!(
            outcome_of(&elsewhere),
            ShareOutcome::Failed("Could not send.".into())
        );
    }
}
