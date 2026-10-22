//! The native drop catcher: a clear, non-activating panel just above the
//! Pocket island that accepts any drag type and reports it through a
//! [`CatcherEvent`] callback. Shown only while a drag is in progress.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSPanel,
    NSPasteboard, NSPasteboardType, NSPasteboardTypeFileURL, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardTypeTIFF, NSPasteboardTypeURL,
    NSPasteboardURLReadingFileURLsOnlyKey, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSURL,
};
use vento_core::geometry::Rect;

use super::screens::{flip_rect, primary_height};
use crate::{CatcherEvent, DropCatcher, Dropped};

/// One above the island's `NSStatusWindowLevel` (25).
const CATCHER_LEVEL: isize = 26;

/// What a drop's pasteboard offers. Each read is lazy, so lower-priority
/// (and possibly large) representations are only fetched when needed.
pub(super) trait DropSource {
    /// Local file URLs as paths; empty if there are none.
    fn file_paths(&self) -> Vec<PathBuf>;
    /// The `public.url` string.
    fn url(&self) -> Option<String>;
    /// `public.png` bytes.
    fn png(&self) -> Option<Vec<u8>>;
    /// `public.tiff` bytes.
    fn tiff(&self) -> Option<Vec<u8>>;
    /// The `public.utf8-plain-text` string.
    fn text(&self) -> Option<String>;
}

/// Pick what was dropped, in priority order (ruling P5): files, then an
/// image (PNG before TIFF), then a (non-file) URL, then text. Images beat
/// URLs because browsers put the image's address beside its data. Empty
/// values are skipped.
pub(super) fn read_dropped(src: &impl DropSource) -> Option<Dropped> {
    let files = src.file_paths();
    if !files.is_empty() {
        return Some(Dropped::Files(files));
    }
    let image = |bytes: Option<Vec<u8>>, ext| {
        bytes
            .filter(|b| !b.is_empty())
            .map(|bytes| Dropped::Image { bytes, ext })
    };
    image(src.png(), "png")
        .or_else(|| image(src.tiff(), "tiff"))
        .or_else(|| {
            src.url()
                .filter(|u| !u.is_empty() && !u.starts_with("file:"))
                .map(Dropped::Url)
        })
        .or_else(|| src.text().filter(|t| !t.is_empty()).map(Dropped::Text))
}

impl DropSource for NSPasteboard {
    fn file_paths(&self) -> Vec<PathBuf> {
        let classes = NSArray::from_slice(&[NSURL::class()]);
        let yes = NSNumber::new_bool(true);
        let key = unsafe { NSPasteboardURLReadingFileURLsOnlyKey };
        let options = NSDictionary::<_, AnyObject>::from_slices(&[key], &[yes.as_ref()]);
        // SAFETY: the class array holds only NSURL, and the option key takes
        // an NSNumber boolean.
        let Some(objects) =
            (unsafe { self.readObjectsForClasses_options(&classes, Some(&options)) })
        else {
            return Vec::new();
        };
        objects
            .iter()
            .filter_map(|o| o.downcast::<NSURL>().ok())
            .filter_map(|u| u.path())
            .map(|p| PathBuf::from(p.to_string()))
            .collect()
    }

    fn url(&self) -> Option<String> {
        string_for(self, unsafe { NSPasteboardTypeURL })
    }

    fn png(&self) -> Option<Vec<u8>> {
        self.dataForType(unsafe { NSPasteboardTypePNG })
            .map(|d| d.to_vec())
    }

    fn tiff(&self) -> Option<Vec<u8>> {
        self.dataForType(unsafe { NSPasteboardTypeTIFF })
            .map(|d| d.to_vec())
    }

    fn text(&self) -> Option<String> {
        string_for(self, unsafe { NSPasteboardTypeString })
    }
}

fn string_for(pasteboard: &NSPasteboard, ty: &NSPasteboardType) -> Option<String> {
    pasteboard.stringForType(ty).map(|s| s.to_string())
}

/// The event handler installed through [`DropCatcher::on_event`].
type Handler = Box<dyn Fn(CatcherEvent)>;

/// State behind the catcher view: the event handler and whether the current
/// drag has already been reported as entered.
#[derive(Default)]
pub(super) struct CatcherIvars {
    callback: RefCell<Option<Handler>>,
    entered: Cell<bool>,
}

