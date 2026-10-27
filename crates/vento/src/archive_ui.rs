//! The Archive action's two windows: a progress pill for long jobs, which
//! never takes the keyboard, and the password prompt for protected
//! archives, which does. Each window exists from its first use until it
//! has been hidden for [`crate::lazy_window::IDLE`].

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use log::{info, warn};
use raw_window_handle::HasWindowHandle;
use slint::{CloseRequestResponse, ComponentHandle, LogicalPosition, Timer, TimerMode};
use vento_core::archive::{Progress, Zeroizing};
use vento_core::geometry::Point;
use vento_core::i18n::{fill, tr};
use vento_platform::Platform;

use crate::appearance;
use crate::lazy_window::LazyWindow;
use crate::toast::position_below;
use crate::ui::{ArchiveProgress, PasswordPrompt, Theme};

/// A job shorter than this never shows its progress window.
const PROGRESS_DELAY: Duration = Duration::from_millis(500);
/// How often the visible progress window reads the job's counters.
const PROGRESS_REFRESH: Duration = Duration::from_millis(100);
/// Progress window size in logical pixels (matches `archive.slint`).
const PROGRESS_SIZE: (f64, f64) = (300.0, 72.0);
/// Password window size in logical pixels (matches `archive.slint`).
const PASSWORD_SIZE: (f64, f64) = (320.0, 150.0);

/// How much of the job is done, from 0 to 1; 0 while the total is unknown.
fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (done as f64 / total as f64).min(1.0) as f32
}

/// What the progress window says the job is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Compressing,
    Extracting,
    Converting,
}

impl Verb {
    /// The verb alone, for before the first item is known.
    fn name(self) -> String {
        match self {
            Verb::Compressing => tr("Compressing"),
            Verb::Extracting => tr("Extracting"),
            Verb::Converting => tr("Converting"),
        }
    }
}

/// The progress window's line: `Extracting Photos.zip…`, or just
/// `Extracting…` before the first item is known.
fn progress_title(verb: Verb, current: &str) -> String {
    if current.is_empty() {
        return format!("{}…", verb.name());
    }
    let template = match verb {
        Verb::Compressing => tr("Compressing {name}…"),
        Verb::Extracting => tr("Extracting {name}…"),
        Verb::Converting => tr("Converting {name}…"),
    };
    fill(&template, &[("name", current)])
}

/// The password window's line for the archive called `name`.
fn prompt_title(name: &str) -> String {
    fill(&tr("\"{name}\" is protected"), &[("name", name)])
}

