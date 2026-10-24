//! Moving files to the Trash.

use std::path::Path;

use gio::prelude::*;

use crate::PlatformError;

/// Move `path` to the Trash of its mount, as Files' "Move to Trash" does.
/// gio moves the item itself (a link, not what it points to).
pub(super) fn trash(path: &Path) -> Result<(), PlatformError> {
    gio::File::for_path(path)
        .trash(gio::Cancellable::NONE)
        .map_err(|e| {
            if e.matches(gio::IOErrorEnum::NotSupported) {
                PlatformError::Unsupported
            } else {
                PlatformError::Failed(e.message().to_owned())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_that_is_not_there_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("never-created");
        assert!(matches!(trash(&gone), Err(PlatformError::Failed(_))));
    }

    /// Puts two small items in the real Trash, so it only runs on demand:
    /// `cargo test -p vento-platform -- --ignored trash_moves_links`.
    #[test]
    #[ignore = "writes to the user's Trash"]
    fn trash_moves_links_not_their_targets() {
        // Under $HOME, so they go to the home Trash and not a /tmp one.
        let home = std::env::var_os("HOME").unwrap();
        let dir = tempfile::tempdir_in(home).unwrap();
        let target = dir.path().join("vento-trash-check-target.txt");
        std::fs::write(&target, "keep me").unwrap();
        let link = dir.path().join("vento-trash-check-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        trash(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
        let folder = dir.path().join("vento-trash-check-folder");
        std::fs::create_dir_all(folder.join("inner")).unwrap();
        trash(&folder).unwrap();
        assert!(!folder.exists());
    }
}