impl CatcherIvars {
    fn set_callback(&self, f: Handler) {
        match self.callback.try_borrow_mut() {
            Ok(mut slot) => *slot = Some(f),
            Err(_) => log::warn!("drop catcher handler replaced from inside itself; ignored"),
        }
    }

    /// `draggingEntered:`: AppKit sends it once per entry, so always report
    /// `Entered`, whatever a stale flag says.
    fn enter(&self) {
        self.entered.set(true);
        self.emit(CatcherEvent::Entered);
    }

    /// `draggingUpdated:`: report `Entered` only if this entry wasn't seen.
    fn update(&self) {
        if !self.entered.replace(true) {
            self.emit(CatcherEvent::Entered);
        }
    }

    /// Forget the current entry without reporting anything (hidden, or the
    /// drag session ended).
    fn reset(&self) {
        self.entered.set(false);
    }

    fn exit(&self) {
        self.entered.set(false);
        self.emit(CatcherEvent::Exited);
    }

    fn drop_with(&self, dropped: Dropped) {
        self.entered.set(false);
        self.emit(CatcherEvent::Dropped(dropped));
    }

    /// Call the handler, never letting a panic unwind into AppKit.
    fn emit(&self, event: CatcherEvent) {
        let Ok(slot) = self.callback.try_borrow() else {
            log::warn!("drop catcher event raised during handler replacement; dropped");
            return;
        };
        if let Some(f) = slot.as_ref()
            && catch_unwind(AssertUnwindSafe(|| f(event))).is_err()
        {
            log::error!("drop catcher handler panicked");
        }
    }
}

define_class!(
    // SAFETY:
    // - NSView has no subclassing requirements beyond calling its designated
    //   initializer, which `CatcherView::new` does.
    // - `CatcherView` does not implement `Drop`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "VentoDropCatcherView"]
    #[ivars = CatcherIvars]
    struct CatcherView;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for CatcherView {}

    // SAFETY: the method signatures match NSDraggingDestination's, and none
    // of them can panic (the handler runs under `catch_unwind`).
    unsafe impl NSDraggingDestination for CatcherView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(
            &self,
            _sender: &ProtocolObject<dyn NSDraggingInfo>,
        ) -> NSDragOperation {
            self.ivars().enter();
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(
            &self,
            _sender: &ProtocolObject<dyn NSDraggingInfo>,
        ) -> NSDragOperation {
            self.ivars().update();
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingEnded:))]
        fn dragging_ended(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) {
            self.ivars().reset();
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            self.ivars().exit();
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            match read_dropped(&*sender.draggingPasteboard()) {
                Some(dropped) => {
                    self.ivars().drop_with(dropped);
                    true
                }
                None => {
                    log::warn!("drop carried nothing the Pocket can hold");
                    self.ivars().exit();
                    false
                }
            }
        }
    }
);

impl CatcherView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CatcherIvars::default());
        // SAFETY: `initWithFrame:` is NSView's designated initializer.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        let types = unsafe {
            [
                NSPasteboardTypeFileURL,
                NSPasteboardTypeURL,
                NSPasteboardTypeString,
                NSPasteboardTypePNG,
                NSPasteboardTypeTIFF,
            ]
        };
        view.registerForDraggedTypes(&NSArray::from_slice(&types));
        view
    }
}

/// AppKit [`DropCatcher`]: a borderless, transparent, non-activating panel
/// one level above the island, never key.
pub(super) struct MacDropCatcher {
    panel: Retained<NSPanel>,
    view: Retained<CatcherView>,
    mtm: MainThreadMarker,
}

impl MacDropCatcher {
    pub(super) fn new(mtm: MainThreadMarker) -> Self {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::ZERO,
            NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: the panel is owned by `MacDropCatcher`, not by AppKit, so it
        // must not release itself when closed.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(CATCHER_LEVEL);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setIgnoresMouseEvents(false);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        let view = CatcherView::new(mtm);
        panel.setContentView(Some(&view));
        Self { panel, view, mtm }
    }
}

