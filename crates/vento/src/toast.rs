//! The result toast: one small always-on-top window that shows a message and
//! an optional Undo button, then hides itself. The window exists from the
//! first toast until it has been hidden for [`crate::lazy_window::IDLE`].

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use log::{info, warn};
use raw_window_handle::HasWindowHandle;
use slint::{ComponentHandle, LogicalPosition, Timer, TimerMode};
use vento_core::geometry::{Point, Rect, screen_for};
use vento_platform::Platform;

use crate::lazy_window::LazyWindow;
use crate::ui::Toast;

/// Toast window width in logical pixels (matches `toast.slint`).
const WIDTH: f64 = 300.0;
/// Toast window height in logical pixels (matches `toast.slint`).
const HEIGHT: f64 = 56.0;
/// Gap between the anchor point and the top of the toast.
const OFFSET_Y: f64 = 24.0;
/// How long a toast stays up.
pub(crate) const LIFETIME: Duration = Duration::from_secs(5);

/// Top-left corner of a toast anchored below `at`, horizontally centred on it
/// and clamped to the screen work area that contains (or is nearest to) `at`.
fn toast_position(at: Point, screens: &[Rect]) -> (f64, f64) {
    position_below(at, (WIDTH, HEIGHT), screens)
}

/// Top-left corner of a window of `size` (width, height) anchored 24 px
/// below `at`, horizontally centred on it and clamped to the screen work
/// area that contains (or is nearest to) `at`. The other small windows that
/// appear near the pointer use it too.
pub(crate) fn position_below(at: Point, size: (f64, f64), screens: &[Rect]) -> (f64, f64) {
    let (width, height) = size;
    let (x, y) = (at.x - width / 2.0, at.y + OFFSET_Y);
    let Some(screen) = screen_for(at, screens) else {
        return (x, y);
    };
    (
        clamp(x, screen.x, screen.x + screen.w - width),
        clamp(y, screen.y, screen.y + screen.h - height),
    )
}

/// `v` limited to `[lo, hi]`; a range that is empty (screen smaller than the
/// toast) resolves to `lo`.
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.min(hi).max(lo)
}

/// Shows short-lived messages in a single reused window.
pub struct Toaster {
    this: Weak<Toaster>,
    platform: Rc<dyn Platform>,
    window: LazyWindow<Toast>,
    timer: Timer,
    undo: RefCell<Option<Box<dyn Fn()>>>,
    /// The window has been made non-key (it needs the native window, which
    /// Slint makes at the end of the turn the window was created in).
    non_key: Cell<bool>,
}

impl Toaster {
    /// Creates the toaster; its window comes with the first toast.
    pub fn new(platform: Rc<dyn Platform>) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            this: this.clone(),
            platform,
            window: LazyWindow::new(),
            timer: Timer::default(),
            undo: RefCell::new(None),
            non_key: Cell::new(false),
        })
    }

    /// The toast window, created and connected if there is none.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    fn window(&self) -> Toast {
        self.window.get_or_create(|| {
            let window = Toast::new().expect("cannot create the toast window");
            // A new native window: it has yet to be made non-key.
            self.non_key.set(false);
            let this = self.this.clone();
            window.on_undo_clicked(move || {
                if let Some(toaster) = this.upgrade() {
                    toaster.undo_clicked();
                }
            });
            window
        })
    }

    /// Shows `text` 24 px under `at`, replacing any toast already up and
    /// restarting the 5 s timer. With `undo`, the toast has an Undo button
    /// that runs it once and closes the toast.
    pub fn show(&self, text: &str, undo: Option<Box<dyn Fn()>>, at: Point) {
        let window = self.window();
        window.set_message(text.into());
        window.set_can_undo(undo.is_some());
        *self.undo.borrow_mut() = undo;
        // A new window shows once it has its native window, so that it can
        // be made non-key as it shows.
        let this = self.this.clone();
        self.window.when_native(move || {
            if let Some(toaster) = this.upgrade() {
                toaster.show_at(at);
            }
        });
    }

    /// Shows the toast as set up by [`Toaster::show`] and starts its timer.
    fn show_at(&self, at: Point) {
        let Some(window) = self.window.get() else {
            return;
        };
        let (x, y) = toast_position(at, &self.platform.screens());
        window
            .window()
            .set_position(LogicalPosition::new(x as f32, y as f32));
        if let Err(e) = window.show() {
            warn!("cannot show the toast: {e}");
            self.window.release_later();
            return;
        }
        if !self.non_key.get() {
            self.make_non_key(&window);
        }
        let this = self.this.clone();
        self.timer.start(TimerMode::SingleShot, LIFETIME, move || {
            if let Some(toaster) = this.upgrade() {
                toaster.hide();
            }
        });
    }

    /// Hides the toast. Its window stays: toasts follow most actions, and
    /// making the window again would cost a new GPU surface each time.
    fn hide(&self) {
        if let Some(window) = self.window.get()
            && let Err(e) = window.hide()
        {
            warn!("cannot hide the toast: {e}");
        }
    }

    /// Keeps the toast from ever becoming the key window, and a click on
    /// its Undo from activating vento, so the frontmost app keeps its
    /// keystrokes.
    fn make_non_key(&self, window: &Toast) {
        let result = match window.window().window_handle().window_handle() {
            Ok(handle) => self
                .platform
                .make_non_key(handle.as_raw())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => {
                self.non_key.set(true);
                info!("toast window never takes key focus");
            }
            Err(e) => warn!("cannot keep the toast from taking key focus: {e}"),
        }
    }

    /// Closes the toast first, so an undo that shows its own toast is not
    /// immediately hidden again, then runs the undo callback.
    fn undo_clicked(&self) {
        self.timer.stop();
        self.hide();
        let undo = self.undo.borrow_mut().take();
        if let Some(undo) = undo {
            undo();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn at(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    #[test]
    fn centred_24px_below_the_anchor() {
        let s = [screen(0.0, 0.0, 1440.0, 900.0)];
        assert_eq!(toast_position(at(700.0, 400.0), &s), (550.0, 424.0));
    }

    #[test]
    fn clamped_to_the_right_and_bottom_edge() {
        let s = [screen(0.0, 0.0, 1440.0, 900.0)];
        assert_eq!(toast_position(at(1435.0, 890.0), &s), (1140.0, 844.0));
    }

    #[test]
    fn clamped_to_the_left_and_top_edge() {
        let s = [screen(0.0, 0.0, 1440.0, 900.0)];
        assert_eq!(toast_position(at(5.0, -30.0), &s).0, 0.0);
    }

    #[test]
    fn uses_the_screen_containing_the_anchor() {
        let s = [
            screen(0.0, 0.0, 1440.0, 900.0),
            screen(-1920.0, 0.0, 1920.0, 1080.0),
        ];
        // Near the left edge of the secondary screen, below the primary's height.
        assert_eq!(toast_position(at(-1915.0, 1070.0), &s), (-1920.0, 1024.0));
    }

    #[test]
    fn anchor_over_a_secondary_dock_stays_on_that_screen() {
        // Work areas: primary, and a secondary on the left with its Dock
        // below y = 1000.
        let s = [
            screen(0.0, 25.0, 1440.0, 875.0),
            screen(-1920.0, 0.0, 1920.0, 1000.0),
        ];
        assert_eq!(toast_position(at(-900.0, 1050.0), &s), (-1050.0, 944.0));
    }

    #[test]
    fn screen_smaller_than_the_toast_pins_to_its_origin() {
        let s = [screen(10.0, 20.0, 200.0, 30.0)];
        assert_eq!(toast_position(at(100.0, 25.0), &s), (10.0, 20.0));
    }
}