/// Keeps `window` from ever becoming the key window; true once it worked.
fn keep_non_key(platform: &dyn Platform, window: &slint::Window, what: &str) -> bool {
    let result = match window.window_handle().window_handle() {
        Ok(handle) => platform
            .make_non_key(handle.as_raw())
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    match result {
        Ok(()) => {
            info!("{what} never takes key focus");
            true
        }
        Err(e) => {
            warn!("cannot keep the {what} from taking key focus: {e}");
            false
        }
    }
}

/// The progress window of the job being tracked: it shows up only once the
/// job has run for 500 ms, then follows the job's counters until closed.
pub struct ProgressView {
    this: Weak<ProgressView>,
    platform: Rc<dyn Platform>,
    window: LazyWindow<ArchiveProgress>,
    /// Waits out [`PROGRESS_DELAY`]; runs only between `track` and the
    /// window appearing.
    delay: Timer,
    /// Refreshes the window; runs only while it is shown.
    tick: Timer,
    progress: RefCell<Option<Arc<Progress>>>,
    verb: Cell<Verb>,
    at: Cell<Point>,
    shown: Cell<bool>,
    /// The window has been made non-key (it needs the native window, which
    /// Slint makes at the end of the turn the window was created in).
    non_key: Cell<bool>,
}

impl ProgressView {
    /// Creates the view; its window comes when a job first outlasts the
    /// delay.
    pub fn new(platform: Rc<dyn Platform>) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            this: this.clone(),
            platform,
            window: LazyWindow::new(),
            delay: Timer::default(),
            tick: Timer::default(),
            progress: RefCell::new(None),
            verb: Cell::new(Verb::Extracting),
            at: Cell::new(Point { x: 0.0, y: 0.0 }),
            shown: Cell::new(false),
            non_key: Cell::new(false),
        })
    }

    /// The progress window, created and connected if there is none.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    fn window(&self) -> ArchiveProgress {
        self.window.get_or_create(|| {
            let window = ArchiveProgress::new().expect("cannot create the progress window");
            appearance::follow(&window, appearance::Look::Full);
            // A new native window: it has yet to be made non-key.
            self.non_key.set(false);
            let this = self.this.clone();
            window.on_cancel_clicked(move || {
                if let Some(view) = this.upgrade()
                    && let Some(progress) = view.progress.borrow().as_ref()
                {
                    progress.cancel();
                }
            });
            window
        })
    }

    /// Starts following `progress`; `verb` says what the job does. The window appears 24 px under `at` after 500 ms, and
    /// not at all if [`ProgressView::close`] comes first. Cancel calls
    /// `progress.cancel()`. Tracking while another job is tracked closes
    /// that one first.
    pub fn track(&self, progress: Arc<Progress>, verb: Verb, at: Point) {
        self.close();
        *self.progress.borrow_mut() = Some(progress);
        self.verb.set(verb);
        self.at.set(at);
        let this = self.this.clone();
        self.delay
            .start(TimerMode::SingleShot, PROGRESS_DELAY, move || {
                if let Some(view) = this.upgrade() {
                    view.appear();
                }
            });
    }

    /// Stops following the job and hides the window (or keeps it from ever
    /// appearing).
    pub fn close(&self) {
        self.delay.stop();
        self.tick.stop();
        self.progress.borrow_mut().take();
        if self.shown.replace(false) {
            if let Some(window) = self.window.get()
                && let Err(e) = window.hide()
            {
                warn!("cannot hide the progress window: {e}");
            }
            self.window.release_later();
        }
    }

    /// The job outlasted the delay: show the window and start refreshing it.
    /// A new window shows once it has its native window, so that it can be
    /// made non-key as it shows.
    fn appear(&self) {
        if self.progress.borrow().is_none() {
            return;
        }
        self.window();
        let this = self.this.clone();
        self.window.when_native(move || {
            if let Some(view) = this.upgrade() {
                view.show_window();
            }
        });
    }

    fn show_window(&self) {
        // Closed in the meantime, or tracking a newer job that has yet to
        // outlast the delay.
        if self.progress.borrow().is_none() || self.delay.running() {
            self.window.release_later();
            return;
        }
        let Some(window) = self.window.get() else {
            return;
        };
        appearance::apply(&window.global::<Theme>());
        self.refresh();
        let (x, y) = position_below(self.at.get(), PROGRESS_SIZE, &self.platform.screens());
        window
            .window()
            .set_position(LogicalPosition::new(x as f32, y as f32));
        if let Err(e) = window.show() {
            warn!("cannot show the progress window: {e}");
            self.window.release_later();
            return;
        }
        self.shown.set(true);
        if !self.non_key.get() {
            self.non_key.set(keep_non_key(
                self.platform.as_ref(),
                window.window(),
                "progress window",
            ));
        }
        let this = self.this.clone();
        self.tick
            .start(TimerMode::Repeated, PROGRESS_REFRESH, move || {
                if let Some(view) = this.upgrade() {
                    view.refresh();
                }
            });
    }

    /// Copies the job's counters into the window.
    fn refresh(&self) {
        let Some(snapshot) = self.progress.borrow().as_ref().map(|p| p.snapshot()) else {
            return;
        };
        let Some(window) = self.window.get() else {
            return;
        };
        window.set_fraction(fraction(snapshot.done, snapshot.total));
        let title = progress_title(self.verb.get(), &snapshot.current);
        window.set_heading(title.into());
    }
}

/// What the password window hands to whoever asked: the typed password, or
/// `None` when the user gave up.
type Answer = Box<dyn FnOnce(Option<Zeroizing<Vec<u8>>>)>;

/// The password window: takes key focus while it is open and answers each
/// question exactly once.
pub struct PasswordView {
    this: Weak<PasswordView>,
    platform: Rc<dyn Platform>,
    window: LazyWindow<PasswordPrompt>,
    answer: RefCell<Option<Answer>>,
    open: Cell<bool>,
}

