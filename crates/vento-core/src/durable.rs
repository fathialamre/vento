//! Flushing written files before they are renamed into place.
//!
//! Every save writes a temporary file and renames it over the real one. What
//! that needs from the disk is order, not an immediate flush: the new content
//! must be stored before the rename is, so a power cut leaves the old file or
//! the new one, never a torn one. On Apple systems `File::sync_all` is
//! `F_FULLFSYNC`, which empties the whole drive cache and takes 5–50 ms or
//! more; here [`sync`] is `F_BARRIERFSYNC` instead (as SQLite uses): the
//! file's data is sent to the drive, followed by a barrier that keeps later
//! writes, the rename among them, from being stored ahead of it. The last
//! save before a power cut may be lost; a half-written one is never seen.
//! Elsewhere both functions are `sync_all`.

use std::fs::File;
use std::io;

/// Makes `file`'s written data reach the disk before anything written after
/// this call, such as the rename that publishes it.
pub fn sync(file: &File) -> io::Result<()> {
    imp::sync(file)
}

/// [`sync`] for several files at once, for content made of many files that
/// is published by one rename: each file's data is sent to the drive, with
/// one barrier after the last instead of one per file.
pub fn sync_files(files: &[File]) -> io::Result<()> {
    imp::sync_files(files)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    pub fn sync(file: &File) -> io::Result<()> {
        // SAFETY: the descriptor is open for as long as `file` is borrowed.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } != -1 {
            return Ok(());
        }
        // Not every file system takes a barrier (some network shares don't):
        // a plain fsync still sends the data.
        fsync(file)
    }

    pub fn sync_files(files: &[File]) -> io::Result<()> {
        let Some((last, rest)) = files.split_last() else {
            return Ok(());
        };
        // Data a plain fsync sent is ahead of the barrier on `last` too.
        for file in rest {
            fsync(file)?;
        }
        sync(last)
    }

    /// `fsync(2)`: on Apple systems it hands the data to the drive without
    /// emptying the drive's cache.
    fn fsync(file: &File) -> io::Result<()> {
        loop {
            // SAFETY: the descriptor is open for as long as `file` is borrowed.
            if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
mod imp {
    use std::fs::File;
    use std::io;

    pub fn sync(file: &File) -> io::Result<()> {
        file.sync_all()
    }

    pub fn sync_files(files: &[File]) -> io::Result<()> {
        files.iter().try_for_each(File::sync_all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sync_keeps_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"hello").unwrap();
        sync(&file).unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn sync_files_takes_none_one_or_many() {
        let dir = tempfile::tempdir().unwrap();
        sync_files(&[]).unwrap();
        let mut files = Vec::new();
        for i in 0..3 {
            let mut file = File::create(dir.path().join(i.to_string())).unwrap();
            file.write_all(i.to_string().as_bytes()).unwrap();
            files.push(file);
        }
        sync_files(&files[..1]).unwrap();
        sync_files(&files).unwrap();
        drop(files);
        for i in 0..3 {
            let got = std::fs::read(dir.path().join(i.to_string())).unwrap();
            assert_eq!(got, i.to_string().as_bytes());
        }
    }
}
