//! Reading zip archives.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zip::read::ZipFile;
use zip::result::ZipError;
use zip::{ExtraField, ZipArchive};

use super::entries::{ArchiveReader, EntryInfo, EntryKind, safe_relative};
use super::job::ArchiveError;
use super::staging::Staging;

/// Bits of a unix mode that give the file type, and the types zip can hold.
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;
/// Longest symlink target read from an archive (the usual `PATH_MAX`).
const MAX_LINK_TARGET: u64 = 4096;

/// A zip archive opened for extraction.
pub(crate) struct ZipReader<'p> {
    archive: ZipArchive<BufReader<File>>,
    password: Option<&'p [u8]>,
    /// Kept entries with their index in the archive, listed once.
    kept: Option<Vec<(usize, EntryInfo)>>,
}

impl<'p> ZipReader<'p> {
    /// Opens the zip at `path`; encrypted entries are read with `password`.
    pub(crate) fn open(path: &Path, password: Option<&'p [u8]>) -> Result<Self, ArchiveError> {
        let file = File::open(path)?;
        let archive = ZipArchive::new(BufReader::new(file)).map_err(zip_error)?;
        Ok(ZipReader {
            archive,
            password,
            kept: None,
        })
    }

    /// The kept entries, read from the central directory (no data is
    /// decrypted or decompressed) the first time.
    fn list(&mut self) -> Result<&[(usize, EntryInfo)], ArchiveError> {
        if self.kept.is_none() {
            let mut kept = Vec::new();
            for index in 0..self.archive.len() {
                let entry = self.archive.by_index_raw(index).map_err(zip_error)?;
                if let Some(info) = entry_info(&entry)? {
                    kept.push((index, info));
                }
            }
            self.kept = Some(kept);
        }
        Ok(self.kept.as_deref().unwrap_or_default())
    }
}

impl ArchiveReader for ZipReader<'_> {
    fn entries(&mut self) -> Result<Vec<EntryInfo>, ArchiveError> {
        Ok(self.list()?.iter().map(|(_, info)| info.clone()).collect())
    }

    fn extract(&mut self, sink: &mut Staging<'_>) -> Result<(), ArchiveError> {
        let kept = self.list()?.to_vec();
        for (index, info) in &kept {
            if info.kind == EntryKind::Dir {
                sink.dir(info)?;
                continue;
            }
            let mut entry = match self.password {
                Some(password) => self.archive.by_index_decrypt(*index, password),
                None => self.archive.by_index(*index),
            }
            .map_err(zip_error)?;
            if info.kind == EntryKind::Symlink {
                sink.symlink(info, &link_target(&mut entry, info.size)?)?;
            } else {
                sink.file(info, &mut entry)?;
            }
        }
        Ok(())
    }
}

/// What `entry` is and where it goes; `None` for an entry that is skipped.
fn entry_info<R: Read>(entry: &ZipFile<'_, R>) -> Result<Option<EntryInfo>, ArchiveError> {
    let name = entry.name().map_err(zip_error)?;
    let Some(path) = safe_relative(&name)? else {
        return Ok(None);
    };
    let file_type = entry.unix_mode().map_or(0, |mode| mode & S_IFMT);
    let kind = if file_type == S_IFLNK {
        EntryKind::Symlink
    } else if file_type == S_IFDIR || entry.is_dir() {
        EntryKind::Dir
    } else if file_type == 0 || file_type == S_IFREG {
        EntryKind::File
    } else {
        return Err(ArchiveError::Unsupported(format!("special file {name}")));
    };
    let size = if kind == EntryKind::Dir {
        0
    } else {
        entry.size()
    };
    Ok(Some(EntryInfo {
        path,
        kind,
        size,
        mode: entry.unix_mode().map(|mode| mode & 0o7777),
        mtime: modified(entry),
    }))
}

/// The entry's modified time: the exact UTC seconds of the Info-ZIP extended
/// timestamp when present, else the DOS date and time read as local time.
fn modified<R: Read>(entry: &ZipFile<'_, R>) -> Option<SystemTime> {
    let extended = entry.extra_data_fields().find_map(|field| match field {
        ExtraField::ExtendedTimestamp(stamp) => stamp.mod_time(),
        _ => None,
    });
    match extended {
        // The field holds a signed 32-bit count, read here as unsigned.
        Some(seconds) => unix_time(seconds.cast_signed()),
        None => entry.last_modified().and_then(dos_time),
    }
}

