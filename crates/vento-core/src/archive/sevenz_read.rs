//! Reading 7z archives.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sevenz_rust2::{ArchiveEntry, EncoderMethod, Error, NtTime, Password};

use super::entries::{ArchiveReader, EntryInfo, EntryKind, safe_relative};
use super::job::ArchiveError;
use super::staging::Staging;

/// Set in an entry's Windows attributes when the high 16 bits hold a unix mode
/// (the convention p7zip and 7-Zip on unix use).
const ATTR_UNIX_EXTENSION: u32 = 0x8000;
/// Bits of a unix mode that give the file type, and the type of a symlink.
const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;

/// A 7z archive opened for extraction.
pub(crate) struct SevenZReader {
    archive: sevenz_rust2::ArchiveReader<File>,
    /// Whether a non-empty password was given.
    has_password: bool,
}

impl SevenZReader {
    /// Opens the 7z at `path`; `password` opens encrypted data and headers.
    ///
    /// An archive whose header is encrypted cannot be listed without its
    /// password, so it fails here with [`ArchiveError::NeedPassword`] (or
    /// [`ArchiveError::WrongPassword`]) rather than from `entries`.
    pub(crate) fn open(path: &Path, password: Option<&[u8]>) -> Result<Self, ArchiveError> {
        let file = File::open(path)?;
        let archive = sevenz_rust2::ArchiveReader::new(file, seven_z_password(password))
            .map_err(sevenz_error)?;
        Ok(SevenZReader {
            archive,
            has_password: password.is_some_and(|bytes| !bytes.is_empty()),
        })
    }

    /// Whether any data in the archive is encrypted.
    fn is_encrypted(&self) -> bool {
        self.archive
            .archive()
            .blocks
            .iter()
            .flat_map(|block| &block.coders)
            .any(|coder| coder.encoder_method_id() == EncoderMethod::ID_AES256_SHA256)
    }
}

impl ArchiveReader for SevenZReader {
    fn entries(&mut self) -> Result<Vec<EntryInfo>, ArchiveError> {
        let mut kept = Vec::new();
        for entry in &self.archive.archive().files {
            kept.extend(entry_info(entry)?);
        }
        Ok(kept)
    }

    /// Hands the entries over in the order the archive stores their data,
    /// which can differ from `entries`: those without data (folders, empty
    /// files) come last. A solid block cannot be skipped into, so entries that
    /// are not kept are still read through.
    fn extract(&mut self, sink: &mut Staging<'_>) -> Result<(), ArchiveError> {
        // A failed read of encrypted data under a password is most likely the
        // wrong password: 7z has no check value to tell it apart from damage.
        let bad_password_likely = self.has_password && self.is_encrypted();
        // The real error is kept here: the crate's own error type cannot carry
        // vento's, and would flatten `UnsafeEntry` or `Cancelled` into noise.
        let mut failure = None;
        let result = self.archive.for_each_entries(|entry, data| {
            match extract_entry(entry, data, sink, bad_password_likely) {
                Ok(()) => Ok(true),
                Err(error) => {
                    failure = Some(error);
                    Err(Error::Other(Cow::Borrowed("extraction stopped")))
                }
            }
        });
        match (failure, result) {
            (Some(error), _) => Err(error),
            (None, Err(error)) => Err(sevenz_error(error)),
            (None, Ok(())) => Ok(()),
        }
    }
}

/// Writes one entry the archive reader produced, or reads it through when it
/// is not kept.
fn extract_entry(
    entry: &ArchiveEntry,
    data: &mut dyn Read,
    sink: &mut Staging<'_>,
    bad_password_likely: bool,
) -> Result<(), ArchiveError> {
    let mut watched = Watched {
        inner: data,
        failed: false,
    };
    let Some(info) = entry_info(entry)? else {
        return io::copy(&mut watched, &mut io::sink())
            .map(drop)
            .map_err(|error| {
                if bad_password_likely {
                    ArchiveError::WrongPassword
                } else {
                    ArchiveError::from_read(error)
                }
            });
    };
    if info.kind == EntryKind::Dir {
        return sink.dir(&info);
    }
    sink.file(&info, &mut watched).map_err(|error| {
        if watched.failed && bad_password_likely {
            ArchiveError::WrongPassword
        } else {
            error
        }
    })
}

