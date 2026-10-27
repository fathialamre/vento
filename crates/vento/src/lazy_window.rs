//! A window created on first use and dropped again once it has stayed
//! hidden for a while.
//!
//! Slint gives every window that exists a native window and a renderer as
//! soon as the event loop runs, and keeps both while the window is hidden;
//! only dropping the window frees them. The windows that show now and then
//! (the Quick panel, the Settings window, the archive and Screen Recording
//! windows) therefore exist only from their first use until they have been
//! hidden for [`IDLE`]. The toast is made on first use too, but kept once made.
//!
//! Slint makes the native window of a new window at the end of the turn of
//! the event loop it was created in, not when it is shown; a window that
//! AppKit must set up before it shows waits for it with
//! [`LazyWindow::when_native`].

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use raw_window_handle::HasWindowHandle;
use slint::{ComponentHandle, Timer, TimerMode};

/// How long a hidden window is kept for a quick reopen before it is dropped.
pub const IDLE: Duration = Duration::from_secs(30);

/// One window, created on demand and dropped [`IDLE`] after it was hidden.
pub struct LazyWindow<T: ComponentHandle + 'static> {
    slot: Rc<RefCell<Option<T>>>,
    /// Runs from a hide until the drop; a use in between stops it.
    release: Timer,
}

impl<T: ComponentHandle + 'static> LazyWindow<T> {
    /// No window yet.
    pub fn new() -> Self {
        Self {
            slot: Rc::new(RefCell::new(None)),
            release: Timer::default(),
        }
    }

    /// The window, if it exists now.
    pub fn get(&self) -> Option<T> {
        self.slot
            .borrow()
            .as_ref()
            .map(ComponentHandle::clone_strong)
    }

    /// The window, made with `create` if there is none, and kept until it
    /// is hidden again. `create` connects the new window's callbacks
    /// (through weak references, or the window could never be dropped).
    pub fn get_or_create(&self, create: impl FnOnce() -> T) -> T {
        self.release.stop();
        if let Some(window) = self.get() {
            return window;
        }
        let window = create();
        *self.slot.borrow_mut() = Some(window.clone_strong());
        window
    }

    /// Calls `then` once the window has its native window: at once when it
    /// has (or there is no window), or else on the next turn of the event
    /// loop, by when Slint has made it.
    pub fn when_native(&self, then: impl FnOnce() + 'static) {
        let pending = self
            .slot
            .borrow()
            .as_ref()
            .is_some_and(|window| window.window().window_handle().window_handle().is_err());
        if pending {
            Timer::single_shot(Duration::ZERO, then);
        } else {
            then();
        }
    }

    /// Drops the window [`IDLE`] from now, on a turn of the event loop of
    /// its own (never inside one of the window's callbacks), unless it is
    /// used or shown again first.
    pub fn release_later(&self) {
        if self.slot.borrow().is_none() {
            return;
        }
        let slot = Rc::downgrade(&self.slot);
        self.release.start(TimerMode::SingleShot, IDLE, move || {
            let Some(slot) = slot.upgrade() else { return };
            let hidden = slot
                .borrow()
                .as_ref()
                .is_some_and(|window| !window.window().is_visible());
            if hidden {
                // Dropped after the borrow ends: dropping runs Slint code.
                let window = slot.borrow_mut().take();
                drop(window);
            }
        });
    }
}
