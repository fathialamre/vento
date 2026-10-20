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
}

impl ActionId {
    /// Stable key used in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            ActionId::Pocket => "pocket",
            ActionId::Copy => "copy",
            ActionId::Move => "move",
            ActionId::CopyPath => "copy_path",
            ActionId::Terminal => "terminal",
        }
    }

    /// Human-readable label shown on the wheel.
    pub fn label(self) -> &'static str {
        match self {
            ActionId::Pocket => "Pocket",
            ActionId::Copy => "Copy",
            ActionId::Move => "Move",
            ActionId::CopyPath => "Copy Path",
            ActionId::Terminal => "Terminal",
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
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_labels_roundtrip() {
        for a in [
            ActionId::Pocket,
            ActionId::Copy,
            ActionId::Move,
            ActionId::CopyPath,
            ActionId::Terminal,
        ] {
            assert_eq!(ActionId::from_key(a.key()), Some(a));
        }
        assert_eq!(ActionId::CopyPath.label(), "Copy Path");
        assert_eq!(ActionId::from_key("teleport"), None);
    }

    #[test]
    fn pocket_action_key_and_label() {
        assert_eq!(ActionId::Pocket.key(), "pocket");
        assert_eq!(ActionId::Pocket.label(), "Pocket");
        assert_eq!(ActionId::from_key("pocket"), Some(ActionId::Pocket));
    }
}
