//! Hold-to-open tracking for the destination ring: pure logic, fed by the shell.

use std::time::{Duration, Instant};

/// What the cursor is over, as far as the ring is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hover {
    Centre,
    Action(usize),
    Destination(usize),
    /// Inside the ring's extent but on no cell.
    Gap,
    /// Beyond the wheel and the ring.
    Outside,
}

/// What [`RingTracker::feed`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingEvent {
    None,
    /// The ring for this action index should open.
    Open(usize),
    Close,
}

/// Opens the ring after the cursor rests on a ring-taking action for the hold
/// delay, and closes it when the cursor leaves the action and the ring.
pub struct RingTracker {
    hold: Duration,
    takes_ring: Box<dyn Fn(usize) -> bool>,
    /// The action whose ring is open.
    open: Option<usize>,
    /// The ring-taking action being hovered while closed, and since when.
    holding: Option<(usize, Instant)>,
}

impl RingTracker {
    pub fn new(hold: Duration, takes_ring: impl Fn(usize) -> bool + 'static) -> Self {
        Self {
            hold,
            takes_ring: Box::new(takes_ring),
            open: None,
            holding: None,
        }
    }

    /// Applies one hover sample. Cheap enough for every poll tick, and reports
    /// [`RingEvent::Open`] once per hold.
    pub fn feed(&mut self, hover: Hover, now: Instant) -> RingEvent {
        let Some(open) = self.open else {
            return self.feed_closed(hover, now);
        };
        match hover {
            Hover::Action(i) if i == open => RingEvent::None,
            Hover::Destination(_) | Hover::Gap => RingEvent::None,
            Hover::Centre | Hover::Action(_) | Hover::Outside => {
                self.open = None;
                // Landing on another ring-taking action starts its hold now.
                self.feed_closed(hover, now);
                RingEvent::Close
            }
        }
    }

    fn feed_closed(&mut self, hover: Hover, now: Instant) -> RingEvent {
        let Hover::Action(i) = hover else {
            self.holding = None;
            return RingEvent::None;
        };
        if !(self.takes_ring)(i) {
            self.holding = None;
            return RingEvent::None;
        }
        let since = match self.holding {
            Some((held, since)) if held == i => since,
            _ => now,
        };
        self.holding = Some((i, since));
        if now.saturating_duration_since(since) >= self.hold {
            self.holding = None;
            self.open = Some(i);
            RingEvent::Open(i)
        } else {
            RingEvent::None
        }
    }

    /// The action whose ring is currently open.
    pub fn open_for(&self) -> Option<usize> {
        self.open
    }

    /// Forgets any open ring and any hold in progress, for when the wheel
    /// closes.
    pub fn reset(&mut self) {
        self.open = None;
        self.holding = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_millis(400);

    fn tracker() -> RingTracker {
        // Actions 2 and 3 take a ring; everything else is a plain action.
        RingTracker::new(HOLD, |i| i == 2 || i == 3)
    }

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn opens_after_hold_on_move() {
        let mut t = tracker();
        let t0 = Instant::now();
        assert_eq!(t.feed(Hover::Action(2), t0), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 399)), RingEvent::None);
        assert_eq!(t.open_for(), None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 400)), RingEvent::Open(2));
        assert_eq!(t.open_for(), Some(2));
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 408)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 900)), RingEvent::None);
    }

    #[test]
    fn moving_off_restarts_the_hold() {
        let mut t = tracker();
        let t0 = Instant::now();
        t.feed(Hover::Action(2), t0);
        // Off to another cell, then back: the count starts over.
        t.feed(Hover::Action(0), ms(t0, 300));
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 350)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 700)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 750)), RingEvent::Open(2));

        // Moving to a different ring-taking action also restarts it.
        let mut t = tracker();
        t.feed(Hover::Action(2), t0);
        t.feed(Hover::Action(3), ms(t0, 300));
        assert_eq!(t.feed(Hover::Action(3), ms(t0, 500)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(3), ms(t0, 700)), RingEvent::Open(3));

        // So does leaving the wheel's cells for the centre.
        let mut t = tracker();
        t.feed(Hover::Action(2), t0);
        t.feed(Hover::Centre, ms(t0, 300));
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 400)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 800)), RingEvent::Open(2));
    }

    #[test]
    fn non_ring_action_never_opens() {
        let mut t = tracker();
        let t0 = Instant::now();
        for n in (0..2000).step_by(8) {
            assert_eq!(t.feed(Hover::Action(1), ms(t0, n)), RingEvent::None);
        }
        assert_eq!(t.open_for(), None);
    }

    #[test]
    fn destination_and_gap_keep_it_open() {
        let mut t = tracker();
        let t0 = Instant::now();
        t.feed(Hover::Action(2), t0);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 400)), RingEvent::Open(2));
        assert_eq!(t.feed(Hover::Destination(1), ms(t0, 500)), RingEvent::None);
        assert_eq!(t.feed(Hover::Gap, ms(t0, 510)), RingEvent::None);
        assert_eq!(t.feed(Hover::Destination(4), ms(t0, 520)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 530)), RingEvent::None);
        assert_eq!(t.open_for(), Some(2));
    }

    #[test]
    fn centre_other_action_and_outside_close() {
        let t0 = Instant::now();
        for leave in [
            Hover::Centre,
            Hover::Action(3),
            Hover::Action(0),
            Hover::Outside,
        ] {
            let mut t = tracker();
            t.feed(Hover::Action(2), t0);
            assert_eq!(t.feed(Hover::Action(2), ms(t0, 400)), RingEvent::Open(2));
            assert_eq!(t.feed(leave, ms(t0, 500)), RingEvent::Close, "{leave:?}");
            assert_eq!(t.open_for(), None, "{leave:?}");
            // Closed means closed: the next tick reports nothing.
            assert_eq!(t.feed(leave, ms(t0, 508)), RingEvent::None, "{leave:?}");
        }
    }

    #[test]
    fn reset_forgets_open_ring() {
        let mut t = tracker();
        let t0 = Instant::now();
        t.feed(Hover::Action(2), t0);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 400)), RingEvent::Open(2));
        t.reset();
        assert_eq!(t.open_for(), None);
        // A fresh wheel starts a fresh hold, and a stale one is not resumed.
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 1000)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 1399)), RingEvent::None);
        assert_eq!(t.feed(Hover::Action(2), ms(t0, 1400)), RingEvent::Open(2));
    }
}