/// A reader that notes whether a read failed, to tell a failed read from a
/// failed write after the copy that used it returns an error. A checksum
/// failure, which the crate reports as an opaque error, is passed on as
/// invalid data like every other undecodable input.
struct Watched<'a> {
    inner: &'a mut dyn Read,
    failed: bool,
}

impl Read for Watched<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf).map_err(|error| {
            self.failed = true;
            let checksum = error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<Error>())
                .is_some_and(|inner| matches!(inner, Error::ChecksumVerificationFailed));
            if checksum {
                io::Error::new(io::ErrorKind::InvalidData, "checksum mismatch")
            } else {
                error
            }
        })
    }
}

/// What `entry` is and where it goes; `None` for an entry that is skipped:
/// an anti-item, macOS metadata or a symlink.
fn entry_info(entry: &ArchiveEntry) -> Result<Option<EntryInfo>, ArchiveError> {
    if entry.is_anti_item() {
        return Ok(None);
    }
    let Some(path) = safe_relative(entry.name())? else {
        return Ok(None);
    };
    let attributes = entry.windows_attributes();
    let unix_mode = (entry.has_windows_attributes && attributes & ATTR_UNIX_EXTENSION != 0)
        .then_some(attributes >> 16);
    if unix_mode.is_some_and(|mode| mode & S_IFMT == S_IFLNK) {
        // Rare in 7z, and skipping is safe: nothing is written for it.
        log::info!("skipping symbolic link {} in a 7z archive", path.display());
        return Ok(None);
    }
    let kind = if entry.is_directory() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Ok(Some(EntryInfo {
        path,
        kind,
        size: if kind == EntryKind::Dir {
            0
        } else {
            entry.size()
        },
        mode: unix_mode.map(|mode| mode & 0o7777),
        mtime: entry
            .has_last_modified_date
            .then(|| modified(entry.last_modified_date()))
            .flatten(),
    }))
}

/// Windows file time ticks (100 ns) per second.
const TICKS_PER_SECOND: u64 = 10_000_000;
/// The file time of the unix epoch: ticks from 1601-01-01 to 1970-01-01.
const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
/// The file time of 9999-12-31 23:59:59, the last time accepted.
const LAST_TICKS: u64 = UNIX_EPOCH_TICKS + 253_402_300_799 * TICKS_PER_SECOND;

/// An entry's modified time, or `None` when the archive's value cannot be
/// trusted: absent (0, which some writers store for "unknown"), or outside
/// 1601 to 9999, which no file system takes as a modified time. The crate's
/// own conversion panics on some values, so this one is checked throughout.
fn modified(date: NtTime) -> Option<SystemTime> {
    let ticks = u64::from(date);
    if ticks == 0 || ticks > LAST_TICKS {
        return None;
    }
    let seconds = |ticks: u64| ticks / TICKS_PER_SECOND;
    let nanos = |ticks: u64| ((ticks % TICKS_PER_SECOND) * 100) as u32;
    if ticks >= UNIX_EPOCH_TICKS {
        let after = ticks - UNIX_EPOCH_TICKS;
        UNIX_EPOCH.checked_add(Duration::new(seconds(after), nanos(after)))
    } else {
        let before = UNIX_EPOCH_TICKS - ticks;
        UNIX_EPOCH.checked_sub(Duration::new(seconds(before), nanos(before)))
    }
}

/// The password in the form 7z derives its key from: UTF-16 little endian.
/// Bytes that are not UTF-8 text are used as they are.
fn seven_z_password(password: Option<&[u8]>) -> Password {
    match password {
        None | Some([]) => Password::empty(),
        Some(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => Password::new(text),
            Err(_) => Password::from_raw(bytes),
        },
    }
}

