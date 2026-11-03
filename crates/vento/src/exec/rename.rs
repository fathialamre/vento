//! Rename: runs the Rename window's batch rename on the file worker, words
//! the toasts, records Undo and puts the names back.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use log::{error, info, warn};
use vento_core::actions::ActionId;
use vento_core::geometry::Point;
use vento_core::i18n::tr;
use vento_core::rename::{
    Os, RealDisk, RenameOutcome, RenameRecord, Step, UndoOutcome, changed_text, done_text, execute,
    failed_text, undo, undo_text,
};

use super::{Executor, LastOp, error_notice, result_notice};
use crate::notice::Notice;

/// Callback told, on the main thread, how a rename ended, so the Rename
/// window can close or show the problem.
pub type RenameDone = Box<dyn FnOnce(&RenameOutcome)>;

/// Toast text for a finished rename: what was renamed, that the folder
/// changed, or which item failed (with a line per item the reversal could
/// not put back).
fn rename_toast_text(outcome: &RenameOutcome) -> String {
    match outcome {
        RenameOutcome::Done(record) => done_text(record),
        RenameOutcome::Changed => changed_text(),
        RenameOutcome::Failed {
            path,
            reason,
            stuck,
        } => failed_text(path, reason, stuck),
    }
}

/// The notice for a finished rename that put nothing in the Undo slot, `text`
/// being what [`rename_toast_text`] said: a folder that changed is something
/// to know, a failure is a failure, and names changed without a way back
/// are done.
fn rename_done_notice(outcome: &RenameOutcome, text: String) -> Notice {
    match outcome {
        RenameOutcome::Done(_) => result_notice(ActionId::Rename, text),
        RenameOutcome::Changed => Notice::info(text).with_icon(ActionId::Rename),
        RenameOutcome::Failed { .. } => error_notice(ActionId::Rename, text),
    }
}

/// Logs each item a reversal left under the wrong name: where it is now, and
/// the name it should have.
fn log_stuck(stuck: &[(PathBuf, PathBuf)]) {
    for (now, old) in stuck {
        error!(
            "rename: {} is stuck, it should be {}",
            now.display(),
            old.display()
        );
    }
}

impl Executor {
    /// Runs `steps` on the file worker, then, on the main thread, runs
    /// `done` and toasts the result (with Undo when names changed). The Rename
    /// window shows its own progress from `progress`. While another job is
    /// running, toasts Busy and never calls `done`, so the window stays as
    /// it is.
    pub fn rename_job(
        &self,
        steps: Vec<Step>,
        done: RenameDone,
        progress: Arc<AtomicUsize>,
        at: Point,
    ) {
        if self.refuse_if_busy(at) {
            return;
        }
        *self.rename_done.borrow_mut() = Some(done);
        let first = steps.first().map(|s| s.from.clone()).unwrap_or_default();
        self.spawn_file_job(
            ActionId::Rename,
            move || execute(&steps, &RealDisk, Os::HOST, &progress),
            move |executor, outcome| executor.rename_finished(outcome, at),
            None,
            at,
        );
        if !self.busy.get() {
            // The worker could not start (already toasted): the window must
            // still hear that nothing was renamed.
            // Taken first, so no borrow is held while `done` runs.
            let done = self.rename_done.borrow_mut().take();
            if let Some(done) = done {
                done(&RenameOutcome::Failed {
                    path: first,
                    reason: tr("cannot start"),
                    stuck: Vec::new(),
                });
            }
        }
    }

