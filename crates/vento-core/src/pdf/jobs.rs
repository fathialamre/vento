//! Writing PDF pages out as new files: save, extract, split and export as
//! images. Opening reads each source once; nothing here touches the path of a
//! source again, and nothing is ever written over an existing file.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::archive::{Made, Partial, Progress, claim_partial_dir, claim_partial_file};
use crate::fileops::{Fingerprint, place_partial};
use crate::i18n::errors::io_reason;
use crate::i18n::tr;

use super::names::{
    ImageKind, SplitMode, extract_name, image_name, images_folder_name, save_name,
    split_folder_name, split_piece_name, split_plan, stem,
};
use super::pages::PageRef;

/// One opened PDF: where it came from, the bytes read when it was opened, and
/// how many pages it has. Later work uses `bytes`, never `path`.
#[derive(Clone)]
pub struct Source {
    /// The file it was read from; names and places outputs.
    pub path: PathBuf,
    /// The whole file as it was when opened.
    pub bytes: Arc<[u8]>,
    /// Pages in the document; at least one.
    pub page_count: usize,
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("path", &self.path)
            .field("bytes", &self.bytes.len())
            .field("page_count", &self.page_count)
            .finish()
    }
}

/// Why a PDF was not opened or written.
#[derive(Debug)]
pub enum PdfError {
    /// The PDF needs a password.
    Locked,
    /// Not a PDF (or a PDF without pages), or a folder.
    NotPdf,
    /// A filesystem call failed.
    Io(io::Error),
    /// The PDF library failed; its text stays English.
    Backend(String),
    /// The user cancelled the job.
    Cancelled,
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::Locked => f.write_str(&tr("locked")),
            PdfError::NotPdf => f.write_str(&tr("not a PDF")),
            PdfError::Io(e) => f.write_str(&io_reason(e)),
            PdfError::Backend(detail) => f.write_str(detail),
            PdfError::Cancelled => f.write_str(&tr("cancelled")),
        }
    }
}

impl std::error::Error for PdfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PdfError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for PdfError {
    fn from(e: io::Error) -> Self {
        PdfError::Io(e)
    }
}

/// How a page is turned into a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageOptions {
    /// PNG, or JPEG on white.
    pub kind: ImageKind,
    /// Resolution: 300 (Print) or 150 (Screen).
    pub dpi: u32,
}

/// Reads and writes PDFs; the real one lives with the platform code.
pub trait PdfBackend {
    /// The number of pages in the PDF held in `bytes`, or why it cannot be
    /// used (`Locked`, `NotPdf`).
    fn inspect(&self, bytes: &[u8]) -> Result<usize, PdfError>;

    /// Writes `pages` (rotation applied) as one PDF at `dst`, taking the
    /// document's metadata from `sources[pages[0].source]`. `dst` may or may
    /// not exist yet; its contents are replaced.
    fn write_pdf(&self, sources: &[Source], pages: &[PageRef], dst: &Path) -> Result<(), PdfError>;

    /// Renders `page` (rotation applied) as an image at `dst`. `dst` may or
    /// may not exist yet; its contents are replaced.
    fn write_image(
        &self,
        source: &Source,
        page: PageRef,
        opts: &ImageOptions,
        dst: &Path,
    ) -> Result<(), PdfError>;
}

/// Reads each of `paths` once and asks `backend` about it. Returns the usable
/// sources in the order given and, for each path that was left out, why
/// (folders and PDFs without pages are `NotPdf`).
pub fn open_sources(
    paths: &[PathBuf],
    backend: &dyn PdfBackend,
) -> (Vec<Source>, Vec<(PathBuf, PdfError)>) {
    let mut opened = Vec::new();
    let mut skipped = Vec::new();
    for path in paths {
        match open_one(path, backend) {
            Ok(source) => opened.push(source),
            Err(e) => skipped.push((path.clone(), e)),
        }
    }
    (opened, skipped)
}

fn open_one(path: &Path, backend: &dyn PdfBackend) -> Result<Source, PdfError> {
    if fs::metadata(path)?.is_dir() {
        return Err(PdfError::NotPdf);
    }
    let bytes: Arc<[u8]> = Arc::from(fs::read(path)?);
    match backend.inspect(&bytes)? {
        0 => Err(PdfError::NotPdf),
        page_count => Ok(Source {
            path: path.to_path_buf(),
            bytes,
            page_count,
        }),
    }
}

