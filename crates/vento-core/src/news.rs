//! What the virtual island announces: running jobs and finished results.
//!
//! A pure board the shell feeds and reads each tick: jobs that run long enough
//! show as work, finished results show for a hold, and everything posted is
//! kept in a short log.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long a job must have been running before the island shows it, so
/// quick jobs never flash work.
pub const WORK_DELAY: Duration = Duration::from_millis(400);
/// How long a result stays up after it was posted, or after the pointer
/// leaves it.
pub const RESULT_HOLD: Duration = Duration::from_millis(3500);
/// How many posts the log keeps; the oldest are dropped.
pub const LOG_MAX: usize = 50;

/// How a result reads: the mark and whether it asks to be seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Done,
    Info,
    /// Always sticky: stays until it has been hovered.
    Error,
    Busy,
}

/// Which face of the news the island shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Work,
    Result,
}

/// What the island shows now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    pub id: u64,
    pub stage: Stage,
    /// Other things going on: running jobs and results still up, apart from
    /// the shown one. The island offers them as a "+N" chip.
    pub extra: usize,
}

/// One post, as kept in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub id: u64,
    pub tone: Tone,
    pub at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct Job {
    id: u64,
    started: Instant,
}

#[derive(Clone, Copy, Debug)]
struct Posted {
    id: u64,
    /// Whether it stays up until seen (every error does).
    sticky: bool,
    /// Whether the result has been hovered while shown, or was posted under
    /// the pointer.
    seen: bool,
    /// When the hold ends (exclusive). `None` while a sticky result waits to
    /// be seen, or while the pointer is on the island.
    hold_until: Option<Instant>,
}

impl Posted {
    /// Whether the result is still up at `now`.
    fn live(&self, now: Instant) -> bool {
        self.hold_until.is_none_or(|end| now < end)
    }
}

/// Tracks running jobs and posted results and decides what the island shows.
#[derive(Clone, Debug, Default)]
pub struct NewsBoard {
    jobs: Vec<Job>,
    /// Oldest first, so the last live one is the newest.
    results: Vec<Posted>,
    log: VecDeque<LogEntry>,
    hovered: bool,
}

