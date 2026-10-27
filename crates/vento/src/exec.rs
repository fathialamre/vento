//! Runs the action a file was dropped on and reports the result in a toast.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use slint::Timer;
use vento_core::actions::ActionId;
use vento_core::archive::Progress;
use vento_core::convert::ring_formats;
use vento_core::destinations::{RecentDestinations, recent_path};
use vento_core::fileops::{
    CopyOutcome, CopyRecord, CopyUndoOutcome, FileOpError, MoveOutcome, MoveRecord, copy_items,
    move_items, undo, undo_copies,
};
use vento_core::geometry::Point;
use vento_core::i18n::errors::io_reason;
use vento_core::i18n::{fill, tr, trn, with_language};
use vento_core::paths::format_paths;
use vento_core::settings::Settings;
use vento_platform::Platform;

use crate::archive_ui::{PasswordView, ProgressView};
use crate::toast::Toaster;
use crate::wheel::WheelDrop;

mod archive;
mod convert;

use archive::archive_undo_toast_text;

/// The side notes of a move or an undo: what failed, and what was moved
/// but could not be removed from where it came from. Each is a clause of
/// its own, in the order the toast says them.
///
/// A `SourceNotRemoved` item is in both `record.moved` and `failed`; it is
/// counted as moved only, and here only as an original that is still there.
fn failure_notes(outcome: &MoveOutcome) -> Vec<String> {
    let leftovers = outcome
        .failed
        .iter()
        .filter(|(_, e)| matches!(e, FileOpError::SourceNotRemoved { .. }))
        .count();
    let failed = outcome.failed.len() - leftovers;
    let mut notes = Vec::new();
    if failed > 0 {
        notes.push(trn("{n} failed", "{n} failed", failed as u64));
    }
    if leftovers > 0 {
        notes.push(trn(
            "{n} original couldn't be removed",
            "{n} originals couldn't be removed",
            leftovers as u64,
        ));
    }
    notes
}

/// `text` followed by each of the `notes` after the separator that sets a
/// side note apart (` · `).
fn with_notes(text: String, notes: &[String]) -> String {
    notes
        .iter()
        .fold(text, |text, note| format!("{text}{}{note}", tr(" · ")))
}

/// Runs `f` reading English: log lines stay English whatever the interface
/// language is, so the text they print is made inside this.
fn in_english<R>(f: impl FnOnce() -> R) -> R {
    with_language("en", f)
}

/// The item's own name, or the whole path for one without a name (`/`).
fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The name of the only one of `paths`, or `None` when there are none or
/// several: a sentence names one item and counts many, and each way is a
/// sentence of its own.
fn only_name(paths: &[&Path]) -> Option<String> {
    match paths {
        [only] => Some(display_name(only)),
        _ => None,
    }
}

/// Toast text for a finished move.
fn move_toast_text(outcome: &MoveOutcome) -> String {
    let main = fill(
        &trn(
            "Moved {n} item to {dest}",
            "Moved {n} items to {dest}",
            outcome.record.moved.len() as u64,
        ),
        &[("dest", &display_name(&outcome.record.dest))],
    );
    with_notes(main, &failure_notes(outcome))
}

/// Toast text for a finished Copy to.
fn copy_toast_text(outcome: &CopyOutcome) -> String {
    let main = fill(
        &trn(
            "Copied {n} item to {dest}",
            "Copied {n} items to {dest}",
            outcome.record.copies.len() as u64,
        ),
        &[("dest", &display_name(&outcome.record.dest))],
    );
    let mut notes = Vec::new();
    if !outcome.failed.is_empty() {
        let n = outcome.failed.len() as u64;
        notes.push(trn("{n} failed", "{n} failed", n));
    }
    with_notes(main, &notes)
}