/// What to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdfJob {
    /// The whole grid as one new PDF.
    Save,
    /// The pages at these grid positions as one new PDF.
    Extract(Vec<usize>),
    /// The grid cut into several PDFs inside a new folder.
    Split(SplitMode),
    /// Pages as images: a file for one, a new folder for several.
    Images {
        /// Grid positions; empty means every page.
        positions: Vec<usize>,
        /// Format and resolution.
        opts: ImageOptions,
    },
}

/// Which kind of job an outcome is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// [`PdfJob::Save`].
    Save,
    /// [`PdfJob::Extract`].
    Extract,
    /// [`PdfJob::Split`].
    Split,
    /// [`PdfJob::Images`].
    Images,
}

/// What a [`run_job`] did.
#[derive(Debug)]
pub struct PdfOutcome {
    /// The kind of job.
    pub job: JobKind,
    /// The file or folder created; empty on failure or cancel.
    pub made: Vec<Made>,
    /// Files written, those inside a folder included; 0 on failure or cancel.
    pub files: usize,
    /// The first error; nothing of the job is left on disk then.
    pub failed: Option<PdfError>,
    /// Whether the user stopped the job.
    pub cancelled: bool,
}

/// One file to write.
enum Work {
    /// A PDF of these pages.
    Pdf(Vec<PageRef>),
    /// A picture of this page.
    Image(PageRef, ImageOptions),
}

/// One output file and what goes in it.
struct Item {
    name: OsString,
    work: Work,
}

/// What a job publishes: one file beside the anchor, or one folder of files.
enum Output {
    File(Item),
    Folder { name: OsString, items: Vec<Item> },
}

impl Output {
    fn file_count(&self) -> usize {
        match self {
            Output::File(_) => 1,
            Output::Folder { items, .. } => items.len(),
        }
    }
}

fn kind_of(job: &PdfJob) -> JobKind {
    match job {
        PdfJob::Save => JobKind::Save,
        PdfJob::Extract(_) => JobKind::Extract,
        PdfJob::Split(_) => JobKind::Split,
        PdfJob::Images { .. } => JobKind::Images,
    }
}

