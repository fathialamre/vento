//! Runs the action a file was dropped on and reports the result in a toast.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use log::{debug, info, warn};
use slint::Timer;
use vento_core::actions::ActionId;
use vento_core::fileops::{FileOpError, MoveOutcome, MoveRecord, move_items, undo};
use vento_core::geometry::Point;
use vento_core::paths::format_paths;
use vento_core::settings::Settings;
use vento_platform::Platform;

use crate::toast::Toaster;

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

/// Toast text for a finished move.
fn move_toast_text(outcome: &MoveOutcome) -> String {
    let dest = &outcome.record.dest;
    let name = dest.file_name().map_or_else(
        || dest.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    format!(
        "Moved {} to {name}{}",
        items(outcome.record.moved.len()),
        failure_suffix(outcome)
    )
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

/// Callback told whether an undo is available.
type UndoListener = Box<dyn Fn(bool)>;

/// Toast shown when a move or undo is requested while one is running.
const BUSY_TEXT: &str = "Busy — a move is still running";

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

/// The most recent move, kept so it can be undone.
struct LastMove {
    record: MoveRecord,
    /// Where toasts about this move are anchored.
    at: Point,
}

/// Performs wheel actions and reports their outcome.
pub struct Executor {
    platform: Rc<dyn Platform>,
    settings: Rc<RefCell<Settings>>,
    toast: Rc<Toaster>,
    last: RefCell<Option<LastMove>>,
    /// Whether a move or undo is running on a worker thread.
    busy: Cell<bool>,
    /// Told whether an undo is available whenever that changes.
    on_undo_changed: RefCell<Option<UndoListener>>,
    this: Weak<Self>,
}

impl Executor {
    /// `settings` is shared with the rest of the app, so a settings reload is
    /// seen by the next action. Call on the main thread.
    pub fn new(
        platform: Rc<dyn Platform>,
        settings: Rc<RefCell<Settings>>,
        toast: Rc<Toaster>,
    ) -> Rc<Self> {
        let executor = Rc::new_cyclic(|this| Self {
            platform,
            settings,
            toast,
            last: RefCell::new(None),
            busy: Cell::new(false),
            on_undo_changed: RefCell::new(None),
            this: this.clone(),
        });
        MAIN_EXECUTOR.with(|slot| *slot.borrow_mut() = Rc::downgrade(&executor));
        executor
    }

    /// Runs `action` on `paths`; toasts anchor at `at`.
    pub fn run(&self, action: ActionId, paths: Vec<PathBuf>, at: Point) {
        if paths.is_empty() {
            debug!("{action:?} with no files");
            return;
        }
        match action {
            ActionId::Pocket => {
                warn!("Pocket is handled by the Pocket controller");
            }
            ActionId::CopyTo => {
                warn!("Copy to is not wired yet");
            }
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
            ActionId::Move => {
                if self.refuse_if_busy(at) {
                    return;
                }
                // The folder picker is modal: let the wheel finish handling
                // the drop before it opens.
                let this = self.this.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(executor) = this.upgrade() {
                        executor.move_with_picker(paths, at);
                    }
                });
            }
        }
    }

    /// Starts undoing the last move on a worker thread. Returns `false` if
    /// there is nothing to undo.
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
        self.toast.show(
            &format!("Restoring {}…", items(last.record.moved.len())),
            None,
            at,
        );
        let record = last.record;
        self.spawn_file_job(
            move || {
                let outcome = undo(&record);
                (record, outcome)
            },
            move |executor, (record, outcome)| executor.undo_finished(&record, outcome, at),
            at,
        );
        true
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
                *last = Some(LastMove {
                    record: remainder,
                    at,
                });
            }
        }
        self.notify_undo_changed();
        self.toast.show(&undo_toast_text(&outcome), None, at);
    }

    /// Registers `f`, called with `has_undo()` after a move records an undo
    /// and after an undo starts or finishes. Replaces any earlier callback.
    pub fn on_undo_changed(&self, f: UndoListener) {
        *self.on_undo_changed.borrow_mut() = Some(f);
    }

    fn notify_undo_changed(&self) {
        if let Some(f) = self.on_undo_changed.borrow().as_ref() {
            f(self.has_undo());
        }
    }

    /// Whether there is a move to undo.
    pub fn has_undo(&self) -> bool {
        self.last.borrow().is_some()
    }

    /// Toasts and returns `true` while a move or undo is running.
    fn refuse_if_busy(&self, at: Point) -> bool {
        if self.busy.get() {
            debug!("busy: request ignored");
            self.toast.show(BUSY_TEXT, None, at);
        }
        self.busy.get()
    }

    fn move_with_picker(&self, paths: Vec<PathBuf>, at: Point) {
        let start = paths.first().and_then(|p| p.parent());
        let Some(dest) = self.platform.pick_folder(start) else {
            debug!("move cancelled");
            return;
        };
        if self.refuse_if_busy(at) {
            return;
        }
        self.toast
            .show(&format!("Moving {}…", items(paths.len())), None, at);
        self.spawn_file_job(
            move || move_items(&paths, &dest),
            move |executor, outcome| executor.move_finished(outcome, at),
            at,
        );
    }

    fn move_finished(&self, outcome: MoveOutcome, at: Point) {
        for (path, error) in &outcome.failed {
            warn!("cannot move {}: {error}", path.display());
        }
        if outcome.record.moved.is_empty() {
            match outcome.failed.first() {
                Some((_, error)) => self.toast_error(ActionId::Move, &error.to_string(), at),
                None => {
                    debug!("nothing to move");
                    self.toast.show("Nothing to move", None, at);
                }
            }
            return;
        }
        info!(
            "moved {} to {}",
            outcome.record.moved.len(),
            outcome.record.dest.display()
        );
        let text = move_toast_text(&outcome);
        *self.last.borrow_mut() = Some(LastMove {
            record: outcome.record,
            at,
        });
        self.notify_undo_changed();
        let this = self.this.clone();
        let on_undo: Box<dyn Fn()> = Box::new(move || {
            if let Some(executor) = this.upgrade() {
                executor.undo_last();
            }
        });
        self.toast.show(&text, Some(on_undo), at);
    }

    /// Runs `job` on a worker thread, then `done` with its result on the main
    /// thread. Marks the executor busy until `done` runs.
    fn spawn_file_job<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&Executor, T) + Send + 'static,
        at: Point,
    ) {
        self.busy.set(true);
        let spawned = std::thread::Builder::new()
            .name("vento-fileops".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let delivered = slint::invoke_from_event_loop(move || {
                    let Some(executor) = MAIN_EXECUTOR.with(|slot| slot.borrow().upgrade()) else {
                        return;
                    };
                    executor.busy.set(false);
                    match result {
                        Ok(value) => done(&executor, value),
                        Err(_) => executor.toast_error(ActionId::Move, "internal error", at),
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report a finished file operation: {e}");
                }
            });
        if let Err(e) = spawned {
            self.busy.set(false);
            self.toast_error(ActionId::Move, &format!("cannot start: {e}"), at);
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
    use vento_core::fileops::MoveRecord;

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