impl DropCatcher for MacDropCatcher {
    fn show(&self, rect: Rect) {
        // Flipping top-left → bottom-left uses the same axis as the reverse.
        let f = flip_rect(primary_height(self.mtm), rect.x, rect.y, rect.w, rect.h);
        let frame = NSRect::new(NSPoint::new(f.x, f.y), NSSize::new(f.w, f.h));
        self.panel.setFrame_display(frame, false);
        self.panel.orderFrontRegardless();
    }

    fn hide(&self) {
        // An ordered-out window may never get `draggingExited:`.
        self.view.ivars().reset();
        self.panel.orderOut(None);
    }

    fn on_event(&self, f: Box<dyn Fn(CatcherEvent)>) {
        self.view.ivars().set_callback(f);
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use objc2_foundation::{NSData, NSString};

    use super::super::test_support::TempPasteboard;
    use super::*;

    #[derive(Default)]
    struct Fake {
        files: Vec<PathBuf>,
        url: Option<String>,
        png: Option<Vec<u8>>,
        tiff: Option<Vec<u8>>,
        text: Option<String>,
    }

    impl DropSource for Fake {
        fn file_paths(&self) -> Vec<PathBuf> {
            self.files.clone()
        }
        fn url(&self) -> Option<String> {
            self.url.clone()
        }
        fn png(&self) -> Option<Vec<u8>> {
            self.png.clone()
        }
        fn tiff(&self) -> Option<Vec<u8>> {
            self.tiff.clone()
        }
        fn text(&self) -> Option<String> {
            self.text.clone()
        }
    }

    fn everything() -> Fake {
        Fake {
            files: vec![PathBuf::from("/tmp/a.txt")],
            url: Some("https://example.com/".into()),
            png: Some(vec![1]),
            tiff: Some(vec![2]),
            text: Some("hello".into()),
        }
    }

    #[test]
    fn files_win_over_everything() {
        assert_eq!(
            read_dropped(&everything()),
            Some(Dropped::Files(vec![PathBuf::from("/tmp/a.txt")]))
        );
    }

    #[test]
    fn image_beats_url_and_text() {
        let src = Fake {
            files: Vec::new(),
            ..everything()
        };
        assert_eq!(
            read_dropped(&src),
            Some(Dropped::Image {
                bytes: vec![1],
                ext: "png"
            })
        );
    }

    #[test]
    fn tiff_image_beats_url() {
        let src = Fake {
            url: Some("https://example.com/cat.jpg".into()),
            tiff: Some(vec![2]),
            ..Fake::default()
        };
        assert_eq!(
            read_dropped(&src),
            Some(Dropped::Image {
                bytes: vec![2],
                ext: "tiff"
            })
        );
    }

    #[test]
    fn url_beats_text() {
        let src = Fake {
            url: Some("https://example.com/".into()),
            text: Some("hello".into()),
            ..Fake::default()
        };
        assert_eq!(
            read_dropped(&src),
            Some(Dropped::Url("https://example.com/".into()))
        );
    }

    #[test]
    fn file_scheme_url_is_not_a_link() {
        let src = Fake {
            url: Some("file:///tmp/a.txt".into()),
            text: Some("hello".into()),
            ..Fake::default()
        };
        assert_eq!(read_dropped(&src), Some(Dropped::Text("hello".into())));
    }

    #[test]
    fn png_beats_tiff_and_text() {
        let src = Fake {
            files: Vec::new(),
            url: None,
            ..everything()
        };
        assert_eq!(
            read_dropped(&src),
            Some(Dropped::Image {
                bytes: vec![1],
                ext: "png"
            })
        );
    }

    #[test]
    fn tiff_when_there_is_no_png() {
        let src = Fake {
            tiff: Some(vec![2]),
            text: Some("hello".into()),
            ..Fake::default()
        };
        assert_eq!(
            read_dropped(&src),
            Some(Dropped::Image {
                bytes: vec![2],
                ext: "tiff"
            })
        );
    }

    #[test]
    fn text_comes_last_and_empty_values_are_skipped() {
        let src = Fake {
            url: Some(String::new()),
            png: Some(Vec::new()),
            text: Some("hello".into()),
            ..Fake::default()
        };
        assert_eq!(read_dropped(&src), Some(Dropped::Text("hello".into())));
        let empty = Fake {
            text: Some(String::new()),
            ..Fake::default()
        };
        assert_eq!(read_dropped(&empty), None);
    }

    #[test]
    fn real_pasteboard_file_urls_become_paths() {
        let pb = TempPasteboard::new();
        let url = NSURL::fileURLWithPath(&NSString::from_str("/tmp/vento drop.txt"));
        let objects = [ProtocolObject::from_retained(url)];
        assert!(pb.0.writeObjects(&NSArray::from_retained_slice(&objects)));
        assert_eq!(
            read_dropped(&*pb.0),
            Some(Dropped::Files(vec![PathBuf::from("/tmp/vento drop.txt")]))
        );
    }

    #[test]
    fn real_pasteboard_link_image_and_text() {
        let pb = TempPasteboard::new();
        pb.0.setString_forType(&NSString::from_str("hello"), unsafe {
            NSPasteboardTypeString
        });
        assert_eq!(read_dropped(&*pb.0), Some(Dropped::Text("hello".into())));

        pb.0.setData_forType(Some(&NSData::with_bytes(&[9, 9])), unsafe {
            NSPasteboardTypeTIFF
        });
        assert_eq!(
            read_dropped(&*pb.0),
            Some(Dropped::Image {
                bytes: vec![9, 9],
                ext: "tiff"
            })
        );

        // A web image carries its address too; the image still wins.
        pb.0.setString_forType(&NSString::from_str("https://example.com/"), unsafe {
            NSPasteboardTypeURL
        });
        assert_eq!(
            read_dropped(&*pb.0),
            Some(Dropped::Image {
                bytes: vec![9, 9],
                ext: "tiff"
            })
        );
    }

    #[test]
    fn real_pasteboard_link_beats_text() {
        let pb = TempPasteboard::new();
        pb.0.setString_forType(&NSString::from_str("Example"), unsafe {
            NSPasteboardTypeString
        });
        pb.0.setString_forType(&NSString::from_str("https://example.com/"), unsafe {
            NSPasteboardTypeURL
        });
        assert_eq!(
            read_dropped(&*pb.0),
            Some(Dropped::Url("https://example.com/".into()))
        );
    }

    fn recorder() -> (Rc<RefCell<Vec<CatcherEvent>>>, Handler) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        (seen, Box::new(move |e| sink.borrow_mut().push(e)))
    }