/// Toast text for a finished undo of a Copy to: what went to the Trash, what
/// was left because it had changed, what was no longer there (renamed or
/// deleted since), and what the Trash refused.
fn copy_undo_toast_text(outcome: &CopyUndoOutcome) -> String {
    if outcome.trashed == 0 && outcome.kept_changed.is_empty() && outcome.failed.is_empty() {
        return tr("Nothing left to remove");
    }
    let mut text = trn(
        "Removed {n} copy",
        "Removed {n} copies",
        outcome.trashed as u64,
    );
    if !outcome.kept_changed.is_empty() {
        let n = outcome.kept_changed.len() as u64;
        text.push_str(&tr(", "));
        text.push_str(&trn("kept {n} changed", "kept {n} changed", n));
    }
    if outcome.gone > 0 {
        text.push_str(&tr(", "));
        text.push_str(&trn(
            "{n} no longer there",
            "{n} no longer there",
            outcome.gone as u64,
        ));
    }
    if !outcome.failed.is_empty() {
        let n = outcome.failed.len() as u64;
        text.push_str(&tr(" · "));
        text.push_str(&trn("{n} failed", "{n} failed", n));
    }
    text
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
    let main = trn(
        "Restored {n} item",
        "Restored {n} items",
        outcome.record.moved.len() as u64,
    );
    with_notes(main, &failure_notes(outcome))
}

/// What a move, copy or undo is busy with, for the toast that says so.
#[derive(Debug, Clone, Copy)]
enum Working {
    Moving,
    Copying,
    Restoring,
    Removing,
}

/// The toast shown while `count` items are being moved, copied or restored,
/// or `count` copies removed.
fn working_text(what: Working, count: usize) -> String {
    let n = count as u64;
    match what {
        Working::Moving => trn("Moving {n} item…", "Moving {n} items…", n),
        Working::Copying => trn("Copying {n} item…", "Copying {n} items…", n),
        Working::Restoring => trn("Restoring {n} item…", "Restoring {n} items…", n),
        Working::Removing => trn("Removing {n} copy…", "Removing {n} copies…", n),
    }
}

/// The toast shown while the outputs of an Archive or Convert (`paths`) go
/// to the Trash.
fn trashing_text(paths: &[&Path]) -> String {
    match only_name(paths) {
        Some(name) => fill(&tr("Moving {name} to the Trash…"), &[("name", &name)]),
        None => trn(
            "Moving {n} item to the Trash…",
            "Moving {n} items to the Trash…",
            paths.len() as u64,
        ),
    }
}

/// The toast for `action` failing for `reason`: one sentence per action, so
/// each language can word the action as it needs to.
fn action_error_text(action: ActionId, reason: &str) -> String {
    let template = match action {
        ActionId::Pocket => tr("Couldn't add to Pocket: {reason}"),
        ActionId::Copy => tr("Couldn't copy: {reason}"),
        ActionId::Move => tr("Couldn't move: {reason}"),
        ActionId::CopyPath => tr("Couldn't copy the path: {reason}"),
        ActionId::Terminal => tr("Couldn't open the terminal: {reason}"),
        ActionId::CopyTo => tr("Couldn't copy to the folder: {reason}"),
        ActionId::Archive => tr("Couldn't archive: {reason}"),
        ActionId::Info => tr("Couldn't show info: {reason}"),
        ActionId::Convert => tr("Couldn't convert: {reason}"),
    };
    fill(&template, &[("reason", reason)])
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
    /// The last Convert (the images it wrote).
    Convert,
}

/// The tray's Undo item text: names what would be undone, and reads
/// "move" when there is nothing to undo.
pub fn undo_menu_text(kind: Option<UndoKind>) -> String {
    match kind {
        Some(UndoKind::Copy) => tr("Undo last copy"),
        Some(UndoKind::Archive) => tr("Undo last archive"),
        Some(UndoKind::Convert) => tr("Undo last convert"),
        Some(UndoKind::Move) | None => tr("Undo last move"),
    }
}

/// Callback told which undo, if any, is available.
type UndoListener = Box<dyn Fn(Option<UndoKind>)>;

/// Toast shown when a move, copy or undo is requested while one is running.
fn busy_text() -> String {
    tr("Busy — still working")
}

/// Toast shown when Convert is released without a format chosen.
fn hold_for_format_text() -> String {
    tr("Hold on Convert to pick a format")
}

/// Longest vento waits, when quitting, for a running job to stop.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// How often quitting checks whether the job has stopped.
const SHUTDOWN_POLL: Duration = Duration::from_millis(10);

/// Waits for `worker` to finish, but no longer than `limit`, so quitting
/// never hangs. Returns whether it finished; one still running is left
/// detached.
fn join_within(worker: JoinHandle<()>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while !worker.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SHUTDOWN_POLL);
    }
    if worker.join().is_err() {
        warn!("a file operation ended in a panic");
    }
    true
}

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

