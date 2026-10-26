//! Keep Awake: the running session, the platform hold that matches it and
//! the timer that ends it. The settings panel shows it and hands it clicks.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use log::{debug, warn};
use slint::{Timer, TimerMode};
use vento_core::awake::{Chip, Session};
use vento_platform::{AwakeHold, Platform};

use crate::settings_hub::SettingsHub;

/// What the status line says while nothing is held.
#[cfg(target_os = "macos")]
const IDLE_STATUS: &str = "Your Mac follows its usual energy settings";
#[cfg(not(target_os = "macos"))]
const IDLE_STATUS: &str = "Your computer follows its usual energy settings";

/// The right-click menu's Keep Awake item: names what a click does, which
/// is the opposite of what runs now.
pub fn keep_awake_menu_text(on: bool) -> &'static str {
    if on {
        "Stop Keep Awake"
    } else {
        "Start Keep Awake"
    }
}

/// Called after every change of the session or its status.
type Listener = Box<dyn Fn()>;

/// Holds the computer awake while a session runs.
pub struct KeepAwake {
    platform: Rc<dyn Platform>,
    hub: Rc<SettingsHub>,
    session: Cell<Option<Session>>,
    /// What the platform holds now.
    held: Cell<Option<AwakeHold>>,
    /// Why the last start failed, shown until the next start or stop.
    error: RefCell<Option<String>>,
    /// Fires when the status may next change or the session ends; idle
    /// while nothing runs.
    timer: Timer,
    listeners: RefCell<Vec<Listener>>,
    this: Weak<Self>,
}

impl KeepAwake {
    /// Nothing held. Follows "Let the screen turn off" in `hub`.
    pub fn new(platform: Rc<dyn Platform>, hub: Rc<SettingsHub>) -> Rc<Self> {
        let keep = Rc::new_cyclic(|this| Self {
            platform,
            hub,
            session: Cell::new(None),
            held: Cell::new(None),
            error: RefCell::new(None),
            timer: Timer::default(),
            listeners: RefCell::new(Vec::new()),
            this: this.clone(),
        });
        // The option applies at once to a running session.
        let this = Rc::downgrade(&keep);
        keep.hub.on_change(Box::new(move |_, _| {
            if let Some(keep) = this.upgrade()
                && keep.session.get().is_some()
            {
                keep.hold(Some(keep.wanted_hold()));
            }
        }));
        keep
    }

    /// The running session, if any.
    pub fn session(&self) -> Option<Session> {
        self.session.get()
    }

    /// The status line.
    pub fn status(&self) -> String {
        if let Some(error) = self.error.borrow().as_ref() {
            return error.clone();
        }
        match self.session.get() {
            Some(session) => session.status(Timestamp::now(), &TimeZone::system()),
            None => IDLE_STATUS.into(),
        }
    }

    /// Starts `chip`, replacing what runs, or stops when `chip` is what
    /// runs.
    pub fn toggle(&self, chip: Chip) {
        if self.session.get().is_some_and(|s| s.chip == chip) {
            self.stop();
        } else {
            self.start(chip);
        }
    }

    /// Starts `chip`, replacing what runs.
    pub fn start(&self, chip: Chip) {
        *self.error.borrow_mut() = None;
        if !self.hold(Some(self.wanted_hold())) {
            // What ran before ends too, rather than holding on unseen.
            self.hold(None);
            self.session.set(None);
            self.timer.stop();
            self.changed();
            return;
        }
        self.session.set(Some(Session::start(
            chip,
            Timestamp::now(),
            &TimeZone::system(),
        )));
        self.arm();
        self.changed();
    }

    /// Lets the computer sleep as usual again.
    pub fn stop(&self) {
        *self.error.borrow_mut() = None;
        self.hold(None);
        self.session.set(None);
        self.timer.stop();
        self.changed();
    }

    /// Calls `f` after every change of the session or its status. A
    /// listener must not add listeners.
    pub fn on_change(&self, f: Listener) {
        self.listeners.borrow_mut().push(f);
    }

    /// The hold a session asks for, with the option as saved now.
    fn wanted_hold(&self) -> AwakeHold {
        AwakeHold {
            screen: !self.hub.current().keep_awake_screen_off,
        }
    }

    /// Asks the platform for `hold` unless it holds that already; false
    /// when it cannot, with the reason kept for the status line.
    fn hold(&self, hold: Option<AwakeHold>) -> bool {
        if self.held.get() == hold {
            return true;
        }
        debug!("Keep Awake: {:?} -> {hold:?}", self.held.get());
        match self.platform.keep_awake(hold) {
            Ok(()) => {
                self.held.set(hold);
                true
            }
            Err(e) => {
                warn!("cannot change Keep Awake to {hold:?}: {e}");
                if hold.is_some() {
                    *self.error.borrow_mut() = Some("Cannot keep the computer awake".into());
                }
                false
            }
        }
    }

    /// Wakes when the running session's status may next change.
    fn arm(&self) {
        let Some(wait) = self
            .session
            .get()
            .and_then(|s| s.next_check(Timestamp::now()))
        else {
            self.timer.stop();
            return;
        };
        let this = self.this.clone();
        self.timer.start(TimerMode::SingleShot, wait, move || {
            if let Some(keep) = this.upgrade() {
                keep.tick();
            }
        });
    }

    fn tick(&self) {
        match self.session.get() {
            Some(session) if session.is_over(Timestamp::now()) => self.stop(),
            Some(_) => {
                self.arm();
                self.changed();
            }
            None => {}
        }
    }

    fn changed(&self) {
        for listener in self.listeners.borrow().iter() {
            listener();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_awake_menu_item_says_what_a_click_does() {
        assert_eq!(keep_awake_menu_text(false), "Start Keep Awake");
        assert_eq!(keep_awake_menu_text(true), "Stop Keep Awake");
    }
}
