//! Archive: compresses the dropped items into a zip, or extracts the
//! dropped archives, on a worker thread, with the progress and password
//! windows following the job.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use log::{debug, info, warn};
use vento_core::actions::ActionId;
use vento_core::archive::{
    ArchiveError, BatchOutcome, Made, PasswordAsk, Plan, Progress, Zeroizing, compress,
    extract_batch, plan,
};
use vento_core::fileops::{CopyRecord, CopyUndoOutcome};
use vento_core::geometry::Point;
use vento_core::i18n::errors::clauses;
use vento_core::i18n::{fill, tr, trn};
use vento_platform::{quarantine_of, set_quarantine};

use super::{Executor, LastOp, display_name, on_main, only_name};
use crate::archive_ui::Verb;

/// Toast for a job the user cancelled.
pub(super) fn cancelled_text() -> String {
    tr("Cancelled")
}

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
        ArchiveResult::Compressed { count, output } => fill(
            &trn(
                "Compressed {n} item to {name}",
                "Compressed {n} items to {name}",
                *count as u64,
            ),
            &[("name", output)],
        ),
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
        return cancelled_text();
    }
    let reason = outcome
        .failed
        .first()
        .map(|(_, error)| error.to_string())
        .unwrap_or_default();
    let mut parts = Vec::new();
    if made == 0 {
        let names: Vec<&Path> = outcome.failed.iter().map(|(a, _)| a.as_path()).collect();
        parts.push(match only_name(&names) {
            Some(name) => fill(
                &tr("Couldn't extract {name}: {reason}"),
                &[("name", &name), ("reason", &reason)],
            ),
            None => fill(
                &trn(
                    "Couldn't extract {n} archive: {reason}",
                    "Couldn't extract {n} archives: {reason}",
                    names.len() as u64,
                ),
                &[("reason", &reason)],
            ),
        });
    } else {
        if outcome.cancelled || failed + skipped == 0 {
            let names: Vec<&Path> = outcome.made.iter().map(|m| m.source.as_path()).collect();
            parts.push(match only_name(&names) {
                Some(name) => fill(&tr("Extracted {name}"), &[("name", &name)]),
                None => trn(
                    "Extracted {n} archive",
                    "Extracted {n} archives",
                    names.len() as u64,
                ),
            });
        } else {
            parts.push(fill(
                &tr("Extracted {made} of {total}"),
                &[
                    ("made", &made.to_string()),
                    ("total", &(made + failed + skipped).to_string()),
                ],
            ));
        }
        if failed > 0 {
            parts.push(failed_with_reason(failed, &reason));
        }
    }
    if skipped > 0 {
        parts.push(trn("{n} skipped", "{n} skipped", skipped as u64));
    }
    if outcome.cancelled {
        parts.push(tr("cancelled"));
    }
    clauses(&parts)
}

/// `1 failed: reason`, the clause for `n` items that failed, the first for
/// `reason`.
pub(super) fn failed_with_reason(n: usize, reason: &str) -> String {
    fill(
        &trn("{n} failed: {reason}", "{n} failed: {reason}", n as u64),
        &[("reason", reason)],
    )
}

/// Toast text for a finished undo of an Archive job; `trashed` are the
/// outputs that went to the Trash.
pub(super) fn archive_undo_toast_text(outcome: &CopyUndoOutcome, trashed: &[PathBuf]) -> String {
    if trashed.is_empty() && outcome.kept_changed.is_empty() && outcome.failed.is_empty() {
        return tr("Nothing left to remove");
    }
    let mut parts = Vec::new();
    if !trashed.is_empty() {
        let names: Vec<&Path> = trashed.iter().map(PathBuf::as_path).collect();
        parts.push(match only_name(&names) {
            Some(name) => fill(&tr("Moved {name} to the Trash"), &[("name", &name)]),
            None => trn(
                "Moved {n} item to the Trash",
                "Moved {n} items to the Trash",
                names.len() as u64,
            ),
        });
    }
    if !outcome.kept_changed.is_empty() {
        let names: Vec<&Path> = outcome.kept_changed.iter().map(PathBuf::as_path).collect();
        let n = names.len() as u64;
        // English starts a sentence with a capital, so the clause that comes
        // first is worded apart from the ones after it.
        let first = parts.is_empty();
        parts.push(match (only_name(&names), first) {
            (Some(name), true) => fill(
                &tr("Kept {name}: changed since it was made"),
                &[("name", &name)],
            ),
            (Some(name), false) => fill(
                &tr("kept {name}: changed since it was made"),
                &[("name", &name)],
            ),
            (None, true) => trn(
                "Kept {n} item: changed since it was made",
                "Kept {n} items: changed since they were made",
                n,
            ),
            (None, false) => trn(
                "kept {n} item: changed since it was made",
                "kept {n} items: changed since they were made",
                n,
            ),
        });
    }
    if outcome.gone > 0 {
        parts.push(trn(
            "{n} no longer there",
            "{n} no longer there",
            outcome.gone as u64,
        ));
    }
    if let Some((_, reason)) = outcome.failed.first() {
        parts.push(failed_with_reason(outcome.failed.len(), reason));
    }
    clauses(&parts)
}

