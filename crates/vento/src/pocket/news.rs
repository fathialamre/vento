//! The virtual island's news: the notices and jobs the Pocket took, on a
//! [`NewsBoard`] that decides which one shows, and the view of it the
//! window draws.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use vento_core::actions::ActionId;
use vento_core::i18n::{fill, tr};
use vento_core::news::{NewsBoard, Stage, Tone};

use super::geometry::ACTIVITY_ROWS;
use crate::notice::{ButtonLabel, CopySource, JobNotice, Notice, next_job_id};

/// How long after the island tells of a copy vento made the clipboard
/// watcher's notice of a copy is taken for that same copy, and dropped.
pub(super) const OWN_COPY_GRACE: Duration = Duration::from_millis(1500);

/// What the news pill shows: a job at work or a result.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct NewsView {
    pub stage: Stage,
    /// How a result reads; work reads as [`Tone::Info`].
    pub tone: Tone,
    pub icon: Option<ActionId>,
    pub text: String,
    pub sub: String,
    /// The board's id of what shows, so a new error is told from the same
    /// one shown again.
    pub id: u64,
    /// The one button's label, if it has one.
    pub button_text: Option<String>,
    /// The button is grey (Details, Change) rather than the accent.
    pub quiet_button: bool,
    /// Other things going on, for the "+N" chip.
    pub extra: usize,
    /// How much of the job is done, 0 to 1, while it can say.
    pub fraction: Option<f32>,
    /// The job's count ("25%", "3 of 5"); empty for none.
    pub count_text: String,
}

/// One row of Activity: a running job, or a result posted today.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ActivityRowData {
    /// The job's or the post's id, for its button.
    pub id: u64,
    pub stage: Stage,
    /// How a result reads; a job reads as [`Tone::Info`].
    pub tone: Tone,
    pub icon: Option<ActionId>,
    pub text: String,
    /// How long ago it was posted, or since the job began.
    pub age: String,
    /// Its button: Cancel for a running job, Undo for the newest Undo while
    /// it still undoes the last operation; no other.
    pub button: Option<ButtonLabel>,
}

/// The notices and jobs the island took, and which one it shows.
#[derive(Default)]
pub(super) struct News {
    board: NewsBoard,
    /// The posted notices the board's log still remembers, by post id.
    notices: HashMap<u64, Notice>,
    /// The running jobs, and when each began, by job id.
    jobs: HashMap<u64, (JobNotice, Instant)>,
    /// Notices and jobs whose button has been pressed: it is not offered
    /// again.
    pressed: HashSet<u64>,
    /// What showed at the last tick, for its button.
    shown: Option<(u64, Stage)>,
    /// When the island last told of a copy vento made.
    own_copy: Option<Instant>,
}

impl News {
    /// Posts `n` as a result, under an id of its own. The watcher's notice
    /// of a copy is dropped within [`OWN_COPY_GRACE`] of one of vento's own
    /// copies: it is that copy, seen again.
    pub fn post(&mut self, n: Notice, now: Instant) {
        match n.copy {
            Some(CopySource::Own) => self.own_copy = Some(now),
            Some(CopySource::Watched)
                if self
                    .own_copy
                    .is_some_and(|t| now.saturating_duration_since(t) < OWN_COPY_GRACE) =>
            {
                return;
            }
            _ => {}
        }
        let id = next_job_id();
        self.board.post(id, n.tone, n.sticky, now);
        self.notices.insert(id, n);
        // The log forgets the oldest posts; their notices go with them.
        let logged: HashSet<u64> = self.board.log().map(|e| e.id).collect();
        self.notices.retain(|id, _| logged.contains(id));
        self.pressed
            .retain(|id| logged.contains(id) || self.jobs.contains_key(id));
    }

    /// A job began.
    pub fn start(&mut self, job: JobNotice, now: Instant) {
        self.board.start_job(job.id, now);
        self.jobs.insert(job.id, (job, now));
    }

