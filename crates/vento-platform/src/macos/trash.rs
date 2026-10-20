//! Moving files to the Trash.

use std::path::Path;

use objc2_foundation::{NSFileManager, NSURL};

use crate::PlatformError;

/// Move `path` to the Trash of its volume. The item itself is moved (a link,
/// not what it points to), as the Finder's "Move to Trash" does.
pub(super) fn trash(path: &Path) -> Result<(), PlatformError> {
    let url = NSURL::from_file_path(path)
        .ok_or_else(|| PlatformError::Failed(format!("not a file path: {}", path.display())))?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| PlatformError::Failed(e.localizedDescription().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_that_is_not_there_is_an_error() {
        let dir = tempfile_dir();
        let gone = dir.join("never-created");
        assert!(matches!(trash(&gone), Err(PlatformError::Failed(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Puts two small files in the real Trash, so it only runs on demand:
    /// `cargo test -p vento-platform -- --ignored trash_moves_links`.
    #[test]
    #[ignore = "writes to the user's Trash"]
    fn trash_moves_links_not_their_targets() {
        let dir = tempfile_dir();
        let target = dir.join("vento-trash-check-target.txt");
        std::fs::write(&target, "keep me").unwrap();
        let link = dir.join("vento-trash-check-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        trash(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
        let folder = dir.join("vento-trash-check-folder");
        std::fs::create_dir_all(folder.join("inner")).unwrap();
        trash(&folder).unwrap();
        assert!(!folder.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vento-trash-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
