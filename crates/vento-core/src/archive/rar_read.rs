//! Reading rar archives, single files and volume sets.

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use rars::{
    Archive, ArchiveMemberMeta, ArchiveReadOptions, AttrSource, ErrorKind, ExtractedEntryMeta,
    ReadCancellation,
};

use super::detect::rar_volumes;
use super::entries::{ArchiveReader, EntryInfo, EntryKind, safe_relative};
use super::job::{ArchiveError, Progress};
use super::staging::{EntryTicket, Staging};

/// Bits of a unix mode that give the file type, and the type of a symlink.
const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;
/// The largest dictionary a rar5 member may declare, as unrar allows by
/// default. The decoder keeps history up to the dictionary (or the member, if
/// smaller), so a tiny hostile archive could otherwise ask for tens of GiB.
const DICTIONARY_LIMIT: u64 = 4 << 30;
/// The flag of a rar 2 to 4 end block that says another volume follows.
const END_NEXT_VOLUME: u16 = 0x0001;
/// How often a running extraction looks for Cancel while the decoder is busy.
/// The decoder itself looks as often as it can between those checks.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// A rar archive opened for extraction: one file, or every volume of a set.
pub(crate) struct RarReader<'p> {
    /// The parsed volumes, first to last; one for an archive in one piece.
    volumes: Vec<Archive>,
    /// The password, `None` when none (or an empty one) was given.
    password: Option<&'p [u8]>,
    /// The largest rar5 dictionary extracted.
    dictionary_limit: u64,
}

impl<'p> RarReader<'p> {
    /// Opens the rar at `path`, with every later volume of its set that sits
    /// beside it. `path` is taken as the first volume.
    ///
    /// An archive whose headers are encrypted cannot be listed without its
    /// password, so it fails here with [`ArchiveError::NeedPassword`] (or
    /// [`ArchiveError::WrongPassword`]) rather than from `entries`. The
    /// password is given as UTF-8 text: rar5 uses it so, and the crate turns it
    /// into the UTF-16 that rar 3 and 4 derive their keys from.
    ///
    /// Sibling volumes are joined only when `path` is itself a volume, and the
    /// set must be whole: it has to start with the first volume and end with
    /// one that says nothing follows. Otherwise it fails with
    /// [`ArchiveError::Damaged`], which the crate alone does not catch when
    /// a volume ends exactly between two files.
    pub(crate) fn open(path: &Path, password: Option<&'p [u8]>) -> Result<Self, ArchiveError> {
        Self::open_limited(path, password, DICTIONARY_LIMIT)
    }

    /// As [`RarReader::open`], refusing rar5 dictionaries above `dictionary_limit`.
    fn open_limited(
        path: &Path,
        password: Option<&'p [u8]>,
        dictionary_limit: u64,
    ) -> Result<Self, ArchiveError> {
        let password = password.filter(|bytes| !bytes.is_empty());
        let paths = rar_volumes(path);
        let mut volumes = vec![read_volume(&paths[0], password)?];
        // A plain archive that happens to sit beside files named like later
        // volumes is not their first volume.
        if is_volume(&volumes[0]) {
            for volume in &paths[1..] {
                volumes.push(read_volume(volume, password)?);
            }
        }
        check_set(&volumes)?;
        Ok(RarReader {
            volumes,
            password,
            dictionary_limit,
        })
    }
}

/// Whether `archive` is a volume of a set.
fn is_volume(archive: &Archive) -> bool {
    match archive {
        Archive::Rar50Plus(archive) => archive.main.is_volume(),
        Archive::Rar15To40(archive) => archive.main.is_volume(),
        // Told apart by nothing but its name.
        _ => true,
    }
}

