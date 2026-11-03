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
use vento_core::rename::RenameRecord;
use vento_core::settings::Settings;
use vento_platform::Platform;

use crate::archive_ui::{PasswordView, ProgressView, Verb, progress_title};
use crate::notice::{CopySource, JobNotice, Notice, middle_cut, next_job_id};
use crate::toast::Toaster;
use crate::wheel::WheelDrop;

mod archive;
mod convert;
mod pdf;
mod rename;
mod share;

use archive::archive_undo_toast_text;
pub use pdf::{PdfDone, pdf_open_notice, pdf_open_toast_text, pdf_toast_text};
pub use rename::RenameDone;

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
        ActionId::Pdf => tr("Couldn't open the PDF: {reason}"),
        ActionId::Share => tr("Couldn't share: {reason}"),
        ActionId::Rename => tr("Couldn't rename: {reason}"),
    };
    fill(&template, &[("reason", reason)])
}

/// The notice for `action` failing for `reason`.
pub(crate) fn action_error_notice(action: ActionId, reason: &str) -> Notice {
    error_notice(action, action_error_text(action, reason))
}

/// An error notice for `action`, worded as `text`.
pub(crate) fn error_notice(action: ActionId, text: String) -> Notice {
    Notice::error(text).with_icon(action)
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
    /// The last PDF write (the file or folder it made).
    Pdf,
    /// The last Rename (the names it changed).
    Rename,
}

impl UndoKind {
    /// The action whose mark an undo of this kind carries.
    fn action(self) -> ActionId {
        match self {
            UndoKind::Move => ActionId::Move,
            UndoKind::Copy => ActionId::CopyTo,
            UndoKind::Archive => ActionId::Archive,
            UndoKind::Convert => ActionId::Convert,
            UndoKind::Pdf => ActionId::Pdf,
            UndoKind::Rename => ActionId::Rename,
        }
    }
}

/// The tray's Undo item text: names what would be undone, and reads
/// "move" when there is nothing to undo.
pub fn undo_menu_text(kind: Option<UndoKind>) -> String {
    match kind {
        Some(UndoKind::Copy) => tr("Undo last copy"),
        Some(UndoKind::Archive) => tr("Undo last archive"),
        Some(UndoKind::Convert) => tr("Undo last convert"),
        Some(UndoKind::Pdf) => tr("Undo last PDF"),
        Some(UndoKind::Rename) => tr("Undo last rename"),
        Some(UndoKind::Move) | None => tr("Undo last move"),
    }
}

/// Whether undoing `kind` trashes files the job made (Archive, Convert, PDF),
/// as opposed to removing the copies of a Copy to.
fn undoes_outputs(kind: UndoKind) -> bool {
    matches!(kind, UndoKind::Archive | UndoKind::Convert | UndoKind::Pdf)
}

/// Callback told which undo, if any, is available.
type UndoListener = Box<dyn Fn(Option<UndoKind>)>;

/// Toast shown when a move, copy or undo is requested while one is running.
fn busy_text() -> String {
    tr("Busy — still working")
}

/// The notice for a move, copy or undo requested while one is running.
pub(crate) fn busy_notice() -> Notice {
    Notice::busy(busy_text())
}

/// Toast shown when Convert is released without a format chosen.
fn hold_for_format_text() -> String {
    tr("Hold on Convert to pick a format")
}

/// The notice for Convert released without a format chosen.
fn hold_for_format_notice() -> Notice {
    Notice::info(hold_for_format_text()).with_icon(ActionId::Convert)
}

/// The notice for a move, copy or undo under way, where the island does not
/// show it as a job: `text` says what.
fn working_notice(action: ActionId, text: String) -> Notice {
    Notice::info(text).with_icon(action)
}

/// The notice for a result of `action` with no Undo: an undo's result, or
/// a job that made nothing to undo.
fn result_notice(action: ActionId, text: String) -> Notice {
    Notice::done(text).with_icon(action)
}

