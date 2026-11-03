//! The virtual island's news: the notices and jobs the Pocket took, on a
//! [`NewsBoard`] that decides which one shows, and the view of it the
//! window draws.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use vento_core::actions::ActionId;
use vento_core::i18n::{fill, tr};
use vento_core::news::{NewsBoard, Stage, Tone};

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

/// The notices and jobs the island took, and which one it shows.
#[derive(Default)]
pub(super) struct News {
    board: NewsBoard,
    /// The posted notices the board's log still remembers, by post id.
    notices: HashMap<u64, Notice>,
    /// The running jobs, by job id.
    jobs: HashMap<u64, JobNotice>,
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
        self.jobs.insert(job.id, job);
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
                .map(|job| work_view(job, shown.extra, pressed)),
            Stage::Result => self
                .notices
                .get(&shown.id)
                .map(|n| result_view(n, shown.extra, pressed)),
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
            Stage::Work => self.jobs.get(&id)?.cancel.clone(),
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
}

/// A job at work: its text, the item it is on, and its count.
fn work_view(job: &JobNotice, extra: usize, pressed: bool) -> NewsView {
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
        button_text: (job.cancel.is_some() && !pressed).then(|| ButtonLabel::Cancel.text()),
        quiet_button: false,
        extra,
        fraction,
        count_text,
    }
}

/// A result, as its notice says it.
fn result_view(n: &Notice, extra: usize, pressed: bool) -> NewsView {
    NewsView {
        stage: Stage::Result,
        tone: n.tone,
        icon: n.icon,
        text: n.text.clone(),
        sub: n.sub.clone(),
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
