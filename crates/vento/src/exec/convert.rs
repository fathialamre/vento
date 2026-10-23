//! Convert: writes the dropped images in the chosen format next to the
//! originals, on a worker thread, with the progress window following.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{info, warn};
use vento_core::actions::ActionId;
use vento_core::archive::Progress;
use vento_core::convert::{ConvertError, ConvertOutcome, ImageCodec, ImageFormat, convert_batch};
use vento_core::fileops::CopyRecord;
use vento_core::geometry::Point;
use vento_platform::{probe_image, write_image};

use super::archive::{CANCELLED_TEXT, copy_quarantine, show_progress};
use super::{Executor, LastOp, name_or_count, sentence};

/// The progress window's verb.
const CONVERTING: &str = "Converting";

/// `"1 image"` / `"3 images"`.
fn images(n: usize) -> String {
    format!("{n} {}", if n == 1 { "image" } else { "images" })
}

/// Toast text for a finished Convert job. Nothing made and nothing failed
/// means the job was cancelled, every image already was in the format, or
/// no path was an image. Failures give the first reason.
pub(super) fn convert_toast_text(outcome: &ConvertOutcome) -> String {
    let format = outcome.format.name();
    let made = outcome.made.len();
    let failed = outcome.failed.len();
    let reason = outcome
        .failed
        .first()
        .map(|(_, error)| error.to_string())
        .unwrap_or_default();
    let mut parts = Vec::new();
    if made == 0 {
        if failed > 0 {
            let names: Vec<&Path> = outcome.failed.iter().map(|(p, _)| p.as_path()).collect();
            parts.push(format!(
                "couldn't convert {}: {reason}",
                name_or_count(&names, images)
            ));
        } else if outcome.cancelled {
            return CANCELLED_TEXT.into();
        } else if outcome.already > 0 {
            return format!("Nothing to convert: already {format}");
        } else {
            return "Couldn't convert: no images".into();
        }
    } else {
        if made == outcome.total {
            let names: Vec<&Path> = outcome.made.iter().map(|m| m.source.as_path()).collect();
            parts.push(format!(
                "converted {} to {format}",
                name_or_count(&names, images)
            ));
        } else {
            parts.push(format!("converted {made} of {} to {format}", outcome.total));
        }
        if outcome.already > 0 {
            parts.push(format!("{} already {format}", outcome.already));
        }
        if outcome.not_images > 0 {
            let n = outcome.not_images;
            let noun = if n == 1 { "not an image" } else { "not images" };
            parts.push(format!("{n} {noun}"));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed: {reason}"));
        }
    }
    if outcome.cancelled {
        parts.push("cancelled".into());
    }
    sentence(&parts)
}

/// The real image codec: ImageIO through the platform crate.
struct SystemCodec;

impl ImageCodec for SystemCodec {
    fn probe(&self, path: &Path) -> Result<Option<ImageFormat>, ConvertError> {
        probe_image(path)
    }

    fn write(&self, src: &Path, dst: &Path, format: ImageFormat) -> Result<(), ConvertError> {
        write_image(src, dst, format)
    }
}

/// The worker's half of Convert: has the progress window follow the job and
/// runs it.
fn convert_job(
    paths: &[PathBuf],
    format: ImageFormat,
    progress: &Arc<Progress>,
    at: Point,
) -> ConvertOutcome {
    show_progress(progress, CONVERTING, at);
    convert_batch(paths, format, &SystemCodec, progress, &copy_quarantine())
}

impl Executor {
    /// Converts the images among `paths` into `format`, each next to its
    /// original, on a worker thread.
    pub(super) fn convert(&self, paths: Vec<PathBuf>, format: ImageFormat, at: Point) {
        if self.refuse_if_busy(at) {
            return;
        }
        let progress = Progress::new();
        let cancel = Some(Arc::clone(&progress));
        self.spawn_file_job(
            ActionId::Convert,
            move || convert_job(&paths, format, &progress, at),
            move |executor, outcome| executor.convert_finished(outcome, at),
            cancel,
            at,
        );
    }

