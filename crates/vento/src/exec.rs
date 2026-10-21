//! Runs the action a file was dropped on and reports the result in a toast.

use std::cell::{Cell, RefCell};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use log::{debug, info, warn};
use slint::{Timer, TimerMode};
use vento_core::actions::ActionId;
use vento_core::archive::{
    ArchiveError, BatchOutcome, Made, PasswordAsk, Plan, Progress, Zeroizing, compress,
    extract_batch, plan,
};
use vento_core::destinations::{RecentDestinations, recent_path};
use vento_core::fileops::{
    CopyOutcome, CopyRecord, CopyUndoOutcome, FileOpError, MoveOutcome, MoveRecord, copy_items,
    move_items, undo, undo_copies,
};
use vento_core::geometry::Point;
use vento_core::paths::format_paths;
use vento_core::settings::Settings;
use vento_platform::{Platform, quarantine_of, set_quarantine};

use crate::archive_ui::{PasswordView, ProgressView};
use crate::toast::Toaster;
use crate::wheel::WheelDrop;

/// `"1 copy"` / `"3 copies"`.
fn plural_copies(n: usize) -> String {
    format!("{n} {}", if n == 1 { "copy" } else { "copies" })
}

/// `"1 item"` / `"3 items"`.
fn items(n: usize) -> String {
    format!("{n} {}", if n == 1 { "item" } else { "items" })
}

/// ` · {k} failed` and ` · {j} original(s) couldn't be removed`, or nothing.
///
/// A `SourceNotRemoved` item is in both `record.moved` and `failed`; it is
/// counted as moved only, and here only as an original that is still there.
fn failure_suffix(outcome: &MoveOutcome) -> String {
    let leftovers = outcome
        .failed
        .iter()
        .filter(|(_, e)| matches!(e, FileOpError::SourceNotRemoved { .. }))
        .count();
    let failed = outcome.failed.len() - leftovers;
    let mut suffix = String::new();
    if failed > 0 {
        suffix.push_str(&format!(" · {failed} failed"));
    }
    if leftovers > 0 {
        let noun = if leftovers == 1 {
            "original"
        } else {
            "originals"
        };
        suffix.push_str(&format!(" · {leftovers} {noun} couldn't be removed"));
    }
    suffix
}

/// `"1 archive"` / `"3 archives"`.
fn archives(n: usize) -> String {
    format!("{n} {}", if n == 1 { "archive" } else { "archives" })
}

/// The item's own name, or the whole path for one without a name (`/`).
fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The name of the only one of `paths`, or `count` of them when there are
/// several: `Photos.zip` / `2 archives`.
fn name_or_count(paths: &[&Path], count: fn(usize) -> String) -> String {
    match paths {
        [only] => display_name(only),
        _ => count(paths.len()),
    }
}

