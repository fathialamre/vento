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
    failed_text, stuck_text, undo, undo_text,
};

use super::{Executor, LastOp, in_english};

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
        } => {
            let mut text = failed_text(path, reason);
            if !stuck.is_empty() {
                text.push('\n');
                text.push_str(&stuck_text(stuck));
            }
            text
        }
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
    /// Runs `steps` on the file worker, then toasts the result (with Undo
    /// when names changed) and runs `done` on the main thread. The Rename
    /// window shows its own progress from `progress`. While another job is
    /// running, toasts Busy and never calls `done`, so the window stays as
    /// it is.
    #[expect(
        dead_code,
        reason = "called by the Rename window controller, not wired yet"
    )]
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
            if let Some(done) = self.rename_done.borrow_mut().take() {
                done(&RenameOutcome::Failed {
                    path: first,
                    reason: tr("cannot start"),
                    stuck: Vec::new(),
                });
            }
        }
    }

    fn rename_finished(&self, outcome: RenameOutcome, at: Point) {
        let text = rename_toast_text(&outcome);
        match &outcome {
            RenameOutcome::Done(record) => {
                info!("rename: renamed {}", record.items.len());
                if record.items.is_empty() {
                    // Nothing could be fingerprinted, so nothing can be put back.
                    self.toast.show(&text, None, at);
                } else {
                    self.record_undo(LastOp::Rename(record.clone()), text, at);
                }
            }
            RenameOutcome::Changed => {
                info!("rename: the folder changed, nothing renamed");
                self.toast.show(&text, None, at);
            }
            RenameOutcome::Failed {
                path,
                reason,
                stuck,
            } => {
                warn!("cannot rename {}: {reason}", path.display());
                log_stuck(stuck);
                self.toast.show(&text, None, at);
            }
        }
        // Taken before the call, so `done` may start another job.
        let done = self.rename_done.borrow_mut().take();
        if let Some(done) = done {
            done(&outcome);
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
        if let Some((path, reason)) = &outcome.failed {
            warn!("cannot put back {}: {reason}", path.display());
        }
        log_stuck(&outcome.stuck);
        info!("undo: {}", in_english(|| undo_text(outcome)));
        self.notify_undo_changed();
        self.toast.show(&undo_text(outcome), None, at);
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
            "Couldn't rename a.jpg: disk removed. Nothing was changed.\n\
             .vento-rename-0123abcd-0 should be a.jpg\n\
             b2.jpg should be b.jpg"
        );
    }
}
