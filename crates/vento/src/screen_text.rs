//! Copy from screen: the shortcut or the menu item dims the screen, the user
//! drags a rectangle, and the text inside it goes to the clipboard.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::thread;
use std::time::Instant;

use log::{info, warn};
use slint::{CloseRequestResponse, ComponentHandle, LogicalPosition};
use vento_core::geometry::{Point, Rect, screen_for};
use vento_core::screen_text::{Selection, TextBox, assemble};
use vento_platform::{Platform, PlatformError, ScreenText};

use crate::ui::ScreenAccess;

/// What a read hands back: the text boxes found, or why there are none.
pub type ReadResult = Result<Vec<TextBox>, PlatformError>;

/// Called once with the outcome of a read, on the main thread.
pub type ReadDone = Box<dyn FnOnce(ReadResult)>;

/// Puts text on the clipboard.
pub type CopyText = Box<dyn Fn(&str) -> Result<(), PlatformError>>;

/// Shows a toast below the given point.
pub type Notify = Box<dyn Fn(&str, Point)>;

/// The toast after a copy.
const COPIED: &str = "Copied text";
/// The toast when the area holds no text; the clipboard keeps what it had.
const NO_TEXT: &str = "No text found";
/// The toast when the capture, the reading or the copy failed.
const FAILED: &str = "Couldn't read the screen";

/// What the controller needs from the rest of vento.
pub struct ScreenTextDeps {
    pub os: Rc<dyn ScreenText>,
    pub is_paused: Box<dyn Fn() -> bool>,
    /// Runs `read_screen_text` on a worker thread; calls back on the main thread.
    pub read: Box<dyn Fn(Selection, ReadDone)>,
    pub copy: CopyText,
    pub notify: Notify,
    /// Asks for Screen Recording (the permission window).
    pub ask_access: Box<dyn Fn()>,
    /// Hides the permission window, if it is showing.
    pub hide_access: Box<dyn Fn()>,
}

/// Runs one Copy from screen at a time: overlay, read, clipboard, toast.
pub struct ScreenTextController {
    deps: ScreenTextDeps,
    /// From the overlay showing until the toast: a press meanwhile does
    /// nothing.
    busy: Cell<bool>,
}

impl ScreenTextController {
    pub fn new(deps: ScreenTextDeps) -> Rc<Self> {
        Rc::new(Self {
            deps,
            busy: Cell::new(false),
        })
    }

    /// Starts Copy from screen, unless vento is paused or one is already
    /// running. Without Screen Recording it asks for it and selects nothing;
    /// with it, a permission window still up from before goes away.
    pub fn start(self: &Rc<Self>) {
        if (self.deps.is_paused)() || self.busy.get() {
            return;
        }
        if !self.deps.os.has_access() {
            (self.deps.ask_access)();
            return;
        }
        (self.deps.hide_access)();
        self.busy.set(true);
        let this = Rc::downgrade(self);
        self.deps.os.select_area(Box::new(move |sel| {
            if let Some(controller) = this.upgrade() {
                controller.selected(sel);
            }
        }));
    }

    /// The overlay closed with `sel`, or `None` when it was cancelled.
    fn selected(self: &Rc<Self>, sel: Option<Selection>) {
        let Some(sel) = sel else {
            self.busy.set(false);
            return;
        };
        let this = Rc::downgrade(self);
        (self.deps.read)(
            sel,
            Box::new(move |result| {
                if let Some(controller) = this.upgrade() {
                    controller.finish(sel.rect, result);
                }
            }),
        );
    }

    /// Copies what was read in `area` and says how it went, below the area.
    fn finish(&self, area: Rect, result: ReadResult) {
        let message = match result {
            Err(e) => {
                warn!("cannot read the screen: {e}");
                FAILED
            }
            Ok(boxes) => {
                let text = assemble(&boxes);
                if text.is_empty() {
                    NO_TEXT
                } else if let Err(e) = (self.deps.copy)(&text) {
                    warn!("cannot copy the text read from the screen: {e}");
                    FAILED
                } else {
                    COPIED
                }
            }
        };
        (self.deps.notify)(message, bottom_centre(area));
        self.busy.set(false);
    }
}