impl PasswordView {
    /// Creates the view; its window comes with the first question.
    pub fn new(platform: Rc<dyn Platform>) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            this: this.clone(),
            platform,
            window: LazyWindow::new(),
            answer: RefCell::new(None),
            open: Cell::new(false),
        })
    }

    /// The password window, created and connected if there is none.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    fn window(&self) -> PasswordPrompt {
        self.window.get_or_create(|| {
            let window = PasswordPrompt::new().expect("cannot create the password window");
            appearance::follow(&window, appearance::Look::Full);
            let this = self.this.clone();
            window.on_submit(move || {
                if let Some(view) = this.upgrade() {
                    view.submit();
                }
            });
            let this = self.this.clone();
            window.on_cancel(move || {
                if let Some(view) = this.upgrade() {
                    view.finish(None);
                }
            });
            // Closing the window any other way (⌘W) is a cancel too.
            let this = self.this.clone();
            window.window().on_close_requested(move || {
                if let Some(view) = this.upgrade() {
                    view.finish(None);
                }
                CloseRequestResponse::KeepWindowShown
            });
            window
        })
    }

    /// Asks for the password of `archive_name`, 24 px under `at`, and takes
    /// key focus. `wrong` adds "Wrong password" (the last answer was
    /// refused). `answer` gets the password, or `None` on Cancel, Escape or
    /// closing the window; a question still open is answered with `None`
    /// first.
    pub fn ask(&self, archive_name: &str, wrong: bool, at: Point, answer: Answer) {
        let earlier = self.answer.borrow_mut().take();
        if let Some(earlier) = earlier {
            earlier(None);
        }
        *self.answer.borrow_mut() = Some(answer);
        let window = self.window();
        appearance::apply(&window.global::<Theme>());
        window.set_heading(prompt_title(archive_name).into());
        window.set_wrong(wrong);
        window.set_password("".into());
        let (x, y) = position_below(at, PASSWORD_SIZE, &self.platform.screens());
        window
            .window()
            .set_position(LogicalPosition::new(x as f32, y as f32));
        self.open.set(true);
        // vento runs as an accessory app: bring it forward so the window
        // becomes key and takes the typing.
        self.platform.activate_app();
        if let Err(e) = window.show() {
            warn!("cannot show the password window: {e}");
            self.finish(None);
            return;
        }
        window.invoke_focus_field();
    }

    /// Closes the window, answering the open question (if any) with `None`.
    pub fn dismiss(&self) {
        if self.open.get() {
            self.finish(None);
        }
    }

    /// Hands the typed password over (never an empty one: the window does
    /// not offer that, and an empty answer would only make the archive ask
    /// again).
    fn submit(&self) {
        let Some(window) = self.window.get() else {
            return;
        };
        let text = window.get_password();
        if text.is_empty() {
            return;
        }
        let bytes = Zeroizing::new(text.as_bytes().to_vec());
        drop(text);
        self.finish(Some(bytes));
    }

    /// Clears the field, hides the window and gives `bytes` to the pending
    /// question, if any.
    fn finish(&self, bytes: Option<Zeroizing<Vec<u8>>>) {
        let answer = self.answer.borrow_mut().take();
        self.hide();
        if let Some(answer) = answer {
            answer(bytes);
        }
    }

    /// Clears the field and hides the window, then hands focus back to the
    /// app that had it. That happens on the next turn of the event loop, so
    /// an answer that asks again straight away keeps the focus.
    fn hide(&self) {
        self.open.set(false);
        if let Some(window) = self.window.get() {
            window.set_password("".into());
            if let Err(e) = window.hide() {
                warn!("cannot hide the password window: {e}");
            }
        }
        self.window.release_later();
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(view) = this.upgrade()
                && !view.open.get()
            {
                view.platform.deactivate_app();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_handles_zero_total() {
        assert_eq!(fraction(0, 0), 0.0);
        assert_eq!(fraction(10, 0), 0.0);
    }

    #[test]
    fn fraction_is_done_over_total_and_never_above_one() {
        assert_eq!(fraction(25, 100), 0.25);
        assert_eq!(fraction(100, 100), 1.0);
        assert_eq!(fraction(150, 100), 1.0);
    }

    #[test]
    fn progress_title_uses_verb_and_name() {
        assert_eq!(
            progress_title(Verb::Extracting, "Photos.zip"),
            "Extracting Photos.zip…"
        );
        assert_eq!(
            progress_title(Verb::Compressing, "a.txt"),
            "Compressing a.txt…"
        );
        assert_eq!(
            progress_title(Verb::Converting, "a.png"),
            "Converting a.png…"
        );
        assert_eq!(progress_title(Verb::Compressing, ""), "Compressing…");
        assert_eq!(progress_title(Verb::Converting, ""), "Converting…");
    }

    #[test]
    fn progress_and_password_lines_read_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            assert_eq!(
                progress_title(Verb::Extracting, "Photos.zip"),
                "جارٍ فك ضغط Photos.zip…"
            );
            assert_eq!(progress_title(Verb::Compressing, "a.txt"), "جارٍ ضغط a.txt…");
            assert_eq!(
                progress_title(Verb::Converting, "a.png"),
                "جارٍ تحويل a.png…"
            );
            assert_eq!(progress_title(Verb::Compressing, ""), "جارٍ الضغط…");
            assert_eq!(prompt_title("Photos.zip"), "«Photos.zip» محمي بكلمة سر");
        });
    }

    #[test]
    fn prompt_title_quotes_the_name() {
        assert_eq!(prompt_title("Photos.zip"), "\"Photos.zip\" is protected");
    }
}
