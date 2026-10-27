//! The actions a wheel cell can trigger.

use crate::i18n::tr;

/// An action that can be placed on the wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    /// Park the dragged items in the Pocket, a holding shelf.
    Pocket,
    /// Put the dragged files on the clipboard, ready to paste.
    Copy,
    /// Move the dragged files to a chosen folder.
    Move,
    /// Put the dragged files' paths on the clipboard.
    CopyPath,
    /// Open a terminal at the dragged files' location.
    Terminal,
    /// Copy the dragged files to a chosen folder.
    CopyTo,
    /// Compress the dragged files to a zip, or extract the archives among them.
    Archive,
    /// Show the size of the dragged files and folders in a window.
    Info,
    /// Convert the dragged images to another format, chosen from a ring.
    Convert,
}

impl ActionId {
    /// Every action, in the order the settings list offers the ones not yet
    /// placed.
    pub const ALL: [ActionId; 9] = [
        ActionId::Pocket,
        ActionId::Copy,
        ActionId::Move,
        ActionId::CopyPath,
        ActionId::Terminal,
        ActionId::CopyTo,
        ActionId::Archive,
        ActionId::Info,
        ActionId::Convert,
    ];

    /// Whether the action opens the folder ring when the pointer rests on its
    /// cell.
    pub fn takes_ring(self) -> bool {
        matches!(self, ActionId::Move | ActionId::CopyTo | ActionId::Convert)
    }

    /// Stable key used in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            ActionId::Pocket => "pocket",
            ActionId::Copy => "copy",
            ActionId::Move => "move",
            ActionId::CopyPath => "copy_path",
            ActionId::Terminal => "terminal",
            ActionId::CopyTo => "copy_to",
            ActionId::Archive => "archive",
            ActionId::Info => "info",
            ActionId::Convert => "convert",
        }
    }

    /// Human-readable label shown on the wheel, in the interface language.
    pub fn label(self) -> String {
        match self {
            ActionId::Pocket => tr("Pocket"),
            ActionId::Copy => tr("Copy"),
            ActionId::Move => tr("Move"),
            ActionId::CopyPath => tr("Copy path"),
            ActionId::Terminal => tr("Terminal"),
            ActionId::CopyTo => tr("Copy to"),
            ActionId::Archive => tr("Archive"),
            ActionId::Info => tr("Info"),
            ActionId::Convert => tr("Convert"),
        }
    }

    /// Parses a settings-file key; `None` for unknown keys.
    pub fn from_key(key: &str) -> Option<ActionId> {
        match key {
            "pocket" => Some(ActionId::Pocket),
            "copy" => Some(ActionId::Copy),
            "move" => Some(ActionId::Move),
            "copy_path" => Some(ActionId::CopyPath),
            "terminal" => Some(ActionId::Terminal),
            "copy_to" => Some(ActionId::CopyTo),
            "archive" => Some(ActionId::Archive),
            "info" => Some(ActionId::Info),
            "convert" => Some(ActionId::Convert),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_labels_roundtrip() {
        for a in ActionId::ALL {
            assert_eq!(ActionId::from_key(a.key()), Some(a));
        }
        assert_eq!(ActionId::CopyPath.label(), "Copy path");
        assert_eq!(ActionId::from_key("teleport"), None);
    }

    #[test]
    fn pocket_action_key_and_label() {
        assert_eq!(ActionId::Pocket.key(), "pocket");
        assert_eq!(ActionId::Pocket.label(), "Pocket");
        assert_eq!(ActionId::from_key("pocket"), Some(ActionId::Pocket));
    }

    #[test]
    fn copy_to_key_label_and_ring() {
        assert_eq!(ActionId::CopyTo.key(), "copy_to");
        assert_eq!(ActionId::CopyTo.label(), "Copy to");
        let ringed: Vec<_> = ActionId::ALL
            .into_iter()
            .filter(|a| a.takes_ring())
            .collect();
        assert_eq!(
            ringed,
            [ActionId::Move, ActionId::CopyTo, ActionId::Convert]
        );
    }

    #[test]
    fn archive_key_label_and_no_ring() {
        assert_eq!(ActionId::Archive.key(), "archive");
        assert_eq!(ActionId::Archive.label(), "Archive");
        assert_eq!(ActionId::from_key("archive"), Some(ActionId::Archive));
        assert!(!ActionId::Archive.takes_ring());
    }

    #[test]
    fn info_key_and_label() {
        assert_eq!(ActionId::Info.key(), "info");
        assert_eq!(ActionId::Info.label(), "Info");
        assert_eq!(ActionId::from_key("info"), Some(ActionId::Info));
        assert!(!ActionId::Info.takes_ring());
    }

    #[test]
    fn convert_key_label_and_ring() {
        assert_eq!(ActionId::Convert.key(), "convert");
        assert_eq!(ActionId::Convert.label(), "Convert");
        assert_eq!(ActionId::from_key("convert"), Some(ActionId::Convert));
        assert!(ActionId::Convert.takes_ring());
        assert_eq!(ActionId::ALL.last(), Some(&ActionId::Convert));
    }

    #[test]
    fn labels_are_translated() {
        crate::i18n::with_language("ar", || {
            let labels: Vec<_> = ActionId::ALL.iter().map(|a| a.label()).collect();
            assert_eq!(
                labels,
                [
                    "الجيب",
                    "نسخ",
                    "نقل",
                    "نسخ المسار",
                    "الطرفية",
                    "نسخ إلى",
                    "أرشفة",
                    "معلومات",
                    "تحويل"
                ]
            );
            assert_eq!(ActionId::Move.label(), "نقل");
        });
        assert_eq!(ActionId::Move.label(), "Move");
    }
}