/// The parts of a toast joined with `; `, the first letter capitalised.
fn sentence(parts: &[String]) -> String {
    let text = parts.join("; ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

/// Toast text for a finished move.
fn move_toast_text(outcome: &MoveOutcome) -> String {
    format!(
        "Moved {} to {}{}",
        items(outcome.record.moved.len()),
        display_name(&outcome.record.dest),
        failure_suffix(outcome)
    )
}

/// Toast text for a finished Copy to.
fn copy_toast_text(outcome: &CopyOutcome) -> String {
    let mut text = format!(
        "Copied {} to {}",
        items(outcome.record.copies.len()),
        display_name(&outcome.record.dest)
    );
    if !outcome.failed.is_empty() {
        text.push_str(&format!(" · {} failed", outcome.failed.len()));
    }
    text
}

/// Toast text for a finished undo of a Copy to: what went to the Trash, what
/// was left because it had changed, what was no longer there (renamed or
/// deleted since), and what the Trash refused.
fn copy_undo_toast_text(outcome: &CopyUndoOutcome) -> String {
    if outcome.trashed == 0 && outcome.kept_changed.is_empty() && outcome.failed.is_empty() {
        return "Nothing left to remove".into();
    }
    let mut text = format!("Removed {}", plural_copies(outcome.trashed));
    if !outcome.kept_changed.is_empty() {
        text.push_str(&format!(", kept {} changed", outcome.kept_changed.len()));
    }
    if outcome.gone > 0 {
        text.push_str(&format!(", {} no longer there", outcome.gone));
    }
    if !outcome.failed.is_empty() {
        text.push_str(&format!(" · {} failed", outcome.failed.len()));
    }
    text
}

/// Toast for a job the user cancelled.
const CANCELLED_TEXT: &str = "Cancelled";

/// What a finished Archive job did, as its toast tells it.
#[derive(Debug)]
enum ArchiveResult<'a> {
    /// `count` items were zipped into the file called `output`.
    Compressed { count: usize, output: String },
    /// A batch of archives was extracted.
    Extracted(&'a BatchOutcome),
}

/// Toast text for a finished Archive job.
fn archive_toast_text(result: &ArchiveResult<'_>) -> String {
    match result {
        ArchiveResult::Compressed { count, output } => {
            format!("Compressed {} to {output}", items(*count))
        }
        ArchiveResult::Extracted(outcome) => extract_toast_text(outcome),
    }
}

/// Toast text for a finished extraction. Nothing extracted and nothing
/// failed means the user cancelled: the job, or every password prompt.
/// Failures give the first reason; a cancelled batch says how far it got
/// without a total, since the archives after it were never tried.
fn extract_toast_text(outcome: &BatchOutcome) -> String {
    let made = outcome.made.len();
    let failed = outcome.failed.len();
    let skipped = outcome.skipped.len();
    if made == 0 && failed == 0 {
        return CANCELLED_TEXT.into();
    }
    let reason = outcome
        .failed
        .first()
        .map(|(_, error)| error.to_string())
        .unwrap_or_default();
    let mut parts = Vec::new();
    if made == 0 {
        let names: Vec<&Path> = outcome.failed.iter().map(|(a, _)| a.as_path()).collect();
        parts.push(format!(
            "couldn't extract {}: {reason}",
            name_or_count(&names, archives)
        ));
    } else {
        if outcome.cancelled || failed + skipped == 0 {
            let names: Vec<&Path> = outcome.made.iter().map(|m| m.source.as_path()).collect();
            parts.push(format!("extracted {}", name_or_count(&names, archives)));
        } else {
            parts.push(format!("extracted {made} of {}", made + failed + skipped));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed: {reason}"));
        }
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    if outcome.cancelled {
        parts.push("cancelled".into());
    }
    sentence(&parts)
}

/// Toast text for a finished undo of an Archive job; `trashed` are the
/// outputs that went to the Trash.
fn archive_undo_toast_text(outcome: &CopyUndoOutcome, trashed: &[PathBuf]) -> String {
    if trashed.is_empty() && outcome.kept_changed.is_empty() && outcome.failed.is_empty() {
        return "Nothing left to remove".into();
    }
    let mut parts = Vec::new();
    if !trashed.is_empty() {
        let names: Vec<&Path> = trashed.iter().map(PathBuf::as_path).collect();
        parts.push(format!(
            "moved {} to the Trash",
            name_or_count(&names, items)
        ));
    }
    if !outcome.kept_changed.is_empty() {
        let names: Vec<&Path> = outcome.kept_changed.iter().map(PathBuf::as_path).collect();
        let since = if names.len() == 1 {
            "it was"
        } else {
            "they were"
        };
        parts.push(format!(
            "kept {}: changed since {since} made",
            name_or_count(&names, items)
        ));
    }
    if outcome.gone > 0 {
        parts.push(format!("{} no longer there", outcome.gone));
    }
    if let Some((_, reason)) = outcome.failed.first() {
        parts.push(format!("{} failed: {reason}", outcome.failed.len()));
    }
    sentence(&parts)
}

/// What a finished move or copy leads to.
#[derive(Debug, PartialEq, Eq)]
enum AfterTransfer {
    /// Nothing went through and something failed: toast this reason; the
    /// destination is not remembered and there is nothing to undo.
    Error(String),
    /// Nothing went through and nothing failed (every item was already
    /// there): toast that; nothing is remembered.
    Nothing,
    /// At least one item went through: remember the destination, keep the
    /// undo and toast the outcome.
    Done,
}

/// Decides what follows a move or copy that handled `done` items, the first
/// failure being `first_error` (if any).
fn after_transfer(done: usize, first_error: Option<String>) -> AfterTransfer {
    if done > 0 {
        AfterTransfer::Done
    } else if let Some(reason) = first_error {
        AfterTransfer::Error(reason)
    } else {
        AfterTransfer::Nothing
    }
}

/// Toast text for a finished undo.
fn undo_toast_text(outcome: &MoveOutcome) -> String {
    format!(
        "Restored {}{}",
        items(outcome.record.moved.len()),
        failure_suffix(outcome)
    )
}

/// Folder a terminal should open in: the first path if it is a folder,
/// otherwise its parent.
fn terminal_dir(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?;
    if first.is_dir() {
        Some(first.clone())
    } else {
        first.parent().map(Path::to_path_buf)
    }
}

/// What the undo slot would undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoKind {
    /// The last move.
    Move,
    /// The last Copy to.
    Copy,
    /// The last Archive (what it compressed or extracted).
    Archive,
}

/// Callback told which undo, if any, is available.
type UndoListener = Box<dyn Fn(Option<UndoKind>)>;

/// Toast shown when a move, copy or undo is requested while one is running.
const BUSY_TEXT: &str = "Busy — still working";

/// How often an open password window checks whether its job was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// The progress window's verbs.
const COMPRESSING: &str = "Compressing";
const EXTRACTING: &str = "Extracting";

/// A password on its way from the password window to the worker.
type Password = Zeroizing<Vec<u8>>;

/// What is left to undo after `outcome`, the undo of `record`: the items that
/// failed to go back, so the undo can be retried. An original that was copied
/// back but could not be removed is left out (retrying would duplicate it).
fn undo_remainder(record: &MoveRecord, outcome: &MoveOutcome) -> Option<MoveRecord> {
    let retry: Vec<(PathBuf, PathBuf)> = record
        .moved
        .iter()
        .filter(|(_, to)| {
            outcome.failed.iter().any(|(path, error)| {
                path == to && !matches!(error, FileOpError::SourceNotRemoved { .. })
            })
        })
        .cloned()
        .collect();
    (!retry.is_empty()).then(|| MoveRecord {
        dest: record.dest.clone(),
        moved: retry,
    })
}

thread_local! {
    /// The executor that file-operation workers report back to; set on the
    /// main thread, which owns the non-`Send` executor.
    static MAIN_EXECUTOR: RefCell<Weak<Executor>> = const { RefCell::new(Weak::new()) };
}

/// Runs `f` with the executor on the main thread. Fails only when the event
/// loop is gone; if the executor is, `f` is dropped without running.
fn on_main(f: impl FnOnce(&Executor) + Send + 'static) -> Result<(), slint::EventLoopError> {
    slint::invoke_from_event_loop(move || {
        if let Some(executor) = MAIN_EXECUTOR.with(|slot| slot.borrow().upgrade()) {
            f(&executor);
        }
    })
}

/// What an Archive job did, handed from the worker to the main thread.
enum ArchiveDone {
    /// `count` items were to be zipped.
    Compressed {
        count: usize,
        result: Result<Made, ArchiveError>,
    },
    Extracted(BatchOutcome),
}

/// The worker's half of Archive: decides between compressing and
/// extracting, has the progress window follow the job, and runs it.
fn archive_job(paths: &[PathBuf], at: Point) -> ArchiveDone {
    let progress = Progress::new();
    match plan(paths) {
        Plan::Compress { dir, name } => {
            show_progress(&progress, COMPRESSING, at);
            ArchiveDone::Compressed {
                count: paths.len(),
                result: compress(paths, &dir, &name, &progress),
            }
        }
        Plan::Extract(archives) => {
            show_progress(&progress, EXTRACTING, at);
            let mark = copy_quarantine();
            let mut ask = |archive: &Path, why| request_password(archive, why, &progress, at);
            ArchiveDone::Extracted(extract_batch(&archives, &progress, &mark, &mut ask))
        }
    }
}

/// Has the main thread's progress window follow `progress`.
fn show_progress(progress: &Arc<Progress>, verb: &'static str, at: Point) {
    let progress = Arc::clone(progress);
    if let Err(e) = on_main(move |executor| executor.progress.track(progress, verb, at)) {
        warn!("cannot show the archive progress: {e}");
    }
}

/// The `mark` of an extraction: gives everything extracted from an archive
/// that archive's quarantine attribute, read once per archive. A failure
/// fails that archive's extraction.
fn copy_quarantine() -> impl Fn(&Path, &Path) -> io::Result<()> {
    let read: RefCell<Option<(PathBuf, Option<Vec<u8>>)>> = RefCell::new(None);
    move |archive: &Path, entry: &Path| {
        let mut read = read.borrow_mut();
        if read.as_ref().is_none_or(|(of, _)| of != archive) {
            *read = Some((archive.to_path_buf(), quarantine_of(archive)));
        }
        match read.as_ref().and_then(|(_, value)| value.as_deref()) {
            Some(value) => set_quarantine(entry, value),
            None => Ok(()),
        }
    }
}

/// The worker's `ask`: has the main thread open the password window and
/// waits for its answer. `None` at once when the job is cancelled or the
/// main thread can't be reached, and `None` when the question is dropped
/// unanswered (its sender goes with it).
fn request_password(
    archive: &Path,
    why: PasswordAsk,
    progress: &Arc<Progress>,
    at: Point,
) -> Option<Password> {
    if progress.is_cancelled() {
        return None;
    }
    let (answer, answered) = mpsc::sync_channel(1);
    let name = display_name(archive);
    let wrong = why == PasswordAsk::Wrong;
    let progress = Arc::clone(progress);
    let asked = on_main(move |executor| {
        executor.prompt_password(&name, wrong, progress, at, answer);
    });
    if let Err(e) = asked {
        warn!("cannot ask for a password: {e}");
        return None;
    }
    answered.recv().ok().flatten()
}

/// What the undo slot holds.
enum LastOp {
    Move(MoveRecord),
    Copy(CopyRecord),
    /// What an Archive job made, undone like a Copy to.
    Archive(CopyRecord),
}

impl LastOp {
    fn kind(&self) -> UndoKind {
        match self {
            LastOp::Move(_) => UndoKind::Move,
            LastOp::Copy(_) => UndoKind::Copy,
            LastOp::Archive(_) => UndoKind::Archive,
        }
    }
}

/// The most recent move or copy, kept so it can be undone.
struct Last {
    op: LastOp,
    /// Where toasts about this operation are anchored.
    at: Point,
}

/// Result of the worker's half of undoing a copy: the check of which copies
/// are still as vento left them, and those to hand to the Trash.
struct CopyUndoChecked {
    /// `trashed` here counts the copies in `to_trash`, not yet trashed.
    outcome: CopyUndoOutcome,
    to_trash: Vec<PathBuf>,
}

/// Performs wheel actions and reports their outcome.
pub struct Executor {
    platform: Rc<dyn Platform>,
    settings: Rc<RefCell<Settings>>,
    toast: Rc<Toaster>,
    last: RefCell<Option<Last>>,
    /// Folders recently moved or copied to; saved after each addition.
    recent: Rc<RefCell<RecentDestinations>>,
    /// Whether a move, copy, archive or undo is running on a worker thread.
    busy: Cell<bool>,
    /// Follows a running Archive job.
    progress: Rc<ProgressView>,
    /// Asks for the passwords an Archive job needs.
    password: Rc<PasswordView>,
    /// Closes the password window if its job is cancelled; runs only while
    /// the window is open.
    prompt_watch: Timer,
    /// Told which undo is available whenever that changes.
    on_undo_changed: RefCell<Option<UndoListener>>,
    this: Weak<Self>,
}

impl Executor {
    /// `settings` is shared with the rest of the app, so a settings reload is
    /// seen by the next action; `recent` is the list the ring reads;
    /// `progress` and `password` are the Archive windows. Call on the main
    /// thread.
    pub fn new(
        platform: Rc<dyn Platform>,
        settings: Rc<RefCell<Settings>>,
        toast: Rc<Toaster>,
        recent: Rc<RefCell<RecentDestinations>>,
        progress: Rc<ProgressView>,
        password: Rc<PasswordView>,
    ) -> Rc<Self> {
        let executor = Rc::new_cyclic(|this| Self {
            platform,
            settings,
            toast,
            last: RefCell::new(None),
            recent,
            busy: Cell::new(false),
            progress,
            password,
            prompt_watch: Timer::default(),
            on_undo_changed: RefCell::new(None),
            this: this.clone(),
        });
        MAIN_EXECUTOR.with(|slot| *slot.borrow_mut() = Rc::downgrade(&executor));
        executor
    }

    /// Runs the action of `drop` on its files; toasts anchor at its wheel.
    /// A Move or Copy to with a destination starts at once; without one the
    /// folder picker opens first.
    pub fn run(&self, drop: WheelDrop) {
        let WheelDrop {
            action,
            paths,
            at,
            dest,
        } = drop;
        if paths.is_empty() {
            debug!("{action:?} with no files");
            return;
        }
        match action {
            ActionId::Pocket => {
                warn!("Pocket is handled by the Pocket controller");
            }
            ActionId::Move | ActionId::CopyTo => match dest {
                Some(dest) => self.transfer(action, paths, dest, at),
                None => {
                    if self.refuse_if_busy(at) {
                        return;
                    }
                    // The folder picker is modal: let the wheel finish
                    // handling the drop before it opens.
                    let this = self.this.clone();
                    Timer::single_shot(Duration::ZERO, move || {
                        if let Some(executor) = this.upgrade() {
                            executor.transfer_with_picker(action, paths, at);
                        }
                    });
                }
            },
            ActionId::Copy => {
                let result = self.platform.copy_files(&paths);
                self.finish(action, result, at, || {
                    format!("copy: {} file(s)", paths.len())
                });
            }
            ActionId::CopyPath => {
                let format = self.settings.borrow().copy_path_format;
                let result = self.platform.copy_text(&format_paths(&paths, format));
                self.finish(action, result, at, || format!("copy path: {}", paths.len()));
            }
            ActionId::Archive => self.archive(paths, at),
            ActionId::Terminal => {
                let terminal = self.settings.borrow().terminal.clone();
                let dir = terminal_dir(&paths);
                let result = match &dir {
                    Some(dir) => self.platform.open_terminal(terminal.as_deref(), dir),
                    None => Err(vento_platform::PlatformError::Failed(
                        "no folder to open".into(),
                    )),
                };
                self.finish(action, result, at, || {
                    format!(
                        "terminal: {}",
                        dir.as_deref().unwrap_or(Path::new("")).display()
                    )
                });
            }
        }
    }

    /// Starts undoing the last move or copy on a worker thread. Returns
    /// `false` if there is nothing to undo.
    pub fn undo_last(&self) -> bool {
        let at = match self.last.borrow().as_ref() {
            Some(last) => last.at,
            None => return false,
        };
        if self.refuse_if_busy(at) {
            return true;
        }
        let Some(last) = self.last.borrow_mut().take() else {
            return false;
        };
        self.notify_undo_changed();
        match last.op {
            LastOp::Move(record) => self.undo_move(record, at),
            LastOp::Copy(record) => self.undo_copy(record, UndoKind::Copy, at),
            LastOp::Archive(record) => self.undo_copy(record, UndoKind::Archive, at),
        }
        true
    }

    fn undo_move(&self, record: MoveRecord, at: Point) {
        self.toast.show(
            &format!("Restoring {}…", items(record.moved.len())),
            None,
            at,
        );
        self.spawn_file_job(
            ActionId::Move,
            move || {
                let outcome = undo(&record);
                (record, outcome)
            },
            move |executor, (record, outcome)| executor.undo_finished(&record, outcome, at),
            at,
        );
    }

    fn undo_finished(&self, record: &MoveRecord, outcome: MoveOutcome, at: Point) {
        for (path, error) in &outcome.failed {
            warn!("cannot restore {}: {error}", path.display());
        }
        info!("undo: restored {}", outcome.record.moved.len());
        if let Some(remainder) = undo_remainder(record, &outcome) {
            info!(
                "undo: {} item(s) kept for another undo",
                remainder.moved.len()
            );
            // A move recorded while this undo ran is newer: keep that one.
            let mut last = self.last.borrow_mut();
            if last.is_none() {
                *last = Some(Last {
                    op: LastOp::Move(remainder),
                    at,
                });
            }
        }
        self.notify_undo_changed();
        self.toast.show(&undo_toast_text(&outcome), None, at);
    }

    /// Undoes a Copy to, or an Archive (`kind`), in two steps. The worker
    /// finds the items that are still exactly as vento made them (the
    /// platform is not `Send`, so it cannot trash from there); the main
    /// thread then trashes just those. Nothing else is re-checked in between.
    fn undo_copy(&self, record: CopyRecord, kind: UndoKind, at: Point) {
        let (text, action) = if kind == UndoKind::Archive {
            let outputs: Vec<&Path> = record.copies.iter().map(|(_, o, _)| o.as_path()).collect();
            let text = format!("Moving {} to the Trash…", name_or_count(&outputs, items));
            (text, ActionId::Archive)
        } else {
            let text = format!("Removing {}…", plural_copies(record.copies.len()));
            (text, ActionId::CopyTo)
        };
        self.toast.show(&text, None, at);
        self.spawn_file_job(
            action,
            move || {
                let to_trash = RefCell::new(Vec::new());
                let outcome = undo_copies(&record, |copy| {
                    to_trash.borrow_mut().push(copy.to_path_buf());
                    Ok::<(), String>(())
                });
                CopyUndoChecked {
                    outcome,
                    to_trash: to_trash.into_inner(),
                }
            },
            move |executor, checked| executor.copy_undo_finished(checked, kind, at),
            at,
        );
    }

    fn copy_undo_finished(&self, checked: CopyUndoChecked, kind: UndoKind, at: Point) {
        let CopyUndoChecked {
            mut outcome,
            to_trash,
        } = checked;
        let mut trashed = Vec::with_capacity(to_trash.len());
        for copy in to_trash {
            match self.platform.trash(&copy) {
                Ok(()) => trashed.push(copy),
                Err(e) => {
                    warn!("cannot trash {}: {e}", copy.display());
                    outcome.failed.push((copy, e.to_string()));
                }
            }
        }
        outcome.trashed = trashed.len();
        for copy in &outcome.kept_changed {
            info!("undo: kept {} (changed since it was made)", copy.display());
        }
        info!(
            "undo: trashed {}, kept {} changed, {} gone, {} failed",
            outcome.trashed,
            outcome.kept_changed.len(),
            outcome.gone,
            outcome.failed.len()
        );
        self.notify_undo_changed();
        let text = if kind == UndoKind::Archive {
            archive_undo_toast_text(&outcome, &trashed)
        } else {
            copy_undo_toast_text(&outcome)
        };
        self.toast.show(&text, None, at);
    }

    /// Registers `f`, called with `has_undo()` after a move or copy records
    /// an undo and after an undo starts or finishes. Replaces any earlier callback.
    pub fn on_undo_changed(&self, f: UndoListener) {
        *self.on_undo_changed.borrow_mut() = Some(f);
    }

    fn notify_undo_changed(&self) {
        if let Some(f) = self.on_undo_changed.borrow().as_ref() {
            f(self.has_undo());
        }
    }

    /// What an undo would undo, if anything.
    pub fn has_undo(&self) -> Option<UndoKind> {
        self.last.borrow().as_ref().map(|last| last.op.kind())
    }

    /// Toasts and returns `true` while a move or undo is running.
    fn refuse_if_busy(&self, at: Point) -> bool {
        if self.busy.get() {
            debug!("busy: request ignored");
            self.toast.show(BUSY_TEXT, None, at);
        }
        self.busy.get()
    }

    fn transfer_with_picker(&self, action: ActionId, paths: Vec<PathBuf>, at: Point) {
        let start = paths.first().and_then(|p| p.parent());
        let Some(dest) = self.platform.pick_folder(start) else {
            debug!("{action:?} cancelled");
            return;
        };
        self.transfer(action, paths, dest, at);
    }

    /// Moves or copies `paths` into `dest` (`action` is `Move` or `CopyTo`)
    /// on a worker thread.
    fn transfer(&self, action: ActionId, paths: Vec<PathBuf>, dest: PathBuf, at: Point) {
        if self.refuse_if_busy(at) {
            return;
        }
        if action == ActionId::CopyTo {
            self.toast
                .show(&format!("Copying {}…", items(paths.len())), None, at);
            self.spawn_file_job(
                action,
                move || copy_items(&paths, &dest),
                move |executor, outcome| executor.copy_finished(outcome, at),
                at,
            );
        } else {
            self.toast
                .show(&format!("Moving {}…", items(paths.len())), None, at);
            self.spawn_file_job(
                action,
                move || move_items(&paths, &dest),
                move |executor, outcome| executor.move_finished(outcome, at),
                at,
            );
        }
    }

    fn move_finished(&self, outcome: MoveOutcome, at: Point) {
        for (path, error) in &outcome.failed {
            warn!("cannot move {}: {error}", path.display());
        }
        let first_error = outcome.failed.first().map(|(_, e)| e.to_string());
        match after_transfer(outcome.record.moved.len(), first_error) {
            AfterTransfer::Error(reason) => {
                self.toast_error(ActionId::Move, &reason, at);
                return;
            }
            AfterTransfer::Nothing => {
                debug!("nothing to move");
                self.toast.show("Nothing to move", None, at);
                return;
            }
            AfterTransfer::Done => {}
        }
        info!(
            "moved {} to {}",
            outcome.record.moved.len(),
            outcome.record.dest.display()
        );
        self.remember(&outcome.record.dest);
        let text = move_toast_text(&outcome);
        self.record_undo(LastOp::Move(outcome.record), text, at);
    }

    fn copy_finished(&self, outcome: CopyOutcome, at: Point) {
        for (path, error) in &outcome.failed {
            warn!("cannot copy {}: {error}", path.display());
        }
        let first_error = outcome.failed.first().map(|(_, e)| e.to_string());
        match after_transfer(outcome.record.copies.len(), first_error) {
            AfterTransfer::Error(reason) => {
                self.toast_error(ActionId::CopyTo, &reason, at);
                return;
            }
            AfterTransfer::Nothing => {
                debug!("nothing to copy");
                self.toast.show("Nothing to copy", None, at);
                return;
            }
            AfterTransfer::Done => {}
        }
        info!(
            "copied {} to {}",
            outcome.record.copies.len(),
            outcome.record.dest.display()
        );
        self.remember(&outcome.record.dest);
        let text = copy_toast_text(&outcome);
        self.record_undo(LastOp::Copy(outcome.record), text, at);
    }

    /// Compresses `paths` into a zip, or extracts them when they are all
    /// archives, on a worker thread.
    fn archive(&self, paths: Vec<PathBuf>, at: Point) {
        if self.refuse_if_busy(at) {
            return;
        }
        self.spawn_file_job(
            ActionId::Archive,
            move || archive_job(&paths, at),
            move |executor, done| executor.archive_finished(done, at),
            at,
        );
    }

    /// Opens the password window for the archive called `name` in place of
    /// the progress window, which comes back once the window is answered.
    /// `answer` gets the password, or `None`; it is dropped unanswered (also
    /// `None` to the worker) when the job is already cancelled. A cancel
    /// while the window is open closes it with `None`.
    fn prompt_password(
        &self,
        name: &str,
        wrong: bool,
        progress: Arc<Progress>,
        at: Point,
        answer: mpsc::SyncSender<Option<Password>>,
    ) {
        if progress.is_cancelled() {
            return;
        }
        self.progress.close();
        let password = Rc::downgrade(&self.password);
        let watched = Arc::clone(&progress);
        self.prompt_watch
            .start(TimerMode::Repeated, CANCEL_POLL, move || {
                if watched.is_cancelled()
                    && let Some(password) = password.upgrade()
                {
                    password.dismiss();
                }
            });
        let this = self.this.clone();
        self.password.ask(
            name,
            wrong,
            at,
            Box::new(move |password| {
                if let Some(executor) = this.upgrade() {
                    executor.prompt_watch.stop();
                    if !progress.is_cancelled() {
                        executor.progress.track(progress, EXTRACTING, at);
                    }
                }
                // Only fails if the worker is gone, and then no one is waiting.
                if answer.send(password).is_err() {
                    debug!("password answered after its job ended");
                }
            }),
        );
    }

    fn archive_finished(&self, done: ArchiveDone, at: Point) {
        let (text, made) = match done {
            ArchiveDone::Compressed {
                result: Err(ArchiveError::Cancelled),
                ..
            } => (CANCELLED_TEXT.to_owned(), Vec::new()),
            ArchiveDone::Compressed { result: Err(e), .. } => {
                warn!("couldn't compress: {e}");
                (format!("Couldn't compress: {e}"), Vec::new())
            }
            ArchiveDone::Compressed {
                count,
                result: Ok(made),
            } => {
                let output = display_name(&made.output);
                let text = archive_toast_text(&ArchiveResult::Compressed { count, output });
                (text, vec![made])
            }
            ArchiveDone::Extracted(outcome) => {
                let text = archive_toast_text(&ArchiveResult::Extracted(&outcome));
                (text, outcome.made)
            }
        };
        info!("archive: {text}");
        let Some(dest) = made
            .first()
            .and_then(|m| m.output.parent())
            .map(Path::to_path_buf)
        else {
            self.toast.show(&text, None, at);
            return;
        };
        let record = CopyRecord {
            dest,
            copies: made
                .into_iter()
                .map(|m| (m.source, m.output, m.fingerprint))
                .collect(),
        };
        self.record_undo(LastOp::Archive(record), text, at);
    }

    /// Keeps `op` as the one thing to undo and toasts `text` with an Undo
    /// button.
    fn record_undo(&self, op: LastOp, text: String, at: Point) {
        *self.last.borrow_mut() = Some(Last { op, at });
        self.notify_undo_changed();
        let this = self.this.clone();
        let on_undo: Box<dyn Fn()> = Box::new(move || {
            if let Some(executor) = this.upgrade() {
                executor.undo_last();
            }
        });
        self.toast.show(&text, Some(on_undo), at);
    }

    /// Puts `dest` first in the recent list and saves it. A failed save is
    /// logged, never toasted.
    fn remember(&self, dest: &Path) {
        let mut recent = self.recent.borrow_mut();
        recent.push(dest.to_path_buf());
        if let Err(e) = recent.save(&recent_path()) {
            warn!("cannot save the recent folders: {e}");
        }
    }

    /// Runs `job` on a worker thread, then `done` with its result on the main
    /// thread. Marks the executor busy until `done` runs, and closes the
    /// progress window (if the job used it) first, also after a panic.
    fn spawn_file_job<T: Send + 'static>(
        &self,
        action: ActionId,
        job: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&Executor, T) + Send + 'static,
        at: Point,
    ) {
        self.busy.set(true);
        let spawned = std::thread::Builder::new()
            .name("vento-fileops".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let delivered = on_main(move |executor| {
                    executor.busy.set(false);
                    executor.progress.close();
                    match result {
                        Ok(value) => done(executor, value),
                        Err(_) => executor.toast_error(action, "internal error", at),
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report a finished file operation: {e}");
                }
            });
        if let Err(e) = spawned {
            self.busy.set(false);
            self.toast_error(action, &format!("cannot start: {e}"), at);
        }
    }

    /// Logs a success (`describe`) or logs and toasts a platform failure;
    /// success needs no toast.
    fn finish(
        &self,
        action: ActionId,
        result: Result<(), vento_platform::PlatformError>,
        at: Point,
        describe: impl FnOnce() -> String,
    ) {
        match result {
            Ok(()) => info!("{}", describe()),
            Err(error) => self.toast_error(action, &error.to_string(), at),
        }
    }

    fn toast_error(&self, action: ActionId, reason: &str, at: Point) {
        let label = action.label().to_lowercase();
        warn!("couldn't {label}: {reason}");
        self.toast
            .show(&format!("Couldn't {label}: {reason}"), None, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use vento_core::archive::{ArchiveError, BatchOutcome, Made};
    use vento_core::fileops::{Fingerprint, MoveRecord};

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn outcome(moved: usize, failed: Vec<(PathBuf, FileOpError)>) -> MoveOutcome {
        MoveOutcome {
            record: MoveRecord {
                dest: p("/dest/Photos"),
                moved: (0..moved)
                    .map(|i| (p(&format!("/src/{i}")), p(&format!("/dest/Photos/{i}"))))
                    .collect(),
            },
            failed,
        }
    }

    fn not_removed(name: &str) -> (PathBuf, FileOpError) {
        (
            p(&format!("/src/{name}")),
            FileOpError::SourceNotRemoved {
                copy: p(&format!("/dest/Photos/{name}")),
                source: io::Error::other("busy"),
            },
        )
    }

    #[test]
    fn move_text_singular_and_plural() {
        assert_eq!(
            move_toast_text(&outcome(1, vec![])),
            "Moved 1 item to Photos"
        );
        assert_eq!(
            move_toast_text(&outcome(2, vec![])),
            "Moved 2 items to Photos"
        );
    }

    #[test]
    fn move_text_counts_plain_failures() {
        let failed = vec![(p("/src/x"), FileOpError::NotFound)];
        assert_eq!(
            move_toast_text(&outcome(2, failed)),
            "Moved 2 items to Photos · 1 failed"
        );
    }

    #[test]
    fn source_not_removed_is_moved_once_and_never_a_failure() {
        // The record and the failed list both hold it; it must count once as
        // moved and only as an unremoved original.
        assert_eq!(
            move_toast_text(&outcome(2, vec![not_removed("0")])),
            "Moved 2 items to Photos · 1 original couldn't be removed"
        );
    }

    #[test]
    fn move_text_mixed_failures() {
        let failed = vec![
            (p("/src/x"), FileOpError::IntoItself),
            not_removed("0"),
            not_removed("1"),
        ];
        assert_eq!(
            move_toast_text(&outcome(3, failed)),
            "Moved 3 items to Photos · 1 failed · 2 originals couldn't be removed"
        );
    }

    #[test]
    fn dest_without_a_file_name_falls_back_to_the_path() {
        let mut o = outcome(1, vec![]);
        o.record.dest = p("/");
        assert_eq!(move_toast_text(&o), "Moved 1 item to /");
    }

    #[test]
    fn undo_text() {
        assert_eq!(undo_toast_text(&outcome(2, vec![])), "Restored 2 items");
        assert_eq!(
            undo_toast_text(&outcome(1, vec![(p("/x"), FileOpError::NotFound)])),
            "Restored 1 item · 1 failed"
        );
    }

    fn copy_outcome(copied: usize, failed: Vec<(PathBuf, FileOpError)>) -> CopyOutcome {
        CopyOutcome {
            record: CopyRecord {
                dest: p("/dest/Photos"),
                copies: (0..copied)
                    .map(|i| {
                        (
                            p(&format!("/src/{i}")),
                            p(&format!("/dest/Photos/{i}")),
                            Fingerprint::Link {
                                target: p("/t"),
                                id: Default::default(),
                            },
                        )
                    })
                    .collect(),
            },
            failed,
        }
    }

    #[test]
    fn copy_toast_texts() {
        assert_eq!(
            copy_toast_text(&copy_outcome(1, vec![])),
            "Copied 1 item to Photos"
        );
        assert_eq!(
            copy_toast_text(&copy_outcome(3, vec![])),
            "Copied 3 items to Photos"
        );
        assert_eq!(
            copy_toast_text(&copy_outcome(2, vec![(p("/src/x"), FileOpError::NotFound)])),
            "Copied 2 items to Photos · 1 failed"
        );
        let undone = |trashed, kept: usize| CopyUndoOutcome {
            trashed,
            kept_changed: (0..kept).map(|i| p(&format!("/c/{i}"))).collect(),
            ..Default::default()
        };
        assert_eq!(copy_undo_toast_text(&undone(3, 0)), "Removed 3 copies");
        // Every copy already gone: nothing was removed, kept or refused.
        assert_eq!(
            copy_undo_toast_text(&CopyUndoOutcome {
                gone: 2,
                ..Default::default()
            }),
            "Nothing left to remove"
        );
        assert_eq!(
            copy_undo_toast_text(&undone(0, 1)),
            "Removed 0 copies, kept 1 changed"
        );
        assert_eq!(copy_undo_toast_text(&undone(1, 0)), "Removed 1 copy");
        assert_eq!(
            copy_undo_toast_text(&undone(2, 1)),
            "Removed 2 copies, kept 1 changed"
        );
        // Copies renamed or deleted since: said so after the rest.
        assert_eq!(
            copy_undo_toast_text(&CopyUndoOutcome {
                gone: 2,
                ..undone(1, 0)
            }),
            "Removed 1 copy, 2 no longer there"
        );
        assert_eq!(
            copy_undo_toast_text(&CopyUndoOutcome {
                gone: 1,
                failed: vec![(p("/c/f"), "denied".into())],
                ..undone(0, 1)
            }),
            "Removed 0 copies, kept 1 changed, 1 no longer there · 1 failed"
        );
    }

    fn made(source: &str, output: &str) -> Made {
        Made {
            source: p(source),
            output: p(output),
            fingerprint: Fingerprint::Link {
                target: p("/t"),
                id: Default::default(),
            },
        }
    }

    fn extracted(made: Vec<Made>, failed: Vec<(PathBuf, ArchiveError)>) -> BatchOutcome {
        BatchOutcome {
            made,
            failed,
            ..Default::default()
        }
    }

    fn damaged(name: &str) -> (PathBuf, ArchiveError) {
        (p(name), ArchiveError::Damaged("crc".into()))
    }

    #[test]
    fn archive_toast_compressed() {
        let compressed = |count, output: &str| ArchiveResult::Compressed {
            count,
            output: output.into(),
        };
        assert_eq!(
            archive_toast_text(&compressed(3, "Archive.zip")),
            "Compressed 3 items to Archive.zip"
        );
        assert_eq!(
            archive_toast_text(&compressed(1, "Report.zip")),
            "Compressed 1 item to Report.zip"
        );
    }

    #[test]
    fn archive_toast_extracted_one() {
        let result = extracted(vec![made("/d/Photos.zip", "/d/Photos")], vec![]);
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&result)),
            "Extracted Photos.zip"
        );
    }

    #[test]
    fn archive_toast_extracted_several() {
        let result = extracted(
            vec![made("/d/a.zip", "/d/a"), made("/d/b.7z", "/d/b")],
            vec![],
        );
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&result)),
            "Extracted 2 archives"
        );
    }

    #[test]
    fn archive_toast_partial_failure() {
        let result = extracted(
            vec![made("/d/a.zip", "/d/a"), made("/d/b.zip", "/d/b")],
            vec![damaged("/d/c.zip")],
        );
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&result)),
            "Extracted 2 of 3; 1 failed: damaged archive: crc"
        );
    }

    #[test]
    fn archive_toast_skipped() {
        let mut outcome = BatchOutcome {
            made: vec![made("/d/a.zip", "/d/a"), made("/d/b.zip", "/d/b")],
            skipped: vec![p("/d/c.zip")],
            ..Default::default()
        };
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&outcome)),
            "Extracted 2 of 3; 1 skipped"
        );
        outcome = BatchOutcome {
            made: vec![made("/d/a.zip", "/d/a")],
            failed: vec![damaged("/d/b.zip")],
            skipped: vec![p("/d/c.zip")],
            ..Default::default()
        };
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&outcome)),
            "Extracted 1 of 3; 1 failed: damaged archive: crc; 1 skipped"
        );
        // Every password prompt was cancelled: nothing made, nothing failed.
        outcome = BatchOutcome {
            skipped: vec![p("/d/a.zip"), p("/d/b.zip")],
            ..Default::default()
        };
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&outcome)),
            "Cancelled"
        );
    }

    #[test]
    fn archive_toast_all_failed() {
        let wrong_format = || ArchiveError::Io(io::Error::other("wrong format"));
        let result = extracted(vec![], vec![(p("/d/Photos.zip"), wrong_format())]);
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&result)),
            "Couldn't extract Photos.zip: wrong format"
        );
        let result = extracted(
            vec![],
            vec![(p("/d/a.zip"), wrong_format()), damaged("/d/b.zip")],
        );
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&result)),
            "Couldn't extract 2 archives: wrong format"
        );
        let outcome = BatchOutcome {
            failed: vec![(p("/d/a.zip"), wrong_format())],
            skipped: vec![p("/d/b.zip")],
            ..Default::default()
        };
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&outcome)),
            "Couldn't extract a.zip: wrong format; 1 skipped"
        );
    }

    #[test]
    fn archive_toast_cancelled() {
        let outcome = BatchOutcome {
            cancelled: true,
            ..Default::default()
        };
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&outcome)),
            "Cancelled"
        );
        // What was finished before Cancel is kept, and said so.
        let outcome = BatchOutcome {
            made: vec![made("/d/Photos.zip", "/d/Photos")],
            cancelled: true,
            ..Default::default()
        };
        assert_eq!(
            archive_toast_text(&ArchiveResult::Extracted(&outcome)),
            "Extracted Photos.zip; cancelled"
        );
    }

    #[test]
    fn archive_undo_texts() {
        let one = [p("/d/Archive.zip")];
        assert_eq!(
            archive_undo_toast_text(
                &CopyUndoOutcome {
                    trashed: 1,
                    ..Default::default()
                },
                &one
            ),
            "Moved Archive.zip to the Trash"
        );
        let two = [p("/d/a"), p("/d/b")];
        assert_eq!(
            archive_undo_toast_text(
                &CopyUndoOutcome {
                    trashed: 2,
                    ..Default::default()
                },
                &two
            ),
            "Moved 2 items to the Trash"
        );
        let kept = CopyUndoOutcome {
            kept_changed: vec![p("/d/Photos")],
            ..Default::default()
        };
        assert_eq!(
            archive_undo_toast_text(&kept, &[]),
            "Kept Photos: changed since it was made"
        );
        let mixed = CopyUndoOutcome {
            trashed: 1,
            kept_changed: vec![p("/d/x"), p("/d/y")],
            gone: 1,
            failed: vec![(p("/d/z"), "denied".into())],
        };
        assert_eq!(
            archive_undo_toast_text(&mixed, &one),
            "Moved Archive.zip to the Trash; kept 2 items: changed since they were made; \
             1 no longer there; 1 failed: denied"
        );
        assert_eq!(
            archive_undo_toast_text(
                &CopyUndoOutcome {
                    gone: 1,
                    ..Default::default()
                },
                &[]
            ),
            "Nothing left to remove"
        );
    }

    #[test]
    fn extracted_items_get_their_archive_quarantine() {
        let tmp = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, "x").unwrap();
            path
        };
        let (marked, plain) = (file("marked.zip"), file("plain.zip"));
        let value = b"0081;6553f000;Safari;";
        set_quarantine(&marked, value).unwrap();
        let mark = copy_quarantine();

        let first = file("a.txt");
        mark(&marked, &first).unwrap();
        assert_eq!(quarantine_of(&first).as_deref(), Some(&value[..]));
        // Read once per archive: later items of the same archive get the
        // value read for the first.
        std::fs::remove_file(&marked).unwrap();
        let second = file("b.txt");
        mark(&marked, &second).unwrap();
        assert_eq!(quarantine_of(&second).as_deref(), Some(&value[..]));
        // Another archive, without the attribute, marks nothing.
        let third = file("c.txt");
        mark(&plain, &third).unwrap();
        assert_eq!(quarantine_of(&third), None);
    }

    #[test]
    fn recent_is_updated_only_when_something_moved() {
        // Failures and nothing done: an error, nothing remembered.
        assert_eq!(
            after_transfer(0, Some("not found".into())),
            AfterTransfer::Error("not found".into())
        );
        // Everything was already there: nothing to remember either.
        assert_eq!(after_transfer(0, None), AfterTransfer::Nothing);
        // Something went through, even with failures beside it.
        assert_eq!(after_transfer(1, None), AfterTransfer::Done);
        assert_eq!(
            after_transfer(3, Some("not found".into())),
            AfterTransfer::Done
        );
    }

    fn record(pairs: &[(&str, &str)]) -> MoveRecord {
        MoveRecord {
            dest: p("/dest"),
            moved: pairs.iter().map(|(a, b)| (p(a), p(b))).collect(),
        }
    }

    #[test]
    fn full_undo_leaves_no_remainder() {
        let rec = record(&[("/src/a", "/dest/a")]);
        let back = MoveOutcome {
            record: record(&[("/dest/a", "/src/a")]),
            failed: vec![],
        };
        assert_eq!(undo_remainder(&rec, &back), None);
    }

    #[test]
    fn failed_items_stay_undoable() {
        let rec = record(&[
            ("/src/a", "/dest/a"),
            ("/src/b", "/dest/b"),
            ("/src/c", "/dest/c"),
        ]);
        let back = MoveOutcome {
            record: record(&[("/dest/b", "/src/b")]),
            failed: vec![
                (p("/dest/a"), FileOpError::NotFound),
                (p("/dest/c"), FileOpError::IntoItself),
            ],
        };
        assert_eq!(
            undo_remainder(&rec, &back),
            Some(record(&[("/src/a", "/dest/a"), ("/src/c", "/dest/c")]))
        );
    }

    #[test]
    fn copied_back_but_not_removed_is_not_retried() {
        // Its copy is already back at /src/a; undoing again would duplicate it.
        let rec = record(&[("/src/a", "/dest/a")]);
        let back = MoveOutcome {
            record: record(&[("/dest/a", "/src/a")]),
            failed: vec![(
                p("/dest/a"),
                FileOpError::SourceNotRemoved {
                    copy: p("/src/a"),
                    source: io::Error::other("busy"),
                },
            )],
        };
        assert_eq!(undo_remainder(&rec, &back), None);
    }

    #[test]
    fn terminal_dir_of_a_folder_is_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(terminal_dir(&[sub.clone(), p("/other")]), Some(sub));
    }

    #[test]
    fn terminal_dir_of_a_file_is_its_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("it's a file.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(terminal_dir(&[file]), Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn terminal_dir_of_no_paths_is_none() {
        assert_eq!(terminal_dir(&[]), None);
    }
}