    #[test]
    fn entered_is_sent_once_per_entry() {
        let ivars = CatcherIvars::default();
        let (seen, f) = recorder();
        ivars.set_callback(f);
        ivars.enter();
        ivars.update();
        ivars.exit();
        // An update without a preceding enter still reports the entry, once.
        ivars.update();
        ivars.update();
        ivars.drop_with(Dropped::Text("x".into()));
        ivars.enter();
        assert_eq!(
            *seen.borrow(),
            vec![
                CatcherEvent::Entered,
                CatcherEvent::Exited,
                CatcherEvent::Entered,
                CatcherEvent::Dropped(Dropped::Text("x".into())),
                CatcherEvent::Entered,
            ]
        );
    }

    #[test]
    fn a_new_entry_after_a_reset_reports_entered_again() {
        let ivars = CatcherIvars::default();
        let (seen, f) = recorder();
        ivars.set_callback(f);
        ivars.enter();
        // Hidden mid-drag: AppKit may never send `draggingExited:`.
        ivars.reset();
        ivars.update();
        ivars.reset();
        ivars.enter();
        assert_eq!(
            *seen.borrow(),
            vec![
                CatcherEvent::Entered,
                CatcherEvent::Entered,
                CatcherEvent::Entered
            ]
        );
    }

    #[test]
    fn enter_always_reports_even_if_the_flag_is_stale() {
        let ivars = CatcherIvars::default();
        let (seen, f) = recorder();
        ivars.set_callback(f);
        ivars.enter();
        ivars.enter();
        assert_eq!(
            *seen.borrow(),
            vec![CatcherEvent::Entered, CatcherEvent::Entered]
        );
    }

    #[test]
    fn replacing_the_handler_from_inside_it_does_not_panic() {
        let ivars = Rc::new(CatcherIvars::default());
        let inner = Rc::downgrade(&ivars);
        ivars.set_callback(Box::new(move |_| {
            if let Some(i) = inner.upgrade() {
                i.set_callback(Box::new(|_| {}));
            }
        }));
        ivars.enter();
    }

    #[test]
    fn events_without_a_handler_are_ignored() {
        CatcherIvars::default().exit();
    }
}