/// What the undo slot holds.
enum LastOp {
    Move(MoveRecord),
    Copy(CopyRecord),
    /// What an Archive job made, undone like a Copy to.
    Archive(CopyRecord),
    /// What a Convert job made, undone like an Archive.
    Convert(CopyRecord),
}

impl LastOp {
    fn kind(&self) -> UndoKind {
        match self {
            LastOp::Move(_) => UndoKind::Move,
            LastOp::Copy(_) => UndoKind::Copy,
            LastOp::Archive(_) => UndoKind::Archive,
            LastOp::Convert(_) => UndoKind::Convert,
        }
    }
}

/// The most recent move or copy, kept so it can be undone.
struct Last {
    op: LastOp,
    /// Where toasts about this operation are anchored.
    at: Point,
}

/// The job running on a worker thread, kept so quitting can stop it.
struct RunningJob {
    worker: JoinHandle<()>,
    /// The progress to cancel, for a job that can be cancelled (Archive,
    /// Convert).
    cancel: Option<Arc<Progress>>,
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
    /// Whether a move, copy, archive, convert or undo is running on a worker
    /// thread.
    busy: Cell<bool>,
    /// That job, until its result is handled on the main thread.
    running: RefCell<Option<RunningJob>>,
    /// Follows a running Archive or Convert job.
    progress: Rc<ProgressView>,
    /// Asks for the passwords an Archive job needs.
    password: Rc<PasswordView>,
    /// Told which undo is available whenever that changes.
    on_undo_changed: RefCell<Vec<UndoListener>>,
    this: Weak<Self>,
}

impl Executor {
    /// `settings` is shared with the rest of the app, so a settings reload is
    /// seen by the next action; `recent` is the list the ring reads;
    /// `progress` is the Archive and Convert progress window, `password` the
    /// Archive password window. Call on the main thread.
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
            running: RefCell::new(None),
            progress,
            password,
            on_undo_changed: RefCell::new(Vec::new()),
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
            format,
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
            ActionId::Info => warn!("Info is handled by the info controller"),
            ActionId::Convert => match format {
                Some(format) => self.convert(paths, format, at),
                None if ring_formats(&paths, &vento_platform::convert_targets()).is_empty() => {
                    self.toast_error_as(action, &tr("no images"), "no images", at);
                }
                None => self.toast.show(&hold_for_format_text(), None, at),
            },
            ActionId::Terminal => {
                let terminal = self.settings.borrow().terminal.clone();
                let dir = terminal_dir(&paths);
                let result = match &dir {
                    Some(dir) => self.platform.open_terminal(
                        terminal.as_deref(),
                        dir,
                        Box::new(move |reason| {
                            let shown = on_main(move |executor| {
                                executor.toast_error(ActionId::Terminal, &reason, at);
                            });
                            if let Err(e) = shown {
                                warn!("cannot report a failed terminal launch: {e}");
                            }
                        }),
                    ),
                    None => Err(vento_platform::PlatformError::Failed(tr(
                        "no folder to open",
                    ))),
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
            LastOp::Convert(record) => self.undo_copy(record, UndoKind::Convert, at),
        }
        true
    }

    fn undo_move(&self, record: MoveRecord, at: Point) {
        self.toast.show(
            &working_text(Working::Restoring, record.moved.len()),
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
            None,
            at,
        );
    }