/// Runs `job` over the grid `pages` of `sources`.
///
/// The anchor is the source of the first page in the grid: outputs go in its
/// folder and its name is the base of theirs. Everything is written under a
/// hidden partial name and given its final name (never an existing one) only
/// when all of it is done, so a failure or a cancel leaves nothing behind. A
/// folder job is all or nothing. `mark(anchor, partial)` runs once on what is
/// about to be published, the file or the folder, before it gets its name; an
/// error from it fails the job.
pub fn run_job(
    job: &PdfJob,
    sources: &[Source],
    pages: &[PageRef],
    backend: &dyn PdfBackend,
    progress: &Progress,
    mark: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> PdfOutcome {
    let mut outcome = PdfOutcome {
        job: kind_of(job),
        made: Vec::new(),
        files: 0,
        failed: None,
        cancelled: false,
    };
    let result = plan(job, sources, pages).and_then(|(anchor, output)| {
        let files = output.file_count();
        write_output(&anchor, output, sources, backend, progress, mark).map(|made| (made, files))
    });
    match result {
        Ok((made, files)) => {
            outcome.made.push(made);
            outcome.files = files;
        }
        Err(PdfError::Cancelled) => outcome.cancelled = true,
        Err(e) => outcome.failed = Some(e),
    }
    outcome
}

/// Works out the anchor and what to write for `job`.
fn plan(
    job: &PdfJob,
    sources: &[Source],
    pages: &[PageRef],
) -> Result<(PathBuf, Output), PdfError> {
    let nothing = || PdfError::Backend(tr("nothing to write"));
    let first = pages.first().ok_or_else(nothing)?;
    if pages.iter().any(|p| p.source >= sources.len()) {
        return Err(nothing());
    }
    let anchor = sources[first.source].path.clone();
    let base = stem(anchor.file_name().unwrap_or_default());
    let output = match job {
        PdfJob::Save => Output::File(Item {
            name: save_name(&save_stems(sources, pages)),
            work: Work::Pdf(pages.to_vec()),
        }),
        PdfJob::Extract(positions) => {
            let positions = valid_positions(positions, pages.len());
            if positions.is_empty() {
                return Err(nothing());
            }
            Output::File(Item {
                name: extract_name(&base, &positions),
                work: Work::Pdf(positions.iter().map(|&i| pages[i]).collect()),
            })
        }
        PdfJob::Split(mode) => Output::Folder {
            name: split_folder_name(&base),
            items: split_plan(pages.len(), mode)
                .into_iter()
                .enumerate()
                .map(|(k, range)| Item {
                    name: split_piece_name(&base, k + 1, &range),
                    work: Work::Pdf(pages[range].to_vec()),
                })
                .collect(),
        },
        PdfJob::Images { positions, opts } => {
            let positions = if positions.is_empty() {
                (0..pages.len()).collect()
            } else {
                valid_positions(positions, pages.len())
            };
            let mut items: Vec<Item> = positions
                .iter()
                .map(|&i| Item {
                    name: image_name(&base, i, opts.kind),
                    work: Work::Image(pages[i], *opts),
                })
                .collect();
            match items.len() {
                0 => return Err(nothing()),
                1 => Output::File(items.remove(0)),
                _ => Output::Folder {
                    name: images_folder_name(&base),
                    items,
                },
            }
        }
    };
    Ok((anchor, output))
}

/// The stem of each source that has pages in the grid, once per source
/// position, in order of first appearance.
fn save_stems(sources: &[Source], pages: &[PageRef]) -> Vec<OsString> {
    let mut seen = HashSet::new();
    pages
        .iter()
        .filter(|p| seen.insert(p.source))
        .map(|p| stem(sources[p.source].path.file_name().unwrap_or_default()))
        .collect()
}

/// `positions` sorted, without duplicates and without any past the grid.
fn valid_positions(positions: &[usize], count: usize) -> Vec<usize> {
    let mut valid: Vec<usize> = positions.iter().copied().filter(|&i| i < count).collect();
    valid.sort_unstable();
    valid.dedup();
    valid
}

/// Writes `output` under a partial name beside `anchor` and publishes it.
fn write_output(
    anchor: &Path,
    output: Output,
    sources: &[Source],
    backend: &dyn PdfBackend,
    progress: &Progress,
    mark: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Made, PdfError> {
    let dir = match anchor.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    progress.start(output.file_count() as u64);
    let write = |item: &Item, dst: &Path| -> Result<(), PdfError> {
        if progress.is_cancelled() {
            return Err(PdfError::Cancelled);
        }
        progress.set_current(&item.name.to_string_lossy());
        write_item(item, sources, backend, dst)?;
        progress.add_done(1);
        Ok(())
    };
    let (name, partial, is_dir) = match &output {
        Output::File(item) => {
            if progress.is_cancelled() {
                return Err(PdfError::Cancelled);
            }
            let (path, file) = claim_partial_file(dir, &item.name)?;
            let partial = Partial::new(path);
            // The backend writes by path and may replace the file.
            drop(file);
            write(item, &partial.path)?;
            (&item.name, partial, false)
        }
        Output::Folder { name, items } => {
            if progress.is_cancelled() {
                return Err(PdfError::Cancelled);
            }
            let partial = Partial::new(claim_partial_dir(dir, name)?);
            for item in items {
                write(item, &partial.path.join(&item.name))?;
            }
            (name, partial, true)
        }
    };
    publish(anchor, dir, name, partial, is_dir, progress, mark)
}

fn write_item(
    item: &Item,
    sources: &[Source],
    backend: &dyn PdfBackend,
    dst: &Path,
) -> Result<(), PdfError> {
    match &item.work {
        Work::Pdf(pages) => backend.write_pdf(sources, pages, dst),
        Work::Image(page, opts) => backend.write_image(&sources[page.source], *page, opts, dst),
    }
}

/// Marks the finished partial and gives it its final name in `dir`.
fn publish(
    anchor: &Path,
    dir: &Path,
    name: &OsStr,
    mut partial: Partial,
    is_dir: bool,
    progress: &Progress,
    mark: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Made, PdfError> {
    mark(anchor, &partial.path)?;
    if progress.is_cancelled() {
        return Err(PdfError::Cancelled);
    }
    let output = place_partial(&partial.path, dir, name, is_dir)?;
    partial.published = true;
    let fingerprint = Fingerprint::of(&output)?;
    Ok(Made {
        source: anchor.to_path_buf(),
        output,
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::fs;

    const ORIGINAL: &[u8] = b"%PDF-original";

    /// Writes the page list as text behind a marker; can fail or cancel on
    /// the n-th write (1-based, counting PDFs and images together).
    #[derive(Default)]
    struct FakeBackend {
        fail_on: Cell<usize>,
        cancel_on: Cell<usize>,
        progress: RefCell<Option<Arc<Progress>>>,
        calls: Cell<usize>,
        last_pages: RefCell<Vec<PageRef>>,
        seen_bytes: RefCell<Vec<Vec<u8>>>,
    }

    impl FakeBackend {
        fn tick(&self) -> Result<(), PdfError> {
            let n = self.calls.get() + 1;
            self.calls.set(n);
            if n == self.cancel_on.get()
                && let Some(p) = self.progress.borrow().as_ref()
            {
                p.cancel();
            }
            if n == self.fail_on.get() {
                return Err(PdfError::Backend("boom".into()));
            }
            Ok(())
        }
    }

    fn text(pages: &[PageRef]) -> String {
        pages
            .iter()
            .map(|p| format!("{}:{}:{}", p.source, p.page, p.turns))
            .collect::<Vec<_>>()
            .join(",")
    }

    impl PdfBackend for FakeBackend {
        fn inspect(&self, bytes: &[u8]) -> Result<usize, PdfError> {
            if bytes.starts_with(b"LOCKED") {
                Err(PdfError::Locked)
            } else if bytes.starts_with(b"%PDF") {
                Ok(8)
            } else if bytes.starts_with(b"EMPTY") {
                Ok(0)
            } else {
                Err(PdfError::NotPdf)
            }
        }

        fn write_pdf(
            &self,
            sources: &[Source],
            pages: &[PageRef],
            dst: &Path,
        ) -> Result<(), PdfError> {
            self.seen_bytes
                .borrow_mut()
                .extend(sources.iter().map(|s| s.bytes.to_vec()));
            *self.last_pages.borrow_mut() = pages.to_vec();
            self.tick()?;
            fs::write(dst, format!("PDF:{}", text(pages)))?;
            Ok(())
        }

        fn write_image(
            &self,
            _source: &Source,
            page: PageRef,
            _opts: &ImageOptions,
            dst: &Path,
        ) -> Result<(), PdfError> {
            self.tick()?;
            fs::write(dst, format!("IMG:{}", text(&[page])))?;
            Ok(())
        }
    }

    fn p(source: usize, page: usize) -> PageRef {
        PageRef {
            source,
            page,
            turns: 0,
        }
    }

    fn src(dir: &Path, name: &str) -> Source {
        let path = dir.join(name);
        fs::write(&path, ORIGINAL).unwrap();
        Source {
            path,
            bytes: Arc::from(ORIGINAL),
            page_count: 8,
        }
    }

    /// `report.pdf` with all eight pages in the grid.
    fn one(dir: &Path) -> (Vec<Source>, Vec<PageRef>) {
        (
            vec![src(dir, "report.pdf")],
            (0..8).map(|i| p(0, i)).collect(),
        )
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn no_mark(_: &Path, _: &Path) -> io::Result<()> {
        Ok(())
    }

    fn name_of(o: &PdfOutcome) -> String {
        o.made[0]
            .output
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn png() -> ImageOptions {
        ImageOptions {
            kind: ImageKind::Png,
            dpi: 300,
        }
    }

    fn run(job: &PdfJob, s: &[Source], pg: &[PageRef], fake: &FakeBackend) -> PdfOutcome {
        let progress = Progress::new();
        *fake.progress.borrow_mut() = Some(Arc::clone(&progress));
        run_job(job, s, pg, fake, &progress, &no_mark)
    }

    #[test]
    fn save_writes_next_to_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let sources = vec![src(dir.path(), "report.pdf"), src(dir.path(), "scan.pdf")];
        let pages = vec![p(0, 0), p(1, 0), p(0, 1)];
        let fake = FakeBackend::default();
        let o = run(&PdfJob::Save, &sources, &pages, &fake);
        assert!(o.failed.is_none());
        assert_eq!(o.job, JobKind::Save);
        assert_eq!(o.files, 1);
        assert_eq!(name_of(&o), "report + scan.pdf");
        assert_eq!(
            fs::read_to_string(&o.made[0].output).unwrap(),
            "PDF:0:0:0,1:0:0,0:1:0"
        );
        assert_eq!(o.made[0].source, sources[0].path);
        assert_eq!(
            listing(dir.path()),
            ["report + scan.pdf", "report.pdf", "scan.pdf"]
        );
    }

    #[test]
    fn save_names_one_stem_per_source_position() {
        let dir = tempfile::tempdir().unwrap();
        // The same file opened twice is two positions with equal stems.
        let sources = vec![src(dir.path(), "report.pdf"), src(dir.path(), "report.pdf")];
        let pages = vec![p(0, 0), p(1, 0), p(0, 1)];
        let o = run(&PdfJob::Save, &sources, &pages, &FakeBackend::default());
        assert_eq!(name_of(&o), "report + report.pdf");
        // A source with no pages left in the grid does not count.
        let sources = vec![src(dir.path(), "a.pdf"), src(dir.path(), "b.pdf")];
        let o = run(&PdfJob::Save, &sources, &[p(1, 0)], &FakeBackend::default());
        assert_eq!(name_of(&o), "b (edited).pdf");
        assert_eq!(o.made[0].output.parent().unwrap(), dir.path());
    }

    #[test]
    fn save_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        fs::write(dir.path().join("report (edited).pdf"), b"mine").unwrap();
        let o = run(&PdfJob::Save, &sources, &pages, &FakeBackend::default());
        assert_eq!(name_of(&o), "report (edited) (2).pdf");
        assert_eq!(
            fs::read(dir.path().join("report (edited).pdf")).unwrap(),
            b"mine"
        );
        assert_eq!(fs::read(&sources[0].path).unwrap(), ORIGINAL);
    }

    #[test]
    fn extract_uses_positions() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let fake = FakeBackend::default();
        let o = run(&PdfJob::Extract(vec![4, 2, 3]), &sources, &pages, &fake);
        assert_eq!(name_of(&o), "report (pages 3–5).pdf");
        assert_eq!(*fake.last_pages.borrow(), [p(0, 2), p(0, 3), p(0, 4)]);
    }

    #[test]
    fn extract_uses_the_grid_not_the_source_order() {
        let dir = tempfile::tempdir().unwrap();
        let sources = vec![src(dir.path(), "report.pdf")];
        let pages = vec![p(0, 5), p(0, 1), p(0, 0)];
        let fake = FakeBackend::default();
        let o = run(&PdfJob::Extract(vec![1, 2]), &sources, &pages, &fake);
        assert_eq!(name_of(&o), "report (pages 2–3).pdf");
        assert_eq!(*fake.last_pages.borrow(), [p(0, 1), p(0, 0)]);
    }

    #[test]
    fn extract_with_nothing_valid_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        for positions in [vec![], vec![99]] {
            let o = run(
                &PdfJob::Extract(positions),
                &sources,
                &pages,
                &FakeBackend::default(),
            );
            assert!(matches!(o.failed, Some(PdfError::Backend(_))));
            assert_eq!(listing(dir.path()), ["report.pdf"]);
        }
    }

    #[test]
    fn empty_grid_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, _) = one(dir.path());
        let o = run(&PdfJob::Save, &sources, &[], &FakeBackend::default());
        assert!(o.failed.is_some() && o.made.is_empty());
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn split_into_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let fake = FakeBackend::default();
        let o = run(
            &PdfJob::Split(SplitMode::AtCuts(vec![2, 5])),
            &sources,
            &pages,
            &fake,
        );
        assert!(o.failed.is_none());
        assert_eq!(o.made.len(), 1);
        assert_eq!(o.files, 3);
        assert_eq!(name_of(&o), "report split");
        assert_eq!(
            listing(&dir.path().join("report split")),
            [
                "report 1 (1–2).pdf",
                "report 2 (3–5).pdf",
                "report 3 (6–8).pdf"
            ]
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("report split/report 2 (3–5).pdf")).unwrap(),
            "PDF:0:2:0,0:3:0,0:4:0"
        );
        assert_eq!(listing(dir.path()), ["report split", "report.pdf"]);
    }

    #[test]
    fn split_with_one_piece_still_makes_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let o = run(
            &PdfJob::Split(SplitMode::Every(100)),
            &sources,
            &pages,
            &FakeBackend::default(),
        );
        assert_eq!(o.files, 1);
        assert_eq!(
            listing(&dir.path().join("report split")),
            ["report 1 (1–8).pdf"]
        );
    }

    #[test]
    fn one_image_sits_beside_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let job = PdfJob::Images {
            positions: vec![0],
            opts: png(),
        };
        let o = run(&job, &sources, &pages, &FakeBackend::default());
        assert_eq!(name_of(&o), "report p1.png");
        assert_eq!(o.files, 1);
        assert_eq!(listing(dir.path()), ["report p1.png", "report.pdf"]);
    }

    #[test]
    fn one_image_by_position_not_source_page() {
        let dir = tempfile::tempdir().unwrap();
        let sources = vec![src(dir.path(), "report.pdf")];
        let pages = vec![p(0, 7), p(0, 3)];
        let job = PdfJob::Images {
            positions: vec![1],
            opts: ImageOptions {
                kind: ImageKind::Jpeg,
                dpi: 150,
            },
        };
        let o = run(&job, &sources, &pages, &FakeBackend::default());
        assert_eq!(name_of(&o), "report p2.jpg");
        assert_eq!(fs::read_to_string(&o.made[0].output).unwrap(), "IMG:0:3:0");
    }

    #[test]
    fn all_images_go_in_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let job = PdfJob::Images {
            positions: vec![],
            opts: png(),
        };
        let o = run(&job, &sources, &pages, &FakeBackend::default());
        assert_eq!(o.files, 8);
        assert_eq!(name_of(&o), "report images");
        assert!(dir.path().join("report images/report p8.png").exists());
        assert_eq!(listing(&dir.path().join("report images")).len(), 8);
    }

    #[test]
    fn duplicate_positions_are_one_image() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let job = PdfJob::Images {
            positions: vec![3, 3],
            opts: png(),
        };
        let o = run(&job, &sources, &pages, &FakeBackend::default());
        assert_eq!(name_of(&o), "report p4.png");
    }

    #[test]
    fn split_failure_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let fake = FakeBackend::default();
        fake.fail_on.set(2);
        let o = run(
            &PdfJob::Split(SplitMode::EveryPage),
            &sources,
            &pages,
            &fake,
        );
        assert!(matches!(o.failed, Some(PdfError::Backend(_))));
        assert!(!o.cancelled && o.made.is_empty() && o.files == 0);
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn images_failure_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let fake = FakeBackend::default();
        fake.fail_on.set(8);
        let job = PdfJob::Images {
            positions: vec![],
            opts: png(),
        };
        let o = run(&job, &sources, &pages, &fake);
        assert!(o.failed.is_some());
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn cancel_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let fake = FakeBackend::default();
        fake.cancel_on.set(1);
        let o = run(&PdfJob::Save, &sources, &pages, &fake);
        assert!(o.cancelled);
        assert!(o.failed.is_none() && o.made.is_empty());
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn cancel_between_split_pieces_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let fake = FakeBackend::default();
        fake.cancel_on.set(2);
        let o = run(
            &PdfJob::Split(SplitMode::EveryPage),
            &sources,
            &pages,
            &fake,
        );
        assert!(o.cancelled && o.made.is_empty() && o.files == 0);
        assert_eq!(fake.calls.get(), 2);
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn already_cancelled_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let progress = Progress::new();
        progress.cancel();
        let fake = FakeBackend::default();
        let o = run_job(&PdfJob::Save, &sources, &pages, &fake, &progress, &no_mark);
        assert!(o.cancelled);
        assert_eq!(fake.calls.get(), 0);
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn backend_cancelled_error_counts_as_cancel() {
        struct Cancelling;
        impl PdfBackend for Cancelling {
            fn inspect(&self, _: &[u8]) -> Result<usize, PdfError> {
                Ok(1)
            }
            fn write_pdf(&self, _: &[Source], _: &[PageRef], _: &Path) -> Result<(), PdfError> {
                Err(PdfError::Cancelled)
            }
            fn write_image(
                &self,
                _: &Source,
                _: PageRef,
                _: &ImageOptions,
                _: &Path,
            ) -> Result<(), PdfError> {
                Err(PdfError::Cancelled)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let o = run_job(
            &PdfJob::Save,
            &sources,
            &pages,
            &Cancelling,
            &Progress::new(),
            &no_mark,
        );
        assert!(o.cancelled && o.failed.is_none());
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn mark_error_fails_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let progress = Progress::new();
        let fail = |_: &Path, _: &Path| Err(io::Error::other("nope"));
        let o = run_job(
            &PdfJob::Split(SplitMode::EveryPage),
            &sources,
            &pages,
            &FakeBackend::default(),
            &progress,
            &fail,
        );
        assert!(matches!(o.failed, Some(PdfError::Io(_))));
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn mark_runs_once_per_published_unit_with_the_partial() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let seen = RefCell::new(Vec::new());
        let mark = |src: &Path, partial: &Path| {
            assert!(partial.exists());
            seen.borrow_mut()
                .push((src.to_path_buf(), partial.to_path_buf()));
            Ok(())
        };
        let progress = Progress::new();
        let o = run_job(
            &PdfJob::Split(SplitMode::EveryPage),
            &sources,
            &pages,
            &FakeBackend::default(),
            &progress,
            &mark,
        );
        assert!(o.failed.is_none());
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, sources[0].path);
        assert!(!seen[0].1.exists(), "the partial was renamed away");
    }

    #[test]
    fn progress_counts_output_files() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        let progress = Progress::new();
        let o = run_job(
            &PdfJob::Split(SplitMode::Every(3)),
            &sources,
            &pages,
            &FakeBackend::default(),
            &progress,
            &no_mark,
        );
        assert_eq!(o.files, 3);
        let snap = progress.snapshot();
        assert_eq!((snap.done, snap.total), (3, 3));
    }

    #[cfg(unix)]
    #[test]
    fn save_into_readonly_dir_leaves_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (sources, pages) = one(dir.path());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let o = run(&PdfJob::Save, &sources, &pages, &FakeBackend::default());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(o.failed, Some(PdfError::Io(_))));
        assert_eq!(listing(dir.path()), ["report.pdf"]);
    }

    #[test]
    fn open_sources_reads_once_and_sorts_out_bad() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.pdf");
        let locked = dir.path().join("locked.pdf");
        let text = dir.path().join("notes.pdf");
        let empty = dir.path().join("empty.pdf");
        let folder = dir.path().join("folder.pdf");
        let missing = dir.path().join("missing.pdf");
        fs::write(&good, ORIGINAL).unwrap();
        fs::write(&locked, b"LOCKED").unwrap();
        fs::write(&text, b"hello").unwrap();
        fs::write(&empty, b"EMPTY").unwrap();
        fs::create_dir(&folder).unwrap();
        let paths = vec![
            folder.clone(),
            good.clone(),
            text.clone(),
            locked.clone(),
            empty.clone(),
            missing.clone(),
        ];
        let (ok, bad) = open_sources(&paths, &FakeBackend::default());
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].path, good);
        assert_eq!(ok[0].page_count, 8);
        assert_eq!(&*ok[0].bytes, ORIGINAL);
        assert_eq!(bad.len(), 5);
        assert!(matches!(bad[0], (ref p, PdfError::NotPdf) if *p == folder));
        assert!(matches!(bad[1], (ref p, PdfError::NotPdf) if *p == text));
        assert!(matches!(bad[2], (ref p, PdfError::Locked) if *p == locked));
        assert!(matches!(bad[3], (ref p, PdfError::NotPdf) if *p == empty));
        assert!(matches!(bad[4], (ref p, PdfError::Io(_)) if *p == missing));
    }

    #[test]
    fn source_change_after_open_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.pdf");
        fs::write(&path, ORIGINAL).unwrap();
        let fake = FakeBackend::default();
        let (src, bad) = open_sources(std::slice::from_ref(&path), &fake);
        assert!(bad.is_empty());
        fs::write(&path, b"changed").unwrap();
        let pages: Vec<PageRef> = (0..8).map(|i| p(0, i)).collect();
        let o = run(&PdfJob::Save, &src, &pages, &fake);
        assert!(o.failed.is_none());
        assert_eq!(*fake.seen_bytes.borrow(), [ORIGINAL.to_vec()]);
    }

    #[test]
    fn errors_read_in_english_and_arabic() {
        use crate::i18n::with_language;
        let texts = || -> Vec<String> {
            [
                PdfError::Locked,
                PdfError::NotPdf,
                PdfError::Io(io::Error::from(io::ErrorKind::NotFound)),
                PdfError::Backend("bad page".into()),
                PdfError::Cancelled,
            ]
            .iter()
            .map(ToString::to_string)
            .collect()
        };
        assert_eq!(
            texts(),
            ["locked", "not a PDF", "not found", "bad page", "cancelled"]
        );
        with_language("ar", || {
            assert_eq!(
                texts(),
                [
                    "محمي بكلمة مرور",
                    "ليس ملف PDF",
                    "غير موجود",
                    "bad page",
                    "أُلغي"
                ]
            );
        });
    }
}