    /// The job `id` is over (or was never taken).
    pub fn end(&mut self, id: u64) {
        self.board.end_job(id);
        self.jobs.remove(&id);
        self.pressed.remove(&id);
    }

    /// What the island shows at `now`, with the pointer on the pill or not.
    pub fn tick(&mut self, now: Instant, hovered: bool) -> Option<NewsView> {
        self.board.set_hover(hovered, now);
        let shown = self.board.current(now);
        self.shown = shown.map(|s| (s.id, s.stage));
        let shown = shown?;
        let pressed = self.pressed.contains(&shown.id);
        match shown.stage {
            Stage::Work => self
                .jobs
                .get(&shown.id)
                .map(|(job, _)| work_view(shown.id, job, shown.extra, pressed)),
            Stage::Result => self
                .notices
                .get(&shown.id)
                .map(|n| result_view(shown.id, n, shown.extra, pressed)),
        }
    }

    /// Presses the button of what showed at the last tick: what it runs (a
    /// result's button, a job's Cancel), once; afterwards it is gone.
    pub fn press_button(&mut self) -> Option<Rc<dyn Fn()>> {
        let (id, stage) = self.shown?;
        if self.pressed.contains(&id) {
            return None;
        }
        let on = match stage {
            Stage::Work => self.jobs.get(&id)?.0.cancel.clone(),
            Stage::Result => {
                let n = self.notices.get(&id)?;
                n.button.as_ref().map(|b| match &n.island_undo {
                    Some(undo) if b.label == ButtonLabel::Undo => undo.clone(),
                    _ => b.on.clone(),
                })
            }
        }?;
        self.pressed.insert(id);
        Some(on)
    }

    /// Activity at `now`: the running jobs, newest first, then what was
    /// posted since `today` began (none: everything), newest first; at most
    /// [`ACTIVITY_ROWS`]. Posts from before today are forgotten. `last_serial`
    /// is the executor's undo serial now, which the newest Undo must still
    /// name to be offered.
    pub fn activity(
        &mut self,
        now: Instant,
        today: Option<Instant>,
        last_serial: Option<u64>,
    ) -> Vec<ActivityRowData> {
        if let Some(today) = today {
            self.forget_before(today);
        }
        let live_undo = self.live_undo(last_serial);
        let mut jobs: Vec<_> = self.jobs.iter().collect();
        jobs.sort_by_key(|(id, (_, since))| std::cmp::Reverse((*since, **id)));
        let jobs = jobs.into_iter().map(|(&id, (job, since))| ActivityRowData {
            id,
            stage: Stage::Work,
            tone: Tone::Info,
            icon: job.icon,
            text: job.text.clone(),
            age: age_text(now.saturating_duration_since(*since)),
            button: (job.cancel.is_some() && !self.pressed.contains(&id))
                .then_some(ButtonLabel::Cancel),
        });
        let posts = self.board.log().filter_map(|e| {
            let n = self.notices.get(&e.id)?;
            Some(ActivityRowData {
                id: e.id,
                stage: Stage::Result,
                tone: n.tone,
                icon: n.icon,
                text: n.text.clone(),
                age: age_text(now.saturating_duration_since(e.at)),
                button: (live_undo == Some(e.id)).then_some(ButtonLabel::Undo),
            })
        });
        jobs.chain(posts).take(ACTIVITY_ROWS).collect()
    }

    /// Presses the button of Activity's row `id` (see
    /// [`activity`](Self::activity)): what it runs, once; afterwards it is
    /// gone, from the pill too.
    pub fn press_row(&mut self, id: u64, last_serial: Option<u64>) -> Option<Rc<dyn Fn()>> {
        if self.pressed.contains(&id) {
            return None;
        }
        let on = match self.jobs.get(&id) {
            Some((job, _)) => job.cancel.clone(),
            None if self.live_undo(last_serial) == Some(id) => {
                self.notices.get(&id)?.island_undo.clone()
            }
            None => None,
        }?;
        self.pressed.insert(id);
        Some(on)
    }