/// Checks that `volumes` are a whole set: the first volume first, in order,
/// and the last one not followed by another.
///
/// A volume can end exactly between two files, which looks the same as the end
/// of the set from the files alone; what tells them apart is the volume
/// numbers (rar5), the first-volume flag (rar 3 and 4) and the end block's
/// "another volume follows" flag.
fn check_set(volumes: &[Archive]) -> Result<(), ArchiveError> {
    match volumes.first() {
        Some(Archive::Rar50Plus(_)) => {
            let mut last_continues = false;
            for (index, volume) in volumes.iter().enumerate() {
                let Archive::Rar50Plus(volume) = volume else {
                    return Err(ArchiveError::Damaged("mixed kinds of volumes".to_owned()));
                };
                // The first volume carries no number, or 0.
                let number = volume.main.volume_number.unwrap_or(0);
                if number != index as u64 {
                    return Err(if index == 0 {
                        missing_volume("the first")
                    } else {
                        missing_volume("a")
                    });
                }
                last_continues = volume.blocks.iter().any(
                    |block| matches!(block, rars::rar50::Block::End(end) if end.has_next_volume()),
                );
            }
            if last_continues {
                return Err(missing_volume("a later"));
            }
        }
        Some(Archive::Rar15To40(first)) => {
            if first.main.is_volume() && !first.main.is_first_volume() {
                return Err(missing_volume("the first"));
            }
            let continues = |volume: &Archive| {
                match volume {
                Archive::Rar15To40(volume) => volume.blocks.iter().any(|block| {
                    matches!(block, rars::rar15_40::Block::End(end) if end.flags & END_NEXT_VOLUME != 0)
                }),
                _ => false,
            }
            };
            if volumes.last().is_some_and(continues) {
                return Err(missing_volume("a later"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Parses one volume.
///
/// Rar 3 and 4 have no check value for encrypted headers, so with a wrong
/// password they decrypt to noise and the volume looks damaged. A volume that
/// fails like that under a password, but asks for a password without one, is
/// taken to have been given the wrong one.
fn read_volume(path: &Path, password: Option<&[u8]>) -> Result<Archive, ArchiveError> {
    let read = |password| {
        let options = ArchiveReadOptions::with_optional_password(password);
        rars::ArchiveReader::read_path_with_options(path, options)
    };
    let error = match read(password) {
        Ok(volume) => return Ok(volume),
        Err(error) => error,
    };
    let looks_damaged = matches!(
        error.kind(),
        ErrorKind::InvalidArchive | ErrorKind::ChecksumMismatch
    );
    if password.is_some()
        && looks_damaged
        && read(None).is_err_and(|e| e.kind() == ErrorKind::PasswordRequired)
    {
        return Err(ArchiveError::WrongPassword);
    }
    Err(rar_error(error))
}

impl ArchiveReader for RarReader<'_> {
    fn entries(&mut self) -> Result<Vec<EntryInfo>, ArchiveError> {
        Ok(members(&self.volumes)?
            .into_iter()
            .filter_map(|slot| match slot.member {
                Member::Keep(info) => Some(info),
                Member::Skip => None,
            })
            .collect())
    }

    /// Hands the entries over in archive order. The crate pushes each entry's
    /// data into a writer rather than letting it be read, so every file entry
    /// gets a [`Staging::file_writer`], which owns what it needs; its ticket is
    /// finished once the crate moves on to the next entry (or is done), by when
    /// the entry's checksum has been checked.
    fn extract(&mut self, sink: &mut Staging<'_>) -> Result<(), ArchiveError> {
        let mut slots = members(&self.volumes)?.into_iter();
        // The crate looks at this token as it decodes, which can go on for
        // seconds without a write (a rar5 member is decoded whole in memory up
        // to 512 MiB), so Cancel is passed on from a thread that watches the
        // progress for as long as the extraction runs.
        let token = ReadCancellation::new();
        let options = ArchiveReadOptions::with_optional_password(self.password)
            .with_cancellation(&token)
            .with_rar50_dictionary_size_limit(self.dictionary_limit);
        // The real error is kept here: the crate's own error type cannot carry
        // vento's, and would flatten `UnsafeEntry` or `Cancelled` into noise.
        let mut failure = None;
        let mut pending: Option<EntryTicket> = None;
        let result = thread::scope(|scope| {
            let (running, stopped) = mpsc::channel::<()>();
            let progress = Arc::clone(sink.progress());
            let watched = token.clone();
            scope.spawn(move || watch(&progress, &watched, stopped));
            let open = |meta: &ExtractedEntryMeta| match open_entry(
                meta,
                &mut slots,
                &mut pending,
                sink,
            ) {
                Ok(writer) => Ok(writer),
                Err(error) => {
                    failure = Some(error);
                    Err(rars::Error::from(io::Error::other("extraction stopped")))
                }
            };
            let result = match self.volumes.as_slice() {
                [only] => only.extract_to_with_options(options, open),
                volumes => rars::extract_volumes_to_with_options(volumes, options, open),
            };
            // Ends the watching thread, which scope waits for.
            drop(running);
            result
        });
        match (failure, result) {
            (Some(error), _) => Err(error),
            (None, Err(error)) => {
                // The writer of the entry in hand knows best why it stopped.
                let writer_failure = pending.as_ref().and_then(EntryTicket::failure);
                Err(writer_failure.unwrap_or_else(|| rar_error(error)))
            }
            (None, Ok(())) => {
                if let Some(ticket) = pending.take() {
                    sink.finish_writer(ticket)?;
                }
                if slots.next().is_some() {
                    return Err(ArchiveError::Damaged(
                        "the archive holds fewer entries than it lists".to_owned(),
                    ));
                }
                Ok(())
            }
        }
    }
}

/// Cancels `token` once `progress` is cancelled, until `stopped` closes.
fn watch(progress: &Progress, token: &ReadCancellation, stopped: Receiver<()>) {
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(CANCEL_POLL) {
        if progress.is_cancelled() {
            token.cancel();
            return;
        }
    }
}

/// What becomes of one member the crate hands over.
enum Member {
    /// It is written.
    Keep(EntryInfo),
    /// It is read through and dropped: macOS metadata, or a link.
    Skip,
}

/// A member in the order the crate hands it over.
struct Slot {
    /// The member's name as stored, to tell the crate's entries from ours.
    name: Vec<u8>,
    member: Member,
}

/// The members of the volume set, in the order [`rars`] calls `open` for them:
/// one per logical file, a file split over volumes counted once. Redirections
/// (rar5 links) are left out, the crate cannot stream them through a writer
/// and never opens one.
fn members(volumes: &[Archive]) -> Result<Vec<Slot>, ArchiveError> {
    // A set that starts or stops in the middle of a file lacks a volume: the
    // first one, or one after the last that was found.
    let starts_mid_file = volumes
        .first()
        .and_then(|first| first.members().next())
        .is_some_and(|member| member.meta.is_split_before);
    if starts_mid_file {
        return Err(missing_volume("the first"));
    }
    let members = rars::volume_members(volumes).map_err(rar_error)?;
    if members
        .last()
        .is_some_and(|member| member.meta.is_split_after)
    {
        return Err(missing_volume("a later"));
    }
    let mut slots = Vec::new();
    for member in members {
        let meta = &member.meta;
        if meta.is_redirection {
            log::info!("skipping link {} in a rar archive", meta.name_lossy());
            continue;
        }
        slots.push(Slot {
            name: meta.name.clone(),
            member: classify(meta)?,
        });
    }
    Ok(slots)
}

/// Decides what becomes of `meta`'s member: its entry, or `Skip` for macOS
/// metadata and unix symlinks (rar 2 to 4 store a link as a file holding the
/// target, which is skipped like rar5's).
fn classify(meta: &ArchiveMemberMeta) -> Result<Member, ArchiveError> {
    let Some(path) = safe_relative(&meta.name_lossy())? else {
        return Ok(Member::Skip);
    };
    // Attributes of other hosts (Windows flags) are not a unix mode.
    let unix_attr = (meta.attr_source() == AttrSource::Unix)
        .then(|| u32::try_from(meta.file_attr).ok())
        .flatten();
    if unix_attr.is_some_and(|attr| attr & S_IFMT == S_IFLNK) {
        log::info!("skipping symbolic link {} in a rar archive", path.display());
        return Ok(Member::Skip);
    }
    let kind = if meta.is_directory {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Ok(Member::Keep(EntryInfo {
        path,
        kind,
        size: if kind == EntryKind::Dir {
            0
        } else {
            meta.unpacked_size
        },
        // No permission bits at all is more likely an unset field than intent.
        mode: unix_attr
            .map(|attr| attr & 0o7777)
            .filter(|mode| mode & 0o777 != 0),
        // The crate converts DOS local time and Unix seconds with checked
        // arithmetic, and gives `None` for fields that name no real time.
        mtime: meta.modification_time(),
    }))
}

/// Handles the crate opening the next entry: ends the one before, then
/// returns the writer for this one.
fn open_entry(
    meta: &ExtractedEntryMeta,
    slots: &mut impl Iterator<Item = Slot>,
    pending: &mut Option<EntryTicket>,
    sink: &mut Staging<'_>,
) -> Result<Box<dyn Write>, ArchiveError> {
    if let Some(ticket) = pending.take() {
        sink.finish_writer(ticket)?;
    }
    let Some(slot) = slots.next().filter(|slot| slot.name == meta.name) else {
        return Err(ArchiveError::Damaged(
            "the archive holds other entries than it lists".to_owned(),
        ));
    };
    match slot.member {
        Member::Skip => Ok(Box::new(io::sink())),
        Member::Keep(info) if info.kind == EntryKind::Dir => {
            sink.dir(&info)?;
            Ok(Box::new(io::sink()))
        }
        Member::Keep(info) => {
            let (writer, ticket) = sink.file_writer(&info)?;
            *pending = Some(ticket);
            Ok(Box::new(writer))
        }
    }
}

fn missing_volume(which: &str) -> ArchiveError {
    ArchiveError::Damaged(format!("{which} volume of the set is missing"))
}

fn rar_error(error: rars::Error) -> ArchiveError {
    match error.kind() {
        ErrorKind::PasswordRequired => ArchiveError::NeedPassword,
        ErrorKind::BadPassword => ArchiveError::WrongPassword,
        ErrorKind::Cancelled => ArchiveError::Cancelled,
        ErrorKind::Io => match error.root_cause() {
            rars::Error::Io(io) => {
                ArchiveError::from_read(io::Error::new(io.kind, io.message.clone()))
            }
            _ => ArchiveError::Io(io::Error::other(error.to_string())),
        },
        ErrorKind::UnsupportedFormat | ErrorKind::UnsupportedFeature | ErrorKind::ResourceLimit => {
            ArchiveError::Unsupported(error.to_string())
        }
        _ => ArchiveError::Damaged(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{CANCEL_POLL, RarReader, watch};
    use crate::archive::detect::{Format, Plan, plan};
    use crate::archive::entries::{ArchiveReader, EntryKind, extract_one, open_reader};
    use crate::archive::job::{ArchiveError, Progress};
    use crate::archive::staging::Staging;
    use crate::archive::test_support::{listing, no_mark, tree};
    use crate::fileops::Fingerprint;
    use rars::{ArchiveVersion, Builder, ReadCancellation};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    /// One entry of a test archive.
    enum E<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        /// A file with unix mode bits and a raw stored modified time (Unix
        /// seconds for rar5, packed DOS fields for rar4).
        Stamped(&'a str, &'a [u8], u32, u32),
        /// A file with a raw stored modified time and no unix mode.
        Timed(&'a str, &'a [u8], u32),
        Link(&'a str, &'a str),
    }

    /// How a test archive is built.
    #[derive(Clone, Copy)]
    struct Build<'a> {
        version: ArchiveVersion,
        solid: bool,
        password: Option<&'a str>,
        encrypt_header: bool,
    }

    const RAR5: Build<'static> = Build {
        version: ArchiveVersion::Rar50,
        solid: false,
        password: None,
        encrypt_header: false,
    };
    const RAR5_SOLID: Build<'static> = Build {
        solid: true,
        ..RAR5
    };
    const RAR4: Build<'static> = Build {
        version: ArchiveVersion::Rar40,
        ..RAR5
    };

    fn builder(build: Build, entries: &[E]) -> Builder {
        let mut builder = Builder::new(build.version)
            .solid(build.solid)
            .password(build.password.map(|p| p.as_bytes().to_vec()))
            .header_encryption(build.encrypt_header);
        for entry in entries {
            match entry {
                E::File(name, data) => {
                    builder
                        .add_bytes(name.as_bytes().to_vec(), data.to_vec(), None, None)
                        .unwrap();
                }
                E::Dir(name) => {
                    builder
                        .add_directory(name.as_bytes().to_vec(), None, None)
                        .unwrap();
                }
                E::Stamped(name, data, mode, time) => {
                    builder
                        .add_bytes(
                            name.as_bytes().to_vec(),
                            data.to_vec(),
                            Some(*time),
                            Some(0o100_000 | mode),
                        )
                        .unwrap();
                }
                E::Timed(name, data, time) => {
                    builder
                        .add_bytes(name.as_bytes().to_vec(), data.to_vec(), Some(*time), None)
                        .unwrap();
                }
                E::Link(name, target) => {
                    builder
                        .add_unix_symlink(
                            name.as_bytes().to_vec(),
                            target.as_bytes().to_vec(),
                            false,
                            None,
                            None,
                        )
                        .unwrap();
                }
            }
        }
        builder
    }

    fn make_rar(path: &Path, build: Build, entries: &[E]) {
        fs::write(path, builder(build, entries).to_bytes().unwrap()).unwrap();
    }

    /// Incompressible bytes, so that volumes fill up predictably.
    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    /// Writes `entries` as a set of three volumes in `dir`, named the new way
    /// (`Big.part1.rar` ...) or the old way (`Big.rar`, `Big.r00` ...), and
    /// returns their paths.
    fn make_volumes(dir: &Path, build: Build, entries: &[E], old_names: bool) -> Vec<PathBuf> {
        let volumes = builder(build, entries)
            .store(true)
            .volume_size(Some(3000))
            .build_volumes(None)
            .unwrap();
        assert_eq!(volumes.len(), 3, "the fixture should fill three volumes");
        volumes
            .iter()
            .enumerate()
            .map(|(index, bytes)| {
                let name = match (old_names, index) {
                    (false, n) => format!("Big.part{}.rar", n + 1),
                    (true, 0) => "Big.rar".to_owned(),
                    (true, n) => format!("Big.r{:02}", n - 1),
                };
                let path = dir.join(name);
                fs::write(&path, bytes).unwrap();
                path
            })
            .collect()
    }

    fn extract(
        archive: &Path,
        password: Option<&[u8]>,
    ) -> Result<crate::archive::Made, ArchiveError> {
        extract_one(archive, Format::Rar, password, &Progress::new(), &no_mark)
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

    fn check_sample(made: &crate::archive::Made, dir: &Path, archive_name: &str) {
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
        assert_eq!(listing(dir), ["Photos", archive_name]);
    }

    #[test]
    fn rar5_plain_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(&archive, RAR5, &sample());

        let made = extract(&archive, None).unwrap();

        check_sample(&made, dir.path(), "Photos.rar");
    }

    #[test]
    fn rar5_solid_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(&archive, RAR5_SOLID, &sample());

        let made = extract(&archive, None).unwrap();

        check_sample(&made, dir.path(), "Photos.rar");
    }

    #[test]
    fn rar4_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(&archive, RAR4, &sample());

        let made = extract(&archive, None).unwrap();

        check_sample(&made, dir.path(), "Photos.rar");
    }

    #[test]
    fn rar4_solid_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(
            &archive,
            Build {
                solid: true,
                ..RAR4
            },
            &sample(),
        );

        let made = extract(&archive, None).unwrap();

        check_sample(&made, dir.path(), "Photos.rar");
    }

    #[test]
    fn rar_single_folder_is_not_double_nested() {
        for build in [RAR5, RAR4] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("Photos.rar");
            make_rar(
                &archive,
                build,
                &[E::Dir("Photos"), E::File("Photos/a.jpg", b"jpg")],
            );

            let made = extract(&archive, None).unwrap();

            assert_eq!(made.output, dir.path().join("Photos"));
            assert_eq!(tree(&made.output), ["a.jpg"]);
        }
    }

    #[test]
    fn rar_sidecars_are_skipped_in_a_solid_archive() {
        for version in [ArchiveVersion::Rar50, ArchiveVersion::Rar40] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("Mixed.rar");
            // The sidecars sit between kept files of one solid stream, so they
            // must still be decoded for what follows to line up.
            make_rar(
                &archive,
                Build {
                    version,
                    solid: true,
                    ..RAR5
                },
                &[
                    E::File("a.txt", b"first"),
                    E::File("__MACOSX/._a.txt", &noise(5000)),
                    E::File("._b.txt", &noise(3000)),
                    E::File("b.txt", b"second"),
                ],
            );
            let progress = Progress::new();

            let made = extract_one(&archive, Format::Rar, None, &progress, &no_mark).unwrap();

            assert_eq!(made.output, dir.path().join("Mixed"));
            assert_eq!(tree(&made.output), ["a.txt", "b.txt"]);
            assert_eq!(fs::read(made.output.join("a.txt")).unwrap(), b"first");
            assert_eq!(fs::read(made.output.join("b.txt")).unwrap(), b"second");
            let snap = progress.snapshot();
            assert_eq!((snap.done, snap.total), (11, 11));
        }
    }

    #[test]
    fn rar_only_sidecars_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Mac.rar");
        make_rar(
            &archive,
            RAR5,
            &[E::Dir("__MACOSX"), E::File("__MACOSX/._x", b"meta")],
        );

        assert!(matches!(extract(&archive, None), Err(ArchiveError::Empty)));
        assert_eq!(listing(dir.path()), ["Mac.rar"]);
    }

    #[test]
    fn rar_unsafe_entry_fails_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();
        let archive = inner.join("Evil.rar");
        // The builder refuses to write such a name, so the archive is forged
        // by renaming an entry of the same length in the finished bytes.
        let mut bytes = builder(
            RAR5,
            &[E::File("ok.txt", b"ok"), E::File("xx/evil.txt", b"evil")],
        )
        .to_bytes()
        .unwrap();
        let at = bytes
            .windows(11)
            .position(|w| w == b"xx/evil.txt")
            .expect("name is stored in clear");
        bytes[at..at + 11].copy_from_slice(b"../evil.txt");
        fs::write(&archive, rewrite_header_crcs(bytes)).unwrap();

        let result = extract(&archive, None);

        assert!(
            matches!(result, Err(ArchiveError::UnsafeEntry(_))),
            "{result:?}"
        );
        assert_eq!(listing(&inner), ["Evil.rar"]);
        assert_eq!(listing(dir.path()), ["inner"]);
    }

    /// Recomputes the CRC-32 of every rar5 header in `bytes`, so that a header
    /// edited in place is still accepted.
    fn rewrite_header_crcs(mut bytes: Vec<u8>) -> Vec<u8> {
        fn vint(bytes: &[u8], at: &mut usize) -> u64 {
            let mut value = 0u64;
            let mut shift = 0;
            loop {
                let byte = bytes[*at];
                *at += 1;
                value |= u64::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    return value;
                }
                shift += 7;
            }
        }
        let mut at = 8;
        while at + 4 < bytes.len() {
            let crc_at = at;
            let mut cursor = at + 4;
            let size = vint(&bytes, &mut cursor) as usize;
            let body = cursor;
            let end = body + size;
            let mut fields = body;
            let _kind = vint(&bytes, &mut fields);
            let flags = vint(&bytes, &mut fields);
            if flags & 1 != 0 {
                vint(&bytes, &mut fields);
            }
            let data = if flags & 2 != 0 {
                vint(&bytes, &mut fields) as usize
            } else {
                0
            };
            let crc = rars::crc32::crc32(&bytes[at + 4..end]);
            bytes[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
            at = end + data;
        }
        bytes
    }

    #[test]
    fn rar5_symlink_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Links.rar");
        make_rar(
            &archive,
            RAR5,
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
    fn rar4_symlink_entries_are_skipped_and_their_data_is_read_through() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Links.rar");
        make_rar(
            &archive,
            Build {
                solid: true,
                ..RAR4
            },
            &[
                E::File("Folder/a.txt", b"a"),
                E::Link("Folder/link", "a.txt"),
                E::File("Folder/b.txt", b"b"),
            ],
        );

        let made = extract(&archive, None).unwrap();

        assert_eq!(made.output, dir.path().join("Folder"));
        assert_eq!(tree(&made.output), ["a.txt", "b.txt"]);
        assert_eq!(fs::read(made.output.join("b.txt")).unwrap(), b"b");
    }

    #[test]
    fn rar5_keeps_mode_and_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Stamped.rar");
        make_rar(
            &archive,
            RAR5,
            &[
                E::Stamped("run.sh", b"#!/bin/sh\n", 0o755, 1_600_000_000),
                E::Timed("old.txt", b"old", u32::MAX),
            ],
        );

        let made = extract(&archive, None).unwrap();

        let run = made.output.join("run.sh");
        let modified = fs::metadata(&run).unwrap().modified().unwrap();
        assert_eq!(modified, UNIX_EPOCH + Duration::from_secs(1_600_000_000));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&run).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        let old = fs::metadata(made.output.join("old.txt")).unwrap();
        assert_eq!(
            old.modified().unwrap(),
            UNIX_EPOCH + Duration::from_secs(u64::from(u32::MAX))
        );
    }

    #[test]
    fn rar4_modified_time_is_read_as_local_time_and_garbage_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Times.rar");
        // 2020-01-02 03:04:06 as packed DOS fields; and month 0, day 0.
        let valid: u32 = (40 << 25) | (1 << 21) | (2 << 16) | (3 << 11) | (4 << 5) | 3;
        let garbage: u32 = (40 << 25) | (31 << 11);
        make_rar(
            &archive,
            RAR4,
            &[
                E::Timed("good.txt", b"g", valid),
                E::Timed("bad.txt", b"b", garbage),
            ],
        );
        let mut reader = open_reader(&archive, Format::Rar, None).unwrap();

        let entries = reader.entries().unwrap();

        assert_eq!(entries.len(), 2);
        let good = entries[0].mtime.expect("a valid DOS time is kept");
        // Local time: within a day of the same wall clock read as UTC.
        let as_utc = UNIX_EPOCH + Duration::from_secs(1_577_934_246);
        let apart = good.duration_since(as_utc).unwrap_or_else(|e| e.duration());
        assert!(apart <= Duration::from_secs(14 * 3600), "{apart:?}");
        assert_eq!(entries[1].mtime, None);
    }

    #[test]
    fn rar4_unix_mode_is_kept_and_dos_attributes_are_not_a_mode() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Modes.rar");
        make_rar(
            &archive,
            RAR4,
            &[
                E::Stamped("run.sh", b"#!/bin/sh\n", 0o755, 0),
                E::File("plain.txt", b"x"),
            ],
        );
        let mut reader = open_reader(&archive, Format::Rar, None).unwrap();

        let entries = reader.entries().unwrap();

        let modes: Vec<_> = entries.iter().map(|e| e.mode).collect();
        assert_eq!(modes, [Some(0o755), None]);
    }

    #[test]
    fn rar_entries_list_kinds_sizes_and_modes() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("List.rar");
        make_rar(
            &archive,
            RAR5,
            &[
                E::Dir("d"),
                E::Stamped("d/f.txt", b"12345", 0o640, 1_600_000_000),
                E::Link("d/l", "f.txt"),
                E::File("d/plain", b"x"),
            ],
        );
        let mut reader = open_reader(&archive, Format::Rar, None).unwrap();

        let entries = reader.entries().unwrap();

        let summary: Vec<_> = entries
            .iter()
            .map(|e| (e.path.to_str().unwrap(), e.kind, e.size, e.mode))
            .collect();
        assert_eq!(
            summary,
            [
                ("d", EntryKind::Dir, 0, None),
                ("d/f.txt", EntryKind::File, 5, Some(0o640)),
                ("d/plain", EntryKind::File, 1, None),
            ]
        );
    }

    #[test]
    fn rar_names_with_non_ascii_text_keep_their_spelling() {
        for build in [RAR5, RAR4] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("Names.rar");
            make_rar(
                &archive,
                build,
                &[
                    E::File("Fotos/caf\u{e9}.txt", b"a"),
                    E::File("Fotos/b.txt", b"b"),
                ],
            );

            let made = extract(&archive, None).unwrap();

            assert_eq!(made.output, dir.path().join("Fotos"));
            assert_eq!(tree(&made.output), ["b.txt", "caf\u{e9}.txt"]);
            assert_eq!(fs::read(made.output.join("caf\u{e9}.txt")).unwrap(), b"a");
        }
    }

    #[test]
    fn rar5_encrypted_needs_password() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(
            &archive,
            Build {
                password: Some("secret"),
                ..RAR5
            },
            &sample(),
        );

        let result = extract(&archive, None);

        assert!(
            matches!(result, Err(ArchiveError::NeedPassword)),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Photos.rar"]);
    }

    #[test]
    fn rar5_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(
            &archive,
            Build {
                password: Some("secret"),
                ..RAR5
            },
            &sample(),
        );

        let result = extract(&archive, Some(b"guess"));

        assert!(
            matches!(result, Err(ArchiveError::WrongPassword)),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Photos.rar"]);
    }

    #[test]
    fn rar5_right_password_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(
            &archive,
            Build {
                password: Some("s\u{e9}cret"),
                ..RAR5_SOLID
            },
            &sample(),
        );

        let made = extract(&archive, Some("s\u{e9}cret".as_bytes())).unwrap();

        check_sample(&made, dir.path(), "Photos.rar");
    }

    #[test]
    fn rar5_header_encrypted_needs_password_before_listing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        let build = Build {
            password: Some("secret"),
            encrypt_header: true,
            ..RAR5
        };
        make_rar(&archive, build, &sample());

        let missing = open_reader(&archive, Format::Rar, None).err();
        let wrong = open_reader(&archive, Format::Rar, Some(b"guess")).err();
        let extracted = extract(&archive, None);

        assert!(matches!(missing, Some(ArchiveError::NeedPassword)));
        assert!(matches!(wrong, Some(ArchiveError::WrongPassword)));
        assert!(matches!(extracted, Err(ArchiveError::NeedPassword)));
        assert_eq!(listing(dir.path()), ["Photos.rar"]);

        let made = extract(&archive, Some(b"secret")).unwrap();
        check_sample(&made, dir.path(), "Photos.rar");
    }

    #[test]
    fn rar4_passwords() {
        for encrypt_header in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("Photos.rar");
            let build = Build {
                password: Some("secret"),
                encrypt_header,
                ..RAR4
            };
            make_rar(&archive, build, &sample());

            let missing = extract(&archive, None);
            let wrong = extract(&archive, Some(b"guess"));

            assert!(
                matches!(missing, Err(ArchiveError::NeedPassword)),
                "{missing:?} (header encrypted: {encrypt_header})"
            );
            assert!(
                matches!(wrong, Err(ArchiveError::WrongPassword)),
                "{wrong:?} (header encrypted: {encrypt_header})"
            );
            assert_eq!(listing(dir.path()), ["Photos.rar"]);
            let made = extract(&archive, Some(b"secret")).unwrap();
            check_sample(&made, dir.path(), "Photos.rar");
        }
    }

    #[test]
    fn rar_empty_password_is_no_password() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(
            &archive,
            Build {
                password: Some("secret"),
                ..RAR5
            },
            &sample(),
        );

        let result = extract(&archive, Some(b""));

        assert!(
            matches!(result, Err(ArchiveError::NeedPassword)),
            "{result:?}"
        );
    }

    /// Three volumes: a small file, then one that spans all three.
    fn big_entries() -> Vec<E<'static>> {
        vec![
            E::File("small.txt", b"small"),
            E::File("big.bin", Box::leak(noise(7000).into_boxed_slice())),
        ]
    }

    /// The same, as far as rar 4 goes: it writes a volume set of one file.
    fn big_only() -> Vec<E<'static>> {
        vec![E::File(
            "big.bin",
            Box::leak(noise(7000).into_boxed_slice()),
        )]
    }

    fn check_big(made: &crate::archive::Made, dir: &Path) {
        assert_eq!(made.output, dir.join("Big"));
        assert_eq!(tree(&made.output), ["big.bin", "small.txt"]);
        assert_eq!(fs::read(made.output.join("small.txt")).unwrap(), b"small");
        assert_eq!(fs::read(made.output.join("big.bin")).unwrap(), noise(7000));
    }

    #[test]
    fn rar_volume_set_extracts_from_first() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR5, &big_entries(), false);

        // Dropping the middle volume selects the whole set through `plan`.
        let Plan::Extract(archives) = plan(&[volumes[1].clone()]) else {
            panic!("a volume is an archive");
        };
        assert_eq!(archives, [volumes[0].clone()]);
        let made = extract(&archives[0], None).unwrap();

        check_big(&made, dir.path());
        let mut names = listing(dir.path());
        names.retain(|name| !name.starts_with("Big.part"));
        assert_eq!(names, ["Big"]);
    }

    #[test]
    fn rar4_volume_set_extracts_from_first() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR4, &big_only(), true);

        let Plan::Extract(archives) = plan(&[volumes[2].clone()]) else {
            panic!("a volume is an archive");
        };
        assert_eq!(archives, [volumes[0].clone()]);
        let made = extract(&archives[0], None).unwrap();

        assert_eq!(made.output, dir.path().join("big.bin"));
        assert_eq!(fs::read(&made.output).unwrap(), noise(7000));
    }

    #[test]
    fn rar_volume_set_lists_the_whole_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR5, &big_entries(), false);
        let mut reader = open_reader(&volumes[0], Format::Rar, None).unwrap();

        let entries = reader.entries().unwrap();

        let summary: Vec<_> = entries
            .iter()
            .map(|e| (e.path.to_str().unwrap(), e.size))
            .collect();
        assert_eq!(summary, [("small.txt", 5), ("big.bin", 7000)]);
    }

    #[test]
    fn rar_encrypted_volume_set_extracts_with_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let build = Build {
            password: Some("secret"),
            ..RAR5
        };
        let volumes = make_volumes(dir.path(), build, &big_entries(), false);

        let missing = extract(&volumes[0], None);
        let made = extract(&volumes[0], Some(b"secret")).unwrap();

        assert!(
            matches!(missing, Err(ArchiveError::NeedPassword)),
            "{missing:?}"
        );
        check_big(&made, dir.path());
    }

    #[test]
    fn rar_missing_volume_is_damaged_and_leaves_nothing() {
        for (build, old_names) in [(RAR5, false), (RAR4, true)] {
            let dir = tempfile::tempdir().unwrap();
            let volumes = make_volumes(dir.path(), build, &big_only(), old_names);
            // The middle volume is gone: the set stops at the gap.
            fs::remove_file(&volumes[1]).unwrap();
            let before = listing(dir.path());

            let result = extract(&volumes[0], None);

            let Err(ArchiveError::Damaged(message)) = result else {
                panic!("{result:?}");
            };
            assert!(message.contains("later volume"), "{message}");
            assert_eq!(listing(dir.path()), before);
        }
    }

    /// Three files, each exactly filling a volume of the fixture, so that the
    /// set is cut between files and no file continues over a cut.
    fn boundary_entries() -> Vec<E<'static>> {
        let leak = |len| -> &'static [u8] { Box::leak(noise(len).into_boxed_slice()) };
        vec![
            E::File("a.bin", leak(3000)),
            E::File("b.bin", leak(3000)),
            E::File("c.bin", leak(1000)),
        ]
    }

    #[test]
    fn rar_set_cut_between_files_extracts_when_whole() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR5, &boundary_entries(), false);

        let made = extract(&volumes[0], None).unwrap();

        assert_eq!(tree(&made.output), ["a.bin", "b.bin", "c.bin"]);
        assert_eq!(fs::metadata(made.output.join("c.bin")).unwrap().len(), 1000);
    }

    #[test]
    fn rar_set_cut_between_files_missing_its_last_volume_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR5, &boundary_entries(), false);
        fs::remove_file(&volumes[2]).unwrap();
        let before = listing(dir.path());

        let result = extract(&volumes[0], None);

        let Err(ArchiveError::Damaged(message)) = result else {
            panic!("{result:?}");
        };
        assert!(message.contains("later volume"), "{message}");
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn rar_set_cut_between_files_missing_a_middle_volume_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR5, &boundary_entries(), false);
        fs::remove_file(&volumes[1]).unwrap();
        let before = listing(dir.path());

        let result = extract(&volumes[0], None);

        assert!(
            matches!(result, Err(ArchiveError::Damaged(_))),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn rar_set_cut_between_files_missing_its_first_volume_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR5, &boundary_entries(), false);
        fs::remove_file(&volumes[0]).unwrap();
        let before = listing(dir.path());

        // `plan` keeps the dropped path when the first volume is not there.
        let Plan::Extract(archives) = plan(&[volumes[1].clone()]) else {
            panic!("a volume is an archive");
        };
        let result = extract(&archives[0], None);

        let Err(ArchiveError::Damaged(message)) = result else {
            panic!("{result:?}");
        };
        assert!(message.contains("first volume"), "{message}");
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn rar_plain_archive_beside_volume_names_does_not_join_them() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("Big.part1.rar");
        make_rar(&first, RAR5, &[E::File("only.txt", b"only")]);
        // Named like the next volume, but not one: it is not even read.
        fs::write(
            dir.path().join("Big.part2.rar"),
            b"Rar!\x1a\x07\x01\x00 junk",
        )
        .unwrap();

        let made = extract(&first, None).unwrap();

        assert_eq!(made.output, dir.path().join("only.txt"));
        assert_eq!(fs::read(&made.output).unwrap(), b"only");
    }

    /// Sets the CRC-16 of the rar4 header of `bytes` that starts at `at` and
    /// is `size` long, after it was edited.
    fn fix_rar4_crc(bytes: &mut [u8], at: usize, size: usize) {
        let crc = rars::crc32::crc32(&bytes[at + 2..at + size]) as u16;
        bytes[at..at + 2].copy_from_slice(&crc.to_le_bytes());
    }

    #[test]
    fn rar4_set_missing_its_first_volume_flag_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR4, &big_only(), true);
        let mut bytes = fs::read(&volumes[0]).unwrap();
        // The main header follows the 7 byte marker; flags at 3, size at 5.
        let size = usize::from(u16::from_le_bytes([bytes[12], bytes[13]]));
        let flags = u16::from_le_bytes([bytes[10], bytes[11]]) & !0x0100;
        bytes[10..12].copy_from_slice(&flags.to_le_bytes());
        fix_rar4_crc(&mut bytes, 7, size);
        fs::write(&volumes[0], bytes).unwrap();
        let before = listing(dir.path());

        let result = extract(&volumes[0], None);

        let Err(ArchiveError::Damaged(message)) = result else {
            panic!("{result:?}");
        };
        assert!(message.contains("first volume"), "{message}");
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn rar4_set_whose_last_volume_says_more_follow_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR4, &big_only(), true);
        // The writer leaves out the end block that other tools write; add
        // one that says another volume follows.
        let mut bytes = fs::read(&volumes[2]).unwrap();
        let mut end = [0u8, 0, 0x7b, 0x01, 0x00, 0x07, 0x00];
        fix_rar4_crc(&mut end, 0, 7);
        bytes.extend_from_slice(&end);
        fs::write(&volumes[2], bytes).unwrap();
        let before = listing(dir.path());

        let result = extract(&volumes[0], None);

        let Err(ArchiveError::Damaged(message)) = result else {
            panic!("{result:?}");
        };
        assert!(message.contains("later volume"), "{message}");
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn rar5_dictionary_above_the_limit_is_unsupported_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        // Compressed (not stored), so the member declares a dictionary.
        make_rar(&archive, RAR5, &[E::File("a.txt", &[b'a'; 20_000])]);
        let progress = Progress::new();
        let mut reader = RarReader::open_limited(&archive, None, 0).unwrap();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();

        let result = reader.extract(&mut staging);

        assert!(
            matches!(result, Err(ArchiveError::Unsupported(_))),
            "{result:?}"
        );
        drop(staging);
        assert_eq!(listing(dir.path()), ["Photos.rar"]);
    }

    #[test]
    fn rar5_dictionary_within_the_limit_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(&archive, RAR5, &[E::File("a.txt", &[b'a'; 20_000])]);
        let progress = Progress::new();
        // 1 GiB is far above what the writer's default level declares.
        let mut reader = RarReader::open_limited(&archive, None, 1 << 30).unwrap();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();

        reader.extract(&mut staging).unwrap();
    }

    #[test]
    fn rar_missing_first_volume_is_damaged_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = make_volumes(dir.path(), RAR5, &big_entries(), false);
        fs::remove_file(&volumes[0]).unwrap();
        let before = listing(dir.path());

        // `plan` keeps the dropped path when the first volume is not there.
        let Plan::Extract(archives) = plan(&[volumes[1].clone()]) else {
            panic!("a volume is an archive");
        };
        let result = extract(&archives[0], None);

        let Err(ArchiveError::Damaged(message)) = result else {
            panic!("{result:?}");
        };
        assert!(message.contains("first volume"), "{message}");
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn rar_cut_short_is_damaged_and_leaves_nothing() {
        for build in [RAR5, RAR4] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("Cut.rar");
            let bytes = builder(build, &[E::File("a.bin", &noise(20_000))])
                .store(true)
                .to_bytes()
                .unwrap();
            fs::write(&archive, &bytes[..bytes.len() - 5000]).unwrap();

            let result = extract(&archive, None);

            assert!(
                matches!(result, Err(ArchiveError::Damaged(_))),
                "{result:?}"
            );
            assert_eq!(listing(dir.path()), ["Cut.rar"]);
        }
    }

    #[test]
    fn rar_flipped_data_is_damaged_and_leaves_nothing() {
        for build in [RAR5, RAR4] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("Flip.rar");
            let mut bytes = builder(build, &[E::File("a.bin", &noise(20_000))])
                .store(true)
                .to_bytes()
                .unwrap();
            let middle = bytes.len() / 2;
            bytes[middle] ^= 0xff;
            fs::write(&archive, &bytes).unwrap();

            let result = extract(&archive, None);

            assert!(
                matches!(result, Err(ArchiveError::Damaged(_))),
                "{result:?}"
            );
            assert_eq!(listing(dir.path()), ["Flip.rar"]);
        }
    }

    #[test]
    fn rar_not_an_archive_is_unsupported_or_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Junk.rar");
        fs::write(&archive, b"Rar!\x1a\x07\x01\x00 and then nothing sensible").unwrap();

        let result = extract(&archive, None);

        assert!(
            matches!(
                result,
                Err(ArchiveError::Damaged(_) | ArchiveError::Unsupported(_))
            ),
            "{result:?}"
        );
        assert_eq!(listing(dir.path()), ["Junk.rar"]);
    }

    #[test]
    fn rar_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();

        let result = extract(&dir.path().join("Gone.rar"), None);

        assert!(matches!(result, Err(ArchiveError::Io(_))), "{result:?}");
    }

    #[test]
    fn rar_cancel_during_extraction_leaves_nothing() {
        for solid in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("Photos.rar");
            make_rar(&archive, Build { solid, ..RAR5 }, &sample());
            let progress = Progress::new();
            // Cancel when the first file has been written and is marked.
            let cancel = Arc::clone(&progress);
            let mark = move |path: &Path| {
                if path.file_name().is_some_and(|name| name == "a.txt") {
                    cancel.cancel();
                }
                Ok(())
            };

            let result = extract_one(&archive, Format::Rar, None, &progress, &mark);

            assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
            assert_eq!(listing(dir.path()), ["Photos.rar"]);
        }
    }

    #[test]
    fn rar_cancelled_before_it_starts_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(&archive, RAR5, &sample());
        let progress = Progress::new();
        progress.cancel();

        let result = extract_one(&archive, Format::Rar, None, &progress, &no_mark);

        assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
        assert_eq!(listing(dir.path()), ["Photos.rar"]);
    }

    #[test]
    fn watch_passes_cancel_on_to_the_token() {
        let progress = Progress::new();
        let token = ReadCancellation::new();
        let (running, stopped) = mpsc::channel::<()>();

        thread::scope(|scope| {
            let (progress, token) = (&progress, &token);
            scope.spawn(move || watch(progress, token, stopped));
            assert!(!token.is_cancelled());
            progress.cancel();
            // The watching thread has to see it within a few polls.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !token.is_cancelled() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            assert!(token.is_cancelled());
            drop(running);
        });
    }

    #[test]
    fn watch_ends_quietly_when_the_extraction_is_over() {
        let progress = Progress::new();
        let token = ReadCancellation::new();
        let (running, stopped) = mpsc::channel::<()>();
        let started = Instant::now();

        thread::scope(|scope| {
            let (progress, token) = (&progress, &token);
            scope.spawn(move || watch(progress, token, stopped));
            drop(running);
        });

        assert!(!token.is_cancelled());
        // Closing the channel wakes the thread, it does not wait out a poll.
        assert!(started.elapsed() < CANCEL_POLL * 20);
    }

    #[test]
    fn rar_progress_counts_every_byte_once() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.rar");
        make_rar(&archive, RAR5, &sample());
        let progress = Progress::new();

        extract_one(&archive, Format::Rar, None, &progress, &no_mark).unwrap();

        let snap = progress.snapshot();
        assert_eq!((snap.done, snap.total), (21, 21));
    }
}
