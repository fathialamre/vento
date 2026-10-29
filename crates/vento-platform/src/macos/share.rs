//! Sharing the dragged items: through one service's own window (AirDrop,
//! Messages, Mail, Notes) with `NSSharingService`, or through the system's
//! share popover with `NSSharingServicePicker`.
//!
//! Everything here runs on the main thread. One share at a time: what a
//! share needs kept alive (AppKit holds its delegates only weakly) lives in
//! [`ShareState`] from [`share`] or [`picker`] until the share is over, and
//! a call meanwhile is refused. A direct share is over when its service
//! reports back; the popover when it is dismissed, or when the service
//! picked from it reports back.

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

/// What [`share`] and [`picker`] call once, with how the share ended.
type Done = Box<dyn Fn(ShareOutcome)>;

/// The share in flight, if any: a service's window or the share popover.
#[derive(Default)]
pub(super) struct ShareState {
    /// The service whose window is on screen, and its delegate. Emptied once
    /// the service reports back.
    sheet: Rc<RefCell<Option<SheetParts>>>,
    /// The share popover, then the service picked from it. Emptied when the
    /// popover is dismissed, or when that service reports back.
    picker: Rc<RefCell<Option<PickerParts>>>,
}

impl ShareState {
    /// Whether a share is still in flight.
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

impl SheetParts {
    /// Lets go of the parts once the call AppKit is making into vento is
    /// over, as it may be the service's call to this very delegate: the run
    /// loop's autorelease pool releases them then.
    fn release(self) {
        let _ = Retained::autorelease_ptr(self.delegate);
        let _ = Retained::autorelease_ptr(self.service);
    }
}

/// The share popover: AppKit holds the picker's delegate only weakly, and
/// the popover points at the anchor panel. Once a service is picked, the
/// panel is closed and the service runs as `chosen`.
struct PickerParts {
    panel: Retained<NSPanel>,
    picker: Retained<NSSharingServicePicker>,
    delegate: Retained<PickerDelegate>,
    /// The service picked from the popover, while it runs, and the delegate
    /// vento gave it.
    chosen: Option<SheetParts>,
}

impl PickerParts {
    /// Lets go of the parts as [`SheetParts::release`] does, taking the
    /// anchor off screen first in case the popover never said it closed.
    fn release(self) {
        self.panel.orderOut(None);
        let _ = Retained::autorelease_ptr(self.picker);
        let _ = Retained::autorelease_ptr(self.delegate);
        let _ = Retained::autorelease_ptr(self.panel);
        if let Some(chosen) = self.chosen {
            chosen.release();
        }
    }
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
    // the closure it holds (from `finishing`) runs the caller's under
    // `catch_unwind`. The items AppKit passes are `NSItemProvider`s by then,
    // not the URLs, and are not looked at.
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

/// What a [`PickerDelegate`] holds.
struct PickerIvars {
    /// [`ShareState::picker`].
    slot: Weak<RefCell<Option<PickerParts>>>,
    /// Given to the service picked from the popover, and called when that
    /// service reports back; taken when it is picked.
    on_done: RefCell<Option<Done>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `PickerDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoSharePickerDelegate"]
    #[ivars = PickerIvars]
    struct PickerDelegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for PickerDelegate {}

    // SAFETY: the signatures match the protocol's. Neither method can panic:
    // the state is only borrowed with `try_borrow` and `try_borrow_mut`.
    unsafe impl NSSharingServicePickerDelegate for PickerDelegate {
        /// The popover closed, and the anchor goes. Dismissed (`service` is
        /// `None`): the share is over. A service picked: the share stays in
        /// flight until that service reports back to the delegate
        /// `delegate_for` gives it.
        #[unsafe(method(sharingServicePicker:didChooseSharingService:))]
        fn did_choose(&self, _picker: &NSSharingServicePicker, service: Option<&NSSharingService>) {
            log::debug!("share popover closed: chose {}", service.is_some());
            if service.is_some() {
                self.close_anchor();
            } else if let Some(parts) = take(&self.ivars().slot) {
                parts.panel.close();
                parts.release();
            }
        }

        /// The service picked, about to run. Its delegate hands focus back
        /// and ends the share when the service reports back.
        #[unsafe(method_id(sharingServicePicker:delegateForSharingService:))]
        fn delegate_for(
            &self,
            _picker: &NSSharingServicePicker,
            service: &NSSharingService,
        ) -> Option<Retained<ProtocolObject<dyn NSSharingServiceDelegate>>> {
            self.chosen(service).map(ProtocolObject::from_retained)
        }
    }
);

impl PickerDelegate {
    /// A delegate that ends the share in `slot` when the popover is
    /// dismissed, or gives `on_done` to the service picked from it.
    fn new(
        slot: Weak<RefCell<Option<PickerParts>>>,
        on_done: Done,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let ivars = PickerIvars {
            slot,
            on_done: RefCell::new(Some(on_done)),
        };
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Closes the anchor panel and keeps the rest of the popover's parts.
    fn close_anchor(&self) {
        let Some(slot) = self.ivars().slot.upgrade() else {
            return;
        };
        let panel = match slot.try_borrow() {
            Ok(parts) => parts.as_ref().map(|p| p.panel.clone()),
            Err(_) => {
                log::warn!("share state in use; the share anchor stays");
                None
            }
        };
        if let Some(panel) = panel {
            panel.close();
        }
    }

    /// A delegate for `service`, picked from the popover, kept in the
    /// popover's parts: it calls `on_done`, then ends the share. `None` when
    /// the share is already over, or a delegate was given before.
    fn chosen(&self, service: &NSSharingService) -> Option<Retained<ShareDelegate>> {
        let ivars = self.ivars();
        let on_done = ivars.on_done.try_borrow_mut().ok()?.take()?;
        let slot = ivars.slot.upgrade()?;
        let Ok(mut held) = slot.try_borrow_mut() else {
            log::warn!("share state in use; the picked service is not followed");
            return None;
        };
        let parts = held.as_mut()?;
        let delegate = ShareDelegate::new(
            finishing(on_done, ivars.slot.clone(), PickerParts::release),
            self.mtm(),
        );
        parts.chosen = Some(SheetParts {
            service: service.retain(),
            delegate: delegate.clone(),
        });
        Some(delegate)
    }
}

/// The closure a [`ShareDelegate`] holds: calls `on_done` with the outcome,
/// never letting a panic unwind into AppKit, then empties `slot` and lets
/// go of what it held with `release`, so the next share may start.
fn finishing<T: 'static>(on_done: Done, slot: Weak<RefCell<Option<T>>>, release: fn(T)) -> Done {
    Box::new(move |outcome| {
        if catch_unwind(AssertUnwindSafe(|| on_done(outcome))).is_err() {
            log::error!("share callback panicked");
        }
        if let Some(parts) = take(&slot) {
            release(parts);
        }
    })
}

/// Empties `slot`, if it still exists and is not borrowed.
fn take<T>(slot: &Weak<RefCell<Option<T>>>) -> Option<T> {
    let slot = slot.upgrade()?;
    let Ok(mut held) = slot.try_borrow_mut() else {
        log::warn!("share state in use; the share stays in flight");
        return None;
    };
    held.take()
}

/// The error a call gets while a share is in flight. Shown in a toast, so it
/// is worded in the interface language.
fn in_flight() -> PlatformError {
    PlatformError::ShareOpen
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

/// Whether `service` takes all of `items`, an array from [`items`].
fn takes(service: &NSSharingService, items: &NSArray) -> bool {
    // SAFETY: the items are NSURLs, which a service takes.
    unsafe { service.canPerformWithItems(Some(items)) }
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

/// Whether `target`'s service takes all of `paths`: always for More; never
/// when `paths` is empty, a path is not valid UTF-8 or the service is
/// missing.
pub(super) fn can_share(target: ShareTarget, paths: &[PathBuf], mtm: MainThreadMarker) -> bool {
    if target == ShareTarget::More {
        return true;
    }
    let Ok(items) = items(paths) else {
        return false;
    };
    service(target, mtm).is_some_and(|service| takes(&service, &items))
}

/// Hands `paths` to `target`'s service, which shows its own window, and
/// returns. `activate` runs once the share is sure to start, just before
/// the hand-off. `on_done` is called at most once, on the main thread, when
/// the service reports back how the share ended, which it may never do;
/// `state` is emptied right after, so the next share may start. More is an
/// error (it is [`picker`]), as are a service that cannot take `paths` (see
/// [`can_share`]) and a call while another share is in flight.
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
    // Handed items it cannot take, a service may never report back, which
    // would keep the share in flight and vento in front.
    if !takes(&service, &items) {
        return Err(PlatformError::Failed(format!(
            "the {} service cannot take these items",
            target.key()
        )));
    }
    let delegate = ShareDelegate::new(
        finishing(on_done, Rc::downgrade(&state.sheet), SheetParts::release),
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
/// and takes its anchor with it. A service picked from it runs on its own;
/// `on_done` is called at most once, on the main thread, when that service
/// reports back, which it may never do, and never when the popover is
/// dismissed. Until then, or until the popover is dismissed, the share is
/// in flight, and a call meanwhile is an error. Needs no activation: the
/// anchor never takes focus.
pub(super) fn picker(
    state: &ShareState,
    paths: &[PathBuf],
    at: Point,
    on_done: Done,
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
    let delegate = PickerDelegate::new(Rc::downgrade(&state.picker), on_done, mtm);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    *state.picker.borrow_mut() = Some(PickerParts {
        panel: panel.clone(),
        picker: picker.clone(),
        delegate,
        chosen: None,
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

    #[test]
    fn nothing_to_share_is_refused() {
        // SAFETY: empty paths are refused before any AppKit call; no main
        // thread is needed.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let state = ShareState::default();
        assert!(!can_share(ShareTarget::Mail, &[], mtm));
        let shared = share(
            &state,
            ShareTarget::Mail,
            &[],
            Box::new(|_| panic!("nothing was shared")),
            || panic!("vento activated for nothing"),
            mtm,
        );
        assert!(matches!(shared, Err(PlatformError::Failed(_))));
        let shown = picker(
            &state,
            &[],
            Point { x: 10.0, y: 10.0 },
            Box::new(|_| panic!("nothing was shared")),
            mtm,
        );
        assert!(matches!(shown, Err(PlatformError::Failed(_))));
        assert!(!state.busy());
    }
}