/// The notice for `text`, a result of `action` that `on_undo` undoes;
/// `serial` names the operation it undoes, which the island's Undo,
/// `island_undo`, undoes alone.
fn undo_notice(
    action: ActionId,
    text: String,
    on_undo: Box<dyn Fn()>,
    serial: u64,
    island_undo: impl Fn() + 'static,
) -> Notice {
    result_notice(action, text)
        .with_undo(Some(on_undo))
        .with_undo_serial(serial)
        .with_island_undo(island_undo)
}

/// The notice that Copy (or the Pocket menu's Copy) put `n` files on the
/// clipboard. The classic toast has nothing to say, so only the island
/// shows it.
pub(crate) fn files_copied_notice(n: usize) -> Notice {
    Notice::done(tr("Copied"))
        .with_icon(ActionId::Copy)
        .with_sub(trn(
            "{n} file ready to paste",
            "{n} files ready to paste",
            n as u64,
        ))
        .with_copy(CopySource::Own)
        .island_only()
}

/// The notice that Copy path (or the Pocket list's) put `copied` on the
/// clipboard: the first line, cut to fit the island. Island only, like [`files_copied_notice`].
pub(crate) fn path_copied_notice(copied: &str) -> Notice {
    let first = copied.lines().next().unwrap_or_default();
    Notice::done(tr("Path copied"))
        .with_icon(ActionId::CopyPath)
        .with_sub(middle_cut(first, PATH_SUB_CHARS))
        .with_copy(CopySource::Own)
        .island_only()
}

/// Longest path, in characters, the island shows under "Path copied".
const PATH_SUB_CHARS: usize = 40;

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
    /// What a PDF write job made, undone like an Archive.
    Pdf(CopyRecord),
    /// The names a Rename changed, put back by name.
    Rename(RenameRecord),
}