fn sevenz_error(error: Error) -> ArchiveError {
    match error {
        Error::PasswordRequired => ArchiveError::NeedPassword,
        Error::MaybeBadPassword(_) => ArchiveError::WrongPassword,
        Error::Io(e, _) | Error::FileOpen(e, _) => ArchiveError::from_read(e),
        Error::BadSignature(_) => ArchiveError::Damaged("not a 7z archive".to_owned()),
        Error::ChecksumVerificationFailed | Error::NextHeaderCrcMismatch => {
            ArchiveError::Damaged("checksum mismatch".to_owned())
        }
        Error::UnsupportedVersion { major, minor } => {
            ArchiveError::Unsupported(format!("7z format version {major}.{minor}"))
        }
        Error::UnsupportedCompressionMethod(method) => {
            ArchiveError::Unsupported(format!("compression method {method}"))
        }
        Error::ExternalUnsupported => ArchiveError::Unsupported("external 7z data".to_owned()),
        Error::Unsupported(message) => ArchiveError::Unsupported(message.into_owned()),
        other => ArchiveError::Damaged(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{LAST_TICKS, TICKS_PER_SECOND, UNIX_EPOCH_TICKS, modified};
    use crate::archive::detect::Format;
    use crate::archive::entries::{extract_one, open_reader};
    use crate::archive::job::{ArchiveError, Progress};
    use crate::archive::test_support::{listing, no_mark, tree};
    use crate::fileops::Fingerprint;
    use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, NtTime, Password, SourceReader};
    use std::ffi::OsStr;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// One entry of a test archive.
    enum E<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        /// A file with unix mode bits and a modified time, stored the way
        /// p7zip does (mode in the high half of the attributes). Only the
        /// unix-only test that checks those bits makes one.
        #[cfg_attr(not(unix), allow(dead_code))]
        Stamped(&'a str, &'a [u8], u32, SystemTime),
        /// A file whose stored modified time is this raw file time.
        RawTime(&'a str, &'a [u8], u64),
        Link(&'a str, &'a str),
        Anti(&'a str),
    }

    /// How a test archive is built.
    #[derive(Clone, Copy)]
    struct Build<'a> {
        solid: bool,
        password: Option<&'a str>,
        encrypt_header: bool,
    }

    const PLAIN: Build<'static> = Build {
        solid: false,
        password: None,
        encrypt_header: false,
    };
    const SOLID: Build<'static> = Build {
        solid: true,
        ..PLAIN
    };

    fn entry_for(e: &E) -> (ArchiveEntry, Option<Vec<u8>>) {
        let unix = |entry: &mut ArchiveEntry, mode: u32| {
            entry.has_windows_attributes = true;
            entry.windows_attributes = (mode << 16) | 0x8000 | 0x20;
        };
        match e {
            E::File(name, data) => (ArchiveEntry::new_file(name), Some(data.to_vec())),
            E::Dir(name) => (ArchiveEntry::new_directory(name), None),
            E::Stamped(name, data, mode, time) => {
                let mut entry = ArchiveEntry::new_file(name);
                unix(&mut entry, 0o100_000 | mode);
                entry.has_last_modified_date = true;
                entry.last_modified_date = NtTime::try_from(*time).unwrap();
                (entry, Some(data.to_vec()))
            }
            E::RawTime(name, data, ticks) => {
                let mut entry = ArchiveEntry::new_file(name);
                entry.has_last_modified_date = true;
                entry.last_modified_date = NtTime::new(*ticks);
                (entry, Some(data.to_vec()))
            }
            E::Link(name, target) => {
                let mut entry = ArchiveEntry::new_file(name);
                unix(&mut entry, 0o120_777);
                (entry, Some(target.as_bytes().to_vec()))
            }
            E::Anti(name) => {
                let mut entry = ArchiveEntry::new_file(name);
                entry.is_anti_item = true;
                (entry, None)
            }
        }
    }

    fn make_7z(path: &Path, build: Build, entries: &[E]) {
        let mut writer = ArchiveWriter::create(path).unwrap();
        if let Some(password) = build.password {
            writer.set_content_methods(vec![
                AesEncoderOptions::new(Password::new(password)).into(),
                Lzma2Options::default().into(),
            ]);
        }
        writer.set_encrypt_header(build.encrypt_header);
        let (streams, empty): (Vec<_>, Vec<_>) = entries
            .iter()
            .map(entry_for)
            .partition(|(_, data)| data.is_some());
        if build.solid {
            let (entries, data): (Vec<_>, Vec<_>) = streams
                .into_iter()
                .map(|(entry, data)| (entry, SourceReader::new(io::Cursor::new(data.unwrap()))))
                .unzip();
            writer.push_archive_entries(entries, data).unwrap();
        } else {
            for (entry, data) in streams {
                writer
                    .push_archive_entry(entry, Some(io::Cursor::new(data.unwrap())))
                    .unwrap();
            }
        }
        for (entry, _) in empty {
            writer.push_archive_entry::<&[u8]>(entry, None).unwrap();
        }
        writer.finish().unwrap();
    }

    fn extract(
        archive: &Path,
        password: Option<&[u8]>,
    ) -> Result<crate::archive::Made, ArchiveError> {
        extract_one(
            archive,
            Format::SevenZ,
            password,
            &Progress::new(),
            &no_mark,
        )
    }

    fn sample() -> Vec<E<'static>> {
        vec![
            E::File("a.txt", b"first file"),
            E::Dir("sub"),
            E::File("sub/b.txt", b"second file"),
            E::File("sub/empty.txt", b""),
            E::Dir("empty-dir"),
        ]
    }

    fn check_sample(made: &crate::archive::Made, dir: &Path) {
        assert_eq!(made.output, dir.join("Photos"));
        assert_eq!(
            tree(&made.output),
            ["a.txt", "empty-dir", "sub", "sub/b.txt", "sub/empty.txt"]
        );
        assert_eq!(fs::read(made.output.join("a.txt")).unwrap(), b"first file");
        assert_eq!(
            fs::read(made.output.join("sub/b.txt")).unwrap(),
            b"second file"
        );
        assert_eq!(fs::read(made.output.join("sub/empty.txt")).unwrap(), b"");
        assert_eq!(Fingerprint::of(&made.output).unwrap(), made.fingerprint);
        assert_eq!(listing(dir), ["Photos", "Photos.7z"]);
    }

    #[test]
    fn sevenz_plain_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.7z");
        make_7z(&archive, PLAIN, &sample());

        let made = extract(&archive, None).unwrap();

        check_sample(&made, dir.path());
    }

    #[test]
    fn sevenz_solid_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.7z");
        make_7z(&archive, SOLID, &sample());

        let made = extract(&archive, None).unwrap();

        check_sample(&made, dir.path());
    }

    #[test]
    fn sevenz_single_folder_is_not_double_nested() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.7z");
        make_7z(
            &archive,
            PLAIN,
            &[E::Dir("Photos"), E::File("Photos/a.jpg", b"jpg")],
        );

        let made = extract(&archive, None).unwrap();

        assert_eq!(made.output, dir.path().join("Photos"));
        assert_eq!(tree(&made.output), ["a.jpg"]);
    }

    #[test]
    fn sevenz_sidecars_and_anti_items_are_skipped_in_a_solid_block() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Mixed.7z");
        // The sidecars sit between kept files of one solid block, so they
        // must still be read through for what follows to line up.
        make_7z(
            &archive,
            SOLID,
            &[
                E::File("a.txt", b"first"),
                E::File("__MACOSX/._a.txt", &[7u8; 5000]),
                E::File("._b.txt", &[8u8; 3000]),
                E::File("b.txt", b"second"),
                E::Anti("gone.txt"),
            ],
        );
        let progress = Progress::new();

        let made = extract_one(&archive, Format::SevenZ, None, &progress, &no_mark).unwrap();

        assert_eq!(made.output, dir.path().join("Mixed"));
        assert_eq!(tree(&made.output), ["a.txt", "b.txt"]);
        assert_eq!(fs::read(made.output.join("a.txt")).unwrap(), b"first");
        assert_eq!(fs::read(made.output.join("b.txt")).unwrap(), b"second");
        let snap = progress.snapshot();
        assert_eq!((snap.done, snap.total), (11, 11));
    }

    #[test]
    fn sevenz_only_sidecars_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Mac.7z");
        make_7z(
            &archive,
            PLAIN,
            &[E::Dir("__MACOSX"), E::File("__MACOSX/._x", b"meta")],
        );

        assert!(matches!(extract(&archive, None), Err(ArchiveError::Empty)));
        assert_eq!(listing(dir.path()), ["Mac.7z"]);
    }

    #[test]
    fn sevenz_unsafe_entry_fails_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();
        let archive = inner.join("Evil.7z");
        make_7z(
            &archive,
            PLAIN,
            &[E::File("ok.txt", b"ok"), E::File("../evil.txt", b"evil")],
        );

        let result = extract(&archive, None);

        assert!(matches!(result, Err(ArchiveError::UnsafeEntry(_))));
        assert_eq!(listing(&inner), ["Evil.7z"]);
        assert_eq!(listing(dir.path()), ["inner"]);
    }

    #[test]
    fn sevenz_symlink_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Links.7z");
        make_7z(
            &archive,
            PLAIN,
            &[
                E::File("Folder/a.txt", b"a"),
                E::Link("Folder/link", "a.txt"),
            ],
        );

        let made = extract(&archive, None).unwrap();

        assert_eq!(made.output, dir.path().join("Folder"));
        assert_eq!(tree(&made.output), ["a.txt"]);
    }

    #[test]
    fn sevenz_encrypted_needs_password() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.7z");
        let build = Build {
            password: Some("hunter2"),
            ..PLAIN
        };
        make_7z(&archive, build, &[E::File("secret.txt", b"top secret")]);

        let result = extract(&archive, None);

        assert!(matches!(result, Err(ArchiveError::NeedPassword)));
        assert_eq!(listing(dir.path()), ["Secret.7z"]);
    }

    #[test]
    fn sevenz_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.7z");
        let build = Build {
            password: Some("hunter2"),
            ..SOLID
        };
        make_7z(
            &archive,
            build,
            &[
                E::File("secret.txt", b"top secret"),
                E::File("more.txt", b"more"),
            ],
        );

        let result = extract(&archive, Some(b"wrong"));

        assert!(
            matches!(result, Err(ArchiveError::WrongPassword)),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Secret.7z"]);
    }

    #[test]
    fn sevenz_right_password_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.7z");
        let build = Build {
            password: Some("hunter2"),
            ..PLAIN
        };
        make_7z(&archive, build, &[E::File("secret.txt", b"top secret")]);

        let made = extract(&archive, Some(b"hunter2")).unwrap();

        assert_eq!(made.output, dir.path().join("secret.txt"));
        assert_eq!(fs::read(&made.output).unwrap(), b"top secret");
    }

    #[test]
    fn sevenz_header_encrypted_needs_password_before_listing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.7z");
        let build = Build {
            password: Some("hunter2"),
            encrypt_header: true,
            ..SOLID
        };
        make_7z(
            &archive,
            build,
            &[E::File("a.txt", b"a"), E::File("b.txt", b"b")],
        );

        // The reader cannot even list the entries without the password.
        let listed = open_reader(&archive, Format::SevenZ, None).and_then(|mut r| r.entries());
        assert!(matches!(listed, Err(ArchiveError::NeedPassword)));
        assert!(matches!(
            extract(&archive, None),
            Err(ArchiveError::NeedPassword)
        ));
        assert_eq!(listing(dir.path()), ["Secret.7z"]);
    }

    #[test]
    fn sevenz_header_encrypted_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.7z");
        let build = Build {
            password: Some("hunter2"),
            encrypt_header: true,
            ..PLAIN
        };
        make_7z(&archive, build, &[E::File("secret.txt", b"top secret")]);

        let result = extract(&archive, Some(b"wrong"));

        assert!(
            matches!(result, Err(ArchiveError::WrongPassword)),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Secret.7z"]);
    }

    #[test]
    fn sevenz_header_encrypted_right_password_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.7z");
        let build = Build {
            password: Some("hunter2"),
            encrypt_header: true,
            ..SOLID
        };
        make_7z(
            &archive,
            build,
            &[E::File("a.txt", b"a"), E::File("b.txt", b"b")],
        );

        let made = extract(&archive, Some(b"hunter2")).unwrap();

        assert_eq!(made.output, dir.path().join("Secret"));
        assert_eq!(tree(&made.output), ["a.txt", "b.txt"]);
    }

    #[test]
    fn sevenz_cancel_mid_way_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.7z");
        make_7z(
            &archive,
            PLAIN,
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

        let result = extract_one(
            &archive,
            Format::SevenZ,
            None,
            &progress,
            &cancel_after_first,
        );

        assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
        assert_eq!(listing(dir.path()), ["Photos.7z"]);
        assert!(progress.snapshot().done < progress.snapshot().total);
    }

    #[test]
    fn sevenz_damaged_data_fails_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Broken.7z");
        let mut writer = ArchiveWriter::create(&archive).unwrap();
        writer.set_content_methods(vec![sevenz_rust2::EncoderMethod::COPY.into()]);
        writer
            .push_archive_entry(
                ArchiveEntry::new_file("b.txt"),
                Some(&b"hello checksum"[..]),
            )
            .unwrap();
        writer.finish().unwrap();
        let mut bytes = fs::read(&archive).unwrap();
        let at = bytes
            .windows(14)
            .position(|w| w == b"hello checksum")
            .unwrap();
        bytes[at] = b'j';
        fs::write(&archive, bytes).unwrap();

        let result = extract(&archive, None);

        assert!(
            matches!(result, Err(ArchiveError::Damaged(_))),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Broken.7z"]);
    }

    #[test]
    fn sevenz_not_an_archive_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Fake.7z");
        fs::write(&archive, b"this is not a 7z archive at all, just text").unwrap();

        let result = extract(&archive, None);

        assert!(
            matches!(result, Err(ArchiveError::Damaged(_))),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Fake.7z"]);
    }

    #[test]
    fn sevenz_progress_reaches_total() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Data.7z");
        make_7z(
            &archive,
            SOLID,
            &[
                E::File("a", &[1u8; 3000]),
                E::File("b", &[2u8; 5000]),
                E::File("__MACOSX/._a", &[0u8; 700]),
            ],
        );
        let progress = Progress::new();

        extract_one(&archive, Format::SevenZ, None, &progress, &no_mark).unwrap();

        let snap = progress.snapshot();
        assert_eq!((snap.done, snap.total), (8000, 8000));
    }

    #[cfg(unix)]
    #[test]
    fn sevenz_keeps_modes_and_times() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Docs.7z");
        let time = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        make_7z(
            &archive,
            PLAIN,
            &[
                E::Stamped("run.sh", b"#!/bin/sh", 0o750, time),
                E::File("plain.txt", b"p"),
            ],
        );

        let out = extract(&archive, None).unwrap().output;

        let meta = fs::metadata(out.join("run.sh")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o750);
        assert_eq!(meta.modified().unwrap(), time);
        let plain = fs::metadata(out.join("plain.txt")).unwrap();
        assert!(plain.modified().unwrap() > time);
    }

    #[test]
    fn sevenz_garbage_times_do_not_fail_the_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Times.7z");
        let before = SystemTime::now() - Duration::from_secs(60);
        make_7z(
            &archive,
            SOLID,
            &[
                E::RawTime("max.txt", b"max", u64::MAX),
                E::RawTime("zero.txt", b"zero", 0),
                E::RawTime("far.txt", b"far", (1 << 63) + 1),
                E::RawTime("old.txt", b"old", 1),
            ],
        );

        let made = extract(&archive, None).unwrap();

        assert_eq!(fs::read(made.output.join("max.txt")).unwrap(), b"max");
        assert_eq!(fs::read(made.output.join("zero.txt")).unwrap(), b"zero");
        assert_eq!(fs::read(made.output.join("far.txt")).unwrap(), b"far");
        assert_eq!(fs::read(made.output.join("old.txt")).unwrap(), b"old");
        // None of those times was applied: the files keep the time of writing.
        for name in ["max.txt", "zero.txt", "far.txt"] {
            let time = fs::metadata(made.output.join(name))
                .unwrap()
                .modified()
                .unwrap();
            assert!(time > before, "{name}");
        }
    }

    #[test]
    fn modified_converts_checked() {
        let at = |seconds_since_1601: u64| NtTime::new(seconds_since_1601 * TICKS_PER_SECOND);
        let unix_seconds = UNIX_EPOCH_TICKS / TICKS_PER_SECOND;
        assert_eq!(modified(NtTime::new(0)), None);
        assert_eq!(modified(NtTime::new(u64::MAX)), None);
        assert_eq!(modified(NtTime::new(LAST_TICKS + 1)), None);
        assert!(modified(NtTime::new(LAST_TICKS)).is_some());
        assert_eq!(modified(at(unix_seconds)), Some(UNIX_EPOCH));
        assert_eq!(
            modified(at(unix_seconds + 1_600_000_000)),
            Some(UNIX_EPOCH + Duration::from_secs(1_600_000_000))
        );
        assert_eq!(
            modified(NtTime::new(UNIX_EPOCH_TICKS + 5)),
            Some(UNIX_EPOCH + Duration::from_nanos(500))
        );
        assert_eq!(
            modified(at(unix_seconds - 100)),
            Some(UNIX_EPOCH - Duration::from_secs(100))
        );
    }
}
