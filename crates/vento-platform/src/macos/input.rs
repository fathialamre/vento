//! Global input sampling via polling; none of it needs a permission prompt.

use std::cell::{Cell, OnceCell};
use std::path::PathBuf;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSEvent, NSEventModifierFlags, NSPasteboard, NSPasteboardNameDrag, NSPasteboardTypeFileURL,
};
use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};
use vento_core::geometry::Point;
use vento_core::trigger::{InputSample, Modifiers};

use super::catcher::DropSource;
use super::screens::primary_height;

/// `kVK_Escape`.
const KEY_ESCAPE: u16 = 53;

/// Remembers whether the drag pasteboard carried file URLs, so its types are
/// only re-read when its change count moves, and holds on to the drag and
/// general pasteboards, so they are not looked up on every sample.
#[derive(Default)]
pub(super) struct DragPasteboardCache {
    change_count: Cell<i64>,
    has_files: Cell<bool>,
    primed: Cell<bool>,
    drag: OnceCell<Retained<NSPasteboard>>,
    general: OnceCell<Retained<NSPasteboard>>,
}

impl DragPasteboardCache {
    /// The system drag pasteboard, looked up once.
    fn drag(&self) -> &NSPasteboard {
        self.drag
            .get_or_init(|| NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag }))
    }

    /// The general (copy and paste) pasteboard, looked up once.
    fn general(&self) -> &NSPasteboard {
        self.general.get_or_init(NSPasteboard::generalPasteboard)
    }

    /// Returns `has_files`, calling `read` only when `count` differs from the
    /// last one seen.
    fn has_files(&self, count: i64, read: impl FnOnce() -> bool) -> bool {
        if !self.primed.get() || self.change_count.get() != count {
            self.change_count.set(count);
            self.has_files.set(read());
            self.primed.set(true);
        }
        self.has_files.get()
    }
}

pub(super) fn sample(cache: &DragPasteboardCache) -> InputSample {
    let flags = NSEvent::modifierFlags_class();
    let modifiers = Modifiers {
        ctrl: flags.contains(NSEventModifierFlags::Control),
        alt: flags.contains(NSEventModifierFlags::Option),
        shift: flags.contains(NSEventModifierFlags::Shift),
        meta: flags.contains(NSEventModifierFlags::Command),
    };

    let pasteboard = cache.drag();
    let count = pasteboard.changeCount() as i64;
    let drag_has_files = cache.has_files(count, || pasteboard_has_file_url(pasteboard));

    let location = NSEvent::mouseLocation();
    let cursor = match MainThreadMarker::new() {
        Some(mtm) => Point {
            x: location.x,
            y: primary_height(mtm) - location.y,
        },
        None => {
            log::warn!("sample_input called off the main thread; cursor unknown");
            Point { x: 0.0, y: 0.0 }
        }
    };

    InputSample {
        left_down: NSEvent::pressedMouseButtons() & 1 == 1,
        modifiers,
        escape_down: CGEventSource::key_state(
            CGEventSourceStateID::CombinedSessionState,
            KEY_ESCAPE,
        ),
        drag_change_count: count,
        drag_has_files,
        clip_change_count: cache.general().changeCount() as i64,
        cursor,
    }
}

/// The file paths on the system drag pasteboard.
pub(super) fn drag_paths() -> Vec<PathBuf> {
    let pasteboard = NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag });
    pasteboard.file_paths()
}

fn pasteboard_has_file_url(pasteboard: &NSPasteboard) -> bool {
    let wanted = unsafe { NSPasteboardTypeFileURL };
    pasteboard
        .types()
        .map(|types| types.iter().any(|t| &*t == wanted))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_are_reread_only_when_count_changes() {
        let cache = DragPasteboardCache::default();
        let reads = Cell::new(0);
        let read = |answer: bool| {
            let reads = &reads;
            move || {
                reads.set(reads.get() + 1);
                answer
            }
        };
        assert!(cache.has_files(5, read(true)));
        assert!(
            cache.has_files(5, read(false)),
            "cached value must be reused"
        );
        assert_eq!(reads.get(), 1);
        assert!(!cache.has_files(6, read(false)));
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn count_zero_is_still_read_once() {
        let cache = DragPasteboardCache::default();
        let reads = Cell::new(0);
        cache.has_files(0, || {
            reads.set(reads.get() + 1);
            false
        });
        assert_eq!(reads.get(), 1);
    }
}