    fn convert_finished(&self, outcome: ConvertOutcome, at: Point) {
        for (path, error) in &outcome.failed {
            warn!("cannot convert {}: {error}", path.display());
        }
        let text = convert_toast_text(&outcome);
        info!("convert: {text}");
        let Some(dest) = outcome
            .made
            .first()
            .and_then(|m| m.output.parent())
            .map(Path::to_path_buf)
        else {
            self.toast.show(&text, None, at);
            return;
        };
        let record = CopyRecord {
            dest,
            copies: outcome
                .made
                .into_iter()
                .map(|m| (m.source, m.output, m.fingerprint))
                .collect(),
        };
        self.record_undo(LastOp::Convert(record), text, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use vento_core::archive::Made;
    use vento_core::fileops::Fingerprint;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn made(source: &str) -> Made {
        Made {
            source: p(source),
            output: p(&format!("{source}.jpg")),
            fingerprint: Fingerprint::Link {
                target: p("/t"),
                id: Default::default(),
            },
        }
    }

    fn denied(name: &str) -> (PathBuf, ConvertError) {
        (
            p(name),
            ConvertError::Io(io::Error::other("permission denied")),
        )
    }

    fn outcome(total: usize, made: Vec<Made>) -> ConvertOutcome {
        ConvertOutcome {
            format: ImageFormat::Jpeg,
            total,
            made,
            already: 0,
            not_images: 0,
            failed: Vec::new(),
            cancelled: false,
        }
    }

    fn many(n: usize) -> Vec<Made> {
        (0..n).map(|i| made(&format!("/d/{i}.png"))).collect()
    }

    #[test]
    fn toast_one_image() {
        let o = outcome(1, vec![made("/d/photo.png")]);
        assert_eq!(convert_toast_text(&o), "Converted photo.png to JPEG");
    }

    #[test]
    fn toast_many() {
        assert_eq!(
            convert_toast_text(&outcome(8, many(8))),
            "Converted 8 images to JPEG"
        );
    }

    #[test]
    fn toast_with_already() {
        let mut o = outcome(8, many(6));
        o.already = 2;
        assert_eq!(
            convert_toast_text(&o),
            "Converted 6 of 8 to JPEG; 2 already JPEG"
        );
    }

    #[test]
    fn toast_with_not_an_image() {
        let mut o = outcome(8, many(7));
        o.not_images = 1;
        assert_eq!(
            convert_toast_text(&o),
            "Converted 7 of 8 to JPEG; 1 not an image"
        );
        let mut o = outcome(9, many(7));
        o.not_images = 2;
        assert_eq!(
            convert_toast_text(&o),
            "Converted 7 of 9 to JPEG; 2 not images"
        );
    }

    #[test]
    fn toast_parts_in_order() {
        let mut o = outcome(8, many(5));
        o.already = 1;
        o.not_images = 1;
        o.failed = vec![denied("/d/x.png")];
        assert_eq!(
            convert_toast_text(&o),
            "Converted 5 of 8 to JPEG; 1 already JPEG; 1 not an image; 1 failed: permission denied"
        );
    }

    #[test]
    fn toast_cancelled_after_some() {
        let mut o = outcome(8, many(3));
        o.cancelled = true;
        assert_eq!(
            convert_toast_text(&o),
            "Converted 3 of 8 to JPEG; cancelled"
        );
    }

    #[test]
    fn toast_cancelled_before_any() {
        let mut o = outcome(8, Vec::new());
        o.cancelled = true;
        assert_eq!(convert_toast_text(&o), "Cancelled");
    }

    #[test]
    fn toast_nothing_made_one_failed() {
        let mut o = outcome(1, Vec::new());
        o.failed = vec![denied("/d/photo.png")];
        assert_eq!(
            convert_toast_text(&o),
            "Couldn't convert photo.png: permission denied"
        );
    }

    #[test]
    fn toast_nothing_made_many_failed() {
        let mut o = outcome(3, Vec::new());
        o.failed = vec![denied("/d/a.png"), denied("/d/b.png"), denied("/d/c.png")];
        assert_eq!(
            convert_toast_text(&o),
            "Couldn't convert 3 images: permission denied"
        );
    }

    #[test]
    fn toast_all_already() {
        let mut o = outcome(2, Vec::new());
        o.already = 2;
        assert_eq!(convert_toast_text(&o), "Nothing to convert: already JPEG");
    }

    #[test]
    fn toast_no_images() {
        let mut o = outcome(2, Vec::new());
        o.not_images = 2;
        assert_eq!(convert_toast_text(&o), "Couldn't convert: no images");
    }
}
