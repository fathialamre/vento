//! Keep Awake: the running session, the platform hold that matches it and
//! the timer that ends it. The Quick panel shows it and hands it clicks.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use log::{debug, warn};
use slint::{Timer, TimerMode};
use vento_core::awake::{Chip, Session, duration_text, start_chip};
use vento_core::i18n::dates::time_hm;
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

/// How long before its end a session is said to end soon.
const ENDING_SOON: Duration = Duration::from_secs(5 * 60);
/// What "+1 h" adds to the time a session has left.
const HOUR_MORE: Duration = Duration::from_secs(60 * 60);

/// How long `session` has left at `now`; `None` for one with no end.
fn time_left(session: Session, now: Timestamp) -> Option<Duration> {
    let ends = session.ends?;
    Some(Duration::try_from(ends.duration_since(now)).unwrap_or(Duration::ZERO))
}

/// Whether `session` ends at most [`ENDING_SOON`] after `now`.
pub fn ends_soon(session: Session, now: Timestamp) -> bool {
    time_left(session, now).is_some_and(|left| left <= ENDING_SOON)
}

/// What the island's resting pill says of `session` at `now`, beside the
/// cup: the time it has left (`"1 h 23 min"`, never under a minute) for a
/// timed one, when it ends (`"18:30"`, in `tz`) for one until a time, and
/// nothing for one with no end.
pub fn rest_status(session: Session, now: Timestamp, tz: &TimeZone) -> String {
    let (Some(ends), Some(left)) = (session.ends, time_left(session, now)) else {
        return String::new();
    };
    match session.chip {
        Chip::For(_) => duration_text(left.as_secs().div_ceil(60).max(1)),
        Chip::Until { .. } | Chip::Forever => time_hm(ends.to_zoned(tz.clone()).time()),
    }
}

/// What "+1 h" starts in place of `session` at `now`: a session for the time
/// it has left and an hour more. `None` for one with no end.
pub fn hour_more(session: Session, now: Timestamp) -> Option<Chip> {
    time_left(session, now).map(|left| Chip::For(left + HOUR_MORE))
}

/// What the island says as Keep Awake changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AwakeNews {
    /// The session ends in [`ENDING_SOON`].
    EndsSoon,
    /// The session ended or was stopped: the computer may sleep again.
    Stopped,
}

/// Follows Keep Awake for the island: says once per session that it ends
/// soon, and that it stopped. A session started with that little time
/// left was chosen so, and is not said to end soon.
#[derive(Default)]
pub struct AwakeWatch {
    session: Option<Session>,
    /// The session has been said to end soon (or started so).
    told: bool,
}

impl AwakeWatch {
    /// The running session is `session` at `now`: what to say, if anything.
    pub fn changed(&mut self, session: Option<Session>, now: Timestamp) -> Option<AwakeNews> {
        let before = std::mem::replace(&mut self.session, session);
        let Some(session) = session else {
            return before.is_some().then_some(AwakeNews::Stopped);
        };
        if before != Some(session) {
            self.told = ends_soon(session, now);
            return None;
        }
        if self.told || !ends_soon(session, now) {
            return None;
        }
        self.told = true;
        Some(AwakeNews::EndsSoon)
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

    /// "+1 h": starts a session for the time the running one has left and
    /// an hour more. Does nothing for one with no end.
    pub fn add_hour(&self) {
        if let Some(chip) = self
            .session
            .get()
            .and_then(|s| hour_more(s, Timestamp::now()))
        {
            self.start(chip);
        }
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
    use std::time::Duration;

    use super::*;

    /// 2026-10-25 at `hh:mm` UTC.
    fn at(hh: i8, mm: i8) -> Timestamp {
        jiff::civil::date(2026, 10, 25)
            .at(hh, mm, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    fn session(chip: Chip, now: Timestamp) -> Session {
        Session::start(chip, now, &TimeZone::UTC)
    }

    #[test]
    fn ending_soon_is_posted_once_per_session() {
        let mut watch = AwakeWatch::default();
        let hour = session(Chip::For(minutes(60)), at(12, 0));
        assert_eq!(watch.changed(Some(hour), at(12, 0)), None);
        assert_eq!(watch.changed(Some(hour), at(12, 54)), None);
        assert_eq!(
            watch.changed(Some(hour), at(12, 55)),
            Some(AwakeNews::EndsSoon)
        );
        // The minutes that follow change the status, not the news.
        assert_eq!(watch.changed(Some(hour), at(12, 56)), None);
        assert_eq!(watch.changed(Some(hour), at(12, 59)), None);
        // +1 h starts a new session, which is told of again near its end.
        let more = session(Chip::For(minutes(64)), at(12, 56));
        assert_eq!(watch.changed(Some(more), at(12, 56)), None);
        assert_eq!(watch.changed(Some(more), at(13, 54)), None);
        assert_eq!(
            watch.changed(Some(more), at(13, 55)),
            Some(AwakeNews::EndsSoon)
        );
        assert_eq!(watch.changed(None, at(14, 0)), Some(AwakeNews::Stopped));
        assert_eq!(watch.changed(None, at(14, 1)), None);
    }

    #[test]
    fn short_and_endless_sessions_never_end_soon() {
        let mut watch = AwakeWatch::default();
        // Chosen that short: not told it ends soon.
        let short = session(Chip::For(minutes(5)), at(12, 0));
        assert_eq!(watch.changed(Some(short), at(12, 0)), None);
        assert_eq!(watch.changed(Some(short), at(12, 3)), None);
        let forever = session(Chip::Forever, at(12, 4));
        assert_eq!(watch.changed(Some(forever), at(12, 4)), None);
        assert_eq!(watch.changed(Some(forever), at(23, 59)), None);
        assert!(!ends_soon(forever, at(23, 59)));
        // Nothing ran: stopping says nothing.
        assert_eq!(AwakeWatch::default().changed(None, at(12, 0)), None);
    }

    #[test]
    fn the_resting_pill_shows_the_time_left() {
        let tz = TimeZone::UTC;
        let timed = session(Chip::For(minutes(90)), at(12, 0));
        assert_eq!(rest_status(timed, at(12, 7), &tz), "1 h 23 min");
        assert_eq!(rest_status(timed, at(13, 29), &tz), "1 min");
        let until = session(
            Chip::Until {
                hour: 18,
                minute: 30,
            },
            at(12, 0),
        );
        assert_eq!(rest_status(until, at(12, 0), &tz), "18:30");
        let forever = session(Chip::Forever, at(12, 0));
        assert_eq!(rest_status(forever, at(12, 0), &tz), "");
    }

    #[test]
    fn an_hour_more_adds_to_the_time_left() {
        let timed = session(Chip::For(minutes(60)), at(12, 0));
        assert_eq!(hour_more(timed, at(12, 56)), Some(Chip::For(minutes(64))));
        let until = session(
            Chip::Until {
                hour: 18,
                minute: 30,
            },
            at(12, 0),
        );
        assert_eq!(hour_more(until, at(18, 26)), Some(Chip::For(minutes(64))));
        assert_eq!(
            hour_more(session(Chip::Forever, at(12, 0)), at(13, 0)),
            None
        );
    }

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