impl NewsBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// A job began. Starting an id that is already running restarts it.
    pub fn start_job(&mut self, id: u64, now: Instant) {
        self.jobs.retain(|j| j.id != id);
        self.jobs.push(Job { id, started: now });
    }

    /// A job finished or was abandoned. Its outcome, if any, comes by
    /// [`post`](Self::post).
    pub fn end_job(&mut self, id: u64) {
        self.jobs.retain(|j| j.id != id);
    }

    /// Announces a result and logs it. A sticky post (and every
    /// [`Tone::Error`]) stays up until it has been hovered once; posting an id
    /// that already has a result replaces that result.
    pub fn post(&mut self, id: u64, tone: Tone, sticky: bool, now: Instant) {
        self.results.retain(|r| r.id != id && r.live(now));
        let sticky = sticky || tone == Tone::Error;
        // Posted under the pointer, it is already being seen.
        let (seen, hold_until) = if self.hovered {
            (true, None)
        } else if sticky {
            (false, None)
        } else {
            (false, Some(now + RESULT_HOLD))
        };
        self.results.push(Posted {
            id,
            sticky,
            seen,
            hold_until,
        });
        self.log.push_back(LogEntry { id, tone, at: now });
        while self.log.len() > LOG_MAX {
            self.log.pop_front();
        }
    }

    /// The pointer entered or left the island. Results up when it enters are
    /// held while it stays, and until [`RESULT_HOLD`] after it leaves. Only
    /// the shown one (the newest) is seen: a sticky result behind it, only a
    /// "+N" so far, waits again once the pointer leaves.
    pub fn set_hover(&mut self, hovered: bool, now: Instant) {
        if hovered == self.hovered {
            return;
        }
        self.hovered = hovered;
        self.results.retain(|r| r.live(now));
        if hovered && let Some(shown) = self.results.last_mut() {
            shown.seen = true;
        }
        for r in &mut self.results {
            r.hold_until = if hovered || r.sticky && !r.seen {
                None
            } else {
                Some(now + RESULT_HOLD)
            };
        }
    }

    /// What the island shows at `now`: the newest result still up, else the
    /// newest job that has run for [`WORK_DELAY`], else nothing.
    pub fn current(&self, now: Instant) -> Option<Shown> {
        let live = || self.results.iter().filter(|r| r.live(now));
        let working = || {
            self.jobs
                .iter()
                .filter(|j| now.saturating_duration_since(j.started) >= WORK_DELAY)
        };
        if let Some(shown) = live().next_back() {
            Some(Shown {
                id: shown.id,
                stage: Stage::Result,
                extra: live().count() - 1 + working().count(),
            })
        } else {
            working().next_back().map(|job| Shown {
                id: job.id,
                stage: Stage::Work,
                extra: working().count() - 1,
            })
        }
    }

    /// The results the board still holds, by id: those up now, and those
    /// whose hold ended since the last post or change of hover. Whatever
    /// tells of them must be kept while they are held.
    pub fn held_results(&self) -> impl Iterator<Item = u64> + '_ {
        self.results.iter().map(|r| r.id)
    }

    /// Every post still remembered, newest first.
    pub fn log(&self) -> impl Iterator<Item = &LogEntry> {
        self.log.iter().rev()
    }

    /// Drops log entries older than `t`, e.g. to keep only today's.
    pub fn forget_log_before(&mut self, t: Instant) {
        self.log.retain(|e| e.at >= t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn nothing_shows_on_an_empty_board() {
        let t0 = Instant::now();
        let b = NewsBoard::new();
        assert_eq!(b.current(t0), None);
        assert_eq!(b.current(t0 + secs(60)), None);
        assert_eq!(b.log().count(), 0);
    }

    #[test]
    fn a_short_job_never_shows_work() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.start_job(1, t0);
        assert_eq!(b.current(t0), None);
        assert_eq!(b.current(t0 + ms(299)), None);
        b.end_job(1);
        assert_eq!(b.current(t0 + ms(300)), None);
        assert_eq!(b.current(t0 + secs(10)), None);
        assert_eq!(b.log().count(), 0, "jobs are not logged");
    }

    #[test]
    fn a_long_job_shows_work_after_the_delay() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.start_job(7, t0);
        assert_eq!(b.current(t0 + ms(399)), None);
        assert_eq!(
            b.current(t0 + ms(400)),
            Some(Shown {
                id: 7,
                stage: Stage::Work,
                extra: 0
            })
        );
    }

    #[test]
    fn a_result_holds_three_and_a_half_seconds() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Done, false, t0);
        let shown = Some(Shown {
            id: 1,
            stage: Stage::Result,
            extra: 0,
        });
        assert_eq!(b.current(t0), shown);
        assert_eq!(b.current(t0 + ms(3499)), shown);
        assert_eq!(b.current(t0 + ms(3500)), None);
    }

    #[test]
    fn hover_holds_a_result() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Info, false, t0);
        b.set_hover(true, t0 + secs(3));
        b.set_hover(false, t0 + secs(9));
        assert!(b.current(t0 + secs(5)).is_some());
        assert!(b.current(t0 + secs(9)).is_some());
        assert!(b.current(t0 + ms(12_499)).is_some());
        assert_eq!(b.current(t0 + ms(12_500)), None);
    }

    #[test]
    fn hover_after_the_hold_does_not_bring_a_result_back() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Done, false, t0);
        b.set_hover(true, t0 + secs(4));
        b.set_hover(false, t0 + secs(5));
        assert_eq!(b.current(t0 + secs(4)), None);
        assert_eq!(b.current(t0 + secs(5)), None);
    }

    #[test]
    fn an_error_waits_to_be_seen() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        // Even a post that asks not to be sticky: errors always are.
        b.post(1, Tone::Error, false, t0);
        assert_eq!(
            b.current(t0 + secs(60)).map(|s| s.stage),
            Some(Stage::Result)
        );
        b.set_hover(true, t0 + secs(60));
        b.set_hover(false, t0 + secs(61));
        assert!(b.current(t0 + ms(64_499)).is_some());
        assert_eq!(b.current(t0 + ms(64_500)), None);
    }

    #[test]
    fn a_sticky_result_waits_to_be_seen() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Info, true, t0);
        assert!(b.current(t0 + secs(600)).is_some());
        b.set_hover(true, t0 + secs(600));
        assert!(b.current(t0 + secs(700)).is_some(), "held while hovered");
        b.set_hover(false, t0 + secs(700));
        assert_eq!(b.current(t0 + ms(703_500)), None);
    }

    #[test]
    fn a_result_posted_under_the_pointer_is_held() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.set_hover(true, t0);
        b.post(1, Tone::Error, false, t0 + secs(1));
        b.set_hover(false, t0 + secs(2));
        assert!(b.current(t0 + ms(5499)).is_some());
        assert_eq!(b.current(t0 + ms(5500)), None);
    }

    #[test]
    fn hovering_a_newer_result_does_not_dismiss_an_older_error() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Error, false, t0);
        b.post(2, Tone::Done, false, t0 + secs(1));
        // B shows, A is only its "+1".
        assert_eq!(
            b.current(t0 + secs(1)),
            Some(Shown {
                id: 2,
                stage: Stage::Result,
                extra: 1
            })
        );
        b.set_hover(true, t0 + secs(2));
        b.set_hover(false, t0 + secs(3));
        // B goes 3.5 s after the pointer left; A, never shown, stays.
        assert_eq!(b.current(t0 + ms(6499)).map(|s| s.id), Some(2));
        assert_eq!(
            b.current(t0 + ms(6500)),
            Some(Shown {
                id: 1,
                stage: Stage::Result,
                extra: 0
            })
        );
        assert_eq!(b.current(t0 + secs(600)).map(|s| s.id), Some(1));
        // Hovered now it is shown, it goes like any seen result.
        b.set_hover(true, t0 + secs(600));
        b.set_hover(false, t0 + secs(601));
        assert!(b.current(t0 + ms(604_499)).is_some());
        assert_eq!(b.current(t0 + ms(604_500)), None);
    }

    #[test]
    fn hover_holds_the_results_behind_the_shown_one() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Done, false, t0);
        b.post(2, Tone::Done, false, t0 + secs(1));
        b.set_hover(true, t0 + secs(2));
        // Result 1's own hold would have ended at 3.5 s.
        assert_eq!(b.current(t0 + secs(10)).map(|s| s.extra), Some(1));
        b.set_hover(false, t0 + secs(10));
        assert_eq!(b.current(t0 + ms(13_499)).map(|s| s.extra), Some(1));
        assert_eq!(b.current(t0 + ms(13_500)), None);
    }

    #[test]
    fn newest_result_shows_and_others_count() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Done, false, t0);
        b.post(2, Tone::Done, false, t0 + secs(1));
        assert_eq!(
            b.current(t0 + secs(1)),
            Some(Shown {
                id: 2,
                stage: Stage::Result,
                extra: 1
            })
        );
        assert_eq!(
            b.current(t0 + ms(3600)),
            Some(Shown {
                id: 2,
                stage: Stage::Result,
                extra: 0
            })
        );
    }

    #[test]
    fn a_result_during_a_job_shows_then_work_returns() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.start_job(1, t0);
        b.post(2, Tone::Done, false, t0 + secs(1));
        assert_eq!(
            b.current(t0 + secs(1)),
            Some(Shown {
                id: 2,
                stage: Stage::Result,
                extra: 1
            })
        );
        assert_eq!(
            b.current(t0 + ms(4500)),
            Some(Shown {
                id: 1,
                stage: Stage::Work,
                extra: 0
            })
        );
    }

    #[test]
    fn the_newest_long_job_shows_and_the_rest_count() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.start_job(1, t0);
        b.start_job(2, t0 + secs(1));
        b.start_job(3, t0 + ms(1900));
        // Job 3 is still inside the delay: 2 shows, 1 counts.
        assert_eq!(
            b.current(t0 + secs(2)),
            Some(Shown {
                id: 2,
                stage: Stage::Work,
                extra: 1
            })
        );
        assert_eq!(
            b.current(t0 + secs(3)),
            Some(Shown {
                id: 3,
                stage: Stage::Work,
                extra: 2
            })
        );
        b.end_job(3);
        b.end_job(2);
        assert_eq!(b.current(t0 + secs(3)).map(|s| s.id), Some(1));
    }

    #[test]
    fn a_sticky_result_is_held_however_much_is_posted_after_it() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Error, false, t0);
        for id in 2..(LOG_MAX as u64 + 10) {
            b.post(id, Tone::Done, false, t0 + secs(id));
        }
        assert!(b.log().all(|e| e.id != 1), "the log forgot it");
        assert!(b.held_results().any(|id| id == 1));
        // Once the others are gone it shows again.
        let later = t0 + secs(LOG_MAX as u64 + 20);
        assert_eq!(b.current(later).map(|s| s.id), Some(1));
    }

    #[test]
    fn posting_again_for_a_result_replaces_it() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Busy, false, t0);
        b.post(1, Tone::Done, false, t0 + secs(1));
        assert_eq!(
            b.current(t0 + secs(2)),
            Some(Shown {
                id: 1,
                stage: Stage::Result,
                extra: 0
            })
        );
        assert_eq!(b.log().count(), 2);
    }

    #[test]
    fn the_log_lists_newest_first() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        b.post(1, Tone::Done, false, t0);
        b.post(2, Tone::Error, false, t0 + secs(1));
        let ids: Vec<u64> = b.log().map(|e| e.id).collect();
        assert_eq!(ids, [2, 1]);
        let first = b.log().next().unwrap();
        assert_eq!(first.tone, Tone::Error);
        assert_eq!(first.at, t0 + secs(1));
    }

    #[test]
    fn the_log_keeps_the_newest_fifty() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        for i in 0..(LOG_MAX as u64 + 10) {
            b.post(i, Tone::Done, false, t0 + secs(i));
        }
        assert_eq!(b.log().count(), LOG_MAX);
        assert_eq!(b.log().next().map(|e| e.id), Some(LOG_MAX as u64 + 9));
        assert_eq!(b.log().last().map(|e| e.id), Some(10));
    }

    #[test]
    fn forgetting_drops_older_log_entries() {
        let t0 = Instant::now();
        let mut b = NewsBoard::new();
        for i in 0..5 {
            b.post(i, Tone::Done, false, t0 + secs(i));
        }
        b.forget_log_before(t0 + secs(3));
        let ids: Vec<u64> = b.log().map(|e| e.id).collect();
        assert_eq!(ids, [4, 3]);
    }
}
