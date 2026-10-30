//! Share: hands the dropped items to AirDrop, Messages, Mail or Notes, or
//! shows the system's share popover. The service's own window does the rest,
//! so sharing runs no job and keeps no undo.

use std::path::PathBuf;

use log::{info, warn};
use vento_core::actions::ActionId;
use vento_core::geometry::Point;
use vento_core::i18n::{fill, tr, trn};
use vento_core::share::{
    ShareOutcome, ShareTarget, direct_target, existing, mixes_folders_and_files,
};

use super::{Executor, in_english};

/// Whether `target` is Notes and `paths` hold a folder together with files,
/// which Notes refuses. Reads the disk, so a refusal asks it once.
pub(super) fn notes_mix(target: ShareTarget, paths: &[PathBuf]) -> bool {
    target == ShareTarget::Notes && mixes_folders_and_files(paths)
}

/// The toast for `target`'s service refusing the dropped items, shown before
/// anything opens. `notes_mix` is what [`notes_mix`] says of them: Notes
/// refusing a folder together with files gets a sentence of its own, which
/// says how to get around it.
pub(super) fn refused_text(target: ShareTarget, notes_mix: bool) -> String {
    if notes_mix {
        tr(
            "Notes can't take a folder together with files. Drop the folder on its own, or zip it with Archive first.",
        )
    } else {
        failed_text(target, &tr("it won't take these items"))
    }
}

/// The notice for `n` dropped items that were gone by the time Share ran,
/// shown when the rest are still shared.
pub(super) fn left_out_text(n: usize) -> String {
    trn(
        "Left out {n} item that no longer exists",
        "Left out {n} items that no longer exist",
        n as u64,
    )
}

/// The toast for a share with `target` that failed for `reason`.
pub(super) fn failed_text(target: ShareTarget, reason: &str) -> String {
    fill(
        &tr("Couldn't share with {target}: {reason}"),
        &[("target", &target.name()), ("reason", reason)],
    )
}

/// The toast, if any, for a share with `target` that ended in `outcome`. Only
/// a failure gets one: the service's own window already said the items went,
/// and AirDrop's Cancel reports the same as a send, so a "sent" toast could
/// follow a cancel.
pub(super) fn outcome_toast(target: ShareTarget, outcome: &ShareOutcome) -> Option<String> {
    match outcome {
        ShareOutcome::Sent | ShareOutcome::Cancelled => None,
        ShareOutcome::Failed(reason) => Some(failed_text(target, reason)),
    }
}