impl LastOp {
    fn kind(&self) -> UndoKind {
        match self {
            LastOp::Move(_) => UndoKind::Move,
            LastOp::Copy(_) => UndoKind::Copy,
            LastOp::Archive(_) => UndoKind::Archive,
            LastOp::Convert(_) => UndoKind::Convert,
            LastOp::Pdf(_) => UndoKind::Pdf,
            LastOp::Rename(_) => UndoKind::Rename,
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
    /// Counts the operations that became `last`: names one for
    /// [`undo_if_last`](Self::undo_if_last).
    last_serial: Cell<u64>,
    /// Folders recently moved or copied to; saved after each addition.
    recent: Rc<RefCell<RecentDestinations>>,
    /// Whether a move, copy, archive, convert, PDF write, rename or undo is
    /// running on a worker thread.
    busy: Cell<bool>,
    /// That job, until its result is handled on the main thread.
    running: RefCell<Option<RunningJob>>,
    /// Follows a running Archive or Convert job.
    progress: Rc<ProgressView>,
    /// The island, not the progress window, follows the running Archive or
    /// Convert job.
    progress_in_island: Cell<bool>,
    /// Asks for the passwords an Archive job needs.
    password: Rc<PasswordView>,
    /// Told which undo is available whenever that changes.
    on_undo_changed: RefCell<Vec<UndoListener>>,
    /// Told how the running PDF job ended; set by `pdf_job`, taken when it
    /// finishes. It lives here, not with the worker, because it is not
    /// `Send`.
    pdf_done: RefCell<Option<pdf::PdfDone>>,
    /// Told how the running Rename ended, as `pdf_done` for a PDF job.
    rename_done: RefCell<Option<RenameDone>>,
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
            last_serial: Cell::new(0),
            recent,
            busy: Cell::new(false),
            running: RefCell::new(None),
            progress,
            progress_in_island: Cell::new(false),
            password,
            on_undo_changed: RefCell::new(Vec::new()),
            pdf_done: RefCell::new(None),
            rename_done: RefCell::new(None),
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
            target,
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
                let copied = self.finish(action, result, at, || {
                    format!("copy: {} file(s)", paths.len())
                });
                if copied {
                    self.toast.notify(files_copied_notice(paths.len()), at);
                }
            }
            ActionId::CopyPath => {
                let format = self.settings.borrow().copy_path_format;
                let text = format_paths(&paths, format);
                let result = self.platform.copy_text(&text);
                let copied =
                    self.finish(action, result, at, || format!("copy path: {}", paths.len()));
                if copied {
                    self.toast.notify(path_copied_notice(&text), at);
                }
            }
            ActionId::Archive => self.archive(paths, at),
            ActionId::Info => warn!("Info is handled by the info controller"),
            ActionId::Pdf => warn!("PDF is handled by the PDF controller"),
            ActionId::Share => self.share(paths, target, at),
            ActionId::Rename => warn!("Rename is handled by the rename controller"),
            ActionId::Convert => match format {
                Some(format) => self.convert(paths, format, at),
                None if ring_formats(&paths, &vento_platform::convert_targets()).is_empty() => {
                    self.toast_error_as(action, &tr("no images"), "no images", at);
                }
                None => self.toast.notify(hold_for_format_notice(), at),
            },
            ActionId::Terminal => {
                let terminal = self.settings.borrow().terminal.clone();
                let dir = terminal_dir(&paths);
                let result = match &dir {
                    Some(dir) => self.platform.open_terminal(
                        terminal.as_deref(),
                        dir,
                        Box::new(move |error| {
                            let shown = on_main(move |executor| {
                                // Worded here, so the toast follows the
                                // language now and the log stays English.
                                let logged = in_english(|| error.to_string());
                                executor.toast_error_as(
                                    ActionId::Terminal,
                                    &error.to_string(),
                                    &logged,
                                    at,
                                );
                            });
                            if let Err(e) = shown {
                                warn!("cannot report a failed terminal launch: {e}");
                            }
                        }),
                    ),
                    // Worded when shown, so the log line stays English.
                    None => Err(vento_platform::PlatformError::NoFolder),
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
            LastOp::Pdf(record) => self.undo_copy(record, UndoKind::Pdf, at),
            LastOp::Rename(record) => self.undo_rename(record, at),
        }
        true
    }

    fn undo_move(&self, record: MoveRecord, at: Point) {
        let job = self.announce_job(
            ActionId::Move,
            working_text(Working::Restoring, record.moved.len()),
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
            Some(job),
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
        self.toast
            .notify(result_notice(ActionId::Move, undo_toast_text(&outcome)), at);
    }

    /// Undoes a Copy to, an Archive, a Convert or a PDF write (`kind`), in
    /// two steps. The worker finds the items that are still exactly as vento
    /// made them (the platform is not `Send`, so it cannot trash from there);
    /// the main thread then trashes just those. Nothing else is re-checked in
    /// between.
    fn undo_copy(&self, record: CopyRecord, kind: UndoKind, at: Point) {
        let (text, action) = if undoes_outputs(kind) {
            let outputs: Vec<&Path> = record.copies.iter().map(|(_, o, _)| o.as_path()).collect();
            let text = trashing_text(&outputs);
            let action = match kind {
                UndoKind::Archive => ActionId::Archive,
                UndoKind::Pdf => ActionId::Pdf,
                _ => ActionId::Convert,
            };
            (text, action)
        } else {
            let text = working_text(Working::Removing, record.copies.len());
            (text, ActionId::CopyTo)
        };
        let job = self.announce_job(action, text, at);
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
            Some(job),
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
        let text = if undoes_outputs(kind) {
            archive_undo_toast_text(&outcome, &trashed)
        } else {
            copy_undo_toast_text(&outcome)
        };
        self.toast.notify(result_notice(kind.action(), text), at);
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

    /// Whether a move, copy, archive, convert, PDF write, rename or undo is
    /// running, which quitting would cancel, and which the Rename window
    /// checks before it waits for its rename.
    pub fn is_busy(&self) -> bool {
        self.busy.get()
    }

    /// Toasts and returns `true` while a move or undo is running.
    fn refuse_if_busy(&self, at: Point) -> bool {
        if self.busy.get() {
            debug!("busy: request ignored");
            self.toast.notify(busy_notice(), at);
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
            let job = self.announce_job(action, working_text(Working::Copying, paths.len()), at);
            self.spawn_file_job(
                action,
                move || copy_items(&paths, &dest),
                move |executor, outcome| executor.copy_finished(outcome, at),
                None,
                Some(job),
                at,
            );
        } else {
            let job = self.announce_job(action, working_text(Working::Moving, paths.len()), at);
            self.spawn_file_job(
                action,
                move || move_items(&paths, &dest),
                move |executor, outcome| executor.move_finished(outcome, at),
                None,
                Some(job),
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
                self.toast.notify(
                    Notice::info(tr("Nothing to move")).with_icon(ActionId::Move),
                    at,
                );
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
                self.toast.notify(
                    Notice::info(tr("Nothing to copy")).with_icon(ActionId::CopyTo),
                    at,
                );
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

    /// Keeps `op` as the one thing to undo and says `text` with an Undo
    /// button.
    fn record_undo(&self, op: LastOp, text: String, at: Point) {
        let action = op.kind().action();
        self.remember_last(op, at);
        self.toast_undo(text, action, at);
    }

    /// Makes `op` the operation Undo undoes, without a toast.
    fn remember_last(&self, op: LastOp, at: Point) {
        self.last_serial.set(self.last_serial.get() + 1);
        *self.last.borrow_mut() = Some(Last { op, at });
        self.notify_undo_changed();
    }

    /// Says `text`, a result of `action`, with Undo for the operation
    /// [`remember_last`](Self::remember_last) has just remembered.
    fn toast_undo(&self, text: String, action: ActionId, at: Point) {
        let this = self.this.clone();
        let on_undo: Box<dyn Fn()> = Box::new(move || {
            if let Some(executor) = this.upgrade() {
                executor.undo_last();
            }
        });
        // The operation just remembered: the island's Undo undoes it while
        // it is still the last, and never whatever came after.
        let serial = self.last_serial.get();
        let island_undo = self.undo_while_last(serial);
        let notice = undo_notice(action, text, on_undo, serial, island_undo);
        self.toast.notify(notice, at);
    }

    /// What the island's Undo for the operation `serial` runs: it undoes
    /// that operation only while it is still the last one.
    fn undo_while_last(&self, serial: u64) -> impl Fn() + 'static {
        let this = self.this.clone();
        move || {
            if let Some(executor) = this.upgrade() {
                executor.undo_if_last(serial);
            }
        }
    }

    /// Offers the island a job of `action` that `text` describes, and toasts
    /// `text` where the island does not take it. Returns the job's id, which
    /// [`spawn_file_job`](Self::spawn_file_job) ends.
    fn announce_job(&self, action: ActionId, text: String, at: Point) -> u64 {
        let id = next_job_id();
        let job = JobNotice {
            id,
            icon: Some(action),
            text: text.clone(),
            progress: None,
            cancel: None,
        };
        if !self.toast.job_started(job) {
            self.toast.notify(working_notice(action, text), at);
        }
        id
    }

    /// Has the island follow `progress`, the Archive or Convert job `job`
    /// doing `verb`, with its Cancel; where the island does not take it, the
    /// progress window does, 24 px under `at`. The island's Cancel cancels as
    /// quitting does: it also closes the password window, if open.
    fn follow_progress(&self, progress: Arc<Progress>, verb: Verb, job: u64, at: Point) {
        let (cancel, this) = (Arc::clone(&progress), self.this.clone());
        let taken = self.toast.job_started(JobNotice {
            id: job,
            icon: Some(verb.action()),
            text: progress_title(verb, ""),
            progress: Some(Arc::clone(&progress)),
            cancel: Some(Rc::new(move || {
                cancel.cancel();
                if let Some(executor) = this.upgrade() {
                    executor.password.dismiss();
                }
            })),
        });
        self.progress_in_island.set(taken);
        if !taken {
            self.progress.track(progress, verb, at);
        }
    }

    /// The serial of the operation Undo would undo now, if any.
    pub fn last_serial(&self) -> Option<u64> {
        self.last.borrow().is_some().then(|| self.last_serial.get())
    }

    /// Undoes the last operation only if it is still the one `serial`
    /// names (another action, or an undo, may have come since). Returns
    /// whether it started.
    pub fn undo_if_last(&self, serial: u64) -> bool {
        self.last_serial() == Some(serial) && self.undo_last()
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
    /// progress window (if the job used it) and ends the island's job `id`
    /// (if it has one) first, also after a panic. `cancel` is the job's
    /// progress, when it can be cancelled; quitting cancels it.
    fn spawn_file_job<T: Send + 'static>(
        &self,
        action: ActionId,
        job: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&Executor, T) + Send + 'static,
        cancel: Option<Arc<Progress>>,
        id: Option<u64>,
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
                    executor.progress_in_island.set(false);
                    if let Some(id) = id {
                        executor.toast.job_ended(id);
                    }
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
                if let Some(id) = id {
                    self.toast.job_ended(id);
                }
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
    /// success needs no toast. Returns whether it succeeded.
    fn finish(
        &self,
        action: ActionId,
        result: Result<(), vento_platform::PlatformError>,
        at: Point,
        describe: impl FnOnce() -> String,
    ) -> bool {
        match result {
            Ok(()) => {
                info!("{}", describe());
                true
            }
            Err(error) => {
                let logged = in_english(|| error.to_string());
                self.toast_error_as(action, &error.to_string(), &logged, at);
                false
            }
        }
    }

    /// Toasts `reason` and logs `logged`, the same reason in English (for a
    /// reason that is English whatever the language, such as one from an
    /// outside library, both are the same text).
    fn toast_error_as(&self, action: ActionId, reason: &str, logged: &str, at: Point) {
        warn!("{action:?} failed: {logged}");
        self.toast.notify(action_error_notice(action, reason), at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::news::Tone;

    #[test]
    fn undo_item_names_what_it_would_undo() {
        assert_eq!(undo_menu_text(Some(UndoKind::Move)), "Undo last move");
        assert_eq!(undo_menu_text(Some(UndoKind::Copy)), "Undo last copy");
        assert_eq!(undo_menu_text(Some(UndoKind::Archive)), "Undo last archive");
        assert_eq!(undo_menu_text(Some(UndoKind::Convert)), "Undo last convert");
        assert_eq!(undo_menu_text(Some(UndoKind::Pdf)), "Undo last PDF");
        assert_eq!(undo_menu_text(Some(UndoKind::Rename)), "Undo last rename");
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
    fn a_terminal_with_no_folder_is_toasted_in_the_language_and_logged_in_english() {
        let error = vento_platform::PlatformError::NoFolder;
        vento_core::i18n::with_language("ar", || {
            // What `finish` toasts and what it logs.
            assert_eq!(
                action_error_text(ActionId::Terminal, &error.to_string()),
                "تعذّر فتح الطرفية: لا يوجد مجلد لفتحه"
            );
            assert_eq!(in_english(|| error.to_string()), "no folder to open");
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
                "Couldn't open the PDF: no folder to open",
                "Couldn't share: no folder to open",
                "Couldn't rename: no folder to open",
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

    /// What the classic toast shows for `n`, through the router with an
    /// island that declines it: its text and whether it has an Undo.
    fn classic(n: Notice) -> Option<(String, bool)> {
        crate::toast::classic_toast(n)
    }

    fn shown(text: &str, undo: bool) -> Option<(String, bool)> {
        Some((text.to_owned(), undo))
    }

    #[test]
    fn classic_fallback_matches_legacy_toasts() {
        assert_eq!(classic(busy_notice()), shown("Busy — still working", false));
        assert_eq!(
            classic(hold_for_format_notice()),
            shown("Hold on Convert to pick a format", false)
        );
        assert_eq!(
            classic(working_notice(
                ActionId::Move,
                working_text(Working::Moving, 2)
            )),
            shown("Moving 2 items…", false)
        );
        assert_eq!(
            classic(result_notice(
                ActionId::Move,
                undo_toast_text(&outcome(2, vec![]))
            )),
            shown("Restored 2 items", false)
        );
        assert_eq!(
            classic(action_error_notice(ActionId::Terminal, "no folder to open")),
            shown("Couldn't open the terminal: no folder to open", false)
        );
        // A result with its Undo, whether or not it names the operation.
        let undo = || -> Box<dyn Fn()> { Box::new(|| {}) };
        assert_eq!(
            classic(undo_notice(
                ActionId::Move,
                move_toast_text(&outcome(2, vec![])),
                undo(),
                4,
                || {}
            )),
            shown("Moved 2 items to Photos", true)
        );
        assert_eq!(
            classic(undo_notice(
                ActionId::CopyTo,
                "Copied 1 item to Photos".into(),
                undo(),
                5,
                || {}
            )),
            shown("Copied 1 item to Photos", true)
        );
        // A button that is not an Undo stays in the island.
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        assert_eq!(
            classic(crate::shortcut_unavailable_notice(
                "Pocket",
                "in use",
                || {}
            )),
            shown("Pocket shortcut unavailable: in use", false)
        );
        // Notices for the island only make no toast.
        assert_eq!(classic(files_copied_notice(2)), None);
        assert_eq!(classic(path_copied_notice("/a")), None);
        vento_core::i18n::with_language("ar", || {
            assert_eq!(classic(busy_notice()), shown("مشغول، العمل جارٍ", false));
        });
    }

    #[test]
    fn the_islands_undo_of_an_older_operation_does_nothing() {
        let platform: Rc<dyn Platform> = Rc::new(vento_platform::StubPlatform);
        let executor = Executor::new(
            platform.clone(),
            Rc::new(RefCell::new(Settings::default())),
            Toaster::new(platform.clone()),
            Rc::new(RefCell::new(RecentDestinations::default())),
            ProgressView::new(platform.clone()),
            PasswordView::new(platform),
        );
        let at = Point { x: 0.0, y: 0.0 };
        let op = || {
            LastOp::Move(MoveRecord {
                dest: p("/dest/Photos"),
                moved: Vec::new(),
            })
        };
        executor.remember_last(op(), at);
        let older = executor.last_serial().expect("an operation to undo");
        let undo = executor.undo_while_last(older);
        // A newer operation came while the island held the older result.
        executor.remember_last(op(), at);
        let newer = executor.last_serial();
        assert_ne!(newer, Some(older));
        undo();
        assert_eq!(executor.last_serial(), newer);
        assert!(!executor.busy.get());
    }

    #[test]
    fn an_undo_notice_names_the_operation_it_undoes() {
        let n = undo_notice(ActionId::Move, "Moved".into(), Box::new(|| {}), 7, || {});
        assert_eq!((n.tone, n.icon), (Tone::Done, Some(ActionId::Move)));
        assert_eq!(n.undo_serial, Some(7));
        assert_eq!(
            n.button.as_ref().map(|b| b.label),
            Some(crate::notice::ButtonLabel::Undo)
        );
    }

    #[test]
    fn the_islands_undo_is_always_its_own() {
        // The toast's Undo undoes the last operation; the island's undoes
        // only the one the notice names, so it never falls back to the
        // toast's.
        let (last, own) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let (l, o) = (last.clone(), own.clone());
        let n = undo_notice(
            ActionId::Move,
            "Moved".into(),
            Box::new(move || l.set(l.get() + 1)),
            7,
            move || o.set(o.get() + 1),
        );
        (n.island_undo.expect("the island's own undo"))();
        assert_eq!((last.get(), own.get()), (0, 1));
    }

    #[test]
    fn the_islands_undo_with_nothing_to_undo_does_nothing() {
        let platform: Rc<dyn Platform> = Rc::new(vento_platform::StubPlatform);
        let executor = Executor::new(
            platform.clone(),
            Rc::new(RefCell::new(Settings::default())),
            Toaster::new(platform.clone()),
            Rc::new(RefCell::new(RecentDestinations::default())),
            ProgressView::new(platform.clone()),
            PasswordView::new(platform),
        );
        assert_eq!(executor.last_serial(), None);
        (executor.undo_while_last(executor.last_serial.get()))();
        assert!(!executor.busy.get());
    }

    #[test]
    fn errors_are_error_notices_with_their_action() {
        let n = action_error_notice(ActionId::Move, "disk full");
        assert_eq!(n.tone, Tone::Error);
        assert_eq!(n.icon, Some(ActionId::Move));
        assert_eq!(n.text, "Couldn't move: disk full");
        assert!(n.sticky && n.button.is_none());
    }

    #[test]
    fn busy_is_a_busy_notice_and_working_is_info() {
        let n = busy_notice();
        assert_eq!(n.tone, Tone::Busy);
        assert_eq!(n.text, "Busy — still working");
        assert!(!n.sticky && n.button.is_none());
        let n = working_notice(ActionId::CopyTo, working_text(Working::Copying, 1));
        assert_eq!((n.tone, n.icon), (Tone::Info, Some(ActionId::CopyTo)));
    }

    #[test]
    fn nothing_to_show_is_info_and_results_are_done() {
        assert_eq!(hold_for_format_notice().tone, Tone::Info);
        let n = result_notice(ActionId::Archive, "Nothing left to remove".into());
        assert_eq!((n.tone, n.icon), (Tone::Done, Some(ActionId::Archive)));
        assert!(!n.island_only);
    }

    #[test]
    fn undo_kinds_name_the_action_that_did_it() {
        assert_eq!(UndoKind::Move.action(), ActionId::Move);
        assert_eq!(UndoKind::Copy.action(), ActionId::CopyTo);
        assert_eq!(UndoKind::Archive.action(), ActionId::Archive);
        assert_eq!(UndoKind::Convert.action(), ActionId::Convert);
        assert_eq!(UndoKind::Pdf.action(), ActionId::Pdf);
        assert_eq!(UndoKind::Rename.action(), ActionId::Rename);
    }

    #[test]
    fn copying_files_says_so_only_in_the_island() {
        let n = files_copied_notice(1);
        assert_eq!((n.tone, n.icon), (Tone::Done, Some(ActionId::Copy)));
        assert_eq!(n.text, "Copied");
        assert_eq!(n.sub, "1 file ready to paste");
        assert!(n.island_only);
        assert_eq!(files_copied_notice(3).sub, "3 files ready to paste");
    }

    #[test]
    fn copying_a_path_shows_it_cut_to_fit() {
        let n = path_copied_notice("/Users/me/Downloads/a/very/long/folder/name/report.pdf");
        assert_eq!((n.tone, n.icon), (Tone::Done, Some(ActionId::CopyPath)));
        assert_eq!(n.text, "Path copied");
        assert_eq!(n.sub.chars().count(), PATH_SUB_CHARS);
        assert!(n.sub.starts_with("/Users/") && n.sub.ends_with("report.pdf"));
        assert!(n.island_only);
        // Several paths: the first.
        assert_eq!(path_copied_notice("/a\n/b").sub, "/a");
    }
}
