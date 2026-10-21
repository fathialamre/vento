//! Native context menus, shown over one of vento's windows.

use std::cell::Cell;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::NSObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSView, NSWorkspace};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSSize, NSString};
use raw_window_handle::RawWindowHandle;
use vento_core::geometry::Point;

use crate::MenuEntry;

/// Side of an item's icon, in points (Finder's Open With size).
const ICON_PT: f64 = 16.0;

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `MenuTarget` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoMenuTarget"]
    #[ivars = Cell<Option<isize>>]
    struct MenuTarget;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        /// The action of every leaf item: remembers which one was chosen.
        /// Cannot panic, so nothing unwinds into AppKit.
        #[unsafe(method(chooseItem:))]
        fn choose_item(&self, item: &NSMenuItem) {
            self.ivars().set(Some(item.tag()));
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(None));
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Shows `entries` as a context menu with its top-left corner at `at` in
/// the view behind `handle`, and returns the id of the item chosen. The
/// menu runs its own tracking loop until it closes; it needs neither key
/// status for the window nor an active app.
pub(super) fn context_menu(
    handle: RawWindowHandle,
    at: Point,
    entries: &[MenuEntry],
) -> Option<u32> {
    let RawWindowHandle::AppKit(h) = handle else {
        return None;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("context_menu called off the main thread; ignored");
        return None;
    };
    if entries.is_empty() {
        return None;
    }
    // SAFETY: an AppKit window handle's `ns_view` points to a live NSView
    // for as long as the window exists, and we are on the main thread.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    autoreleasepool(|_| {
        // The items hold their target weakly: it lives until the menu is
        // done.
        let target = MenuTarget::new(mtm);
        let menu = build(mtm, entries, &target);
        let point = view_point(at, view.isFlipped(), view.bounds().size.height);
        let chose = menu.popUpMenuPositioningItem_atLocation_inView(None, point, Some(view));
        let tag = target.ivars().get();
        log::debug!("context menu closed: chose {chose}, tag {tag:?}");
        tag.and_then(|t| u32::try_from(t).ok())
    })
}

/// An NSMenu of `entries` whose leaves send `chooseItem:` to `target`,
/// enabled exactly as the entries say.
fn build(mtm: MainThreadMarker, entries: &[MenuEntry], target: &MenuTarget) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    for entry in entries {
        match entry {
            MenuEntry::Item {
                id,
                title,
                enabled,
                icon,
            } => {
                let item = new_item(mtm, title, true);
                // SAFETY: `target` implements `chooseItem:`, the action set
                // above, and outlives the menu's tracking.
                unsafe { item.setTarget(Some(target)) };
                item.setTag(isize::try_from(*id).unwrap_or(-1));
                item.setEnabled(*enabled);
                if let Some(path) = icon.as_deref().and_then(|p| p.to_str()) {
                    let image =
                        NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
                    image.setSize(NSSize::new(ICON_PT, ICON_PT));
                    item.setImage(Some(&image));
                }
                menu.addItem(&item);
            }
            MenuEntry::Submenu {
                title,
                enabled,
                children,
            } => {
                let item = new_item(mtm, title, false);
                let submenu = build(mtm, children, target);
                submenu.setTitle(&NSString::from_str(title));
                item.setSubmenu(Some(&submenu));
                item.setEnabled(*enabled);
                menu.addItem(&item);
            }
            MenuEntry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
        }
    }
    menu
}

/// A menu item titled `title`, with the `chooseItem:` action if it is a
/// leaf.
fn new_item(mtm: MainThreadMarker, title: &str, leaf: bool) -> Retained<NSMenuItem> {
    let action = leaf.then_some(sel!(chooseItem:));
    // SAFETY: `chooseItem:` takes the item and returns nothing, as an
    // action must.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::new(),
        )
    }
}

/// Where `at` (window-local, top-left origin) is in a view of `height`
/// points, flipped (top-left origin, as winit's view) or not.
fn view_point(at: Point, flipped: bool, height: f64) -> NSPoint {
    let y = if flipped { at.y } else { height - at.y };
    NSPoint::new(at.x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_flip_only_for_unflipped_views() {
        let at = Point { x: 30.0, y: 20.0 };
        let p = view_point(at, true, 145.0);
        assert_eq!((p.x, p.y), (30.0, 20.0));
        let p = view_point(at, false, 145.0);
        assert_eq!((p.x, p.y), (30.0, 125.0));
    }
}