/// The middle of the bottom edge of `r`, where the toast hangs.
fn bottom_centre(r: Rect) -> Point {
    Point {
        x: r.x + r.w / 2.0,
        y: r.y + r.h,
    }
}

thread_local! {
    /// The callbacks of the reads in flight, by read id; they live on the
    /// main thread, where the non-`Send` controller is.
    static PENDING: RefCell<HashMap<u64, ReadDone>> = RefCell::new(HashMap::new());
    /// The id of the next read.
    static NEXT_READ: Cell<u64> = const { Cell::new(0) };
}

/// The recognition languages are logged once, after the first read.
static LANGUAGES: Once = Once::new();

/// Reads the text in `sel` on a worker thread and calls `done` with it on
/// the main thread. Only the (`Send`) answer crosses back; `done` waits on
/// the main thread under the read's id. Call on the main thread.
pub fn read_on_worker(sel: Selection, done: ReadDone) {
    let id = NEXT_READ.with(|next| next.replace(next.get() + 1));
    PENDING.with(|pending| pending.borrow_mut().insert(id, done));
    let spawned = thread::Builder::new()
        .name("vento-screen-text".into())
        .spawn(move || {
            let started = Instant::now();
            // A panic must still answer, or `done` would never run and Copy
            // from screen would stay busy until vento restarts.
            let result =
                panic::catch_unwind(AssertUnwindSafe(|| vento_platform::read_screen_text(&sel)))
                    .unwrap_or_else(|_| Err(PlatformError::Failed("internal error".into())));
            info!("screen read in {} ms", started.elapsed().as_millis());
            let delivered = slint::invoke_from_event_loop(move || {
                if let Some(done) = PENDING.with(|pending| pending.borrow_mut().remove(&id)) {
                    done(result);
                }
            });
            if let Err(e) = delivered {
                warn!("cannot hand the screen text back: {e}");
            }
            // After the answer is on its way, so it costs the user no time;
            // and only when it would be logged, as it takes a Vision request.
            if log::log_enabled!(log::Level::Info) {
                LANGUAGES.call_once(|| {
                    let languages = vento_platform::screen_text_languages();
                    info!("Vision languages: {}", languages.join(", "));
                });
            }
        });
    if let Err(e) = spawned
        && let Some(done) = PENDING.with(|pending| pending.borrow_mut().remove(&id))
    {
        done(Err(PlatformError::Failed(format!(
            "cannot start the reader: {e}"
        ))));
    }
}

/// Fallback height of the permission window, should its layout report none.
const ACCESS_HEIGHT: f64 = 200.0;
/// Width of the permission window in logical pixels (matches
/// `screen_access.slint`).
const ACCESS_WIDTH: f64 = 380.0;

/// The permission window: says why Copy from screen needs Screen Recording
/// and opens System Settings for it.
pub struct ScreenAccessView {
    this: Weak<ScreenAccessView>,
    platform: Rc<dyn Platform>,
    os: Rc<dyn ScreenText>,
    window: ScreenAccess,
}

