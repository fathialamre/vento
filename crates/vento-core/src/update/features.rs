//! The features a changelog entry can belong to: the key that opens an entry,
//! the name shown for it, its icon and the Settings page that configures it.

use crate::i18n::{tr, trc};

/// Which drawing stands for a feature in the What's new window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureIcon {
    /// The action wheel.
    Wheel,
    /// Moving files.
    Move,
    /// The Pocket shelf.
    Pocket,
    /// The clipboard history.
    Clipboard,
    /// Copying text from the screen.
    ScanText,
    /// Converting images.
    Convert,
    /// PDF pages.
    Pdf,
    /// Archives.
    Archive,
    /// File information.
    Info,
    /// Sharing.
    Share,
    /// Keeping the computer awake.
    Awake,
    /// The drop mark, for Vento itself (updates).
    Drop,
    /// Anything general.
    General,
}

/// One feature of Vento, as a changelog entry names it.
#[derive(Clone, Copy, Debug)]
pub struct Feature {
    /// The English key that opens a changelog entry (`- Pocket: ...`).
    pub key: &'static str,
    /// The feature's name in the interface language.
    pub name: fn() -> String,
    /// Its icon.
    pub icon: FeatureIcon,
    /// The Settings page that holds its settings (`Page::key()` in the
    /// Settings window), if it has one.
    pub settings_page: Option<&'static str>,
}

/// Every feature, in the order the What's new window lists them.
pub static FEATURES: &[Feature] = &[
    Feature {
        key: "Wheel",
        name: || tr("Wheel"),
        icon: FeatureIcon::Wheel,
        settings_page: Some("wheel"),
    },
    Feature {
        key: "Move",
        name: || trc("page", "Move"),
        icon: FeatureIcon::Move,
        settings_page: Some("move"),
    },
    Feature {
        key: "Pocket",
        name: || tr("Pocket"),
        icon: FeatureIcon::Pocket,
        settings_page: Some("pocket"),
    },
    Feature {
        key: "Clipboard",
        name: || tr("Clipboard"),
        icon: FeatureIcon::Clipboard,
        settings_page: Some("clipboard"),
    },
    Feature {
        key: "Copy from screen",
        name: || tr("Copy from screen"),
        icon: FeatureIcon::ScanText,
        settings_page: Some("copy_from_screen"),
    },
    Feature {
        key: "Convert",
        name: || tr("Convert"),
        icon: FeatureIcon::Convert,
        settings_page: None,
    },
    Feature {
        key: "PDF Pages",
        name: || tr("PDF Pages"),
        icon: FeatureIcon::Pdf,
        settings_page: None,
    },
    Feature {
        key: "Archive",
        name: || tr("Archive"),
        icon: FeatureIcon::Archive,
        settings_page: None,
    },
    Feature {
        key: "Info",
        name: || tr("Info"),
        icon: FeatureIcon::Info,
        settings_page: None,
    },
    Feature {
        key: "Share",
        name: || tr("Share"),
        icon: FeatureIcon::Share,
        settings_page: None,
    },
    Feature {
        key: "Keep Awake",
        name: || tr("Keep Awake"),
        icon: FeatureIcon::Awake,
        settings_page: Some("keep_awake"),
    },
    Feature {
        key: "Updates",
        name: || tr("Updates"),
        icon: FeatureIcon::Drop,
        settings_page: Some("about"),
    },
    Feature {
        key: "Settings",
        name: || tr("Settings"),
        icon: FeatureIcon::General,
        settings_page: Some("general"),
    },
    Feature {
        key: "Welcome",
        name: || tr("Welcome setup"),
        icon: FeatureIcon::Info,
        settings_page: Some("about"),
    },
];

/// The feature whose key is `key`, ignoring ASCII case.
pub fn find(key: &str) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| f.key.eq_ignore_ascii_case(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_ignores_ascii_case() {
        assert_eq!(find("pdf pages").unwrap().key, "PDF Pages");
        assert_eq!(find("KEEP AWAKE").unwrap().key, "Keep Awake");
        assert!(find("Nothing").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn keys_are_unique() {
        for (i, f) in FEATURES.iter().enumerate() {
            assert!(
                FEATURES[i + 1..]
                    .iter()
                    .all(|g| !g.key.eq_ignore_ascii_case(f.key)),
                "duplicate key {}",
                f.key
            );
        }
    }

    #[test]
    fn names_are_not_empty() {
        for f in FEATURES {
            assert!(!(f.name)().is_empty(), "{}", f.key);
        }
    }

    #[test]
    fn settings_pages_are_settings_window_keys() {
        let pages = [
            "general",
            "wheel",
            "move",
            "pocket",
            "clipboard",
            "copy_from_screen",
            "keep_awake",
            "about",
        ];
        for f in FEATURES {
            if let Some(p) = f.settings_page {
                assert!(pages.contains(&p), "{} -> {p}", f.key);
            }
        }
    }
}
