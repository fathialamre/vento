//! Converting a batch of images, each into a new file next to its original.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::archive::{Made, Partial, Progress, claim_partial_file};
use crate::fileops::{Fingerprint, place_partial};
use crate::i18n::errors::io_reason;
use crate::i18n::tr;

use super::ImageFormat;

/// Why one image was not converted. The texts in the `String` fields come
/// from the system's image code and stay English.
#[derive(Debug)]
pub enum ConvertError {
    /// The file cannot be decoded as an image.
    NotAnImage,
    /// The image is readable but this conversion is not supported.
    Unsupported(String),
    /// A filesystem call failed.
    Io(io::Error),
    /// The encoder failed.
    Encode(String),
    /// The user cancelled the job.
    Cancelled,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConvertError::NotAnImage => f.write_str(&tr("not an image")),
            ConvertError::Unsupported(detail) | ConvertError::Encode(detail) => f.write_str(detail),
            ConvertError::Io(e) => f.write_str(&io_reason(e)),
            ConvertError::Cancelled => f.write_str(&tr("cancelled")),
        }
    }
}

impl std::error::Error for ConvertError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConvertError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ConvertError {
    fn from(e: io::Error) -> Self {
        ConvertError::Io(e)
    }
}

/// Reads and writes images; the real one lives with the platform code.
pub trait ImageCodec {
    /// The format `path` decodes as: `None` when it is readable but not one of
    /// the target formats (AVIF, say), `Err(NotAnImage)` when it cannot be
    /// decoded at all.
    fn probe(&self, path: &Path) -> Result<Option<ImageFormat>, ConvertError>;

    /// Converts `src` into `format`, filling the existing empty file at `dst`.
    fn write(&self, src: &Path, dst: &Path, format: ImageFormat) -> Result<(), ConvertError>;
}

/// The converted file's name: the original's stem plus the format's
/// extension, so `photo.final.png` becomes `photo.final.jpg`.
pub fn output_name(src: &Path, format: ImageFormat) -> OsString {
    let mut name = src.file_stem().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(format.extension());
    name
}

/// What a [`convert_batch`] did.
#[derive(Debug)]
pub struct ConvertOutcome {
    /// The format asked for.
    pub format: ImageFormat,
    /// How many paths were given.
    pub total: usize,
    /// The files created.
    pub made: Vec<Made>,
    /// Images skipped because they already are in the target format.
    pub already: usize,
    /// Folders and files that are not images.
    pub not_images: usize,
    /// Images that could not be converted, with why.
    pub failed: Vec<(PathBuf, ConvertError)>,
    /// Whether the batch was stopped before the end.
    pub cancelled: bool,
}

/// What became of one path.
enum Step {
    Made(Made),
    Already,
    NotImage,
    Failed(ConvertError),
    Cancelled,
}

