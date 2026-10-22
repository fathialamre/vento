//! Drag-trigger state machine: decides when the wheel opens, moves, releases
//! or is cancelled, from periodic input samples.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::geometry::Point;
use crate::pocket_zone::ShakeDetector;

/// A keyboard modifier that can be configured as the wheel trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

/// The set of modifier keys currently held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl Modifiers {
    /// Whether the given modifier is currently held.
    pub fn has(&self, m: Modifier) -> bool {
        match m {
            Modifier::Ctrl => self.ctrl,
            Modifier::Alt => self.alt,
            Modifier::Shift => self.shift,
            Modifier::Meta => self.meta,
        }
    }
}

/// One snapshot of global input state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputSample {
    pub left_down: bool,
    pub modifiers: Modifiers,
    pub escape_down: bool,
    /// Change counter of the drag pasteboard; it bumps when a drag starts.
    pub drag_change_count: i64,
    /// Whether the drag pasteboard carries file URLs.
    pub drag_has_files: bool,
    /// Change counter of the general pasteboard; it bumps on every copy.
    pub clip_change_count: i64,
    pub cursor: Point,
}

/// What the machine decided for a sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TriggerEvent {
    None,
    Open(Point),
    Move(Point),
    Release,
    Cancel,
}

/// Turns input samples into wheel lifecycle events.
#[derive(Debug, Clone)]
pub struct TriggerMachine {
    trigger: Modifier,
    prev_left: bool,
    prev_trigger: bool,
    prev_escape: bool,
    baseline: i64,
    /// Drag change count of the most recent sample.
    last_count: i64,
    /// Drag change count of the most recent sample with the button up.
    last_up_count: Option<i64>,
    open: bool,
    finished: bool,
    /// Watches the cursor during a file drag when a shake opens the wheel.
    shake: Option<ShakeDetector>,
}

impl TriggerMachine {
    pub fn new(trigger: Modifier) -> Self {
        Self {
            trigger,
            prev_left: false,
            prev_trigger: false,
            prev_escape: false,
            baseline: 0,
            last_count: 0,
            last_up_count: None,
            open: false,
            finished: false,
            shake: None,
        }
    }

    /// With `on`, a quick side-to-side shake of the cursor during a file
    /// drag opens the wheel too, as the trigger modifier does.
    pub fn with_shake(mut self, on: bool) -> Self {
        self.shake = on.then(ShakeDetector::new);
        self
    }

    /// Processes one sample and returns the resulting event.
    pub fn feed(&mut self, s: &InputSample) -> TriggerEvent {
        self.feed_at(s, Instant::now())
    }

    /// [`feed`](Self::feed) for a sample taken at `now`.
    pub fn feed_at(&mut self, s: &InputSample, now: Instant) -> TriggerEvent {
        let trigger_down = s.modifiers.has(self.trigger);
        let trigger_rose = trigger_down && !self.prev_trigger;
        let escape_rose = s.escape_down && !self.prev_escape;
        if !s.left_down {
            self.last_up_count = Some(s.drag_change_count);
        } else if !self.prev_left {
            // A drag that started inside the polling gap has already bumped
            // the count, so compare against the last count seen with the
            // button up, not the one on this first down sample.
            self.baseline = self.last_up_count.unwrap_or(s.drag_change_count);
        }
        self.last_count = s.drag_change_count;
        self.prev_left = s.left_down;
        self.prev_trigger = trigger_down;
        self.prev_escape = s.escape_down;

        if self.open {
            if self.finished {
                return TriggerEvent::None;
            }
            if !s.left_down {
                self.finished = true;
                return TriggerEvent::Release;
            }
            if escape_rose {
                self.finished = true;
                return TriggerEvent::Cancel;
            }
            return TriggerEvent::Move(s.cursor);
        }

        let drag_active = s.left_down && s.drag_change_count != self.baseline && s.drag_has_files;
        if !drag_active && let Some(shake) = &mut self.shake {
            // Each drag shakes from scratch.
            shake.reset();
        }
        let shaken = drag_active
            && self
                .shake
                .as_mut()
                .is_some_and(|shake| shake.feed(s.cursor.x, now));
        if drag_active && (trigger_rose || shaken) {
            self.open = true;
            return TriggerEvent::Open(s.cursor);
        }
        TriggerEvent::None
    }

