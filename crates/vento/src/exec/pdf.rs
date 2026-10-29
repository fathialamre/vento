//! PDF Pages: runs the window's write jobs on the file worker, words the
//! toasts, and records Undo.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{info, warn};
use vento_core::actions::ActionId;
use vento_core::archive::Progress;
use vento_core::fileops::CopyRecord;
use vento_core::geometry::Point;
use vento_core::i18n::{fill, tr, trn};
use vento_core::pdf::{JobKind, PageRef, PdfError, PdfJob, PdfOutcome, Source, run_job};
use vento_platform::{SystemPdf, quarantine_of, set_quarantine};

use super::archive::cancelled_text;
use super::{Executor, LastOp, display_name, in_english};

/// Callback told, on the main thread, how a PDF job ended. It returns
/// whether it showed the result itself (the PDF window, staying open, says
/// it in its footer): then there is no toast.
pub type PdfDone = Box<dyn FnOnce(&PdfOutcome) -> bool>;

/// Toast text for a finished PDF write job. A file (Save, Extract, a single
/// image) is named; a folder (Split, several images) is named with how many
/// files it holds. The PDF window's footer says the same.
pub fn pdf_toast_text(outcome: &PdfOutcome) -> String {
    if outcome.cancelled || matches!(outcome.failed, Some(PdfError::Cancelled)) {
        return cancelled_text();
    }
    let couldnt = |reason: &str| fill(&tr("Couldn't save: {reason}"), &[("reason", reason)]);
    if let Some(error) = &outcome.failed {
        return couldnt(&error.to_string());
    }
    let Some(made) = outcome.made.first() else {
        return couldnt(&tr("nothing to write"));
    };
    let name = display_name(&made.output);
    let n = outcome.files as u64;
    let saved_file = || fill(&tr("Saved {name}"), &[("name", &name)]);
    match outcome.job {
        JobKind::Save | JobKind::Extract => saved_file(),
        JobKind::Split => fill(
            &trn(
                "Saved {n} file in {folder}",
                "Saved {n} files in {folder}",
                n,
            ),
            &[("folder", &name)],
        ),
        // One page is a file beside the PDF, several are a folder.
        JobKind::Images if outcome.files <= 1 => saved_file(),
        JobKind::Images => fill(
            &trn(
                "Saved {n} image in {folder}",
                "Saved {n} images in {folder}",
                n,
            ),
            &[("folder", &name)],
        ),
    }
}

/// Toast text for opening PDFs where some paths were left out (`skipped`,
/// with why), or `None` when nothing was.
pub fn pdf_open_toast_text(opened: usize, skipped: &[(PathBuf, PdfError)]) -> Option<String> {
    let (path, error) = skipped.first()?;
    if opened == 0 {
        return Some(match (skipped.len(), error) {
            (1, PdfError::Locked) => {
                fill(&tr("{name} is locked"), &[("name", &display_name(path))])
            }
            _ => tr("No PDFs to open"),
        });
    }
    Some(fill(
        &trn(
            "Opened {n} PDF; skipped {m}: {reason}",
            "Opened {n} PDFs; skipped {m}: {reason}",
            opened as u64,
        ),
        &[
            ("m", &skipped.len().to_string()),
            ("reason", &error.to_string()),
        ],
    ))
}

/// The quarantine value of the first source, in page order, that has one.
/// Each source is asked once.
fn first_quarantine(
    sources: &[Source],
    pages: &[PageRef],
    read: impl Fn(&Path) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let mut asked = vec![false; sources.len()];
    for page in pages {
        let Some(source) = sources.get(page.source) else {
            continue;
        };
        if !std::mem::replace(&mut asked[page.source], true)
            && let Some(value) = read(&source.path)
        {
            return Some(value);
        }
    }
    None
}

