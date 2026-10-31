//! Where macOS keeps screenshots, how it marks them, and which folders it
//! guards.

use std::path::{Component, Path, PathBuf};

use objc2::AnyThread;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL, NSUserDefaults, ns_string};

use crate::PlatformError;

/// Privacy & Security → Files & Folders in System Settings.
const FILES_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";

/// The attribute macOS puts on every screenshot it saves, whatever the file
/// is later called or wherever it is copied.
const SCREEN_CAPTURE: &str = "com.apple.metadata:kMDItemIsScreenCapture";

/// The folders in the home folder that macOS asks the user's leave to read.
const PROTECTED: [&str; 3] = ["Desktop", "Documents", "Downloads"];

/// The user's home folder.
fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_default()
}

/// `path` with a leading `~` (alone or before a `/`) taken for `home`.
fn expand_tilde(home: &Path, path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

/// The folder macOS saves screenshots in: the `location` the Screenshot app
/// sets, with `~` expanded, when it is a folder; else `~/Desktop`.
pub(crate) fn screenshot_folder() -> PathBuf {
    let defaults = NSUserDefaults::initWithSuiteName(
        NSUserDefaults::alloc(),
        Some(ns_string!("com.apple.screencapture")),
    );
    let location = defaults
        .and_then(|defaults| defaults.stringForKey(ns_string!("location")))
        .map(|location| location.to_string());
    folder_from(location.as_deref(), &home())
}

/// The screenshots folder for the Screenshot app's `location` setting: that
/// folder (`~` expanded) when it is an existing absolute one, else
/// `home/Desktop`.
fn folder_from(location: Option<&str>, home: &Path) -> PathBuf {
    if let Some(location) = location {
        let folder = expand_tilde(home, location);
        if folder.is_absolute() && folder.is_dir() {
            return folder;
        }
        log::debug!("screenshot location {location:?} is not a folder; using the Desktop");
    }
    home.join("Desktop")
}

/// Whether macOS marked `path` as a screenshot it saved. False when the file
/// has no mark and when it can't be read.
pub(crate) fn is_screen_capture(path: &Path) -> bool {
    match xattr::get(path, SCREEN_CAPTURE) {
        Ok(value) => value.is_some(),
        Err(e) => {
            log::debug!("screen capture mark of {} unreadable: {e}", path.display());
            false
        }
    }
}

/// Whether the folder `path` is one macOS asks the user's leave to read:
/// see [`protected_under`].
pub(crate) fn is_protected_folder(path: &Path) -> bool {
    protected_under(&home(), path)
}

/// Whether `path` is `home`'s Desktop, Documents or Downloads, or inside
/// one of them. Compares whole path parts (`Desktops` is not `Desktop`).
pub(crate) fn protected_under(home: &Path, path: &Path) -> bool {
    let Ok(below) = path.strip_prefix(home) else {
        return false;
    };
    matches!(
        below.components().next(),
        Some(Component::Normal(first)) if PROTECTED.iter().any(|name| first == *name)
    )
}

/// Opens Files & Folders in System Settings.
pub(crate) fn open_files_privacy() -> Result<(), PlatformError> {
    let url = NSURL::URLWithString(&NSString::from_str(FILES_SETTINGS_URL))
        .ok_or_else(|| PlatformError::Failed("bad Files & Folders settings URL".into()))?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(PlatformError::Failed(
            "cannot open the Files & Folders settings".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_under_desktop_and_inside() {
        let home = Path::new("/Users/a");
        assert!(protected_under(home, Path::new("/Users/a/Desktop")));
        assert!(protected_under(home, Path::new("/Users/a/Desktop/Shots")));
        assert!(protected_under(home, Path::new("/Users/a/Documents")));
        assert!(protected_under(home, Path::new("/Users/a/Downloads/x/y")));
        assert!(!protected_under(home, Path::new("/Users/a/Pictures")));
        assert!(!protected_under(home, Path::new("/Users/a/Desktops")));
        assert!(!protected_under(home, Path::new("/Users/b/Desktop")));
    }

    #[test]
    fn screenshot_folder_is_a_directory() {
        let folder = screenshot_folder();
        assert!(folder.is_absolute(), "{folder:?}");
        // Either a folder that exists, or the Desktop to fall back to.
        assert!(
            folder.is_dir() || folder == home().join("Desktop"),
            "{folder:?}"
        );
    }

    #[test]
    fn is_screen_capture_false_for_a_plain_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Screenshot 2026.png");
        std::fs::write(&file, b"x").unwrap();
        assert!(!is_screen_capture(&file));
        assert!(!is_screen_capture(&dir.path().join("gone.png")));
    }

    #[test]
    fn is_screen_capture_true_with_the_attribute() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.png");
        std::fs::write(&file, b"x").unwrap();
        xattr::set(&file, SCREEN_CAPTURE, b"bplist00").unwrap();
        assert!(is_screen_capture(&file));
    }

    #[test]
    fn the_screenshot_location_is_used_when_it_is_a_folder() {
        let home = tempfile::tempdir().unwrap();
        let shots = home.path().join("Shots");
        std::fs::create_dir(&shots).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let desktop = home.path().join("Desktop");
        // A custom absolute folder.
        let custom = elsewhere.path().to_str().unwrap();
        assert_eq!(folder_from(Some(custom), home.path()), elsewhere.path());
        // One under `~`.
        assert_eq!(folder_from(Some("~/Shots"), home.path()), shots);
        // Unset, missing, or not a folder: the Desktop.
        assert_eq!(folder_from(None, home.path()), desktop);
        assert_eq!(folder_from(Some("~/Nope"), home.path()), desktop);
        assert_eq!(folder_from(Some("Shots"), home.path()), desktop);
    }

    #[test]
    fn a_tilde_is_the_home_folder() {
        let home = Path::new("/Users/a");
        assert_eq!(expand_tilde(home, "~"), PathBuf::from("/Users/a"));
        assert_eq!(
            expand_tilde(home, "~/Pictures/Shots"),
            PathBuf::from("/Users/a/Pictures/Shots")
        );
        assert_eq!(expand_tilde(home, "/tmp/x"), PathBuf::from("/tmp/x"));
        assert_eq!(expand_tilde(home, "~b/x"), PathBuf::from("~b/x"));
    }
}