    /// Whether a drag is under way: the button is down and the drag pasteboard
    /// change count has moved since the baseline (ruling R15), whatever types it
    /// carries. The Pocket accepts non-file drags, unlike the wheel.
    pub fn drag_in_progress(&self) -> bool {
        self.prev_left && self.last_count != self.baseline
    }

    /// Whether the wheel is open (stays true after release/cancel until [`close`](Self::close)).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Returns to idle after the wheel has been dismissed.
    pub fn close(&mut self) {
        self.open = false;
        self.finished = false;
    }

    /// How long the caller should wait before sampling again.
    pub fn poll_interval(&self) -> Duration {
        if self.prev_left || self.open {
            Duration::from_millis(8)
        } else {
            Duration::from_millis(100)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURSOR: Point = Point { x: 10., y: 20. };

    fn sample(down: bool, ctrl: bool, escape: bool, cc: i64, files: bool) -> InputSample {
        InputSample {
            left_down: down,
            modifiers: Modifiers {
                ctrl,
                ..Modifiers::default()
            },
            escape_down: escape,
            drag_change_count: cc,
            drag_has_files: files,
            clip_change_count: 0,
            cursor: CURSOR,
        }
    }

    fn s(down: bool, ctrl: bool, cc: i64, files: bool) -> InputSample {
        sample(down, ctrl, false, cc, files)
    }

    fn opened() -> TriggerMachine {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        m.feed(&s(false, false, 5, false));
        m.feed(&s(true, false, 5, false));
        m.feed(&s(true, false, 6, true));
        assert_eq!(m.feed(&s(true, true, 6, true)), TriggerEvent::Open(CURSOR));
        m
    }

    /// Feeds a file drag that shakes: the cursor swings 50 points between
    /// x 0 and 50, `swings` times, 20 ms apart. Returns the last event.
    fn shake(m: &mut TriggerMachine, files: bool, swings: usize) -> TriggerEvent {
        let t0 = Instant::now();
        let at = |x: f64| InputSample {
            cursor: Point { x, y: 20. },
            ..s(true, false, 6, files)
        };
        m.feed_at(&s(false, false, 5, false), t0);
        m.feed_at(&s(true, false, 5, false), t0);
        let mut last = TriggerEvent::None;
        for i in 0..=swings {
            let x = if i % 2 == 0 { 0.0 } else { 50.0 };
            let now = t0 + Duration::from_millis(20 * (i as u64 + 1));
            last = m.feed_at(&at(x), now);
            if last != TriggerEvent::None {
                break;
            }
        }
        last
    }

    #[test]
    fn opens_on_a_shake_during_a_file_drag() {
        let mut m = TriggerMachine::new(Modifier::Ctrl).with_shake(true);
        assert!(matches!(shake(&mut m, true, 6), TriggerEvent::Open(_)));
        assert!(m.is_open());
    }

    #[test]
    fn a_shake_does_nothing_when_off_without_files_or_too_short() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        assert_eq!(shake(&mut m, true, 6), TriggerEvent::None);
        let mut m = TriggerMachine::new(Modifier::Ctrl).with_shake(true);
        assert_eq!(shake(&mut m, false, 6), TriggerEvent::None);
        let mut m = TriggerMachine::new(Modifier::Ctrl).with_shake(true);
        assert_eq!(shake(&mut m, true, 3), TriggerEvent::None);
        assert!(!m.is_open());
    }

    #[test]
    fn a_shake_rearms_after_the_button_is_released() {
        let mut m = TriggerMachine::new(Modifier::Ctrl).with_shake(true);
        assert_eq!(shake(&mut m, true, 3), TriggerEvent::None);
        // Released: the half-made shake is forgotten, the next drag starts over.
        assert_eq!(shake(&mut m, true, 3), TriggerEvent::None);
        assert!(matches!(shake(&mut m, true, 6), TriggerEvent::Open(_)));
    }

    #[test]
    fn opens_on_modifier_after_file_drag() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        assert_eq!(m.feed(&s(false, false, 5, false)), TriggerEvent::None);
        assert_eq!(m.feed(&s(true, false, 5, false)), TriggerEvent::None);
        assert_eq!(m.feed(&s(true, false, 6, true)), TriggerEvent::None);
        assert_eq!(m.feed(&s(true, true, 6, true)), TriggerEvent::Open(CURSOR));
        assert!(m.is_open());
    }

