//! Hidden partial items that archive output is built under before it gets its
//! final name, and the staging folder an archive is extracted into.

use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use super::detect::archive_stem;
use super::entries::{EntryInfo, Layout};
use super::job::{ArchiveError, Counting, Progress};
use crate::fileops::{
    Fingerprint, NAME_ATTEMPTS, partial_name, place_partial, remove_any, unique_destination,
};

/// Most links followed while checking where one symlink leads; a longer chain
/// (or a loop) is refused. Matches the usual kernel limit.
const MAX_LINK_HOPS: usize = 40;
/// Bytes read from an entry at a time.
const COPY_BUFFER: usize = 64 * 1024;

/// A partial item that is removed when dropped, unless it was published.
pub(crate) struct Partial {
    pub(crate) path: PathBuf,
    pub(crate) published: bool,
}

impl Partial {
    pub(crate) fn new(path: PathBuf) -> Self {
        Partial {
            path,
            published: false,
        }
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.published {
            remove_any(&self.path);
        }
    }
}

/// A hidden partial folder that an archive is extracted into, entry by entry,
/// before the result is published beside the archive in one step.
///
/// Nothing is ever written outside the partial folder: every folder on the
/// way to an entry must be a real folder inside it (never a symlink, so a
/// link created by an earlier entry is never followed), and a symlink is only
/// created when its target stays inside. Dropping an unpublished `Staging`
/// removes the partial folder with everything in it.
pub(crate) struct Staging<'a> {
    partial: Partial,
    /// The folder the result is published into.
    dir: PathBuf,
    /// The archive's name without extension, for a wrapping folder.
    stem: OsString,
    progress: Arc<Progress>,
    mark: &'a dyn Fn(&Path) -> io::Result<()>,
    /// Folders whose modified time is set once every entry is written.
    dir_times: Vec<(PathBuf, SystemTime)>,
    /// Every symlink created, checked again against the final layout.
    links: Vec<PathBuf>,
}

