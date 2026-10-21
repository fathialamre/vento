//! Hidden partial items that archive output is built under before it gets its
//! final name.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::fileops::{NAME_ATTEMPTS, partial_name};

/// Creates an empty file under the first free partial name in `dir`.
///
/// An item already at a partial name is skipped and left alone: the file is
/// opened with `create_new`, which never follows or replaces anything.
pub(crate) fn claim_partial_file(dir: &Path, name: &OsStr) -> io::Result<(PathBuf, File)> {
    for attempt in 1..=NAME_ATTEMPTS {
        let candidate = dir.join(partial_name(name, attempt));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(no_free_name())
}

/// Creates an empty folder under the first free partial name in `dir`.
///
/// An item already at a partial name is skipped and left alone.
// Used by extraction (a later task).
#[allow(dead_code)]
pub(crate) fn claim_partial_dir(dir: &Path, name: &OsStr) -> io::Result<PathBuf> {
    for attempt in 1..=NAME_ATTEMPTS {
        let candidate = dir.join(partial_name(name, attempt));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(no_free_name())
}

fn no_free_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for a temporary item",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn claims_file_under_partial_name() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut file) = claim_partial_file(dir.path(), OsStr::new("a.zip")).unwrap();
        assert_eq!(path, dir.path().join(partial_name(OsStr::new("a.zip"), 1)));
        file.write_all(b"x").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"x");
    }

    #[test]
    fn file_claim_skips_and_keeps_existing_partial_names() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = dir.path().join(partial_name(OsStr::new("a.zip"), 1));
        fs::write(&theirs, b"theirs").unwrap();
        let (path, _file) = claim_partial_file(dir.path(), OsStr::new("a.zip")).unwrap();
        assert_eq!(path, dir.path().join(partial_name(OsStr::new("a.zip"), 2)));
        assert_eq!(fs::read(&theirs).unwrap(), b"theirs");
    }

    #[cfg(unix)]
    #[test]
    fn file_claim_skips_a_dangling_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = dir.path().join(partial_name(OsStr::new("a.zip"), 1));
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &theirs).unwrap();
        let (path, _file) = claim_partial_file(dir.path(), OsStr::new("a.zip")).unwrap();
        assert_ne!(path, theirs);
        assert!(!dir.path().join("nowhere").exists());
    }

    #[test]
    fn claims_dir_and_skips_existing_partial_names() {
        let dir = tempfile::tempdir().unwrap();
        let first = claim_partial_dir(dir.path(), OsStr::new("Photos")).unwrap();
        assert_eq!(
            first,
            dir.path().join(partial_name(OsStr::new("Photos"), 1))
        );
        fs::write(first.join("keep"), b"k").unwrap();
        let second = claim_partial_dir(dir.path(), OsStr::new("Photos")).unwrap();
        assert_eq!(
            second,
            dir.path().join(partial_name(OsStr::new("Photos"), 2))
        );
        assert!(second.is_dir());
        assert_eq!(fs::read(first.join("keep")).unwrap(), b"k");
    }
}