    #[test]
    fn drag_started_before_first_down_sample_still_opens() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        assert_eq!(m.feed(&s(false, false, 5, false)), TriggerEvent::None);
        // The drag began between polls: the first down sample already shows
        // the bumped count and the files.
        assert_eq!(m.feed(&s(true, false, 6, true)), TriggerEvent::None);
        assert_eq!(m.feed(&s(true, true, 6, true)), TriggerEvent::Open(CURSOR));
    }

    #[test]
    fn no_open_if_modifier_held_since_before_drag() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        m.feed(&s(true, true, 5, false));
        assert_eq!(m.feed(&s(true, true, 6, true)), TriggerEvent::None);
        assert_eq!(m.feed(&s(true, false, 6, true)), TriggerEvent::None);
        assert_eq!(m.feed(&s(true, true, 6, true)), TriggerEvent::Open(CURSOR));
    }

    #[test]
    fn no_open_for_non_file_drag() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        m.feed(&s(true, false, 5, false));
        assert_eq!(m.feed(&s(true, false, 6, false)), TriggerEvent::None);
        assert_eq!(m.feed(&s(true, true, 6, false)), TriggerEvent::None);
        assert!(!m.is_open());
    }

    #[test]
    fn no_open_without_drag() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        m.feed(&s(true, false, 5, false));
        assert_eq!(m.feed(&s(true, true, 5, false)), TriggerEvent::None);
        assert!(!m.is_open());
    }

    #[test]
    fn stays_open_after_modifier_release() {
        let mut m = opened();
        assert_eq!(m.feed(&s(true, false, 6, true)), TriggerEvent::Move(CURSOR));
        assert!(m.is_open());
    }

    #[test]
    fn release_once_then_quiet_until_close() {
        let mut m = opened();
        assert_eq!(m.feed(&s(false, false, 6, true)), TriggerEvent::Release);
        assert_eq!(m.feed(&s(false, false, 6, true)), TriggerEvent::None);
        assert!(m.is_open());
        m.close();
        assert!(!m.is_open());
    }

    #[test]
    fn drag_in_progress_ignores_types() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        assert_eq!(m.feed(&s(false, false, 5, false)), TriggerEvent::None);
        assert!(!m.drag_in_progress());
        assert_eq!(m.feed(&s(true, false, 6, false)), TriggerEvent::None);
        assert!(m.drag_in_progress());
    }

    #[test]
    fn drag_in_progress_false_after_release() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        m.feed(&s(false, false, 5, false));
        m.feed(&s(true, false, 6, false));
        assert!(m.drag_in_progress());
        m.feed(&s(false, false, 6, false));
        assert!(!m.drag_in_progress());
    }

    #[test]
    fn poll_intervals() {
        let mut m = TriggerMachine::new(Modifier::Ctrl);
        assert_eq!(m.poll_interval(), Duration::from_millis(100));
        m.feed(&s(true, false, 5, false));
        assert_eq!(m.poll_interval(), Duration::from_millis(8));
        let mut m = opened();
        assert_eq!(m.poll_interval(), Duration::from_millis(8));
        m.feed(&s(false, false, 6, true));
        assert_eq!(m.poll_interval(), Duration::from_millis(8));
    }

    #[test]
    fn escape_cancels_once() {
        let mut m = opened();
        assert_eq!(
            m.feed(&sample(true, true, true, 6, true)),
            TriggerEvent::Cancel
        );
        assert_eq!(m.feed(&s(true, true, 6, true)), TriggerEvent::None);
        assert!(m.is_open());
        m.close();
        assert!(!m.is_open());
    }
}
