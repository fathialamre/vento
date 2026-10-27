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
use vento_core::i18n::errors::clauses;
use vento_core::i18n::{fill, tr, trn};
use vento_platform::{probe_image, write_image};

use super::archive::{cancelled_text, copy_quarantine, failed_with_reason, show_progress};
use super::{Executor, LastOp, action_error_text, only_name};
use crate::archive_ui::Verb;

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
            parts.push(match only_name(&names) {
                Some(name) => fill(
                    &tr("Couldn't convert {name}: {reason}"),
                    &[("name", &name), ("reason", &reason)],
                ),
                None => fill(
                    &trn(
                        "Couldn't convert {n} image: {reason}",
                        "Couldn't convert {n} images: {reason}",
                        names.len() as u64,
                    ),
                    &[("reason", &reason)],
                ),
            });
        } else if outcome.cancelled {
            return cancelled_text();
        } else if outcome.already > 0 {
            return fill(
                &tr("Nothing to convert: already {format}"),
                &[("format", format)],
            );
        } else {
            return action_error_text(ActionId::Convert, &tr("no images"));
        }
    } else {
        if made == outcome.total {
            let names: Vec<&Path> = outcome.made.iter().map(|m| m.source.as_path()).collect();
            parts.push(match only_name(&names) {
                Some(name) => fill(
                    &tr("Converted {name} to {format}"),
                    &[("name", &name), ("format", format)],
                ),
                None => fill(
                    &trn(
                        "Converted {n} image to {format}",
                        "Converted {n} images to {format}",
                        names.len() as u64,
                    ),
                    &[("format", format)],
                ),
            });
        } else {
            parts.push(fill(
                &tr("Converted {made} of {total} to {format}"),
                &[
                    ("made", &made.to_string()),
                    ("total", &outcome.total.to_string()),
                    ("format", format),
                ],
            ));
        }
        if outcome.already > 0 {
            parts.push(fill(
                &trn(
                    "{n} already {format}",
                    "{n} already {format}",
                    outcome.already as u64,
                ),
                &[("format", format)],
            ));
        }
        if outcome.not_images > 0 {
            parts.push(trn(
                "{n} not an image",
                "{n} not images",
                outcome.not_images as u64,
            ));
        }
        if failed > 0 {
            parts.push(failed_with_reason(failed, &reason));
        }
    }
    if outcome.cancelled {
        parts.push(tr("cancelled"));
    }
    clauses(&parts)
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
    show_progress(progress, Verb::Converting, at);
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

    #[test]
    fn toasts_read_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            let one = outcome(1, vec![made("/d/photo.png")]);
            assert_eq!(convert_toast_text(&one), "تم تحويل photo.png إلى JPEG");
            assert_eq!(
                convert_toast_text(&outcome(2, many(2))),
                "تم تحويل صورتين إلى JPEG"
            );
            assert_eq!(
                convert_toast_text(&outcome(11, many(11))),
                "تم تحويل 11 صورة إلى JPEG"
            );
            let mut o = outcome(5, many(2));
            o.already = 1;
            o.not_images = 1;
            o.failed = vec![denied("/d/x.png")];
            o.cancelled = true;
            assert_eq!(
                convert_toast_text(&o),
                "تم تحويل 2 من 5 إلى JPEG؛ \
                 لا حاجة إلى تحويل عنصر واحد (بصيغة JPEG أصلاً)؛ \
                 تم تخطي عنصر واحد ليس صورة؛ \
                 تعذّرت معالجة عنصر واحد: permission denied؛ \
                 أُلغي"
            );
            let mut o = outcome(1, Vec::new());
            o.failed = vec![denied("/d/photo.png")];
            assert_eq!(
                convert_toast_text(&o),
                "تعذّر تحويل photo.png: permission denied"
            );
            let mut o = outcome(3, Vec::new());
            o.failed = vec![denied("/d/a.png"), denied("/d/b.png"), denied("/d/c.png")];
            assert_eq!(
                convert_toast_text(&o),
                "تعذّر تحويل 3 صور: permission denied"
            );
            let mut o = outcome(2, Vec::new());
            o.already = 2;
            assert_eq!(convert_toast_text(&o), "لا يوجد ما يُحوَّل: الصيغة JPEG أصلاً");
            let mut o = outcome(2, Vec::new());
            o.not_images = 2;
            assert_eq!(convert_toast_text(&o), "تعذّر التحويل: لا توجد صور");
        });
    }
}