    /// The post whose Undo Activity offers: the newest that undoes an
    /// operation, while that operation is still the last (`last_serial`)
    /// and its Undo has not been pressed.
    fn live_undo(&self, last_serial: Option<u64>) -> Option<u64> {
        let (id, n) = self.board.log().find_map(|e| {
            let n = self.notices.get(&e.id)?;
            (n.undo_serial.is_some() && n.island_undo.is_some()).then_some((e.id, n))
        })?;
        (n.undo_serial == last_serial && !self.pressed.contains(&id)).then_some(id)
    }

    /// Forgets the posts from before `t`, and their notices.
    fn forget_before(&mut self, t: Instant) {
        self.board.forget_log_before(t);
        let logged: HashSet<u64> = self.board.log().map(|e| e.id).collect();
        self.notices.retain(|id, _| logged.contains(id));
        self.pressed
            .retain(|id| logged.contains(id) || self.jobs.contains_key(id));
    }
}

/// `n` as the island tells it: an error with no button of its own gets
/// Details, which runs `open` (opens Activity, where the error is listed).
pub(super) fn with_details(n: Notice, open: impl Fn() + 'static) -> Notice {
    if n.tone == Tone::Error && n.button.is_none() {
        n.with_button(ButtonLabel::Details, open)
    } else {
        n
    }
}

/// How long ago, as Activity says it: "now" under a minute, then whole
/// minutes ("2 min"), then whole hours ("1 h").
fn age_text(ago: Duration) -> String {
    let minutes = ago.as_secs() / 60;
    match minutes {
        0 => tr("now"),
        1..60 => fill(&tr("{m} min"), &[("m", &minutes.to_string())]),
        _ => fill(&tr("{h} h"), &[("h", &(minutes / 60).to_string())]),
    }
}

/// When today began in `tz` (local midnight), as an [`Instant`] on the
/// clock `now` is read from, `now_ts` being the same moment; `None` when
/// that clock does not reach back so far.
pub(super) fn today_start(now: Instant, now_ts: Timestamp, tz: &TimeZone) -> Option<Instant> {
    let midnight = now_ts.to_zoned(tz.clone()).start_of_day().ok()?;
    let since = Duration::try_from(now_ts.duration_since(midnight.timestamp())).ok()?;
    now.checked_sub(since)
}

/// A job at work, `id` on the board: its text, the item it is on, and its
/// count.
fn work_view(id: u64, job: &JobNotice, extra: usize, pressed: bool) -> NewsView {
    let (fraction, count_text, sub) = match &job.progress {
        Some(progress) => {
            let s = progress.snapshot();
            let (fraction, count) = progress_count(job.icon, s.done, s.total);
            (fraction, count, s.current)
        }
        None => (None, String::new(), String::new()),
    };
    NewsView {
        stage: Stage::Work,
        tone: Tone::Info,
        icon: job.icon,
        text: job.text.clone(),
        sub,
        id,
        button_text: (job.cancel.is_some() && !pressed).then(|| ButtonLabel::Cancel.text()),
        quiet_button: false,
        extra,
        fraction,
        count_text,
    }
}

/// A result, `id` on the board, as its notice says it.
fn result_view(id: u64, n: &Notice, extra: usize, pressed: bool) -> NewsView {
    NewsView {
        stage: Stage::Result,
        tone: n.tone,
        icon: n.icon,
        text: n.text.clone(),
        sub: n.sub.clone(),
        id,
        button_text: n
            .button
            .as_ref()
            .filter(|_| !pressed)
            .map(|b| b.label.text()),
        quiet_button: n.button.as_ref().is_some_and(|b| b.label.quiet()),
        extra,
        fraction: None,
        count_text: String::new(),
    }
}