/// A password on its way from the password window to the worker.
type Password = Zeroizing<Vec<u8>>;

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
fn archive_job(paths: &[PathBuf], progress: &Arc<Progress>, at: Point) -> ArchiveDone {
    match plan(paths) {
        Plan::Compress { dir, name } => {
            show_progress(progress, Verb::Compressing, at);
            ArchiveDone::Compressed {
                count: paths.len(),
                result: compress(paths, &dir, &name, progress),
            }
        }
        Plan::Extract(archives) => {
            show_progress(progress, Verb::Extracting, at);
            let mark = copy_quarantine();
            let mut ask = |archive: &Path, why| request_password(archive, why, progress, at);
            ArchiveDone::Extracted(extract_batch(&archives, progress, &mark, &mut ask))
        }
    }
}

/// Has the main thread's progress window follow `progress`.
pub(super) fn show_progress(progress: &Arc<Progress>, verb: Verb, at: Point) {
    let progress = Arc::clone(progress);
    if let Err(e) = on_main(move |executor| executor.progress.track(progress, verb, at)) {
        warn!("cannot show the progress: {e}");
    }
}

/// The `mark` of an extraction or conversion: gives everything made from a
/// source (an archive, an image) that source's quarantine attribute, read
/// once per source. A failure fails that source's job.
pub(super) fn copy_quarantine() -> impl Fn(&Path, &Path) -> io::Result<()> {
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

impl Executor {
    /// Compresses `paths` into a zip, or extracts them when they are all
    /// archives, on a worker thread.
    pub(super) fn archive(&self, paths: Vec<PathBuf>, at: Point) {
        if self.refuse_if_busy(at) {
            return;
        }
        let progress = Progress::new();
        let cancel = Some(Arc::clone(&progress));
        self.spawn_file_job(
            ActionId::Archive,
            move || archive_job(&paths, &progress, at),
            move |executor, done| executor.archive_finished(done, at),
            cancel,
            at,
        );
    }

    /// Opens the password window for the archive called `name` in place of
    /// the progress window, which comes back once the window is answered.
    /// `answer` gets the password, or `None`; it is dropped unanswered (also
    /// `None` to the worker) when the job is already cancelled. Nothing
    /// can cancel the job while the window is open except
    /// [`Executor::shutdown`], which closes the window itself, with `None`.
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
        // Takes the progress window's Cancel away with it.
        self.progress.close();
        let this = self.this.clone();
        self.password.ask(
            name,
            wrong,
            at,
            Box::new(move |password| {
                if let Some(executor) = this.upgrade()
                    && !progress.is_cancelled()
                {
                    executor.progress.track(progress, Verb::Extracting, at);
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
            } => (cancelled_text(), Vec::new()),
            ArchiveDone::Compressed { result: Err(e), .. } => {
                warn!("couldn't compress: {e}");
                let text = fill(
                    &tr("Couldn't compress: {reason}"),
                    &[("reason", &e.to_string())],
                );
                (text, Vec::new())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::fileops::Fingerprint;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
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
    fn archive_toasts_read_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            let compressed = |count, output: &str| ArchiveResult::Compressed {
                count,
                output: output.into(),
            };
            assert_eq!(
                archive_toast_text(&compressed(2, "Archive.zip")),
                "تم ضغط عنصرين في Archive.zip"
            );
            assert_eq!(
                archive_toast_text(&compressed(11, "Archive.zip")),
                "تم ضغط 11 عنصراً في Archive.zip"
            );
            // A name stands in for the count: never "1 items".
            let one = extracted(vec![made("/d/Photos.zip", "/d/Photos")], vec![]);
            assert_eq!(
                archive_toast_text(&ArchiveResult::Extracted(&one)),
                "تم فك ضغط Photos.zip"
            );
            let two = extracted(
                vec![made("/d/a.zip", "/d/a"), made("/d/b.7z", "/d/b")],
                vec![],
            );
            assert_eq!(
                archive_toast_text(&ArchiveResult::Extracted(&two)),
                "تم فك ضغط أرشيفين"
            );
            // The detail after "damaged archive:" stays as the library words it.
            let partial = BatchOutcome {
                made: vec![made("/d/a.zip", "/d/a")],
                failed: vec![damaged("/d/b.zip")],
                skipped: vec![p("/d/c.zip")],
                cancelled: false,
            };
            assert_eq!(
                archive_toast_text(&ArchiveResult::Extracted(&partial)),
                "تم فك ضغط 1 من 3؛ تعذّرت معالجة عنصر واحد: الأرشيف تالف: crc؛ تم تخطي عنصر واحد"
            );
            let all_failed = extracted(vec![], vec![damaged("/d/a.zip"), damaged("/d/b.zip")]);
            assert_eq!(
                archive_toast_text(&ArchiveResult::Extracted(&all_failed)),
                "تعذّر فك ضغط أرشيفين: الأرشيف تالف: crc"
            );
            let lone = extracted(vec![], vec![damaged("/d/Photos.zip")]);
            assert_eq!(
                archive_toast_text(&ArchiveResult::Extracted(&lone)),
                "تعذّر فك ضغط Photos.zip: الأرشيف تالف: crc"
            );
            let cancelled = BatchOutcome {
                made: vec![made("/d/Photos.zip", "/d/Photos")],
                cancelled: true,
                ..Default::default()
            };
            assert_eq!(
                archive_toast_text(&ArchiveResult::Extracted(&cancelled)),
                "تم فك ضغط Photos.zip؛ أُلغي"
            );
            assert_eq!(
                archive_toast_text(&ArchiveResult::Extracted(&BatchOutcome::default())),
                "أُلغي"
            );
        });
    }

    #[test]
    fn archive_undo_toasts_read_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            let one = [p("/d/Archive.zip")];
            let trashed = |n| CopyUndoOutcome {
                trashed: n,
                ..Default::default()
            };
            assert_eq!(
                archive_undo_toast_text(&trashed(1), &one),
                "تم نقل Archive.zip إلى سلة المهملات"
            );
            let two = [p("/d/a"), p("/d/b")];
            assert_eq!(
                archive_undo_toast_text(&trashed(2), &two),
                "تم نقل عنصرين إلى سلة المهملات"
            );
            let kept = CopyUndoOutcome {
                kept_changed: vec![p("/d/Photos")],
                ..Default::default()
            };
            assert_eq!(
                archive_undo_toast_text(&kept, &[]),
                "أُبقي Photos: تغيّر بعد إنشائه"
            );
            let mixed = CopyUndoOutcome {
                trashed: 1,
                kept_changed: vec![p("/d/x"), p("/d/y")],
                gone: 1,
                failed: vec![(p("/d/z"), "denied".into())],
            };
            assert_eq!(
                archive_undo_toast_text(&mixed, &one),
                "تم نقل Archive.zip إلى سلة المهملات؛ \
                 تم إبقاء عنصرين: تغيّرا بعد إنشائهما؛ \
                 لم يُعثر على عنصر واحد بعد الآن؛ \
                 تعذّرت معالجة عنصر واحد: denied"
            );
            assert_eq!(
                archive_undo_toast_text(
                    &CopyUndoOutcome {
                        gone: 1,
                        ..Default::default()
                    },
                    &[]
                ),
                "لم يبقَ شيء لإزالته"
            );
        });
    }

    #[test]
    fn a_kept_clause_starts_with_a_capital_only_when_it_comes_first() {
        // Two items kept after one went to the Trash: the clause follows
        // another, so it starts in lower case.
        let one = [p("/d/Archive.zip")];
        let mixed = CopyUndoOutcome {
            trashed: 1,
            kept_changed: vec![p("/d/x"), p("/d/y")],
            ..Default::default()
        };
        assert_eq!(
            archive_undo_toast_text(&mixed, &one),
            "Moved Archive.zip to the Trash; kept 2 items: changed since they were made"
        );
        let first = CopyUndoOutcome {
            kept_changed: vec![p("/d/x"), p("/d/y")],
            ..Default::default()
        };
        assert_eq!(
            archive_undo_toast_text(&first, &[]),
            "Kept 2 items: changed since they were made"
        );
    }

    // Quarantine is a macOS attribute; elsewhere nothing is marked.
    #[cfg(target_os = "macos")]
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
}