impl ScreenAccessView {
    /// Creates the (hidden) window.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    pub fn new(platform: Rc<dyn Platform>, os: Rc<dyn ScreenText>) -> Rc<Self> {
        let window = ScreenAccess::new().expect("cannot create the Screen Recording window");
        let view = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            platform,
            os,
            window,
        });
        let this = view.this.clone();
        view.window.on_open_settings(move || {
            if let Some(view) = this.upgrade() {
                // Hide and hand focus back first: that also forgets the app
                // vento took focus from, which a later `deactivate_app`
                // could otherwise give focus to out of turn. System
                // Settings then comes forward and keeps it.
                view.cancel();
                view.os.request_access();
                view.os.open_access_settings();
            }
        });
        let this = view.this.clone();
        view.window.on_cancel(move || {
            if let Some(view) = this.upgrade() {
                view.cancel();
            }
        });
        // Closing the window any other way (⌘W) is a cancel too.
        let this = view.this.clone();
        view.window.window().on_close_requested(move || {
            if let Some(view) = this.upgrade() {
                view.cancel();
            }
            CloseRequestResponse::KeepWindowShown
        });
        view
    }

    /// Shows the window in the middle of the screen under the pointer and
    /// brings vento forward, so the window takes the keys.
    pub fn show(&self) {
        let height = match f64::from(self.window.get_content_height()) {
            h if h > 0.0 => h,
            _ => ACCESS_HEIGHT,
        };
        let cursor = self.platform.sample_input().cursor;
        let screens = self.platform.screens();
        let (x, y) = screen_for(cursor, &screens).map_or((cursor.x, cursor.y), |s| {
            (s.x + (s.w - ACCESS_WIDTH) / 2.0, s.y + (s.h - height) / 2.0)
        });
        self.window
            .window()
            .set_position(LogicalPosition::new(x as f32, y as f32));
        // vento runs as an accessory app: bring it forward so the window
        // becomes key and Return and Escape reach it.
        self.platform.activate_app();
        if let Err(e) = self.window.show() {
            warn!("cannot show the Screen Recording window: {e}");
            self.cancel();
            return;
        }
        self.window.invoke_focus_keys();
    }

    /// Hides the window and hands focus back to the app that had it.
    fn cancel(&self) {
        self.hide();
        self.platform.deactivate_app();
    }

    /// Hides the window if it is showing; focus is left where it is, since
    /// the overlay that follows takes it.
    pub fn hide_if_shown(&self) {
        if self.window.window().is_visible() {
            self.hide();
        }
    }

    fn hide(&self) {
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the Screen Recording window: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Done = Box<dyn FnOnce(Option<Selection>)>;

    /// Answers `has_access` from a field and keeps the overlay's callback,
    /// so a test closes the overlay by hand.
    #[derive(Default)]
    struct FakeScreenText {
        access: Cell<bool>,
        selects: Cell<usize>,
        done: RefCell<Option<Done>>,
    }

    impl ScreenText for FakeScreenText {
        fn supported(&self) -> bool {
            true
        }
        fn has_access(&self) -> bool {
            self.access.get()
        }
        fn request_access(&self) {}
        fn open_access_settings(&self) {}
        fn select_area(&self, done: Done) {
            self.selects.set(self.selects.get() + 1);
            *self.done.borrow_mut() = Some(done);
        }
    }

    /// What the controller did, and the callbacks it is waiting on.
    #[derive(Default)]
    struct Log {
        paused: Cell<bool>,
        asks: Cell<usize>,
        hides: Cell<usize>,
        reads: RefCell<Vec<Selection>>,
        pending: RefCell<Option<ReadDone>>,
        copied: RefCell<Vec<String>>,
        copy_fails: Cell<bool>,
        notes: RefCell<Vec<(String, Point)>>,
    }

    struct Rig {
        os: Rc<FakeScreenText>,
        log: Rc<Log>,
        controller: Rc<ScreenTextController>,
    }

    impl Rig {
        fn new(access: bool) -> Self {
            let os = Rc::new(FakeScreenText::default());
            os.access.set(access);
            let log = Rc::new(Log::default());
            let deps = ScreenTextDeps {
                os: os.clone(),
                is_paused: Box::new({
                    let log = log.clone();
                    move || log.paused.get()
                }),
                read: Box::new({
                    let log = log.clone();
                    move |sel, done| {
                        log.reads.borrow_mut().push(sel);
                        *log.pending.borrow_mut() = Some(done);
                    }
                }),
                copy: Box::new({
                    let log = log.clone();
                    move |text| {
                        if log.copy_fails.get() {
                            return Err(PlatformError::Failed("no pasteboard".into()));
                        }
                        log.copied.borrow_mut().push(text.to_owned());
                        Ok(())
                    }
                }),
                notify: Box::new({
                    let log = log.clone();
                    move |text, at| log.notes.borrow_mut().push((text.to_owned(), at))
                }),
                ask_access: Box::new({
                    let log = log.clone();
                    move || log.asks.set(log.asks.get() + 1)
                }),
                hide_access: Box::new({
                    let log = log.clone();
                    move || log.hides.set(log.hides.get() + 1)
                }),
            };
            Self {
                os,
                log,
                controller: ScreenTextController::new(deps),
            }
        }

        /// Closes the overlay with `sel` (`None`: cancelled).
        fn select(&self, sel: Option<Selection>) {
            let done = self.os.done.borrow_mut().take().expect("the overlay is up");
            done(sel);
        }

        /// Answers the read in flight.
        fn answer(&self, result: ReadResult) {
            let done = self
                .log
                .pending
                .borrow_mut()
                .take()
                .expect("a read is in flight");
            done(result);
        }

        fn note_texts(&self) -> Vec<String> {
            self.log
                .notes
                .borrow()
                .iter()
                .map(|(t, _)| t.clone())
                .collect()
        }
    }

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn sel() -> Selection {
        Selection {
            display_id: 1,
            display: r(0.0, 0.0, 1440.0, 900.0),
            rect: r(100.0, 200.0, 300.0, 40.0),
        }
    }

    fn b(y: f64, text: &str) -> TextBox {
        TextBox {
            rect: r(0.0, y, 20.0, 18.0),
            text: text.into(),
        }
    }

    #[test]
    fn start_shows_the_overlay_once() {
        let rig = Rig::new(true);
        rig.controller.start();
        assert_eq!(rig.os.selects.get(), 1);
    }

    #[test]
    fn start_while_busy_does_nothing() {
        let rig = Rig::new(true);
        rig.controller.start();
        rig.controller.start();
        assert_eq!(rig.os.selects.get(), 1);

        rig.select(Some(sel()));
        rig.controller.start();
        assert_eq!(rig.os.selects.get(), 1);

        rig.answer(Ok(vec![b(0.0, "a")]));
        rig.controller.start();
        assert_eq!(rig.os.selects.get(), 2);
    }

    #[test]
    fn start_while_paused_does_nothing() {
        let rig = Rig::new(false);
        rig.log.paused.set(true);
        rig.controller.start();
        assert_eq!(rig.os.selects.get(), 0);
        assert_eq!(rig.log.asks.get(), 0);
    }

    #[test]
    fn start_without_access_asks_and_selects_nothing() {
        let rig = Rig::new(false);
        rig.controller.start();
        assert_eq!(rig.log.asks.get(), 1);
        assert_eq!(rig.os.selects.get(), 0);
        assert!(rig.log.copied.borrow().is_empty());
        assert_eq!(rig.log.hides.get(), 0);
    }

    #[test]
    fn access_granted_hides_the_access_window() {
        let rig = Rig::new(false);
        rig.controller.start();
        assert_eq!(rig.log.asks.get(), 1);
        // The user switches Screen Recording on and presses again.
        rig.os.access.set(true);
        rig.controller.start();
        assert_eq!(rig.log.hides.get(), 1);
        assert_eq!(rig.os.selects.get(), 1);
    }

    #[test]
    fn cancel_copies_nothing() {
        let rig = Rig::new(true);
        rig.controller.start();
        rig.select(None);
        assert!(rig.log.reads.borrow().is_empty());
        assert!(rig.log.copied.borrow().is_empty());
        assert!(rig.log.notes.borrow().is_empty());
        // No longer busy: the next press shows the overlay again.
        rig.controller.start();
        assert_eq!(rig.os.selects.get(), 2);
    }

    #[test]
    fn text_is_copied_and_toasted_below_the_selection() {
        let rig = Rig::new(true);
        rig.controller.start();
        rig.select(Some(sel()));
        assert_eq!(*rig.log.reads.borrow(), vec![sel()]);
        rig.answer(Ok(vec![b(0.0, "a"), b(30.0, "b")]));
        assert_eq!(*rig.log.copied.borrow(), vec!["a\nb".to_owned()]);
        assert_eq!(
            *rig.log.notes.borrow(),
            vec![("Copied text".to_owned(), Point { x: 250.0, y: 240.0 })]
        );
    }

    #[test]
    fn blank_result_leaves_clipboard() {
        let rig = Rig::new(true);
        rig.controller.start();
        rig.select(Some(sel()));
        rig.answer(Ok(vec![b(0.0, "  "), b(30.0, "\t")]));
        assert!(rig.log.copied.borrow().is_empty());
        assert_eq!(rig.note_texts(), vec!["No text found".to_owned()]);
    }

    #[test]
    fn read_error_says_so() {
        let rig = Rig::new(true);
        rig.controller.start();
        rig.select(Some(sel()));
        rig.answer(Err(PlatformError::Failed("capture timed out".into())));
        assert!(rig.log.copied.borrow().is_empty());
        assert_eq!(
            rig.note_texts(),
            vec!["Couldn't read the screen".to_owned()]
        );
    }
}
