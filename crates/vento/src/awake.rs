//! Keep Awake: the running session, the platform hold that matches it and
//! the timer that ends it. The Quick panel shows it and hands it clicks.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use log::{debug, warn};
use slint::{Timer, TimerMode};
use vento_core::awake::{Chip, Session, start_chip};
use vento_core::i18n::tr;
use vento_platform::{AwakeHold, Platform};

use crate::settings_hub::SettingsHub;

/// What the status line says while nothing is held.
fn idle_status() -> String {
    if cfg!(target_os = "macos") {
        tr("Your Mac follows its usual energy settings")
    } else {
        tr("Your computer follows its usual energy settings")
    }
}

/// The right-click menu's Keep Awake item: names what a click does, which
/// is the opposite of what runs now.
pub fn keep_awake_menu_text(on: bool) -> String {
    if on {
        tr("Stop Keep Awake")
    } else {
        tr("Start Keep Awake")
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
    /// Whether the last start failed, shown in the status until the next
    /// start or stop. The text is made when it is read, so it follows the
    /// language.
    failed: Cell<bool>,
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
            failed: Cell::new(false),
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
        if self.failed.get() {
            return tr("Cannot keep the computer awake");
        }
        match self.session.get() {
            Some(session) => session.status(Timestamp::now(), &TimeZone::system()),
            None => idle_status(),
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

    /// Stops what runs, or starts what the tile starts, as saved now.
    pub fn toggle_start(&self) {
        if self.session.get().is_some() {
            self.stop();
        } else {
            self.start(start_chip(self.hub.current().keep_awake_start));
        }
    }

    /// Starts `chip`, replacing what runs.
    pub fn start(&self, chip: Chip) {
        self.failed.set(false);
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
        self.failed.set(false);
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
    /// when it cannot, with the failure kept for the status line.
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
                    self.failed.set(true);
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

    #[test]
    fn the_idle_status_and_the_menu_item_read_in_arabic() {
        let idle = idle_status();
        if cfg!(target_os = "macos") {
            assert_eq!(idle, "Your Mac follows its usual energy settings");
        } else {
            assert_eq!(idle, "Your computer follows its usual energy settings");
        }
        vento_core::i18n::with_language("ar", || {
            let expected = if cfg!(target_os = "macos") {
                "يتبع جهاز Mac إعدادات الطاقة المعتادة"
            } else {
                "يتبع جهازك إعدادات الطاقة المعتادة"
            };
            assert_eq!(idle_status(), expected);
            assert_eq!(keep_awake_menu_text(false), "بدء إبقاء الجهاز مستيقظاً");
            assert_eq!(keep_awake_menu_text(true), "إيقاف إبقاء الجهاز مستيقظاً");
        });
    }
}
