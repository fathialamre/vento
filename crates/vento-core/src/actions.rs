//! The actions a wheel cell can trigger.

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
}

impl ActionId {
    /// Every action, in the order the settings list offers the ones not yet
    /// placed.
    pub const ALL: [ActionId; 7] = [
        ActionId::Pocket,
        ActionId::Copy,
        ActionId::Move,
        ActionId::CopyPath,
        ActionId::Terminal,
        ActionId::CopyTo,
        ActionId::Archive,
    ];

    /// Whether the action opens the folder ring when the pointer rests on its
    /// cell.
    pub fn takes_ring(self) -> bool {
        matches!(self, ActionId::Move | ActionId::CopyTo)
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
        }
    }

    /// Human-readable label shown on the wheel.
    pub fn label(self) -> &'static str {
        match self {
            ActionId::Pocket => "Pocket",
            ActionId::Copy => "Copy",
            ActionId::Move => "Move",
            ActionId::CopyPath => "Copy path",
            ActionId::Terminal => "Terminal",
            ActionId::CopyTo => "Copy to",
            ActionId::Archive => "Archive",
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
        assert_eq!(ringed, [ActionId::Move, ActionId::CopyTo]);
    }

    #[test]
    fn archive_key_label_and_no_ring() {
        assert_eq!(ActionId::Archive.key(), "archive");
        assert_eq!(ActionId::Archive.label(), "Archive");
        assert_eq!(ActionId::from_key("archive"), Some(ActionId::Archive));
        assert!(!ActionId::Archive.takes_ring());
        assert_eq!(ActionId::ALL.last(), Some(&ActionId::Archive));
    }
}