    /// Tells the window first (it closes on success), then toasts and,
    /// when names changed that can be put back, records Undo.
    fn rename_finished(&self, outcome: RenameOutcome, at: Point) {
        let notice = rename_done_notice(&outcome, rename_toast_text(&outcome));
        // Taken first, so no borrow is held while `done` runs (it may start
        // another job).
        let done = self.rename_done.borrow_mut().take();
        if let Some(done) = done {
            done(&outcome);
        }
        // The reasons are in the toast's language; the core logged the
        // underlying error in English where it happened.
        match outcome {
            RenameOutcome::Done(record) => {
                info!(
                    "rename: renamed {}, {} can be undone",
                    record.renamed.len(),
                    record.items.len()
                );
                if record.items.is_empty() {
                    // No print could be read, so nothing can be put back.
                    self.toast.notify(notice, at);
                } else {
                    self.record_undo(LastOp::Rename(record), notice.text, at);
                }
            }
            RenameOutcome::Changed => {
                info!("rename: the folder changed, nothing renamed");
                self.toast.notify(notice, at);
            }
            RenameOutcome::Failed { path, stuck, .. } => {
                warn!("rename failed at {}, reversed", path.display());
                log_stuck(&stuck);
                self.toast.notify(notice, at);
            }
        }
    }

    /// Puts back the names of the last rename on the file worker.
    pub(super) fn undo_rename(&self, record: RenameRecord, at: Point) {
        self.spawn_file_job(
            ActionId::Rename,
            move || undo(&record, &RealDisk, Os::HOST),
            move |executor, outcome| executor.rename_undo_finished(&outcome, at),
            None,
            at,
        );
    }

    fn rename_undo_finished(&self, outcome: &UndoOutcome, at: Point) {
        if let Some((path, _)) = &outcome.failed {
            warn!("undo: put-back failed at {}, reversed", path.display());
        }
        if outcome.changed {
            info!("undo: the folder changed since its check, nothing put back");
        }
        log_stuck(&outcome.stuck);
        info!(
            "undo: put back {}, skipped {} changed since",
            outcome.put_back, outcome.skipped
        );
        self.notify_undo_changed();
        self.toast
            .notify(result_notice(ActionId::Rename, undo_text(outcome)), at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn toast_changed() {
        assert_eq!(
            rename_toast_text(&RenameOutcome::Changed),
            "Something changed in the folder. Check the list and try again."
        );
    }

    #[test]
    fn toast_failed_names_the_item() {
        let outcome = RenameOutcome::Failed {
            path: p("/d/IMG_4823.HEIC"),
            reason: "permission denied".into(),
            stuck: Vec::new(),
        };
        assert_eq!(
            rename_toast_text(&outcome),
            "Couldn't rename IMG_4823.HEIC: permission denied. Nothing was changed."
        );
    }

    #[test]
    fn toast_failed_lists_stuck_items() {
        let outcome = RenameOutcome::Failed {
            path: p("/d/a.jpg"),
            reason: "disk removed".into(),
            stuck: vec![
                (p("/d/.vento-rename-0123abcd-0"), p("/d/a.jpg")),
                (p("/d/b2.jpg"), p("/d/b.jpg")),
            ],
        };
        assert_eq!(
            rename_toast_text(&outcome),
            "Couldn't rename a.jpg: disk removed. These items could not be put back:\n\
             .vento-rename-0123abcd-0 should be a.jpg\n\
             b2.jpg should be b.jpg"
        );
    }

    #[test]
    fn rename_notices_follow_how_the_rename_ended() {
        use vento_core::news::Tone;
        let notice = |o: &RenameOutcome| rename_done_notice(o, rename_toast_text(o));
        let n = notice(&RenameOutcome::Changed);
        assert_eq!((n.tone, n.icon), (Tone::Info, Some(ActionId::Rename)));
        assert_eq!(
            n.text,
            "Something changed in the folder. Check the list and try again."
        );
        let failed = RenameOutcome::Failed {
            path: p("/d/a.jpg"),
            reason: "permission denied".into(),
            stuck: Vec::new(),
        };
        let n = notice(&failed);
        assert_eq!((n.tone, n.icon), (Tone::Error, Some(ActionId::Rename)));
        assert!(n.text.starts_with("Couldn't rename a.jpg"));
        let done = RenameOutcome::Done(RenameRecord {
            renamed: vec![p("/d/b.jpg")],
            items: Vec::new(),
        });
        assert_eq!(notice(&done).tone, Tone::Done);
    }
}