/// Gives `target` the quarantine `value` and, when it is a folder, everything
/// inside it.
fn mark_tree(
    target: &Path,
    value: &[u8],
    set: &dyn Fn(&Path, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    set(target, value)?;
    // `file_type` does not follow links: only what vento wrote is marked.
    if std::fs::symlink_metadata(target)?.is_dir() {
        for entry in std::fs::read_dir(target)? {
            mark_tree(&entry?.path(), value, set)?;
        }
    }
    Ok(())
}

/// The worker's half of a PDF job: runs it with the real PDF library and
/// marks what it makes with the quarantine of the first source that has one
/// (spec 3.8). A panic ends the job as a failure, so the window still hears
/// of it.
fn pdf_work(
    job: &PdfJob,
    sources: &[Source],
    pages: &[PageRef],
    progress: &Progress,
) -> PdfOutcome {
    let run = || {
        let quarantine = first_quarantine(sources, pages, quarantine_of);
        let mark = |_anchor: &Path, made: &Path| match &quarantine {
            Some(value) => mark_tree(made, value, &set_quarantine),
            None => Ok(()),
        };
        run_job(job, sources, pages, &SystemPdf, progress, &mark)
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| PdfOutcome {
        job: match job {
            PdfJob::Save => JobKind::Save,
            PdfJob::Extract(_) => JobKind::Extract,
            PdfJob::Split(_) => JobKind::Split,
            PdfJob::Images { .. } => JobKind::Images,
        },
        made: Vec::new(),
        files: 0,
        failed: Some(PdfError::Backend(tr("internal error"))),
        cancelled: false,
    })
}

impl Executor {
    /// Writes `job` over the grid `pages` of `sources` on the file worker,
    /// then runs `done` on the main thread and toasts the result with Undo
    /// unless `done` showed it. The
    /// PDF window shows its own progress, so `progress` is only handed to the
    /// job (and cancelled when vento quits). Returns `false`, with a Busy
    /// toast and `done` never called, when another job is running.
    pub fn pdf_job(
        &self,
        job: PdfJob,
        sources: Arc<[Source]>,
        pages: Vec<PageRef>,
        progress: Arc<Progress>,
        at: Point,
        done: PdfDone,
    ) -> bool {
        if self.refuse_if_busy(at) {
            return false;
        }
        *self.pdf_done.borrow_mut() = Some(done);
        let cancel = Some(Arc::clone(&progress));
        self.spawn_file_job(
            ActionId::Pdf,
            move || pdf_work(&job, &sources, &pages, &progress),
            move |executor, outcome| executor.pdf_finished(outcome, at),
            cancel,
            at,
        );
        if !self.busy.get() {
            // The worker could not start (already toasted): the window must
            // still hear that nothing was written.
            self.pdf_finished(
                PdfOutcome {
                    job: JobKind::Save,
                    made: Vec::new(),
                    files: 0,
                    failed: Some(PdfError::Backend(tr("cannot start"))),
                    cancelled: false,
                },
                at,
            );
        }
        true
    }

    fn pdf_finished(&self, outcome: PdfOutcome, at: Point) {
        if let Some(error) = &outcome.failed
            && !matches!(error, PdfError::Cancelled)
        {
            warn!("cannot write the PDF: {}", in_english(|| error.to_string()));
        }
        let text = pdf_toast_text(&outcome);
        info!("pdf: {}", in_english(|| pdf_toast_text(&outcome)));
        let dest = outcome
            .made
            .first()
            .and_then(|m| m.output.parent())
            .map(Path::to_path_buf);
        let recorded = match dest {
            Some(dest) => {
                let record = CopyRecord {
                    dest,
                    copies: outcome
                        .made
                        .iter()
                        .map(|m| (m.source.clone(), m.output.clone(), m.fingerprint.clone()))
                        .collect(),
                };
                self.remember_last(LastOp::Pdf(record), at);
                true
            }
            None => false,
        };
        // Taken before the call, so `done` may start another job.
        let done = self.pdf_done.borrow_mut().take();
        let shown = done.is_some_and(|done| done(&outcome));
        if !shown {
            if recorded {
                self.toast_undo(&text, at);
            } else {
                self.toast.show(&text, None, at);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::archive::Made;
    use vento_core::fileops::Fingerprint;
    use vento_core::i18n::with_language;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn made(output: &str) -> Made {
        Made {
            source: p("/d/report.pdf"),
            output: p(output),
            fingerprint: Fingerprint::Link {
                target: p("/t"),
                id: Default::default(),
            },
        }
    }

    fn outcome(job: JobKind, output: &str, files: usize) -> PdfOutcome {
        PdfOutcome {
            job,
            made: vec![made(output)],
            files,
            failed: None,
            cancelled: false,
        }
    }

    fn saved(output: &str) -> PdfOutcome {
        outcome(JobKind::Save, output, 1)
    }

    fn split(folder: &str, files: usize) -> PdfOutcome {
        outcome(JobKind::Split, folder, files)
    }

    fn images(folder: &str, files: usize) -> PdfOutcome {
        outcome(JobKind::Images, folder, files)
    }

    fn failed(error: PdfError) -> PdfOutcome {
        PdfOutcome {
            job: JobKind::Save,
            made: Vec::new(),
            files: 0,
            failed: Some(error),
            cancelled: false,
        }
    }

    #[test]
    fn toast_save() {
        assert_eq!(
            pdf_toast_text(&saved("/d/report + scan.pdf")),
            "Saved report + scan.pdf"
        );
    }

    #[test]
    fn toast_extract_names_the_file() {
        let o = outcome(JobKind::Extract, "/d/report (pages 3–5).pdf", 1);
        assert_eq!(pdf_toast_text(&o), "Saved report (pages 3–5).pdf");
    }

    #[test]
    fn toast_split() {
        assert_eq!(
            pdf_toast_text(&split("/d/report split", 3)),
            "Saved 3 files in report split"
        );
    }

    #[test]
    fn toast_split_of_one_file_is_singular() {
        assert_eq!(
            pdf_toast_text(&split("/d/report split", 1)),
            "Saved 1 file in report split"
        );
    }

    #[test]
    fn toast_images() {
        assert_eq!(
            pdf_toast_text(&images("/d/report images", 8)),
            "Saved 8 images in report images"
        );
    }

    #[test]
    fn toast_one_image_is_a_file() {
        assert_eq!(
            pdf_toast_text(&images("/d/report p1.png", 1)),
            "Saved report p1.png"
        );
    }

    #[test]
    fn toast_failed() {
        assert_eq!(
            pdf_toast_text(&failed(PdfError::Backend("boom".into()))),
            "Couldn't save: boom"
        );
    }

    #[test]
    fn toast_cancelled_is_plain() {
        let o = PdfOutcome {
            job: JobKind::Split,
            made: Vec::new(),
            files: 0,
            failed: None,
            cancelled: true,
        };
        assert_eq!(pdf_toast_text(&o), "Cancelled");
        assert_eq!(pdf_toast_text(&failed(PdfError::Cancelled)), "Cancelled");
    }

    #[test]
    fn open_toast_skips() {
        assert_eq!(
            pdf_open_toast_text(2, &[(p("/d/a.txt"), PdfError::NotPdf)]).unwrap(),
            "Opened 2 PDFs; skipped 1: not a PDF"
        );
        assert_eq!(
            pdf_open_toast_text(0, &[(p("/d/r.pdf"), PdfError::Locked)]).unwrap(),
            "r.pdf is locked"
        );
        assert_eq!(pdf_open_toast_text(1, &[]), None);
    }

    #[test]
    fn open_toast_singular_and_nothing_usable() {
        assert_eq!(
            pdf_open_toast_text(1, &[(p("/d/a.txt"), PdfError::NotPdf)]).unwrap(),
            "Opened 1 PDF; skipped 1: not a PDF"
        );
        let two = [
            (p("/d/a.txt"), PdfError::NotPdf),
            (p("/d/b.pdf"), PdfError::Locked),
        ];
        assert_eq!(pdf_open_toast_text(0, &two).unwrap(), "No PDFs to open");
        assert_eq!(
            pdf_open_toast_text(0, &[(p("/d/a.txt"), PdfError::NotPdf)]).unwrap(),
            "No PDFs to open"
        );
    }

    #[test]
    fn toast_texts_in_arabic() {
        with_language("ar", || {
            assert!(!pdf_toast_text(&saved("/d/r.pdf")).starts_with("Saved"));
            assert!(!pdf_toast_text(&split("/d/r split", 3)).starts_with("Saved"));
            assert!(!pdf_toast_text(&failed(PdfError::NotPdf)).starts_with("Couldn't"));
            let locked = pdf_open_toast_text(0, &[(p("/d/r.pdf"), PdfError::Locked)]).unwrap();
            assert!(!locked.contains("is locked"));
        });
    }

    fn source(path: &str) -> Source {
        Source {
            path: p(path),
            bytes: Arc::from(Vec::new()),
            page_count: 1,
        }
    }

    fn page(source: usize) -> PageRef {
        PageRef {
            source,
            page: 0,
            turns: 0,
        }
    }

    #[test]
    fn quarantine_comes_from_the_first_source_in_page_order() {
        let sources = [source("/d/a.pdf"), source("/d/b.pdf"), source("/d/c.pdf")];
        // The grid starts with c, then a; b is not in it.
        let pages = [page(2), page(0), page(2)];
        let read = |path: &Path| match path.to_str().unwrap() {
            "/d/a.pdf" => Some(b"from a".to_vec()),
            "/d/b.pdf" => Some(b"from b".to_vec()),
            _ => None,
        };
        assert_eq!(
            first_quarantine(&sources, &pages, read),
            Some(b"from a".to_vec())
        );
    }

    #[test]
    fn quarantine_asks_each_source_once_and_may_find_none() {
        let sources = [source("/d/a.pdf")];
        let asked = std::cell::Cell::new(0);
        let read = |_: &Path| {
            asked.set(asked.get() + 1);
            None
        };
        assert_eq!(first_quarantine(&sources, &[page(0), page(0)], read), None);
        assert_eq!(asked.get(), 1);
        // A page naming a missing source is passed over.
        assert_eq!(first_quarantine(&sources, &[page(5)], |_| None), None);
    }

    #[test]
    fn marking_a_folder_marks_it_and_every_file_inside() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("report split");
        std::fs::create_dir_all(folder.join("deeper")).unwrap();
        std::fs::write(folder.join("one.pdf"), b"1").unwrap();
        std::fs::write(folder.join("deeper").join("two.pdf"), b"2").unwrap();
        let seen = std::cell::RefCell::new(Vec::new());
        mark_tree(&folder, b"q", &|path, value| {
            assert_eq!(value, b"q");
            seen.borrow_mut().push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        let mut seen = seen.into_inner();
        seen.sort();
        let mut want = vec![
            folder.clone(),
            folder.join("deeper"),
            folder.join("deeper").join("two.pdf"),
            folder.join("one.pdf"),
        ];
        want.sort();
        assert_eq!(seen, want);
    }

    #[test]
    fn marking_a_file_marks_only_it_and_a_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report (edited).pdf");
        std::fs::write(&file, b"x").unwrap();
        let count = std::cell::Cell::new(0);
        mark_tree(&file, b"q", &|_, _| {
            count.set(count.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(count.get(), 1);
        let denied = mark_tree(&file, b"q", &|_, _| Err(io::Error::other("denied")));
        assert!(denied.is_err());
    }
}