    fn undo_finished(&self, record: &MoveRecord, outcome: MoveOutcome, at: Point) {
        for (path, error) in &outcome.failed {
            warn!(
                "cannot restore {}: {}",
                path.display(),
                in_english(|| error.to_string())
            );
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

    /// Undoes a Copy to, an Archive or a Convert (`kind`), in two steps. The
    /// worker finds the items that are still exactly as vento made them (the
    /// platform is not `Send`, so it cannot trash from there); the main
    /// thread then trashes just those. Nothing else is re-checked in between.
    fn undo_copy(&self, record: CopyRecord, kind: UndoKind, at: Point) {
        let (text, action) = if matches!(kind, UndoKind::Archive | UndoKind::Convert) {
            let outputs: Vec<&Path> = record.copies.iter().map(|(_, o, _)| o.as_path()).collect();
            let text = trashing_text(&outputs);
            let action = if kind == UndoKind::Archive {
                ActionId::Archive
            } else {
                ActionId::Convert
            };
            (text, action)
        } else {
            let text = working_text(Working::Removing, record.copies.len());
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
            None,
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
        let text = if matches!(kind, UndoKind::Archive | UndoKind::Convert) {
            archive_undo_toast_text(&outcome, &trashed)
        } else {
            copy_undo_toast_text(&outcome)
        };
        self.toast.show(&text, None, at);
    }

    /// Adds `f`, called with `has_undo()` after a move or copy records an
    /// undo and after an undo starts or finishes. The tray's Undo item and
    /// the Quick panel's Undo row each follow it.
    pub fn on_undo_changed(&self, f: UndoListener) {
        self.on_undo_changed.borrow_mut().push(f);
    }

    fn notify_undo_changed(&self) {
        let kind = self.has_undo();
        for f in self.on_undo_changed.borrow().iter() {
            f(kind);
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
            self.toast.show(&busy_text(), None, at);
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
                .show(&working_text(Working::Copying, paths.len()), None, at);
            self.spawn_file_job(
                action,
                move || copy_items(&paths, &dest),
                move |executor, outcome| executor.copy_finished(outcome, at),
                None,
                at,
            );
        } else {
            self.toast
                .show(&working_text(Working::Moving, paths.len()), None, at);
            self.spawn_file_job(
                action,
                move || move_items(&paths, &dest),
                move |executor, outcome| executor.move_finished(outcome, at),
                None,
                at,
            );
        }
    }

    fn move_finished(&self, outcome: MoveOutcome, at: Point) {
        for (path, error) in &outcome.failed {
            warn!(
                "cannot move {}: {}",
                path.display(),
                in_english(|| error.to_string())
            );
        }
        let first_error = outcome.failed.first().map(|(_, e)| e.to_string());
        let logged = in_english(|| outcome.failed.first().map(|(_, e)| e.to_string()));
        match after_transfer(outcome.record.moved.len(), first_error) {
            AfterTransfer::Error(reason) => {
                self.toast_error_as(ActionId::Move, &reason, &logged.unwrap_or_default(), at);
                return;
            }
            AfterTransfer::Nothing => {
                debug!("nothing to move");
                self.toast.show(&tr("Nothing to move"), None, at);
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
            warn!(
                "cannot copy {}: {}",
                path.display(),
                in_english(|| error.to_string())
            );
        }
        let first_error = outcome.failed.first().map(|(_, e)| e.to_string());
        let logged = in_english(|| outcome.failed.first().map(|(_, e)| e.to_string()));
        match after_transfer(outcome.record.copies.len(), first_error) {
            AfterTransfer::Error(reason) => {
                self.toast_error_as(ActionId::CopyTo, &reason, &logged.unwrap_or_default(), at);
                return;
            }
            AfterTransfer::Nothing => {
                debug!("nothing to copy");
                self.toast.show(&tr("Nothing to copy"), None, at);
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
    /// `cancel` is the job's progress, when it can be cancelled; quitting
    /// cancels it.
    fn spawn_file_job<T: Send + 'static>(
        &self,
        action: ActionId,
        job: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&Executor, T) + Send + 'static,
        cancel: Option<Arc<Progress>>,
        at: Point,
    ) {
        self.busy.set(true);
        let spawned = std::thread::Builder::new()
            .name("vento-fileops".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let delivered = on_main(move |executor| {
                    executor.busy.set(false);
                    executor.running.take();
                    executor.progress.close();
                    match result {
                        Ok(value) => done(executor, value),
                        Err(_) => executor.toast_error_as(
                            action,
                            &tr("internal error"),
                            "internal error",
                            at,
                        ),
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report a finished file operation: {e}");
                }
            });
        match spawned {
            Ok(worker) => *self.running.borrow_mut() = Some(RunningJob { worker, cancel }),
            Err(e) => {
                self.busy.set(false);
                let reason = fill(&tr("cannot start: {reason}"), &[("reason", &io_reason(&e))]);
                let logged = in_english(|| {
                    fill(&tr("cannot start: {reason}"), &[("reason", &io_reason(&e))])
                });
                self.toast_error_as(action, &reason, &logged, at);
            }
        }
    }

    /// Stops the running job before vento quits, so it leaves no partial
    /// output behind: an Archive or Convert job is cancelled (and Archive's
    /// password window, if open, closed), which removes its partial item;
    /// then the worker gets up to two seconds to finish. A move or copy
    /// cannot be cancelled and gets the same wait. Never blocks longer. Call
    /// on the main thread.
    pub fn shutdown(&self) {
        let Some(job) = self.running.take() else {
            return;
        };
        if let Some(progress) = &job.cancel {
            progress.cancel();
            // A worker waiting for a password would wait past the deadline.
            self.password.dismiss();
        }
        if join_within(job.worker, SHUTDOWN_WAIT) {
            info!("quit: the running job stopped");
        } else {
            warn!("quit: the running job did not stop in time");
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
            Err(error) => {
                let logged = in_english(|| error.to_string());
                self.toast_error_as(action, &error.to_string(), &logged, at);
            }
        }
    }

    fn toast_error(&self, action: ActionId, reason: &str, at: Point) {
        self.toast_error_as(action, reason, reason, at);
    }

    /// Toasts `reason` and logs `logged`, the same reason in English (a
    /// reason that is English whatever the language, such as one from an
    /// outside library, goes through `toast_error`, which logs it as it is).
    fn toast_error_as(&self, action: ActionId, reason: &str, logged: &str, at: Point) {
        warn!("{action:?} failed: {logged}");
        self.toast
            .show(&action_error_text(action, reason), None, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_item_names_what_it_would_undo() {
        assert_eq!(undo_menu_text(Some(UndoKind::Move)), "Undo last move");
        assert_eq!(undo_menu_text(Some(UndoKind::Copy)), "Undo last copy");
        assert_eq!(undo_menu_text(Some(UndoKind::Archive)), "Undo last archive");
        assert_eq!(undo_menu_text(Some(UndoKind::Convert)), "Undo last convert");
        assert_eq!(undo_menu_text(None), "Undo last move");
    }
    use std::io;
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

    /// A move of `moved` items into the folder called `dest`.
    fn moved_into(dest: &str, moved: usize) -> MoveOutcome {
        let mut o = outcome(moved, vec![]);
        o.record.dest = p(&format!("/dest/{dest}"));
        o
    }

    #[test]
    fn move_text_reads_in_arabic_with_the_noun_after_the_verb() {
        vento_core::i18n::with_language("ar", || {
            let say = |n| move_toast_text(&moved_into("المستندات", n));
            assert_eq!(say(1), "تم نقل عنصر واحد إلى المستندات");
            assert_eq!(say(2), "تم نقل عنصرين إلى المستندات");
            assert_eq!(say(3), "تم نقل 3 عناصر إلى المستندات");
            assert_eq!(say(11), "تم نقل 11 عنصراً إلى المستندات");
            assert_eq!(say(100), "تم نقل 100 عنصر إلى المستندات");
            assert_eq!(say(0), "لم يُنقل أي عنصر إلى المستندات");
            // A Latin name stays whole inside the Arabic sentence.
            assert_eq!(
                move_toast_text(&moved_into("Photos", 11)),
                "تم نقل 11 عنصراً إلى Photos"
            );
        });
    }

    #[test]
    fn side_notes_are_whole_clauses_after_the_separator() {
        let failed = vec![
            (p("/src/x"), FileOpError::IntoItself),
            not_removed("0"),
            not_removed("1"),
        ];
        vento_core::i18n::with_language("ar", || {
            assert_eq!(
                move_toast_text(&outcome(3, failed)),
                "تم نقل 3 عناصر إلى Photos · تعذّرت معالجة عنصر واحد · تعذّرت إزالة أصلين"
            );
            assert_eq!(
                undo_toast_text(&outcome(2, vec![(p("/x"), FileOpError::NotFound)])),
                "تمت استعادة عنصرين · تعذّرت معالجة عنصر واحد"
            );
        });
    }

    #[test]
    fn copy_texts_read_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            assert_eq!(
                copy_toast_text(&copy_outcome(2, vec![])),
                "تم نسخ عنصرين إلى Photos"
            );
            assert_eq!(
                copy_toast_text(&copy_outcome(3, vec![(p("/src/x"), FileOpError::NotFound)])),
                "تم نسخ 3 عناصر إلى Photos · تعذّرت معالجة عنصر واحد"
            );
            let undone = CopyUndoOutcome {
                trashed: 2,
                kept_changed: vec![p("/c/0")],
                gone: 11,
                ..Default::default()
            };
            assert_eq!(
                copy_undo_toast_text(&undone),
                "تمت إزالة نسختين، تم إبقاء عنصر واحد لأنه تغيّر، لم يُعثر على 11 عنصراً بعد الآن"
            );
            assert_eq!(
                copy_undo_toast_text(&CopyUndoOutcome::default()),
                "لم يبقَ شيء لإزالته"
            );
        });
    }

    #[test]
    fn an_error_is_one_sentence_per_action() {
        let say = |action| action_error_text(action, "no folder to open");
        assert_eq!(
            say(ActionId::Terminal),
            "Couldn't open the terminal: no folder to open"
        );
        let all: Vec<_> = ActionId::ALL.iter().map(|&a| say(a)).collect();
        assert_eq!(
            all,
            [
                "Couldn't add to Pocket: no folder to open",
                "Couldn't copy: no folder to open",
                "Couldn't move: no folder to open",
                "Couldn't copy the path: no folder to open",
                "Couldn't open the terminal: no folder to open",
                "Couldn't copy to the folder: no folder to open",
                "Couldn't archive: no folder to open",
                "Couldn't show info: no folder to open",
                "Couldn't convert: no folder to open",
            ]
        );
        vento_core::i18n::with_language("ar", || {
            assert_eq!(
                action_error_text(ActionId::Terminal, "x"),
                "تعذّر فتح الطرفية: x"
            );
            assert_eq!(action_error_text(ActionId::Move, "x"), "تعذّر النقل: x");
            assert_eq!(
                action_error_text(ActionId::Convert, &tr("no images")),
                "تعذّر التحويل: لا توجد صور"
            );
        });
    }

    #[test]
    fn short_toasts_read_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            assert_eq!(busy_text(), "مشغول، العمل جارٍ");
            assert_eq!(
                hold_for_format_text(),
                "اضغط مطوّلاً على «تحويل» لاختيار الصيغة"
            );
            assert_eq!(working_text(Working::Moving, 2), "جارٍ نقل عنصرين…");
            assert_eq!(working_text(Working::Copying, 11), "جارٍ نسخ 11 عنصراً…");
            assert_eq!(
                working_text(Working::Restoring, 1),
                "جارٍ استعادة عنصر واحد…"
            );
            assert_eq!(working_text(Working::Removing, 2), "جارٍ إزالة نسختين…");
        });
        assert_eq!(working_text(Working::Moving, 1), "Moving 1 item…");
        assert_eq!(working_text(Working::Copying, 3), "Copying 3 items…");
        assert_eq!(working_text(Working::Restoring, 2), "Restoring 2 items…");
        assert_eq!(working_text(Working::Removing, 1), "Removing 1 copy…");
        assert_eq!(busy_text(), "Busy — still working");
    }

    #[test]
    fn moving_to_the_trash_names_one_and_counts_many() {
        let one = [p("/d/Archive.zip")];
        let two = [p("/d/a"), p("/d/b")];
        fn names(paths: &[PathBuf]) -> Vec<&Path> {
            paths.iter().map(PathBuf::as_path).collect()
        }
        assert_eq!(
            trashing_text(&names(&one)),
            "Moving Archive.zip to the Trash…"
        );
        assert_eq!(trashing_text(&names(&two)), "Moving 2 items to the Trash…");
        vento_core::i18n::with_language("ar", || {
            assert_eq!(
                trashing_text(&names(&one)),
                "جارٍ نقل Archive.zip إلى سلة المهملات…"
            );
            assert_eq!(
                trashing_text(&names(&two)),
                "جارٍ نقل عنصرين إلى سلة المهملات…"
            );
        });
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

    #[test]
    fn join_within_waits_for_a_worker_that_finishes() {
        let worker = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(20)));
        assert!(join_within(worker, Duration::from_secs(2)));
    }

    #[test]
    fn join_within_gives_up_on_a_worker_that_does_not_finish() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let _ = wait.recv();
        });
        let start = std::time::Instant::now();

        assert!(!join_within(worker, Duration::from_millis(50)));

        assert!(start.elapsed() < Duration::from_secs(1));
        drop(release);
    }
}