fn unix_time(seconds: i32) -> Option<SystemTime> {
    let offset = Duration::from_secs(u64::from(seconds.unsigned_abs()));
    if seconds >= 0 {
        UNIX_EPOCH.checked_add(offset)
    } else {
        UNIX_EPOCH.checked_sub(offset)
    }
}

/// A zip DOS timestamp, which has no time zone, read as local time.
fn dos_time(stamp: zip::DateTime) -> Option<SystemTime> {
    let local = chrono::NaiveDate::from_ymd_opt(
        stamp.year().into(),
        stamp.month().into(),
        stamp.day().into(),
    )?
    .and_hms_opt(
        stamp.hour().into(),
        stamp.minute().into(),
        stamp.second().into(),
    )?
    .and_local_timezone(chrono::Local)
    .earliest()?;
    Some(local.into())
}

/// Reads a symlink entry's data: its target.
fn link_target(entry: &mut dyn Read, size: u64) -> Result<String, ArchiveError> {
    if size > MAX_LINK_TARGET {
        return Err(ArchiveError::Damaged("symlink target too long".to_owned()));
    }
    let mut target = Vec::new();
    entry
        .read_to_end(&mut target)
        .map_err(ArchiveError::from_read)?;
    String::from_utf8(target)
        .map_err(|_| ArchiveError::Damaged("symlink target is not UTF-8".to_owned()))
}

