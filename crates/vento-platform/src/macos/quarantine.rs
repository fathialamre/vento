//! The quarantine attribute Gatekeeper uses to tag files that came from
//! elsewhere.

use std::path::Path;

/// Name of the attribute Gatekeeper reads.
const QUARANTINE: &str = "com.apple.quarantine";

/// `path`'s quarantine attribute, or `None` when it has none or can't be
/// read. `xattr::get` works on a link itself, never on what it points to.
pub(crate) fn quarantine_of(path: &Path) -> Option<Vec<u8>> {
    match xattr::get(path, QUARANTINE) {
        Ok(value) => value,
        Err(e) => {
            log::debug!("quarantine of {} unreadable: {e}", path.display());
            None
        }
    }
}

/// Give `path` the quarantine attribute `value`. `xattr::set` works on a
/// link itself, never on what it points to.
pub(crate) fn set_quarantine(path: &Path, value: &[u8]) -> std::io::Result<()> {
    xattr::set(path, QUARANTINE, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUE: &[u8] = b"0083;00000000;vento;";

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vento-quarantine-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn quarantine_round_trips() {
        let dir = scratch("round");
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        set_quarantine(&file, VALUE).unwrap();
        assert_eq!(quarantine_of(&file).as_deref(), Some(VALUE));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_quarantine_is_none() {
        let dir = scratch("missing");
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(quarantine_of(&file), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_path_that_is_not_there_is_none() {
        let dir = scratch("gone");
        assert_eq!(quarantine_of(&dir.join("never-created")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn marking_a_link_leaves_its_target_alone() {
        let dir = scratch("link");
        let target = dir.join("target.txt");
        std::fs::write(&target, "x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        set_quarantine(&link, VALUE).unwrap();
        assert_eq!(quarantine_of(&link).as_deref(), Some(VALUE));
        assert_eq!(quarantine_of(&target), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_does_not_report_its_targets_mark() {
        let dir = scratch("target-marked");
        let target = dir.join("target.txt");
        std::fs::write(&target, "x").unwrap();
        set_quarantine(&target, VALUE).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(quarantine_of(&link), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dangling_link_can_be_marked() {
        let dir = scratch("dangling");
        let link = dir.join("dangling");
        std::os::unix::fs::symlink(dir.join("nowhere"), &link).unwrap();
        set_quarantine(&link, VALUE).unwrap();
        assert_eq!(quarantine_of(&link).as_deref(), Some(VALUE));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