impl Executor {
    /// Hands the dropped items that still exist to `target`, or, for a
    /// release on the Share cell itself (`None`), to AirDrop when the machine
    /// offers it and to the share popover otherwise. The service's window
    /// opens and this returns; dropped items that are gone, a refusal, or a
    /// failure the service reports later, are toasted at `at`. Sharing runs
    /// on the main thread with no job, so it is never refused as busy.
    pub(super) fn share(&self, paths: Vec<PathBuf>, target: Option<ShareTarget>, at: Point) {
        let target = target.unwrap_or_else(|| direct_target(&self.platform.share_targets()));
        let (paths, missing) = existing(&paths);
        if missing > 0 {
            info!("share: left out {missing} item(s) that are gone");
        }
        if paths.is_empty() {
            self.toast_error_as(
                ActionId::Share,
                &tr("nothing to share"),
                "nothing to share",
                at,
            );
            return;
        }
        // Said just before the items are handed over, so a refusal's toast
        // does not follow it.
        let tell_left_out = || {
            if missing > 0 {
                self.toast.show(&left_out_text(missing), None, at);
            }
        };
        if target == ShareTarget::More {
            tell_left_out();
            let result = self.platform.share_picker(&paths, at);
            self.finish(ActionId::Share, result, at, || "share picker".into());
            return;
        }
        if !self.platform.can_share(target, &paths) {
            let mix = notes_mix(target, &paths);
            warn!(
                "share {} refused: {}",
                target.key(),
                in_english(|| refused_text(target, mix))
            );
            self.toast.show(&refused_text(target, mix), None, at);
            return;
        }
        tell_left_out();
        let this = self.this.clone();
        let count = paths.len();
        let result = self.platform.share(
            target,
            &paths,
            Box::new(move |outcome| {
                match &outcome {
                    ShareOutcome::Sent => info!("share {}: sent {count} item(s)", target.key()),
                    ShareOutcome::Cancelled => info!("share {}: cancelled", target.key()),
                    ShareOutcome::Failed(reason) => {
                        warn!("share {} failed: {reason}", target.key());
                    }
                }
                let Some(executor) = this.upgrade() else {
                    return;
                };
                if let Some(text) = outcome_toast(target, &outcome) {
                    executor.toast.show(&text, None, at);
                }
            }),
        );
        self.finish(ActionId::Share, result, at, || {
            format!("share {}: {count} item(s)", target.key())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::i18n::with_language;

    use crate::exec::action_error_text;

    /// A folder holding one file and one sub-folder, and those two paths.
    fn file_and_folder() -> (tempfile::TempDir, [PathBuf; 2]) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "f").unwrap();
        let folder = dir.path().join("d");
        std::fs::create_dir(&folder).unwrap();
        (dir, [file, folder])
    }

    #[test]
    fn notes_refusal_names_the_mix() {
        let (_dir, both) = file_and_folder();
        assert!(notes_mix(ShareTarget::Notes, &both));
        assert!(!notes_mix(ShareTarget::Mail, &both));
        assert!(!notes_mix(ShareTarget::Notes, &both[..1]));
        assert_eq!(
            refused_text(ShareTarget::Notes, true),
            "Notes can't take a folder together with files. Drop the folder on its own, or zip it with Archive first."
        );
        assert_eq!(
            refused_text(ShareTarget::Mail, false),
            "Couldn't share with Mail: it won't take these items"
        );
    }

    #[test]
    fn cancelled_share_shows_nothing() {
        assert_eq!(
            outcome_toast(ShareTarget::AirDrop, &ShareOutcome::Cancelled),
            None
        );
        assert_eq!(outcome_toast(ShareTarget::Mail, &ShareOutcome::Sent), None);
        assert_eq!(
            outcome_toast(ShareTarget::AirDrop, &ShareOutcome::Sent),
            None
        );
        assert_eq!(
            outcome_toast(
                ShareTarget::Mail,
                &ShareOutcome::Failed("no account".into())
            ),
            Some("Couldn't share with Mail: no account".into())
        );
    }

    #[test]
    fn nothing_left_to_share_toast() {
        assert_eq!(
            action_error_text(ActionId::Share, &tr("nothing to share")),
            "Couldn't share: nothing to share"
        );
        with_language("ar", || {
            assert_eq!(
                action_error_text(ActionId::Share, &tr("nothing to share")),
                "تعذّرت المشاركة: لا شيء للمشاركة"
            );
        });
    }

    #[test]
    fn items_gone_at_the_drop_are_counted() {
        assert_eq!(left_out_text(1), "Left out 1 item that no longer exists");
        assert_eq!(left_out_text(3), "Left out 3 items that no longer exist");
        with_language("ar", || {
            assert_eq!(left_out_text(1), "تم تخطي عنصر واحد لم يعد موجوداً");
            assert_eq!(left_out_text(2), "تم تخطي عنصرين لم يعودا موجودين");
            assert_eq!(left_out_text(3), "تم تخطي 3 عناصر لم تعد موجودة");
            assert_eq!(left_out_text(11), "تم تخطي 11 عنصراً لم تعد موجودة");
            assert_eq!(left_out_text(100), "تم تخطي 100 عنصر لم تعد موجودة");
        });
    }

    #[test]
    fn share_toasts_read_in_arabic() {
        with_language("ar", || {
            assert_eq!(
                failed_text(ShareTarget::Mail, "x"),
                "تعذّرت المشاركة عبر البريد: x"
            );
            assert_eq!(
                refused_text(ShareTarget::Notes, true),
                "الملاحظات لا تقبل مجلدًا مع ملفات. أفلت المجلد وحده، أو اضغطه بـ«أرشفة» أولًا."
            );
            assert_eq!(
                refused_text(ShareTarget::Mail, false),
                "تعذّرت المشاركة عبر البريد: لا يقبل هذه العناصر"
            );
        });
    }
}