/// How far a job of `action` has got, `done` of `total`: the share done and
/// the count. Convert counts pictures, the picture under way first ("3 of
/// 5"); the others (and a job of no action) count bytes, as a percent.
/// Nothing while the total is unknown.
fn progress_count(action: Option<ActionId>, done: u64, total: u64) -> (Option<f32>, String) {
    if total == 0 {
        return (None, String::new());
    }
    let fraction = (done as f64 / total as f64).min(1.0) as f32;
    let count = match action {
        Some(ActionId::Convert) => fill(
            &tr("{current} of {total}"),
            &[
                ("current", &(done + 1).min(total).to_string()),
                ("total", &total.to_string()),
            ],
        ),
        _ => fill(
            &tr("{}%"),
            &[("", &(done.min(total) * 100 / total).to_string())],
        ),
    };
    (Some(fraction), count)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::time::Duration;

    use vento_core::archive::Progress;
    use vento_core::news::{RESULT_HOLD, WORK_DELAY};

    use super::*;

    fn job(icon: ActionId, progress: Option<Arc<Progress>>) -> JobNotice {
        JobNotice {
            id: next_job_id(),
            icon: Some(icon),
            text: "Working…".into(),
            progress,
            cancel: None,
        }
    }

    #[test]
    fn archive_progress_reads_as_a_percent() {
        let (t0, mut news) = (Instant::now(), News::default());
        let progress = Progress::new();
        progress.start(200);
        progress.add_done(50);
        news.start(job(ActionId::Archive, Some(progress)), t0);
        let view = news.tick(t0 + WORK_DELAY, false).expect("work shows");
        assert_eq!(view.stage, Stage::Work);
        assert_eq!(view.icon, Some(ActionId::Archive));
        assert_eq!(view.fraction, Some(0.25));
        assert_eq!(view.count_text, "25%");
    }

    #[test]
    fn convert_progress_reads_as_i_of_n() {
        let (t0, mut news) = (Instant::now(), News::default());
        let progress = Progress::new();
        progress.start(5);
        progress.add_done(2);
        news.start(job(ActionId::Convert, Some(progress)), t0);
        let view = news.tick(t0 + WORK_DELAY, false).expect("work shows");
        assert_eq!(view.count_text, "3 of 5");
        assert_eq!(view.fraction, Some(0.4));
    }

    #[test]
    fn a_move_shows_no_count() {
        let (t0, mut news) = (Instant::now(), News::default());
        news.start(job(ActionId::Move, None), t0);
        let view = news.tick(t0 + WORK_DELAY, false).expect("work shows");
        assert_eq!((view.fraction, view.count_text.as_str()), (None, ""));
        assert_eq!(view.text, "Working…");
        assert_eq!(view.button_text, None);
    }

    #[test]
    fn a_finished_job_leaves_the_view() {
        let (t0, mut news) = (Instant::now(), News::default());
        let j = job(ActionId::Move, None);
        let id = j.id;
        news.start(j, t0);
        assert!(news.tick(t0 + WORK_DELAY, false).is_some());
        news.end(id);
        assert_eq!(
            news.tick(t0 + WORK_DELAY + Duration::from_millis(100), false),
            None
        );
    }

    #[test]
    fn the_shown_notice_carries_its_button() {
        let (t0, mut news) = (Instant::now(), News::default());
        let ran = Rc::new(Cell::new(0));
        let counter = ran.clone();
        let n = Notice::done("Moved 3 items to Documents")
            .with_icon(ActionId::Move)
            .with_undo(Some(Box::new(move || counter.set(counter.get() + 1))));
        news.post(n, t0);
        let view = news.tick(t0, false).expect("the result shows");
        assert_eq!(view.stage, Stage::Result);
        assert_eq!(view.tone, Tone::Done);
        assert_eq!(view.text, "Moved 3 items to Documents");
        assert_eq!(view.button_text.as_deref(), Some("Undo"));
        (news.press_button().expect("a button"))();
        assert_eq!(ran.get(), 1);
        // Pressed once, it is gone.
        assert!(news.press_button().is_none());
        assert_eq!(news.tick(t0, false).expect("still up").button_text, None);
        assert_eq!(news.tick(t0 + RESULT_HOLD, false), None);
    }

    #[test]
    fn the_island_undoes_with_its_own_undo() {
        let (t0, mut news) = (Instant::now(), News::default());
        let (toast, island) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let (t, i) = (toast.clone(), island.clone());
        let n = Notice::done("Moved")
            .with_undo(Some(Box::new(move || t.set(t.get() + 1))))
            .with_island_undo(move || i.set(i.get() + 1));
        news.post(n, t0);
        news.tick(t0, false).expect("the result shows");
        (news.press_button().expect("Undo"))();
        assert_eq!((toast.get(), island.get()), (0, 1));
    }

    #[test]
    fn a_cancellable_job_offers_cancel() {
        let (t0, mut news) = (Instant::now(), News::default());
        let progress = Progress::new();
        let cancel = Arc::clone(&progress);
        let mut j = job(ActionId::Archive, Some(Arc::clone(&progress)));
        j.cancel = Some(Rc::new(move || cancel.cancel()));
        news.start(j, t0);
        let view = news.tick(t0 + WORK_DELAY, false).expect("work shows");
        assert_eq!(view.button_text.as_deref(), Some("Cancel"));
        (news.press_button().expect("Cancel"))();
        assert!(progress.is_cancelled());
    }

    #[test]
    fn sticky_results_wait_to_be_seen() {
        let (t0, mut news) = (Instant::now(), News::default());
        let n = Notice::done("Update ready")
            .with_button(ButtonLabel::Restart, || {})
            .sticky();
        news.post(n, t0);
        let later = t0 + RESULT_HOLD * 20;
        let view = news.tick(later, false).expect("still up, not seen yet");
        assert_eq!(view.text, "Update ready");
        assert_eq!(view.button_text.as_deref(), Some("Restart"));
        // Seen once, it goes the usual hold after the pointer leaves.
        assert!(news.tick(later, true).is_some());
        assert!(news.tick(later + RESULT_HOLD, false).is_some());
        assert_eq!(news.tick(later + RESULT_HOLD * 2, false), None);
    }

    #[test]
    fn an_own_copy_hides_the_watchers_echo() {
        let (t0, mut news) = (Instant::now(), News::default());
        let watched = || {
            Notice::done("Copied")
                .with_sub("report.pdf")
                .with_copy(CopySource::Watched)
        };
        news.post(
            Notice::done("Copied")
                .with_sub("1 file ready to paste")
                .with_copy(CopySource::Own),
            t0,
        );
        // The watcher sees the same copy a moment later: it is not told
        // twice.
        news.post(watched(), t0 + Duration::from_millis(200));
        let view = news
            .tick(t0 + Duration::from_millis(200), false)
            .expect("up");
        assert_eq!(
            (view.sub.as_str(), view.extra),
            ("1 file ready to paste", 0)
        );
        // A copy made later in another app is news again.
        news.post(watched(), t0 + OWN_COPY_GRACE);
        let view = news.tick(t0 + OWN_COPY_GRACE, false).expect("up");
        assert_eq!(view.sub, "report.pdf");
    }

    #[test]
    fn the_watcher_alone_is_told() {
        let (t0, mut news) = (Instant::now(), News::default());
        let n = Notice::done("Copied").with_copy(CopySource::Watched);
        news.post(n, t0);
        assert_eq!(news.tick(t0, false).expect("up").text, "Copied");
    }

    #[test]
    fn details_and_change_are_quiet_buttons() {
        let (t0, mut news) = (Instant::now(), News::default());
        news.post(Notice::error("Couldn't move"), t0);
        let quiet = |news: &mut News, label| {
            news.post(Notice::info("x").with_button(label, || {}), t0);
            news.tick(t0, false).expect("up").quiet_button
        };
        assert!(quiet(&mut news, ButtonLabel::Details));
        assert!(quiet(&mut news, ButtonLabel::Change));
        assert!(!quiet(&mut news, ButtonLabel::Undo));
        assert!(!quiet(&mut news, ButtonLabel::Restart));
    }

    #[test]
    fn a_job_without_an_action_counts_a_percent() {
        let (t0, mut news) = (Instant::now(), News::default());
        let progress = Progress::new();
        progress.start(1000);
        progress.add_done(420);
        let mut j = job(ActionId::Move, Some(progress));
        j.icon = None;
        news.start(j, t0);
        let view = news.tick(t0 + WORK_DELAY, false).expect("work shows");
        assert_eq!(view.icon, None);
        assert_eq!(view.count_text, "42%");
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn activity_lists_today_newest_first() {
        let (t0, mut news) = (Instant::now(), News::default());
        news.post(Notice::done("Yesterday's"), t0);
        news.post(
            Notice::done("Moved 3 items").with_icon(ActionId::Move),
            t0 + secs(10),
        );
        news.post(Notice::error("Couldn't copy"), t0 + secs(3600));
        let mut j = job(ActionId::Archive, None);
        j.text = "Archiving…".into();
        j.cancel = Some(Rc::new(|| {}));
        news.start(j, t0 + secs(3690));
        news.post(
            Notice::info("Awake for 1 h").with_button(ButtonLabel::AddHour, || {}),
            t0 + secs(3695),
        );
        // Today began 5 s after the first post.
        let rows = news.activity(t0 + secs(3700), Some(t0 + secs(5)), None);
        let seen: Vec<_> = rows
            .iter()
            .map(|r| (r.stage, r.text.as_str(), r.age.as_str(), r.button))
            .collect();
        assert_eq!(
            seen,
            [
                // The running job on top, with its Cancel.
                (Stage::Work, "Archiving…", "now", Some(ButtonLabel::Cancel)),
                // A "+1 h" is not offered here.
                (Stage::Result, "Awake for 1 h", "now", None),
                (Stage::Result, "Couldn't copy", "1 min", None),
                (Stage::Result, "Moved 3 items", "1 h", None),
            ]
        );
        assert_eq!(rows[2].tone, Tone::Error);
        assert_eq!(rows[3].icon, Some(ActionId::Move));
        // Yesterday's is forgotten for good.
        assert_eq!(news.board.log().count(), 3);
        assert_eq!(news.notices.len(), 3);
    }

    #[test]
    fn activity_lists_six_at_most() {
        let (t0, mut news) = (Instant::now(), News::default());
        for i in 0..9 {
            news.post(Notice::done(format!("{i}")), t0 + secs(i));
        }
        let rows = news.activity(t0 + secs(9), None, None);
        let texts: Vec<_> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["8", "7", "6", "5", "4", "3"]);
    }

    #[test]
    fn only_the_newest_undo_is_live() {
        let (t0, mut news) = (Instant::now(), News::default());
        let undone = Rc::new(Cell::new(0));
        let undo = |serial: u64| {
            let undone = undone.clone();
            Notice::done(format!("Moved {serial}"))
                .with_undo(Some(Box::new(|| {})))
                .with_undo_serial(serial)
                .with_island_undo(move || undone.set(serial))
        };
        news.post(undo(1), t0);
        news.post(undo(2), t0 + secs(1));
        news.post(Notice::done("Copied"), t0 + secs(2));
        let buttons = |news: &mut News, last: Option<u64>| -> Vec<Option<ButtonLabel>> {
            news.activity(t0 + secs(3), None, last)
                .iter()
                .map(|r| r.button)
                .collect()
        };
        assert_eq!(
            buttons(&mut news, Some(2)),
            [None, Some(ButtonLabel::Undo), None]
        );
        // Undone, or overtaken by an operation with no notice here: none.
        assert_eq!(buttons(&mut news, None), [None, None, None]);
        assert_eq!(buttons(&mut news, Some(3)), [None, None, None]);
        // An older one never comes back.
        assert_eq!(buttons(&mut news, Some(1)), [None, None, None]);
        // Pressed, it undoes its own operation, once.
        let id = news.activity(t0 + secs(3), None, Some(2))[1].id;
        assert!(news.press_row(id, Some(1)).is_none(), "no longer the last");
        (news.press_row(id, Some(2)).expect("Undo"))();
        assert_eq!(undone.get(), 2);
        assert!(news.press_row(id, Some(2)).is_none());
        assert_eq!(buttons(&mut news, Some(2)), [None, None, None]);
    }

    #[test]
    fn a_rows_cancel_cancels_its_job() {
        let (t0, mut news) = (Instant::now(), News::default());
        let progress = Progress::new();
        let cancel = Arc::clone(&progress);
        let mut j = job(ActionId::Convert, Some(Arc::clone(&progress)));
        j.cancel = Some(Rc::new(move || cancel.cancel()));
        let id = j.id;
        news.start(j, t0);
        (news.press_row(id, None).expect("Cancel"))();
        assert!(progress.is_cancelled());
        // Pressed once, the pill offers it no more either.
        let view = news.tick(t0 + WORK_DELAY, false).expect("work shows");
        assert_eq!(view.button_text, None);
        assert_eq!(news.activity(t0 + WORK_DELAY, None, None)[0].button, None);
    }

    #[test]
    fn an_error_offers_details() {
        let (t0, mut news) = (Instant::now(), News::default());
        let opened = Rc::new(Cell::new(0));
        let open = || {
            let opened = opened.clone();
            move || opened.set(opened.get() + 1)
        };
        news.post(with_details(Notice::error("Couldn't move"), open()), t0);
        let view = news.tick(t0, false).expect("the error shows");
        assert_eq!(view.button_text.as_deref(), Some("Details"));
        assert!(view.quiet_button);
        (news.press_button().expect("Details"))();
        assert_eq!(opened.get(), 1);
        // A notice with a button of its own, or no error, keeps what it has.
        let n = with_details(
            Notice::error("Update failed").with_button(ButtonLabel::GetIt, || {}),
            open(),
        );
        assert_eq!(n.button.map(|b| b.label), Some(ButtonLabel::GetIt));
        assert!(with_details(Notice::done("Moved"), open()).button.is_none());
    }

    #[test]
    fn ages_read_now_minutes_and_hours() {
        assert_eq!(age_text(secs(59)), "now");
        assert_eq!(age_text(secs(60)), "1 min");
        assert_eq!(age_text(secs(59 * 60 + 59)), "59 min");
        assert_eq!(age_text(secs(3600)), "1 h");
        assert_eq!(age_text(secs(5 * 3600 + 1800)), "5 h");
    }

    #[test]
    fn today_starts_at_local_midnight() {
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        let now = Instant::now() + secs(3 * 3600);
        let start = today_start(now, at("2026-11-03T02:30:00Z"), &TimeZone::UTC);
        assert_eq!(start, now.checked_sub(secs(2 * 3600 + 1800)));
        // In Tripoli (UTC+2) it is 04:30 then.
        let tripoli = TimeZone::fixed(jiff::tz::offset(2));
        let start = today_start(now, at("2026-11-03T02:30:00Z"), &tripoli);
        assert_eq!(start, now.checked_sub(secs(4 * 3600 + 1800)));
    }

    #[test]
    fn a_result_shows_over_work_and_counts_it() {
        let (t0, mut news) = (Instant::now(), News::default());
        news.start(job(ActionId::Move, None), t0);
        news.post(Notice::error("Couldn't copy: disk full"), t0 + WORK_DELAY);
        let view = news.tick(t0 + WORK_DELAY, false).expect("the error shows");
        assert_eq!(
            (view.stage, view.tone, view.extra),
            (Stage::Result, Tone::Error, 1)
        );
    }
}