impl<'a> Staging<'a> {
    /// Claims a partial folder in `dir`, named after `archive`, and passes it
    /// to `mark`.
    ///
    /// `mark` runs on every item created inside (folders, files and symlinks
    /// alike, so it must not follow links) before anything is published; an
    /// error from it fails the extraction.
    pub(crate) fn new(
        dir: &Path,
        archive: &Path,
        progress: &Arc<Progress>,
        mark: &'a dyn Fn(&Path) -> io::Result<()>,
    ) -> Result<Staging<'a>, ArchiveError> {
        if progress.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let stem = archive_stem(archive);
        let root = claim_partial_dir(dir, &stem)?;
        let staging = Staging {
            partial: Partial::new(root),
            dir: dir.to_path_buf(),
            stem,
            progress: Arc::clone(progress),
            mark,
            dir_times: Vec::new(),
            links: Vec::new(),
        };
        staging.mark(staging.root())?;
        Ok(staging)
    }

    /// Writes a file entry from `data`, counting the bytes into the progress.
    ///
    /// A path already taken inside staging (the same name twice, or names
    /// that differ only in case on a case-insensitive volume) gets the next
    /// free name, `a (2).txt`. Data that fails to decode or to match its
    /// checksum, or that runs past the entry's declared size (which the disk
    /// space check relied on), is [`ArchiveError::Damaged`].
    pub(crate) fn file(
        &mut self,
        info: &EntryInfo,
        data: &mut dyn Read,
    ) -> Result<(), ArchiveError> {
        let (path, file) = self.create_file(info)?;
        // One byte more than declared is enough to tell the entry lied.
        let mut capped = Read::take(data, info.size.saturating_add(1));
        let copied = copy_data(&mut capped, &mut Counting::new(&file, &*self.progress))?;
        self.finish_file(info, &path, &file, copied)
    }

    /// Starts a file entry for a decoder that pushes its output in, rather
    /// than being read from: returns the writer to hand it and the ticket
    /// that ends the entry.
    ///
    /// The file is created, named, counted and capped exactly as for
    /// [`Staging::file`]; the writer refuses data past the entry's declared
    /// size and stops once the job is cancelled. Once the decoder is done with
    /// the writer, [`Staging::finish_writer`] with the ticket marks the file and
    /// sets its time and mode. Until then, or if it never is, the file is only
    /// a partial item that dropping the staging removes.
    pub(crate) fn file_writer(
        &mut self,
        info: &EntryInfo,
    ) -> Result<(EntryWriter, EntryTicket), ArchiveError> {
        let (path, file) = self.create_file(info)?;
        let state = Rc::new(EntryState::default());
        let writer = EntryWriter {
            out: Counting::new(file.try_clone()?, Arc::clone(&self.progress)),
            name: info.path.clone(),
            size: info.size,
            state: Rc::clone(&state),
        };
        let ticket = EntryTicket {
            info: info.clone(),
            path,
            file,
            state,
        };
        Ok((writer, ticket))
    }

    /// Ends an entry started with [`Staging::file_writer`] once its writer is
    /// done: the same finishing as for [`Staging::file`]. Fails with whatever
    /// made the writer stop, if it did.
    pub(crate) fn finish_writer(&self, ticket: EntryTicket) -> Result<(), ArchiveError> {
        if let Some(failure) = ticket.failure() {
            return Err(failure);
        }
        let copied = ticket.state.written.get();
        self.finish_file(&ticket.info, &ticket.path, &ticket.file, copied)
    }

    /// The job's progress, shared with whoever watches it.
    pub(crate) fn progress(&self) -> &Arc<Progress> {
        &self.progress
    }

    /// Creates the file for `info` at its first free name, below real folders
    /// only, and names it in the progress.
    fn create_file(&mut self, info: &EntryInfo) -> Result<(PathBuf, File), ArchiveError> {
        let (parent, name) = self.prepare(info)?;
        self.progress.set_current(&name.to_string_lossy());
        create_unique(&parent, name, |path| {
            OpenOptions::new().write(true).create_new(true).open(path)
        })
    }

    /// Checks that `copied` bytes are no more than `info` declares, then marks
    /// the written file and gives it its time and mode.
    fn finish_file(
        &self,
        info: &EntryInfo,
        path: &Path,
        file: &File,
        copied: u64,
    ) -> Result<(), ArchiveError> {
        if copied > info.size {
            return Err(too_large(&info.path));
        }
        // Marked before the mode is applied: a read-only file could refuse it.
        self.mark(path)?;
        if let Some(mtime) = info.mtime {
            set_time(path, mtime, |time| file.set_modified(time));
        }
        if let Some(mode) = permissions(info) {
            set_file_mode(file, mode)?;
        }
        Ok(())
    }

    /// Creates a folder entry, or reuses the folder when an earlier entry
    /// already created it. The owner always keeps full access, so the folder
    /// can be filled, fingerprinted and removed; its modified time is set
    /// when publishing, after everything inside it is written.
    pub(crate) fn dir(&mut self, info: &EntryInfo) -> Result<(), ArchiveError> {
        let (parent, name) = self.prepare(info)?;
        let path = parent.join(name);
        self.real_dir(&path, &info.path)?;
        if let Some(mode) = permissions(info) {
            set_dir_mode(&path, mode | 0o700)?;
        }
        if let Some(mtime) = info.mtime {
            self.dir_times.push((path, mtime));
        }
        Ok(())
    }

    /// Creates a symlink entry pointing at `target`, which must stay inside
    /// staging when followed from the link's own folder; a taken path gets
    /// the next free name as for files.
    pub(crate) fn symlink(&mut self, info: &EntryInfo, target: &str) -> Result<(), ArchiveError> {
        let (parent, name) = self.prepare(info)?;
        let from = parent.strip_prefix(self.root()).unwrap_or(&parent);
        let safe = !target.is_empty()
            && !target.contains('\0')
            && link_stays_inside(self.root(), Path::new(""), from, Path::new(target));
        if !safe {
            return Err(ArchiveError::UnsafeEntry(format!(
                "{} -> {target}",
                info.path.display()
            )));
        }
        let (path, ()) = create_unique(&parent, name, |path| make_symlink(target, path))?;
        self.mark(&path)?;
        self.links.push(path);
        Ok(())
    }

    /// Sets the folders' modified times and publishes the result under a
    /// free name in the archive's folder, never replacing anything.
    ///
    /// For [`Layout::Direct`] the one top-level item is moved out and the
    /// emptied partial folder removed; for [`Layout::Wrapped`] the partial
    /// folder itself becomes the result. `Direct` falls back to `Wrapped` in
    /// a folder named after the archive when staging holds anything besides
    /// the top item (an entry renamed because its name was taken), so nothing
    /// extracted is ever left behind. Every symlink is checked again
    /// against what is published, so a link cannot reach outside it through
    /// another link created later, or (for `Direct`) to the item's siblings.
    /// Returns the published path and its fingerprint, taken just before the
    /// move (which keeps every entry's identity and times).
    pub(crate) fn publish(
        mut self,
        layout: Layout,
    ) -> Result<(PathBuf, Fingerprint), ArchiveError> {
        if self.progress.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        for (path, mtime) in &self.dir_times {
            // Checked to be a real folder when it was made; nothing replaces it.
            let dir = open_dir_for_times(path)?;
            set_time(path, *mtime, |time| dir.set_modified(time));
        }
        let layout = match layout {
            Layout::Direct(top) if !self.holds_only(&top)? => {
                // An entry was renamed beside the top item (the same name
                // twice, or a file named like the top folder); publishing the
                // top alone would lose it.
                Layout::Wrapped(self.stem.clone())
            }
            layout => layout,
        };
        let (item, bound) = match &layout {
            Layout::Direct(top) => (self.root().join(top), top.as_path()),
            Layout::Wrapped(_) => (self.root().to_path_buf(), Path::new("")),
        };
        for link in &self.links {
            let relative = link.strip_prefix(self.root()).unwrap_or(link);
            let from = relative.parent().unwrap_or(Path::new(""));
            if !link_stays_inside(self.root(), bound, from, &fs::read_link(link)?) {
                return Err(ArchiveError::UnsafeEntry(relative.display().to_string()));
            }
        }
        let is_dir = fs::symlink_metadata(&item)?.is_dir();
        let fingerprint = Fingerprint::of(&item)?;
        let output = match &layout {
            Layout::Direct(top) => {
                let output = place_partial(&item, &self.dir, top.as_os_str(), is_dir)?;
                self.partial.published = true;
                // Not recursive: anything still inside stays where it can be
                // seen rather than vanish.
                if let Err(e) = fs::remove_dir(self.root()) {
                    log::warn!(
                        "could not remove emptied staging folder {}: {e}",
                        self.root().display()
                    );
                }
                output
            }
            Layout::Wrapped(stem) => {
                let output = place_partial(&item, &self.dir, stem, true)?;
                self.partial.published = true;
                output
            }
        };
        Ok((output, fingerprint))
    }

    /// Whether staging holds nothing but `top`.
    fn holds_only(&self, top: &Path) -> io::Result<bool> {
        for entry in fs::read_dir(self.root())? {
            if Path::new(&entry?.file_name()) != top {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn root(&self) -> &Path {
        &self.partial.path
    }

    fn mark(&self, path: &Path) -> Result<(), ArchiveError> {
        (self.mark)(path).map_err(ArchiveError::Io)
    }

    /// Checks the job is still wanted and `info`'s path is plain, and makes
    /// sure every folder above it is a real folder inside staging, creating
    /// the missing ones. Returns the entry's folder and its name.
    fn prepare<'i>(&self, info: &'i EntryInfo) -> Result<(PathBuf, &'i OsStr), ArchiveError> {
        if self.progress.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let plain = info
            .path
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        let name = match info.path.file_name() {
            Some(name) if plain => name,
            _ => return Err(ArchiveError::UnsafeEntry(info.path.display().to_string())),
        };
        let mut parent = self.root().to_path_buf();
        for folder in info.path.parent().into_iter().flat_map(Path::iter) {
            parent.push(folder);
            self.real_dir(&parent, &info.path)?;
        }
        Ok((parent, name))
    }

    /// Makes sure `path` is a real folder (not a symlink to one), creating and
    /// marking it when missing. `entry` names the entry for errors.
    fn real_dir(&self, path: &Path, entry: &Path) -> Result<(), ArchiveError> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(meta) if meta.file_type().is_symlink() => {
                Err(ArchiveError::UnsafeEntry(entry.display().to_string()))
            }
            Ok(_) => Err(ArchiveError::Damaged(format!(
                "{} is both a file and a folder",
                entry.display()
            ))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(path)?;
                self.mark(path)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// What an entry's writer and its ticket share.
#[derive(Default)]
struct EntryState {
    /// Bytes the writer has accepted.
    written: Cell<u64>,
    /// Why the writer stopped, when it did.
    failure: RefCell<Option<ArchiveError>>,
}

impl EntryState {
    /// Notes why the writer stopped; the first reason stands.
    fn fail(&self, failure: ArchiveError) {
        self.failure.borrow_mut().get_or_insert(failure);
    }
}

/// The writer of a file entry started with [`Staging::file_writer`]. It owns
/// everything it uses, so a decoder can hold it without borrowing the staging.
pub(crate) struct EntryWriter {
    out: Counting<Arc<Progress>, File>,
    /// The entry's path, for the message when it outgrows its size.
    name: PathBuf,
    /// The size the entry declares, which it may not exceed.
    size: u64,
    state: Rc<EntryState>,
}

impl Write for EntryWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.state.written.get();
        if buf.len() as u64 > self.size - written {
            // Refused before anything past the size reaches the disk.
            self.state.fail(too_large(&self.name));
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "entry is larger than the archive declares",
            ));
        }
        match self.out.write(buf) {
            Ok(count) => {
                self.state.written.set(written + count as u64);
                Ok(count)
            }
            Err(error) => {
                // The decoder's own error type cannot carry ours, so the real
                // reason is kept for the ticket.
                let copy = io::Error::new(error.kind(), error.to_string());
                self.state.fail(ArchiveError::from_io(copy));
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Ends a file entry started with [`Staging::file_writer`].
pub(crate) struct EntryTicket {
    info: EntryInfo,
    path: PathBuf,
    file: File,
    state: Rc<EntryState>,
}

impl EntryTicket {
    /// Why the entry's writer stopped, if it did: the entry outgrew its size,
    /// the job was cancelled, or the disk failed. Taken, so a second call
    /// gives `None`.
    pub(crate) fn failure(&self) -> Option<ArchiveError> {
        self.state.failure.borrow_mut().take()
    }
}

fn too_large(entry: &Path) -> ArchiveError {
    ArchiveError::Damaged(format!(
        "{} is larger than the archive declares",
        entry.display()
    ))
}

/// Creates an item with `create` at the first free name for `name` in
/// `parent`; `create` must fail with `AlreadyExists` rather than replace.
fn create_unique<T>(
    parent: &Path,
    name: &OsStr,
    mut create: impl FnMut(&Path) -> io::Result<T>,
) -> Result<(PathBuf, T), ArchiveError> {
    for _ in 0..NAME_ATTEMPTS {
        let path = unique_destination(parent, name);
        match create(&path) {
            Ok(made) => return Ok((path, made)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(no_free_name().into())
}

/// Copies an entry's data and returns the number of bytes copied, telling a
/// failed read (damage) from a failed write (disk, or Cancel).
fn copy_data(data: &mut impl Read, out: &mut impl Write) -> Result<u64, ArchiveError> {
    let mut buffer = vec![0u8; COPY_BUFFER];
    let mut copied = 0u64;
    loop {
        let read = match data.read(&mut buffer) {
            Ok(0) => return Ok(copied),
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ArchiveError::from_read(e)),
        };
        out.write_all(&buffer[..read])
            .map_err(ArchiveError::from_io)?;
        copied += read as u64;
    }
}

/// One step of a symlink target.
enum Step {
    Up,
    Into(OsString),
}

/// The steps of a relative link target; `None` for an absolute one.
fn steps(target: &Path) -> Option<Vec<Step>> {
    target
        .components()
        .filter_map(|component| match component {
            Component::CurDir => None,
            Component::ParentDir => Some(Some(Step::Up)),
            Component::Normal(name) => Some(Some(Step::Into(name.to_os_string()))),
            Component::RootDir | Component::Prefix(_) => Some(None),
        })
        .collect()
}

/// Whether a link in the folder `from` pointing at `target` stays inside
/// `bound` at every step. `from` and `bound` are relative to `root`.
///
/// Links already inside `root` are followed on the way (a chain longer than
/// [`MAX_LINK_HOPS`] counts as leaving), so `a -> b/..` with `b -> .` is
/// caught; names that do not exist yet are taken as plain folders.
fn link_stays_inside(root: &Path, bound: &Path, from: &Path, target: &Path) -> bool {
    let floor = bound.iter().count();
    if !from.starts_with(bound) {
        return false;
    }
    let mut at: Vec<OsString> = from.iter().map(OsStr::to_os_string).collect();
    let Some(mut pending) = steps(target) else {
        return false;
    };
    pending.reverse();
    let mut hops = 0;
    while let Some(step) = pending.pop() {
        match step {
            Step::Up if at.len() <= floor => return false,
            Step::Up => {
                at.pop();
            }
            Step::Into(name) => {
                at.push(name);
                let here = root.join(at.iter().collect::<PathBuf>());
                if let Ok(next) = fs::read_link(&here) {
                    hops += 1;
                    at.pop();
                    let Some(more) = steps(&next).filter(|_| hops <= MAX_LINK_HOPS) else {
                        return false;
                    };
                    pending.extend(more.into_iter().rev());
                }
            }
        }
    }
    true
}

#[cfg(unix)]
fn make_symlink(target: &str, path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn make_symlink(_target: &str, _path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symlinks in archives are not supported here",
    ))
}

/// Gives the item at `path` its modified `time` with `set`. A time the
/// filesystem refuses is logged and left out: it never fails the extraction.
fn set_time(path: &Path, time: SystemTime, set: impl FnOnce(SystemTime) -> io::Result<()>) {
    if let Err(e) = set(time) {
        log::warn!("could not set the modified time of {}: {e}", path.display());
    }
}

/// The permission bits to give `info`'s item, if any. A mode with none of
/// them set (some writers mark an entry as unix-made but fill in only its
/// DOS attributes) is more likely an unset field than intent: the item keeps
/// the default mode rather than become unreadable. Every reader relies on
/// this.
fn permissions(info: &EntryInfo) -> Option<u32> {
    info.mode.map(|mode| mode & 0o777).filter(|&mode| mode != 0)
}

#[cfg(unix)]
fn set_file_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_file_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Opens the folder at `path` so its modified time can be set.
#[cfg(not(windows))]
fn open_dir_for_times(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// Opens the folder at `path` so its modified time can be set. Windows opens
/// a folder only with backup semantics, and setting a time needs the right to
/// write attributes, which a plain read-only open lacks.
#[cfg(windows)]
fn open_dir_for_times(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

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
    use crate::archive::entries::EntryKind;
    use crate::archive::test_support::{listing, no_mark};
    use std::io::Write;

    fn info(path: &str, kind: EntryKind) -> EntryInfo {
        EntryInfo {
            path: PathBuf::from(path),
            kind,
            size: 0,
            mode: None,
            mtime: None,
        }
    }

    /// A file entry declaring `data`'s size.
    fn file_info(path: &str, data: &[u8]) -> EntryInfo {
        EntryInfo {
            size: data.len() as u64,
            ..info(path, EntryKind::File)
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_modified_time_is_skipped() {
        let asked = Cell::new(None);
        set_time(
            Path::new("/staging/a.txt"),
            SystemTime::UNIX_EPOCH,
            |time| {
                asked.set(Some(time));
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            },
        );
        assert_eq!(asked.get(), Some(SystemTime::UNIX_EPOCH));
    }

    #[cfg(unix)]
    #[test]
    fn staging_never_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Evil.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        // A link that got into staging somehow; later entries must not follow it.
        std::os::unix::fs::symlink(outside.path(), staging.partial.path.join("a")).unwrap();

        let file = staging.file(&info("a/evil", EntryKind::File), &mut &b"x"[..]);
        let folder = staging.dir(&info("a/sub", EntryKind::Dir));
        let link = staging.symlink(&info("a/link", EntryKind::Symlink), "x");

        assert!(matches!(file, Err(ArchiveError::UnsafeEntry(_))));
        assert!(matches!(folder, Err(ArchiveError::UnsafeEntry(_))));
        assert!(matches!(link, Err(ArchiveError::UnsafeEntry(_))));
        assert!(listing(outside.path()).is_empty());
        drop(staging);
        assert!(listing(dir.path()).is_empty());
        assert!(outside.path().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn link_chain_leaving_the_output_is_refused_at_publish() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Chain.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        // Each link stays inside when it is created, but `l` resolves through
        // `b`, created later, to the folder above the output.
        staging
            .symlink(&info("l", EntryKind::Symlink), "b/..")
            .unwrap();
        staging
            .symlink(&info("b", EntryKind::Symlink), ".")
            .unwrap();

        let result = staging.publish(Layout::Wrapped("Chain".into()));

        assert!(matches!(result, Err(ArchiveError::UnsafeEntry(_))));
        assert!(listing(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn link_to_a_sibling_of_a_direct_top_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        // Inside staging, but once `Photos` is published on its own the link
        // would point at whatever sits beside it.
        staging
            .symlink(&info("Photos/up", EntryKind::Symlink), "../Photos.zip")
            .unwrap();

        let result = staging.publish(Layout::Direct("Photos".into()));

        assert!(matches!(result, Err(ArchiveError::UnsafeEntry(_))));
        assert!(listing(dir.path()).is_empty());
    }

    #[test]
    fn taken_names_inside_staging_get_free_names() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Dup.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        staging
            .file(&file_info("a.txt", b"one"), &mut &b"one"[..])
            .unwrap();
        staging
            .file(&file_info("a.txt", b"two"), &mut &b"two"[..])
            .unwrap();

        let (output, _) = staging.publish(Layout::Wrapped("Dup".into())).unwrap();

        assert_eq!(output, dir.path().join("Dup"));
        assert_eq!(fs::read(output.join("a.txt")).unwrap(), b"one");
        assert_eq!(fs::read(output.join("a (2).txt")).unwrap(), b"two");
    }

    #[test]
    fn duplicate_top_level_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Dup.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        // Every entry shares the top `a.txt`, but the second one had to be
        // renamed inside staging, so publishing `a.txt` alone would lose it.
        staging
            .file(&file_info("a.txt", b"one"), &mut &b"one"[..])
            .unwrap();
        staging
            .file(&file_info("a.txt", b"two"), &mut &b"two"[..])
            .unwrap();

        let (output, fingerprint) = staging.publish(Layout::Direct("a.txt".into())).unwrap();

        assert_eq!(output, dir.path().join("Dup"));
        assert_eq!(fs::read(output.join("a.txt")).unwrap(), b"one");
        assert_eq!(fs::read(output.join("a (2).txt")).unwrap(), b"two");
        assert_eq!(Fingerprint::of(&output).unwrap(), fingerprint);
        assert_eq!(listing(dir.path()), ["Dup"]);
    }

    #[test]
    fn direct_publish_removes_the_emptied_staging_folder() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Photos.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        staging
            .file(&file_info("Photos/a.jpg", b"jpg"), &mut &b"jpg"[..])
            .unwrap();

        let (output, _) = staging.publish(Layout::Direct("Photos".into())).unwrap();

        assert_eq!(output, dir.path().join("Photos"));
        assert_eq!(listing(dir.path()), ["Photos"]);
    }

    #[test]
    fn file_longer_than_declared_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Lies.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        staging
            .file(&file_info("exact.txt", b"abc"), &mut &b"abc"[..])
            .unwrap();

        let result = staging.file(&file_info("big.txt", b"abc"), &mut &b"abcdef"[..]);

        assert!(
            matches!(result, Err(ArchiveError::Damaged(_))),
            "{result:?}"
        );
        assert!(progress.snapshot().done <= 7);
        drop(staging);
        assert!(listing(dir.path()).is_empty());
    }

    /// Writes `pieces` through a writer the way a decoder that pushes its
    /// output would, then finishes the entry.
    fn push_file(
        staging: &mut Staging<'_>,
        info: &EntryInfo,
        pieces: &[&[u8]],
    ) -> Result<(), ArchiveError> {
        let (mut writer, ticket) = staging.file_writer(info)?;
        let mut result = Ok(());
        for piece in pieces {
            if let Err(error) = writer.write_all(piece) {
                result = Err(ticket.failure().unwrap_or(ArchiveError::from_io(error)));
                break;
            }
        }
        drop(writer);
        result?;
        staging.finish_writer(ticket)
    }

    #[test]
    fn writer_entry_is_written_counted_and_stamped_like_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Pushed.zip");
        let progress = Progress::new();
        let marked = std::cell::RefCell::new(Vec::new());
        let mark = |path: &Path| {
            marked
                .borrow_mut()
                .push(path.file_name().unwrap().to_owned());
            Ok(())
        };
        let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let mut staging = Staging::new(dir.path(), &archive, &progress, &mark).unwrap();
        let info = EntryInfo {
            mode: Some(0o640),
            mtime: Some(mtime),
            ..file_info("sub/a.txt", b"hello")
        };

        push_file(&mut staging, &info, &[b"hel", b"lo"]).unwrap();
        let (output, _) = staging.publish(Layout::Wrapped("Pushed".into())).unwrap();

        let path = output.join("sub/a.txt");
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o640);
        }
        assert_eq!(progress.snapshot().done, 5);
        assert_eq!(progress.snapshot().current, "a.txt");
        assert!(marked.borrow().iter().any(|name| name == "a.txt"));
        assert_eq!(listing(dir.path()), ["Pushed"]);
    }

    #[test]
    fn writer_entry_takes_the_next_free_name_when_its_path_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Dup.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();

        push_file(&mut staging, &file_info("a.txt", b"one"), &[b"one"]).unwrap();
        push_file(&mut staging, &file_info("a.txt", b"two"), &[b"two"]).unwrap();
        let (output, _) = staging.publish(Layout::Wrapped("Dup".into())).unwrap();

        assert_eq!(fs::read(output.join("a.txt")).unwrap(), b"one");
        assert_eq!(fs::read(output.join("a (2).txt")).unwrap(), b"two");
    }

    #[test]
    fn writer_entry_longer_than_declared_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Lies.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();

        let result = push_file(
            &mut staging,
            &file_info("big.txt", b"abc"),
            &[b"ab", b"cdef"],
        );

        assert!(
            matches!(result, Err(ArchiveError::Damaged(_))),
            "{result:?}"
        );
        assert_eq!(progress.snapshot().done, 2);
        drop(staging);
        assert!(listing(dir.path()).is_empty());
    }

    #[test]
    fn writer_entry_stops_once_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Stop.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        let (mut writer, ticket) = staging.file_writer(&file_info("a", b"abcd")).unwrap();
        writer.write_all(b"ab").unwrap();

        progress.cancel();
        let error = writer.write_all(b"cd").unwrap_err();

        assert!(matches!(ticket.failure(), Some(ArchiveError::Cancelled)));
        assert!(matches!(
            ArchiveError::from_io(error),
            ArchiveError::Cancelled
        ));
        drop((writer, ticket));
        drop(staging);
        assert!(listing(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn writer_entry_never_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Evil.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        std::os::unix::fs::symlink(outside.path(), staging.partial.path.join("a")).unwrap();

        let result = staging.file_writer(&file_info("a/evil", b"x"));

        assert!(matches!(result, Err(ArchiveError::UnsafeEntry(_))));
        assert!(listing(outside.path()).is_empty());
    }

    #[test]
    fn dropped_staging_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Gone.zip");
        let progress = Progress::new();
        let mut staging = Staging::new(dir.path(), &archive, &progress, &no_mark).unwrap();
        staging.dir(&info("x/y", EntryKind::Dir)).unwrap();
        staging
            .file(&file_info("x/y/z.txt", b"z"), &mut &b"z"[..])
            .unwrap();
        drop(staging);
        assert!(listing(dir.path()).is_empty());
    }

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
