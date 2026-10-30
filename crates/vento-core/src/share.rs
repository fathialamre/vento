//! The places the Share action can send the dragged items to, and the rules
//! around them: which ones the ring shows, what a direct release means, the
//! texts, and which dropped paths are worth sharing. The system calls that do
//! the sharing live in the app crate, so nothing here depends on the OS.

use std::path::PathBuf;

use crate::i18n::tr;

/// A place the dragged items can be shared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShareTarget {
    /// AirDrop. Not on the ring: releasing on the Share cell itself sends
    /// here when the machine offers it.
    AirDrop,
    /// Send the items in a new message.
    Messages,
    /// Attach the items to a new mail.
    Mail,
    /// Add the items to a note.
    Notes,
    /// The system's share menu, which lists every other service.
    More,
}

impl ShareTarget {
    /// The targets on the ring, in the order they sit. AirDrop is left out on
    /// purpose, it is what the cell itself does. A machine may lack some of
    /// them, see [`ring_targets`].
    pub const RING: [ShareTarget; 4] = [
        ShareTarget::Messages,
        ShareTarget::Mail,
        ShareTarget::Notes,
        ShareTarget::More,
    ];

    /// Stable key, used in log lines.
    pub fn key(self) -> &'static str {
        match self {
            ShareTarget::AirDrop => "airdrop",
            ShareTarget::Messages => "messages",
            ShareTarget::Mail => "mail",
            ShareTarget::Notes => "notes",
            ShareTarget::More => "more",
        }
    }

    /// Short name shown on the ring cell, in the interface language.
    pub fn name(self) -> String {
        match self {
            ShareTarget::AirDrop => tr("AirDrop"),
            ShareTarget::Messages => tr("Messages"),
            ShareTarget::Mail => tr("Mail"),
            ShareTarget::Notes => tr("Notes"),
            ShareTarget::More => tr("More…"),
        }
    }

    /// Sentence shown at the wheel's centre while the pointer rests on this
    /// target, in the interface language.
    pub fn hover_text(self) -> String {
        match self {
            ShareTarget::AirDrop => tr("Share with AirDrop"),
            ShareTarget::Messages => tr("Send with Messages"),
            ShareTarget::Mail => tr("Send with Mail"),
            ShareTarget::Notes => tr("Add to Notes"),
            ShareTarget::More => tr("More ways to share"),
        }
    }
}

/// The targets the ring shows, in ring order. Messages, Mail and Notes appear
/// only when `available` lists them, since a machine can lack a service;
/// More is always there, so the ring is never empty.
pub fn ring_targets(available: &[ShareTarget]) -> Vec<ShareTarget> {
    ShareTarget::RING
        .into_iter()
        .filter(|t| *t == ShareTarget::More || available.contains(t))
        .collect()
}

/// The target a release on the Share cell itself means: AirDrop when
/// `available` lists it, otherwise the system's share menu.
pub fn direct_target(available: &[ShareTarget]) -> ShareTarget {
    if available.contains(&ShareTarget::AirDrop) {
        ShareTarget::AirDrop
    } else {
        ShareTarget::More
    }
}

/// The text at the wheel's centre while the pointer rests on the Share cell,
/// naming what a release there does, see [`direct_target`].
pub fn centre_text(available: &[ShareTarget]) -> String {
    if direct_target(available) == ShareTarget::AirDrop {
        tr("Share with AirDrop")
    } else {
        tr("Share…")
    }
}

/// Splits off the dropped paths that are gone by the time the action runs: the
/// ones that still exist, in their order, and how many were left out. A
/// symlink that points at nothing counts as gone.
pub fn existing(paths: &[PathBuf]) -> (Vec<PathBuf>, usize) {
    let kept: Vec<PathBuf> = paths.iter().filter(|p| p.exists()).cloned().collect();
    let left_out = paths.len() - kept.len();
    (kept, left_out)
}

/// Whether the paths hold at least one folder and at least one thing that is
/// not a folder. A path that does not exist counts as not a folder.
pub fn mixes_folders_and_files(paths: &[PathBuf]) -> bool {
    paths.iter().any(|p| p.is_dir()) && paths.iter().any(|p| !p.is_dir())
}

/// How a share ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareOutcome {
    /// The service took the items.
    Sent,
    /// The person closed the service's window without sending.
    Cancelled,
    /// The service could not take the items; the text says why.
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_language;

    #[test]
    fn ring_keeps_order_and_drops_missing_services() {
        assert_eq!(
            ring_targets(&[ShareTarget::AirDrop, ShareTarget::Mail]),
            [ShareTarget::Mail, ShareTarget::More]
        );
        assert_eq!(ring_targets(&[]), [ShareTarget::More]);
        let all = [
            ShareTarget::AirDrop,
            ShareTarget::Messages,
            ShareTarget::Mail,
            ShareTarget::Notes,
        ];
        assert_eq!(ring_targets(&all), ShareTarget::RING);
    }

    #[test]
    fn direct_target_with_airdrop() {
        assert_eq!(direct_target(&[ShareTarget::AirDrop]), ShareTarget::AirDrop);
    }

    #[test]
    fn direct_target_without_airdrop_is_more() {
        assert_eq!(direct_target(&[ShareTarget::Mail]), ShareTarget::More);
        assert_eq!(centre_text(&[ShareTarget::Mail]), "Share…");
        assert_eq!(centre_text(&[ShareTarget::AirDrop]), "Share with AirDrop");
    }

    #[test]
    fn names_and_hover_texts() {
        assert_eq!(ShareTarget::More.name(), "More…");
        assert_eq!(ShareTarget::Notes.hover_text(), "Add to Notes");
        assert_eq!(ShareTarget::More.hover_text(), "More ways to share");
    }

    #[test]
    fn existing_leaves_out_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, "a").unwrap();
        let gone = dir.path().join("gone.txt");
        assert_eq!(existing(&[gone.clone(), a.clone()]), (vec![a], 1));
        assert_eq!(existing(&[gone]), (vec![], 1));
    }

    #[test]
    fn mixed_folders_and_files_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "f").unwrap();
        let d = dir.path().join("d");
        std::fs::create_dir(&d).unwrap();
        assert!(mixes_folders_and_files(&[f.clone(), d.clone()]));
        assert!(!mixes_folders_and_files(&[d]));
        assert!(!mixes_folders_and_files(&[f]));
    }

    #[test]
    fn names_read_in_arabic() {
        with_language("ar", || {
            assert_eq!(ShareTarget::Mail.name(), "البريد");
            assert_eq!(ShareTarget::AirDrop.name(), "\u{200f}AirDrop");
        });
    }
}