fn zip_error(error: ZipError) -> ArchiveError {
    match error {
        ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED) => ArchiveError::NeedPassword,
        ZipError::InvalidPassword => ArchiveError::WrongPassword,
        ZipError::Io(e) => ArchiveError::from_read(e),
        ZipError::InvalidArchive(message) => ArchiveError::Damaged(message.into_owned()),
        ZipError::UnsupportedArchive(message) => ArchiveError::Unsupported(message.to_owned()),
        ZipError::CompressionMethodNotSupported(method) => {
            ArchiveError::Unsupported(format!("compression method {method}"))
        }
        other => ArchiveError::Damaged(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::detect::Format;
    use crate::archive::entries::extract_one;
    use crate::archive::job::Progress;
    use crate::fileops::{Fingerprint, partial_name};
    use std::ffi::OsStr;
    use std::fs;
    use std::io::{self, Write};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use zip::write::SimpleFileOptions;
    use zip::{AesMode, CompressionMethod, ZipWriter};

    /// One entry of a test zip.
    enum E<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Link(&'a str, &'a str),
    }

    fn stored() -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
    }

    fn make_zip(path: &Path, entries: &[E]) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        for entry in entries {
            match entry {
                E::File(name, data) => {
                    zip.start_file(*name, stored()).unwrap();
                    zip.write_all(data).unwrap();
                }
                E::Dir(name) => zip.add_directory(*name, stored()).unwrap(),
                E::Link(name, target) => zip.add_symlink(*name, *target, stored()).unwrap(),
            }
        }
        zip.finish().unwrap();
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    /// Every path below `dir`, relative to it, sorted.
    fn tree(dir: &Path) -> Vec<String> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    walk(root, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    fn no_mark(_: &Path) -> io::Result<()> {
        Ok(())
    }

    fn extract(archive: &Path) -> Result<crate::archive::Made, ArchiveError> {
        extract_one(archive, Format::Zip, None, &Progress::new(), &no_mark)
    }

    #[test]
    fn extract_wraps_in_folder_named_after_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        make_zip(
            &archive,
            &[
                E::File("a.txt", b"a"),
                E::Dir("sub/"),
                E::File("sub/b.txt", b"b"),
            ],
        );

        let made = extract(&archive).unwrap();

        assert_eq!(made.source, archive);
        assert_eq!(made.output, dir.path().join("Photos"));
        assert_eq!(tree(&made.output), ["a.txt", "sub", "sub/b.txt"]);
        assert_eq!(fs::read(made.output.join("sub/b.txt")).unwrap(), b"b");
        assert_eq!(Fingerprint::of(&made.output).unwrap(), made.fingerprint);
        assert_eq!(listing(dir.path()), ["Photos", "Photos.zip"]);
    }

    #[test]
    fn extract_single_folder_is_not_double_nested() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        make_zip(
            &archive,
            &[
                E::Dir("Photos/"),
                E::File("Photos/a.jpg", b"jpg"),
                E::Dir("__MACOSX/"),
                E::File("__MACOSX/Photos/._a.jpg", b"meta"),
                E::File("Photos/._a.jpg", b"meta"),
            ],
        );

        let made = extract(&archive).unwrap();

        assert_eq!(made.output, dir.path().join("Photos"));
        assert_eq!(tree(&made.output), ["a.jpg"]);
        assert_eq!(listing(dir.path()), ["Photos", "Photos.zip"]);
        assert_eq!(Fingerprint::of(&made.output).unwrap(), made.fingerprint);
    }

    #[test]
    fn extract_single_file_lands_beside_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Notes.zip");
        make_zip(&archive, &[E::File("notes.txt", b"n")]);

        let made = extract(&archive).unwrap();

        assert_eq!(made.output, dir.path().join("notes.txt"));
        assert_eq!(fs::read(&made.output).unwrap(), b"n");
        assert_eq!(listing(dir.path()), ["Notes.zip", "notes.txt"]);
        assert_eq!(Fingerprint::of(&made.output).unwrap(), made.fingerprint);
    }

    #[test]
    fn file_named_like_the_top_folder_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Mixed.zip");
        make_zip(
            &archive,
            &[E::File("Photos/x", b"x"), E::File("Photos", b"file")],
        );

        let made = extract(&archive).unwrap();

        assert_eq!(made.output, dir.path().join("Mixed"));
        assert_eq!(tree(&made.output), ["Photos", "Photos (2)", "Photos/x"]);
        assert_eq!(fs::read(made.output.join("Photos (2)")).unwrap(), b"file");
        assert_eq!(listing(dir.path()), ["Mixed", "Mixed.zip"]);
        assert_eq!(Fingerprint::of(&made.output).unwrap(), made.fingerprint);
    }

    #[test]
    fn extract_beside_existing_folder_gets_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Photos");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("old.txt"), b"old").unwrap();
        let archive = dir.path().join("Photos.zip");
        make_zip(&archive, &[E::File("Photos/new.txt", b"new")]);

        let made = extract(&archive).unwrap();

        assert_eq!(made.output, dir.path().join("Photos (2)"));
        assert_eq!(tree(&made.output), ["new.txt"]);
        assert_eq!(tree(&old), ["old.txt"]);
        assert_eq!(fs::read(old.join("old.txt")).unwrap(), b"old");
        assert_eq!(listing(dir.path()), ["Photos", "Photos (2)", "Photos.zip"]);
    }

    #[test]
    fn windows_backslash_names_become_folders() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Win.zip");
        make_zip(
            &archive,
            &[E::File("dir\\file.txt", b"f"), E::File("other.txt", b"o")],
        );

        let made = extract(&archive).unwrap();

        assert_eq!(tree(&made.output), ["dir", "dir/file.txt", "other.txt"]);
    }

    #[test]
    fn zip_slip_entry_fails_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();
        let archive = inner.join("Evil.zip");
        make_zip(
            &archive,
            &[E::File("ok.txt", b"ok"), E::File("../evil.txt", b"evil")],
        );

        let result = extract(&archive);

        assert!(matches!(result, Err(ArchiveError::UnsafeEntry(_))));
        assert_eq!(listing(&inner), ["Evil.zip"]);
        assert_eq!(listing(dir.path()), ["inner"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_staging_fails() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Links.zip");
        make_zip(
            &archive,
            &[E::File("a.txt", b"a"), E::Link("link", "../../x")],
        );

        let result = extract(&archive);

        assert!(matches!(result, Err(ArchiveError::UnsafeEntry(_))));
        assert_eq!(listing(dir.path()), ["Links.zip"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inside_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Links.zip");
        make_zip(
            &archive,
            &[
                E::File("Folder/a.txt", b"a"),
                E::Link("Folder/link", "a.txt"),
                E::Link("Folder/sub/up", "../a.txt"),
            ],
        );

        let made = extract(&archive).unwrap();

        assert_eq!(made.output, dir.path().join("Folder"));
        let link = made.output.join("link");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("a.txt"));
        assert_eq!(fs::read(&link).unwrap(), b"a");
        assert_eq!(fs::read(made.output.join("sub/up")).unwrap(), b"a");
    }

    #[cfg(unix)]
    #[test]
    fn entry_through_symlink_cannot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Evil.zip");
        let target = outside.path().to_str().unwrap().to_owned();
        make_zip(
            &archive,
            &[E::Link("a", &target), E::File("a/evil", b"evil")],
        );

        let result = extract(&archive);

        assert!(matches!(result, Err(ArchiveError::UnsafeEntry(_))));
        assert!(listing(outside.path()).is_empty());
        assert_eq!(listing(dir.path()), ["Evil.zip"]);
    }

    #[test]
    fn empty_archive_is_an_error_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("Empty.zip");
        make_zip(&empty, &[]);
        let sidecars = dir.path().join("Mac.zip");
        make_zip(
            &sidecars,
            &[E::Dir("__MACOSX/"), E::File("__MACOSX/._x", b"meta")],
        );

        assert!(matches!(extract(&empty), Err(ArchiveError::Empty)));
        assert!(matches!(extract(&sidecars), Err(ArchiveError::Empty)));
        assert_eq!(listing(dir.path()), ["Empty.zip", "Mac.zip"]);
    }

    fn make_encrypted(path: &Path) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        zip.start_file(
            "secret.txt",
            SimpleFileOptions::default().with_aes_encryption(AesMode::Aes256, "hunter2"),
        )
        .unwrap();
        zip.write_all(b"top secret").unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn encrypted_zip_without_password_needs_one() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.zip");
        make_encrypted(&archive);

        let result = extract(&archive);

        assert!(matches!(result, Err(ArchiveError::NeedPassword)));
        assert_eq!(listing(dir.path()), ["Secret.zip"]);
    }

    #[test]
    fn encrypted_zip_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.zip");
        make_encrypted(&archive);

        let result = extract_one(
            &archive,
            Format::Zip,
            Some(b"wrong"),
            &Progress::new(),
            &no_mark,
        );

        assert!(matches!(result, Err(ArchiveError::WrongPassword)));
        assert_eq!(listing(dir.path()), ["Secret.zip"]);
    }

    #[test]
    fn encrypted_zip_right_password_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.zip");
        make_encrypted(&archive);

        let made = extract_one(
            &archive,
            Format::Zip,
            Some(b"hunter2"),
            &Progress::new(),
            &no_mark,
        )
        .unwrap();

        assert_eq!(made.output, dir.path().join("secret.txt"));
        assert_eq!(fs::read(&made.output).unwrap(), b"top secret");
    }

    #[test]
    fn damaged_entry_fails_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Broken.zip");
        make_zip(
            &archive,
            &[
                E::File("a.txt", b"first file"),
                E::File("b.txt", b"hello checksum"),
            ],
        );
        let mut bytes = fs::read(&archive).unwrap();
        let at = bytes
            .windows(14)
            .position(|w| w == b"hello checksum")
            .unwrap();
        bytes[at] = b'j';
        fs::write(&archive, bytes).unwrap();

        let result = extract(&archive);

        assert!(
            matches!(result, Err(ArchiveError::Damaged(_))),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Broken.zip"]);
    }

    #[cfg(unix)]
    #[test]
    fn special_file_entry_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Fifo.zip");
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        zip.start_file("fifo", stored().external_attributes(0o010_644 << 16))
            .unwrap();
        zip.finish().unwrap();

        let result = extract(&archive);

        assert!(
            matches!(result, Err(ArchiveError::Unsupported(_))),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Fifo.zip"]);
    }

    #[cfg(unix)]
    #[test]
    fn extract_into_read_only_folder_fails_clean() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        make_zip(&archive, &[E::File("a.txt", b"a"), E::File("b.txt", b"b")]);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        let result = extract(&archive);

        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(ArchiveError::Io(_))), "{result:?}");
        assert_eq!(listing(dir.path()), ["Photos.zip"]);
    }

    #[test]
    fn extract_cancel_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        make_zip(&archive, &[E::File("a.txt", b"a"), E::File("b.txt", b"b")]);
        let progress = Progress::new();
        progress.cancel();

        let result = extract_one(&archive, Format::Zip, None, &progress, &no_mark);

        assert!(matches!(result, Err(ArchiveError::Cancelled)));
        assert_eq!(listing(dir.path()), ["Photos.zip"]);
    }

    #[test]
    fn extract_cancel_midway_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        make_zip(
            &archive,
            &[E::File("a.txt", b"first"), E::File("b.txt", b"second")],
        );
        let progress = Progress::new();
        // Cancel once the first file is written, as a user would mid-job.
        let cancel_after_first = |path: &Path| {
            if path.file_name() == Some(OsStr::new("a.txt")) {
                progress.cancel();
            }
            Ok(())
        };

        let result = extract_one(&archive, Format::Zip, None, &progress, &cancel_after_first);

        assert!(matches!(result, Err(ArchiveError::Cancelled)));
        assert_eq!(listing(dir.path()), ["Photos.zip"]);
        assert!(progress.snapshot().done < progress.snapshot().total);
    }

    #[test]
    fn mark_runs_on_every_entry_before_publish() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        let mut entries = vec![
            E::File("a.txt", b"a"),
            E::Dir("empty/"),
            E::File("deep/er/b.txt", b"b"),
        ];
        if cfg!(unix) {
            entries.push(E::Link("link", "a.txt"));
        }
        make_zip(&archive, &entries);
        let partial = dir.path().join(partial_name(OsStr::new("Photos"), 1));
        let marked = Mutex::new(Vec::new());
        let mark = |path: &Path| {
            assert!(fs::symlink_metadata(path).is_ok(), "{path:?} exists");
            marked.lock().unwrap().push(path.to_path_buf());
            Ok(())
        };

        let made = extract_one(&archive, Format::Zip, None, &Progress::new(), &mark).unwrap();

        let marked = marked.into_inner().unwrap();
        assert!(marked.iter().all(|path| path.starts_with(&partial)));
        let mut relative: Vec<String> = marked
            .iter()
            .filter(|path| **path != partial)
            .map(|path| {
                path.strip_prefix(&partial)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        relative.sort();
        assert_eq!(relative, tree(&made.output));
        assert!(
            marked.contains(&partial),
            "the output folder itself is marked"
        );
    }

    #[test]
    fn mark_failure_fails_the_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        make_zip(&archive, &[E::File("a.txt", b"a"), E::File("b.txt", b"b")]);
        let refuse = |_: &Path| Err(io::Error::other("no quarantine"));

        let result = extract_one(&archive, Format::Zip, None, &Progress::new(), &refuse);

        assert!(matches!(result, Err(ArchiveError::Io(_))));
        assert_eq!(listing(dir.path()), ["Photos.zip"]);
    }

    #[test]
    fn progress_reaches_total() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Data.zip");
        make_zip(
            &archive,
            &[
                E::File("a", &[1u8; 3000]),
                E::File("b", &[2u8; 5000]),
                E::File("__MACOSX/._a", &[0u8; 700]),
            ],
        );
        let progress = Progress::new();

        extract_one(&archive, Format::Zip, None, &progress, &no_mark).unwrap();

        let snap = progress.snapshot();
        assert_eq!(snap.total, 8000);
        assert_eq!(snap.done, 8000);
    }

    #[cfg(unix)]
    #[test]
    fn extract_keeps_modes_and_times() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let folder = src.join("Docs");
        fs::create_dir_all(folder.join("sub")).unwrap();
        let file = folder.join("sub/run.sh");
        fs::write(&file, b"#!/bin/sh").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o750)).unwrap();
        let file_time = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let dir_time = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(file_time)
            .unwrap();
        for folder in [folder.join("sub"), folder.clone()] {
            File::open(&folder).unwrap().set_modified(dir_time).unwrap();
        }
        let made = crate::archive::compress(
            std::slice::from_ref(&folder),
            dir.path(),
            OsStr::new("Docs.zip"),
            &Progress::new(),
        )
        .unwrap();
        fs::remove_dir_all(&src).unwrap();

        let out = extract(&made.output).unwrap().output;

        assert_eq!(out, dir.path().join("Docs"));
        let meta = fs::metadata(out.join("sub/run.sh")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o750);
        assert_eq!(meta.modified().unwrap(), file_time);
        assert_eq!(
            fs::metadata(out.join("sub")).unwrap().modified().unwrap(),
            dir_time
        );
        assert_eq!(fs::metadata(&out).unwrap().modified().unwrap(), dir_time);
    }

    #[test]
    fn dos_time_reads_as_local_time() {
        let stamp = zip::DateTime::from_date_and_time(2024, 3, 5, 12, 34, 56).unwrap();
        let local = chrono::NaiveDate::from_ymd_opt(2024, 3, 5)
            .unwrap()
            .and_hms_opt(12, 34, 56)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap();
        assert_eq!(dos_time(stamp), Some(SystemTime::from(local)));
    }
}