/// Converts each of `paths` into `format`, next to the original, under a free
/// name (`a.jpg`, then `a (2).jpg`, ...). Nothing existing is replaced.
///
/// An image's type is decided by its content, so an image that already is in
/// `format` is skipped whatever its extension. One path failing never stops
/// the others. `mark(original, partial)` runs on each converted file before
/// it gets its final name; an error from it fails that image.
pub fn convert_batch(
    paths: &[PathBuf],
    format: ImageFormat,
    codec: &dyn ImageCodec,
    progress: &Progress,
    mark: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> ConvertOutcome {
    let mut outcome = ConvertOutcome {
        format,
        total: paths.len(),
        made: Vec::new(),
        already: 0,
        not_images: 0,
        failed: Vec::new(),
        cancelled: false,
    };
    progress.start(paths.len() as u64);
    for path in paths {
        if progress.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        progress.set_current(&path.file_name().unwrap_or_default().to_string_lossy());
        match convert_one(path, format, codec, progress, mark) {
            Step::Made(made) => outcome.made.push(made),
            Step::Already => outcome.already += 1,
            Step::NotImage => outcome.not_images += 1,
            Step::Failed(e) => outcome.failed.push((path.clone(), e)),
            Step::Cancelled => {
                outcome.cancelled = true;
                break;
            }
        }
        progress.add_done(1);
    }
    outcome
}

fn convert_one(
    path: &Path,
    format: ImageFormat,
    codec: &dyn ImageCodec,
    progress: &Progress,
    mark: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Step {
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => return Step::NotImage,
        Ok(_) => {}
        Err(e) => return Step::Failed(e.into()),
    }
    match codec.probe(path) {
        Ok(Some(found)) if found == format => return Step::Already,
        Ok(_) => {}
        Err(ConvertError::NotAnImage) => return Step::NotImage,
        Err(e) => return Step::Failed(e),
    }
    match write_next_to(path, format, codec, progress, mark) {
        Ok(made) => Step::Made(made),
        Err(ConvertError::Cancelled) => Step::Cancelled,
        Err(e) => Step::Failed(e),
    }
}

/// Writes the converted copy under a hidden partial name, then gives it its
/// final name beside `src`. The partial is removed on any failure.
fn write_next_to(
    src: &Path,
    format: ImageFormat,
    codec: &dyn ImageCodec,
    progress: &Progress,
    mark: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Made, ConvertError> {
    let dir = match src.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = output_name(src, format);
    let (partial, file) = claim_partial_file(dir, &name)?;
    let mut partial = Partial::new(partial);
    // The codec writes by path and may replace the file.
    drop(file);
    codec.write(src, &partial.path, format)?;
    mark(src, &partial.path)?;
    if progress.is_cancelled() {
        return Err(ConvertError::Cancelled);
    }
    let output = place_partial(&partial.path, dir, &name, false)?;
    partial.published = true;
    let fingerprint = Fingerprint::of(&output)?;
    Ok(Made {
        source: src.to_path_buf(),
        output,
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    const MARKER: &[u8] = b"CONVERTED:";

    /// What the fake probe says about a path.
    enum Probe {
        Format(Option<ImageFormat>),
        NotImage,
    }

    /// Writes the source bytes behind a marker; probe answers come from a map.
    #[derive(Default)]
    struct FakeCodec {
        probes: HashMap<PathBuf, Probe>,
        fail_write: HashSet<PathBuf>,
        /// Sources whose write cancels `progress` part way through.
        cancel_write: HashSet<PathBuf>,
        progress: Option<Arc<Progress>>,
        /// Whether a cancelled write reports `Cancelled` or finishes.
        cancel_errors: bool,
        written: RefCell<Vec<PathBuf>>,
    }

    impl ImageCodec for FakeCodec {
        fn probe(&self, path: &Path) -> Result<Option<ImageFormat>, ConvertError> {
            match self.probes.get(path) {
                None => Ok(None),
                Some(Probe::Format(f)) => Ok(*f),
                Some(Probe::NotImage) => Err(ConvertError::NotAnImage),
            }
        }

        fn write(&self, src: &Path, dst: &Path, _format: ImageFormat) -> Result<(), ConvertError> {
            self.written.borrow_mut().push(src.to_path_buf());
            if self.fail_write.contains(src) {
                return Err(ConvertError::Encode("boom".into()));
            }
            let mut bytes = MARKER.to_vec();
            bytes.extend(fs::read(src)?);
            fs::write(dst, bytes)?;
            if self.cancel_write.contains(src) {
                self.progress.as_ref().unwrap().cancel();
                if self.cancel_errors {
                    return Err(ConvertError::Cancelled);
                }
            }
            Ok(())
        }
    }

    fn no_mark(_: &Path, _: &Path) -> io::Result<()> {
        Ok(())
    }

    fn image(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn no_partials(dir: &Path) {
        let left: Vec<String> = names(dir)
            .into_iter()
            .filter(|n| n.contains("vento-partial"))
            .collect();
        assert!(left.is_empty(), "partials left behind: {left:?}");
    }

    fn run(paths: &[PathBuf], codec: &FakeCodec) -> ConvertOutcome {
        convert_batch(paths, ImageFormat::Jpeg, codec, &Progress::new(), &no_mark)
    }

    #[test]
    fn converts_next_to_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.png", b"pixels");
        let out = run(std::slice::from_ref(&a), &FakeCodec::default());
        assert_eq!(out.made.len(), 1);
        assert_eq!(out.total, 1);
        assert_eq!(out.format, ImageFormat::Jpeg);
        assert_eq!(out.made[0].source, a);
        assert_eq!(out.made[0].output, dir.path().join("a.jpg"));
        assert_eq!(
            fs::read(dir.path().join("a.jpg")).unwrap(),
            b"CONVERTED:pixels"
        );
        assert_eq!(fs::read(&a).unwrap(), b"pixels");
        assert_eq!(
            out.made[0].fingerprint,
            Fingerprint::of(&out.made[0].output).unwrap()
        );
        no_partials(dir.path());
    }

    #[test]
    fn outputs_go_next_to_each_original() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        fs::create_dir(&one).unwrap();
        fs::create_dir(&two).unwrap();
        let a = image(&one, "a.png", b"1");
        let b = image(&two, "b.png", b"2");
        let out = run(&[a, b], &FakeCodec::default());
        assert_eq!(out.made.len(), 2);
        assert_eq!(names(&one), ["a.jpg", "a.png"]);
        assert_eq!(names(&two), ["b.jpg", "b.png"]);
    }

    #[test]
    fn existing_output_name_gets_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.png", b"new");
        image(dir.path(), "a.jpg", b"old");
        let out = run(&[a], &FakeCodec::default());
        assert_eq!(out.made[0].output, dir.path().join("a (2).jpg"));
        assert_eq!(fs::read(dir.path().join("a.jpg")).unwrap(), b"old");
        assert_eq!(
            fs::read(dir.path().join("a (2).jpg")).unwrap(),
            b"CONVERTED:new"
        );
    }

    #[test]
    fn already_is_decided_by_content_not_extension() {
        let dir = tempfile::tempdir().unwrap();
        let x = image(dir.path(), "x.png", b"really a jpeg");
        let mut codec = FakeCodec::default();
        codec
            .probes
            .insert(x.clone(), Probe::Format(Some(ImageFormat::Jpeg)));
        let out = run(&[x], &codec);
        assert_eq!(out.already, 1);
        assert!(out.made.is_empty());
        assert!(codec.written.borrow().is_empty());
        assert_eq!(names(dir.path()), ["x.png"]);
    }

    #[test]
    fn a_jpg_that_is_really_a_png_is_converted() {
        let dir = tempfile::tempdir().unwrap();
        let x = image(dir.path(), "x.jpg", b"png inside");
        let mut codec = FakeCodec::default();
        codec
            .probes
            .insert(x.clone(), Probe::Format(Some(ImageFormat::Png)));
        let out = run(&[x], &codec);
        assert_eq!(out.already, 0);
        assert_eq!(out.made.len(), 1);
        assert_eq!(out.made[0].output, dir.path().join("x (2).jpg"));
    }

    #[test]
    fn folders_and_non_images_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("pics");
        fs::create_dir(&folder).unwrap();
        let text = image(dir.path(), "notes.png", b"not really");
        let mut codec = FakeCodec::default();
        codec.probes.insert(text.clone(), Probe::NotImage);
        let out = run(&[folder, text], &codec);
        assert_eq!(out.not_images, 2);
        assert!(out.made.is_empty());
        assert!(out.failed.is_empty());
    }

    #[test]
    fn one_failure_does_not_stop_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.png", b"a");
        let b = image(dir.path(), "b.png", b"b");
        let c = image(dir.path(), "c.png", b"c");
        let mut codec = FakeCodec::default();
        codec.fail_write.insert(a.clone());
        let out = run(&[a.clone(), b, c], &codec);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, a);
        assert!(matches!(out.failed[0].1, ConvertError::Encode(_)));
        assert_eq!(out.made.len(), 2);
        assert!(!out.cancelled);
        assert!(!dir.path().join("a.jpg").exists());
        no_partials(dir.path());
    }

    #[test]
    fn a_missing_file_fails_that_image_only() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone.png");
        let b = image(dir.path(), "b.png", b"b");
        let out = run(&[gone.clone(), b], &FakeCodec::default());
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, gone);
        assert!(
            matches!(&out.failed[0].1, ConvertError::Io(e) if e.kind() == io::ErrorKind::NotFound)
        );
        assert_eq!(out.made.len(), 1);
    }

    fn cancelling(a: &Path, errors: bool, progress: &Arc<Progress>) -> FakeCodec {
        let mut codec = FakeCodec {
            progress: Some(Arc::clone(progress)),
            cancel_errors: errors,
            ..FakeCodec::default()
        };
        codec.cancel_write.insert(a.to_path_buf());
        codec
    }

    #[test]
    fn cancel_mid_write_removes_the_partial() {
        for errors in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let a = image(dir.path(), "a.png", b"a");
            let b = image(dir.path(), "b.png", b"b");
            let c = image(dir.path(), "c.png", b"c");
            let progress = Progress::new();
            let codec = cancelling(&b, errors, &progress);
            let out = convert_batch(&[a, b, c], ImageFormat::Jpeg, &codec, &progress, &no_mark);
            assert!(out.cancelled);
            assert_eq!(out.made.len(), 1, "earlier output stays");
            assert!(out.failed.is_empty());
            assert_eq!(
                names(dir.path()),
                ["a.jpg", "a.png", "b.png", "c.png"],
                "errors = {errors}"
            );
        }
    }

    #[test]
    fn already_cancelled_converts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.png", b"a");
        let progress = Progress::new();
        progress.cancel();
        let out = convert_batch(
            &[a],
            ImageFormat::Jpeg,
            &FakeCodec::default(),
            &progress,
            &no_mark,
        );
        assert!(out.cancelled);
        assert!(out.made.is_empty());
        assert_eq!(names(dir.path()), ["a.png"]);
    }

    #[test]
    fn mark_runs_on_the_partial_before_publish() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.png", b"a");
        let seen = RefCell::new(Vec::new());
        let mark = |original: &Path, partial: &Path| {
            let bytes = fs::read(partial)?;
            seen.borrow_mut()
                .push((original.to_path_buf(), partial.to_path_buf(), bytes));
            Ok(())
        };
        let out = convert_batch(
            std::slice::from_ref(&a),
            ImageFormat::Jpeg,
            &FakeCodec::default(),
            &Progress::new(),
            &mark,
        );
        assert_eq!(out.made.len(), 1);
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, a);
        let partial_name = seen[0].1.file_name().unwrap().to_string_lossy();
        assert!(partial_name.contains("vento-partial"), "{partial_name}");
        assert_eq!(seen[0].2, b"CONVERTED:a", "mark sees the written file");
    }

    #[test]
    fn mark_failure_fails_that_image() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.png", b"a");
        let b = image(dir.path(), "b.png", b"b");
        let mark = |original: &Path, _: &Path| {
            if original.file_name().unwrap() == "a.png" {
                Err(io::Error::other("no tag"))
            } else {
                Ok(())
            }
        };
        let out = convert_batch(
            &[a.clone(), b],
            ImageFormat::Jpeg,
            &FakeCodec::default(),
            &Progress::new(),
            &mark,
        );
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, a);
        assert!(matches!(out.failed[0].1, ConvertError::Io(_)));
        assert_eq!(out.made.len(), 1);
        assert!(!dir.path().join("a.jpg").exists());
        no_partials(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn read_only_folder_fails_that_image_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        let open = dir.path().join("open");
        fs::create_dir(&locked).unwrap();
        fs::create_dir(&open).unwrap();
        let a = image(&locked, "a.png", b"a");
        let b = image(&open, "b.png", b"b");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let out = run(&[a.clone(), b], &FakeCodec::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, a);
        assert!(matches!(
            &out.failed[0].1,
            ConvertError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(out.made.len(), 1);
        assert_eq!(names(&locked), ["a.png"]);
        assert_eq!(names(&open), ["b.jpg", "b.png"]);
    }

    #[test]
    fn progress_counts_images() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.png", b"a");
        let folder = dir.path().join("pics");
        fs::create_dir(&folder).unwrap();
        let c = image(dir.path(), "c.png", b"c");
        let progress = Progress::new();
        convert_batch(
            &[a, folder, c],
            ImageFormat::Jpeg,
            &FakeCodec::default(),
            &progress,
            &no_mark,
        );
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.total, 3);
        assert_eq!(snapshot.done, 3);
        assert_eq!(snapshot.current, "c.png");
    }

    #[test]
    fn output_name_keeps_inner_dots() {
        assert_eq!(
            output_name(Path::new("/x/photo.final.png"), ImageFormat::Jpeg),
            OsString::from("photo.final.jpg")
        );
        assert_eq!(
            output_name(Path::new("a.HEIC"), ImageFormat::Tiff),
            OsString::from("a.tiff")
        );
    }

    #[test]
    fn errors_read_in_english_and_arabic() {
        use crate::i18n::with_language;
        let all = || {
            vec![
                ConvertError::NotAnImage,
                ConvertError::Unsupported("no alpha".into()),
                ConvertError::Io(io::Error::from(io::ErrorKind::NotFound)),
                ConvertError::Encode("bad frame".into()),
                ConvertError::Cancelled,
            ]
        };
        let texts = || -> Vec<String> { all().iter().map(ToString::to_string).collect() };
        assert_eq!(
            texts(),
            [
                "not an image",
                "no alpha",
                "not found",
                "bad frame",
                "cancelled"
            ]
        );
        with_language("ar", || {
            assert_eq!(
                texts(),
                ["ليست صورة", "no alpha", "غير موجود", "bad frame", "أُلغي"]
            );
        });
    }
}
