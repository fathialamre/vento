//! Safe file moves: never overwrite, never delete a source before a verified copy exists.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Why a single item could not be moved.
#[derive(thiserror::Error, Debug)]
pub enum FileOpError {
    /// An underlying filesystem call failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// The destination is the item itself or lies inside it.
    #[error("cannot move an item into itself")]
    IntoItself,
    /// The source does not exist.
    #[error("source not found")]
    NotFound,
    /// A cross-device copy did not match its source; the copy was removed.
    #[error("copy could not be verified; source left untouched")]
    VerifyFailed,
    /// A cross-device move was copied and verified, but the original could not
    /// be (fully) removed. The complete copy is at `copy`; the leftover source
    /// must not be mistaken for a stray duplicate.
    #[error("moved to {}, but the original could not be fully removed: {source}", .copy.display())]
    SourceNotRemoved {
        /// Where the verified, complete copy lives.
        copy: PathBuf,
        /// Why removing the original failed.
        source: io::Error,
    },
}

/// Items successfully moved, as `(from, to)` pairs, in input order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRecord {
    /// Destination folder the items were moved into.
    pub dest: PathBuf,
    /// `(from, to)` for every item that actually moved.
    pub moved: Vec<(PathBuf, PathBuf)>,
}

/// Result of a batch move or undo: what moved and what failed.
#[derive(Debug)]
pub struct MoveOutcome {
    /// Successful moves.
    pub record: MoveRecord,
    /// Items that did not move, with the reason.
    pub failed: Vec<(PathBuf, FileOpError)>,
}

fn no_file_name() -> FileOpError {
    FileOpError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        "path has no file name",
    ))
}

/// Whether anything (including a dangling symlink) occupies `path`.
fn occupied(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Name for the `n`-th duplicate: `a.txt` -> `a (2).txt`, `.env` -> `.env (2)`.
pub(crate) fn numbered_name(name: &OsStr, n: u32) -> OsString {
    let path = Path::new(name);
    // `file_stem` is the whole name for dotfiles, so a leading dot never splits.
    let (stem, ext) = (path.file_stem().unwrap_or(name), path.extension());
    let mut out = OsString::from(stem);
    out.push(format!(" ({n})"));
    if let Some(ext) = ext {
        out.push(".");
        out.push(ext);
    }
    out
}

/// First free path in `dir` for `name`, counting up `name (2)`, `name (3)`, ...
///
/// Checks the real filesystem without following symlinks, so a dangling
/// symlink counts as occupied.
pub fn unique_destination(dir: &Path, name: &OsStr) -> PathBuf {
    let plain = dir.join(name);
    if !occupied(&plain) {
        return plain;
    }
    (2u32..)
        .map(|n| dir.join(numbered_name(name, n)))
        .find(|candidate| !occupied(candidate))
        .expect("u32 range is not exhausted")
}

/// Moves `src` into `dest_dir` under `name` (renamed if taken). `name` is the
/// wanted name in `dest_dir`; it may differ from `src`'s own name (undo).
///
/// Returns the final path, or `None` when `src` already lives in `dest_dir`.
fn move_one(src: &Path, dest_dir: &Path, name: &OsStr) -> Result<Option<PathBuf>, FileOpError> {
    // `symlink_metadata` so a dangling symlink still counts as an existing source.
    fs::symlink_metadata(src).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => FileOpError::NotFound,
        _ => FileOpError::Io(e),
    })?;
    let src_name = src.file_name().ok_or_else(no_file_name)?;

    // Canonicalize the folders (not the source itself, so a symlink is moved as
    // a link) so that `..` and symlinked folders cannot hide a self-move. The
    // checks use the source's real name, not the name it is moved under.
    let canon_dest = fs::canonicalize(dest_dir)?;
    let src_parent = match src.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let canon_src = fs::canonicalize(src_parent)?.join(src_name);

    if canon_src.parent() == Some(canon_dest.as_path()) {
        return Ok(None);
    }
    if canon_dest.starts_with(&canon_src) {
        return Err(FileOpError::IntoItself);
    }

    let target = unique_destination(dest_dir, name);
    match fs::rename(src, &target) {
        Ok(()) => Ok(Some(target)),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            move_by_copy(src, dest_dir, name).map(Some)
        }
        Err(e) => Err(FileOpError::Io(e)),
    }
}

/// Copies a file, folder or symlink into `dir` under a free name (`a.txt` ->
/// `a (2).txt` when taken) and returns the new path.
///
/// Symlinks are recreated, not followed. Nothing that exists is replaced or
/// deleted and `src` is left untouched. The copy is built under a hidden
/// partial name, verified, and only then given its final name; on any failure
/// only the partial item this call created is removed.
pub fn copy_into(src: &Path, dir: &Path) -> Result<PathBuf, FileOpError> {
    let meta = fs::symlink_metadata(src).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => FileOpError::NotFound,
        _ => FileOpError::Io(e),
    })?;
    let name = src.file_name().ok_or_else(no_file_name)?;
    if meta.is_dir() {
        // A folder copied into itself would grow forever.
        let src_parent = match src.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let canon_src = fs::canonicalize(src_parent)?.join(name);
        if fs::canonicalize(dir)?.starts_with(&canon_src) {
            return Err(FileOpError::IntoItself);
        }
    }
    let partial = claim_partial(src, &meta, dir, name)?;
    let verified = fill_entry(src, &meta, &partial)
        .and_then(|()| copies_match(src, &partial))
        .map_err(FileOpError::Io);
    match verified {
        Ok(true) => {}
        Ok(false) => {
            remove_any(&partial);
            return Err(FileOpError::VerifyFailed);
        }
        Err(e) => {
            remove_any(&partial);
            return Err(e);
        }
    }
    place_partial(&partial, dir, name, meta.is_dir()).map_err(|e| {
        remove_any(&partial);
        FileOpError::Io(e)
    })
}

/// Which file on disk an entry is: `(device, inode)` on unix. Two different
/// files can share size and modification time (a copy keeps its source's
/// mtime), but never this. Targets without file ids carry `()`, so only the
/// size/time/target checks apply there.
#[cfg(unix)]
pub type FileId = (u64, u64);
/// See the unix definition; other targets have no file id.
#[cfg(not(unix))]
pub type FileId = ();

#[cfg(unix)]
fn file_id(meta: &fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn file_id(_meta: &fs::Metadata) -> FileId {}

/// One entry below a fingerprinted folder.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntryPrint {
    /// Path relative to the folder.
    pub path: PathBuf,
    /// Size in bytes.
    pub len: u64,
    /// Last modification time.
    pub modified: SystemTime,
    /// Which file this is.
    pub id: FileId,
    /// Target, for a symlink; `None` for anything else.
    pub link: Option<PathBuf>,
}

/// What a copy looked like right after vento made it, so undo can tell
/// whether it has been touched since, or replaced by something else (such as
/// the user's own original moved into its place).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fingerprint {
    /// A regular (or other non-folder, non-link) file.
    File {
        /// Size in bytes.
        len: u64,
        /// Last modification time.
        modified: SystemTime,
        /// Which file this is.
        id: FileId,
    },
    /// A folder: its own identity and every entry below it, sorted by path.
    Dir {
        /// Which folder this is.
        id: FileId,
        /// Every entry below the folder.
        entries: Vec<EntryPrint>,
    },
    /// A symlink, identified by its target and itself; never followed.
    Link {
        /// Where it points.
        target: PathBuf,
        /// Which link this is.
        id: FileId,
    },
}

impl Fingerprint {
    /// Fingerprints `path` without following symlinks.
    ///
    /// Any error (a missing entry, an unreadable folder) is returned rather
    /// than skipped, so callers can treat "could not tell" as "changed".
    pub fn of(path: &Path) -> io::Result<Fingerprint> {
        let meta = fs::symlink_metadata(path)?;
        let kind = meta.file_type();
        let id = file_id(&meta);
        if kind.is_symlink() {
            Ok(Fingerprint::Link {
                target: fs::read_link(path)?,
                id,
            })
        } else if kind.is_dir() {
            let mut entries = Vec::new();
            collect_entries(path, Path::new(""), &mut entries)?;
            entries.sort();
            Ok(Fingerprint::Dir { id, entries })
        } else {
            Ok(Fingerprint::File {
                len: meta.len(),
                modified: meta.modified()?,
                id,
            })
        }
    }
}

/// Appends every entry below `dir` to `out`. Symlinks are listed as
/// themselves (with their target) and not followed.
fn collect_entries(dir: &Path, rel: &Path, out: &mut Vec<EntryPrint>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let rel_path = rel.join(entry.file_name());
        let link = if meta.file_type().is_symlink() {
            Some(fs::read_link(&path)?)
        } else {
            None
        };
        out.push(EntryPrint {
            path: rel_path.clone(),
            len: meta.len(),
            modified: meta.modified()?,
            id: file_id(&meta),
            link,
        });
        if meta.is_dir() {
            collect_entries(&path, &rel_path, out)?;
        }
    }
    Ok(())
}

/// Copies made by one Copy to action: what undo needs to recognise them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRecord {
    /// Destination folder the items were copied into.
    pub dest: PathBuf,
    /// `(source, copy, fingerprint taken right after the copy)` per item.
    pub copies: Vec<(PathBuf, PathBuf, Fingerprint)>,
}

/// Result of a batch copy: what was copied and what failed.
#[derive(Debug)]
pub struct CopyOutcome {
    /// Copies that were made and fingerprinted.
    pub record: CopyRecord,
    /// Items that were not copied, with the reason.
    pub failed: Vec<(PathBuf, FileOpError)>,
}

/// Copies `paths` into `dest` under free names, never overwriting.
///
/// One item failing does not stop the others. A copy that was made and
/// verified but could not be fingerprinted is reported as failed and left in
/// place; it is not recorded, so undo never touches it.
pub fn copy_items(paths: &[PathBuf], dest: &Path) -> CopyOutcome {
    let mut copies = Vec::new();
    let mut failed = Vec::new();
    for path in paths {
        let copy = match copy_into(path, dest) {
            Ok(copy) => copy,
            Err(e) => {
                failed.push((path.clone(), e));
                continue;
            }
        };
        match Fingerprint::of(&copy) {
            Ok(fingerprint) => copies.push((path.clone(), copy, fingerprint)),
            Err(e) => {
                log::warn!(
                    "copied {} to {} but could not fingerprint it: {e}",
                    path.display(),
                    copy.display()
                );
                failed.push((path.clone(), FileOpError::Io(e)));
            }
        }
    }
    CopyOutcome {
        record: CopyRecord {
            dest: dest.to_path_buf(),
            copies,
        },
        failed,
    }
}

/// Result of undoing a Copy to.
#[derive(Debug, Default)]
pub struct CopyUndoOutcome {
    /// Copies handed to the trash.
    pub trashed: usize,
    /// Copies left in place because they changed since vento made them (or
    /// could not be checked).
    pub kept_changed: Vec<PathBuf>,
    /// Copies that no longer exist.
    pub gone: usize,
    /// Copies the trash refused, with its message.
    pub failed: Vec<(PathBuf, String)>,
}

/// Sends the copies in `record` to the trash, but only those still exactly as
/// vento left them.
///
/// Only the `trash` closure ever removes anything; sources are never touched
/// and symlinks are never followed. A copy that cannot be fingerprinted counts
/// as changed and is kept.
pub fn undo_copies<E: std::fmt::Display>(
    record: &CopyRecord,
    trash: impl Fn(&Path) -> Result<(), E>,
) -> CopyUndoOutcome {
    let mut out = CopyUndoOutcome::default();
    for (_, copy, recorded) in &record.copies {
        match fs::symlink_metadata(copy) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                out.gone += 1;
                continue;
            }
            Err(_) => {
                out.kept_changed.push(copy.clone());
                continue;
            }
            Ok(_) => {}
        }
        match Fingerprint::of(copy) {
            Ok(now) if now == *recorded => match trash(copy) {
                Ok(()) => out.trashed += 1,
                Err(e) => out.failed.push((copy.clone(), e.to_string())),
            },
            _ => out.kept_changed.push(copy.clone()),
        }
    }
    out
}

/// Moves `paths` into `dest`, renaming on conflict and never overwriting.
///
/// Items already inside `dest` are skipped silently. One item failing does not
/// stop the others.
pub fn move_items(paths: &[PathBuf], dest: &Path) -> MoveOutcome {
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for path in paths {
        let result = match path.file_name() {
            Some(name) => move_one(path, dest, name),
            None => Err(no_file_name()),
        };
        record_step(path, result, &mut moved, &mut failed);
    }
    MoveOutcome {
        record: MoveRecord {
            dest: dest.to_path_buf(),
            moved,
        },
        failed,
    }
}

/// Moves every item of `record` back to where it came from.
///
/// If the original path is taken the item gets a numbered name beside it. The
/// returned record's pairs are `(where it was, where it went)`, and its `dest`
/// is the original folder of the first restored item.
pub fn undo(record: &MoveRecord) -> MoveOutcome {
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for (from, to) in &record.moved {
        let result = match (from.parent(), from.file_name()) {
            (Some(dir), Some(name)) => move_one(to, dir, name),
            _ => Err(FileOpError::NotFound),
        };
        record_step(to, result, &mut moved, &mut failed);
    }
    let dest = moved
        .first()
        .and_then(|(_, back)| back.parent())
        .map_or_else(|| record.dest.clone(), Path::to_path_buf);
    MoveOutcome {
        record: MoveRecord { dest, moved },
        failed,
    }
}

/// Files one item's result into the moved/failed lists.
///
/// A verified copy whose original could not be removed is recorded as moved
/// (the copy is the result, so undo can reach it) and also reported as failed
/// under the source path, so the leftover original is surfaced.
fn record_step(
    source: &Path,
    result: Result<Option<PathBuf>, FileOpError>,
    moved: &mut Vec<(PathBuf, PathBuf)>,
    failed: &mut Vec<(PathBuf, FileOpError)>,
) {
    match result {
        Ok(Some(to)) => moved.push((source.to_path_buf(), to)),
        Ok(None) => {}
        Err(e) => {
            if let FileOpError::SourceNotRemoved { copy, .. } = &e {
                log::warn!(
                    "{} was copied to {} but could not be fully removed: {e}",
                    source.display(),
                    copy.display()
                );
                moved.push((source.to_path_buf(), copy.clone()));
            }
            failed.push((source.to_path_buf(), e));
        }
    }
}

/// Longest original name kept inside a partial-copy name; longer names are
/// left out so the temporary name stays within the 255-byte name limit.
const MAX_NAME_IN_PARTIAL: usize = 200;
/// How many partial-copy names, and final names, are tried before giving up.
pub(crate) const NAME_ATTEMPTS: u32 = 1000;

/// Hidden name a cross-device copy is built under before it gets its final
/// name: `.{name}.vento-partial-{pid}`, then `...-{pid}-2`, `-3`, ...
pub(crate) fn partial_name(name: &OsStr, attempt: u32) -> OsString {
    let mut out = OsString::from(".");
    if name.len() <= MAX_NAME_IN_PARTIAL {
        out.push(name);
        out.push(".");
    }
    out.push(format!("vento-partial-{}", std::process::id()));
    if attempt > 1 {
        out.push(format!("-{attempt}"));
    }
    out
}

/// Moves across devices: copy into a hidden partial item in `dest_dir`,
/// verify it, give it its final (unique) name, and only then remove the source.
///
/// Refuses up front (source untouched) when the source obviously cannot be
/// removed or contains special files. On a failed copy or verification only
/// the partial item this call created is deleted, and the source is left
/// untouched. If removal still fails after the copy is in place, returns
/// [`FileOpError::SourceNotRemoved`] and keeps the copy. Returns the final path.
fn move_by_copy(from: &Path, dest_dir: &Path, name: &OsStr) -> Result<PathBuf, FileOpError> {
    move_by_copy_with(from, dest_dir, name, copies_match)
}

/// [`move_by_copy`] with the verification step injected (tests).
fn move_by_copy_with(
    from: &Path,
    dest_dir: &Path,
    name: &OsStr,
    verify: impl Fn(&Path, &Path) -> io::Result<bool>,
) -> Result<PathBuf, FileOpError> {
    ensure_removable(from)?;
    let meta = fs::symlink_metadata(from)?;
    let partial = claim_partial(from, &meta, dest_dir, name)?;
    let verified = fill_entry(from, &meta, &partial)
        .and_then(|()| verify(from, &partial))
        .map_err(FileOpError::Io);
    match verified {
        Ok(true) => {}
        Ok(false) => {
            remove_any(&partial);
            return Err(FileOpError::VerifyFailed);
        }
        Err(e) => {
            remove_any(&partial);
            return Err(e);
        }
    }
    let to = match place_partial(&partial, dest_dir, name, meta.is_dir()) {
        Ok(to) => to,
        Err(e) => {
            // The verified copy never got its final name; the source is intact.
            remove_any(&partial);
            return Err(FileOpError::Io(e));
        }
    };
    remove_source_after_copy(from, &to)?;
    Ok(to)
}

/// Creates the top-level entry of the copy under the first free partial name
/// in `dest_dir`. An item already at a partial name is skipped, never touched.
fn claim_partial(
    from: &Path,
    meta: &fs::Metadata,
    dest_dir: &Path,
    name: &OsStr,
) -> Result<PathBuf, FileOpError> {
    for attempt in 1..=NAME_ATTEMPTS {
        let candidate = dest_dir.join(partial_name(name, attempt));
        match create_entry(from, meta, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(FileOpError::Io(e)),
        }
    }
    Err(FileOpError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for a temporary copy",
    )))
}

/// Gives the verified `partial` its final unique name in `dest_dir`.
///
/// Files and symlinks are hard-linked to the final name, which refuses to
/// replace anything, and the partial name is then unlinked. Folders (and
/// files on volumes without hard links) are renamed right after checking the
/// name is free; `rename` itself could replace an item created in between.
pub(crate) fn place_partial(
    partial: &Path,
    dest_dir: &Path,
    name: &OsStr,
    is_dir: bool,
) -> io::Result<PathBuf> {
    for _ in 0..NAME_ATTEMPTS {
        let to = unique_destination(dest_dir, name);
        if !is_dir {
            match fs::hard_link(partial, &to) {
                Ok(()) => {
                    if let Err(e) = fs::remove_file(partial) {
                        log::warn!("could not remove temporary name {}: {e}", partial.display());
                    }
                    return Ok(to);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => log::debug!("hard link to {} failed ({e}); renaming", to.display()),
            }
        }
        if occupied(&to) {
            continue;
        }
        fs::rename(partial, &to)?;
        return Ok(to);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for the moved item",
    ))
}

/// Removes the original once `to` holds a verified copy; the copy is kept
/// whatever happens.
fn remove_source_after_copy(from: &Path, to: &Path) -> Result<(), FileOpError> {
    remove_any_checked(from).map_err(|source| FileOpError::SourceNotRemoved {
        copy: to.to_path_buf(),
        source,
    })
}

/// Refuses early when removing `path` would certainly fail part-way: its parent
/// folder or any folder inside it is read-only (a locked file or read-only
/// volume can still fail later; that case is reported by
/// [`FileOpError::SourceNotRemoved`]). Also refuses special files (FIFOs,
/// sockets, devices) anywhere in the tree, which cannot be copied.
fn ensure_removable(path: &Path) -> Result<(), FileOpError> {
    let denied = |what: &Path| {
        FileOpError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("cannot remove from read-only folder {}", what.display()),
        ))
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && fs::metadata(parent)?.permissions().readonly()
    {
        return Err(denied(parent));
    }
    check_tree(path).map_err(|e| match e {
        TreeCheck::ReadOnly(dir) => denied(&dir),
        TreeCheck::Special(item) => FileOpError::Io(special_file(&item)),
        TreeCheck::Io(e) => FileOpError::Io(e),
    })
}

fn special_file(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("cannot move special file {}", path.display()),
    )
}

enum TreeCheck {
    ReadOnly(PathBuf),
    Special(PathBuf),
    Io(io::Error),
}

fn check_tree(path: &Path) -> Result<(), TreeCheck> {
    let meta = fs::symlink_metadata(path).map_err(TreeCheck::Io)?;
    let kind = meta.file_type();
    if kind.is_file() || kind.is_symlink() {
        return Ok(());
    }
    if !kind.is_dir() {
        return Err(TreeCheck::Special(path.to_path_buf()));
    }
    if meta.permissions().readonly() {
        return Err(TreeCheck::ReadOnly(path.to_path_buf()));
    }
    for entry in fs::read_dir(path).map_err(TreeCheck::Io)? {
        check_tree(&entry.map_err(TreeCheck::Io)?.path())?;
    }
    Ok(())
}

/// Best-effort removal of a partial copy that this module created.
pub(crate) fn remove_any(path: &Path) {
    if let Err(e) = remove_any_checked(path) {
        log::warn!("could not clean up partial copy {}: {e}", path.display());
    }
}

fn remove_any_checked(path: &Path) -> io::Result<()> {
    // `symlink_metadata` so a symlink is unlinked, never followed.
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Copies a file, folder or symlink to the free path `to`; symlinks are
/// recreated, not followed. Never replaces anything at `to`.
fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    create_entry(from, &meta, to)?;
    fill_entry(from, &meta, to)
}

/// Creates `to` as an empty folder, an empty file or the finished symlink.
/// Fails with `AlreadyExists`, having created nothing, if `to` is taken.
fn create_entry(from: &Path, meta: &fs::Metadata, to: &Path) -> io::Result<()> {
    let kind = meta.file_type();
    if kind.is_symlink() {
        copy_symlink(from, to)
    } else if kind.is_dir() {
        fs::create_dir(to)
    } else if kind.is_file() {
        fs::File::create_new(to).map(drop)
    } else {
        Err(special_file(from))
    }
}

/// Copies the contents of `from` into the entry [`create_entry`] made at `to`.
fn fill_entry(from: &Path, meta: &fs::Metadata, to: &Path) -> io::Result<()> {
    let kind = meta.file_type();
    if kind.is_dir() {
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        // Applied last: writing children bumps the folder's mtime and a
        // read-only source folder would block creating them.
        fs::set_permissions(to, meta.permissions())
    } else if kind.is_file() {
        // `to` is the empty file this copy just created.
        fs::copy(from, to)?;
        // Best effort: keep the modification time a move would have kept.
        if let (Ok(modified), Ok(file)) =
            (meta.modified(), fs::File::options().write(true).open(to))
        {
            let _ = file.set_modified(modified);
        }
        Ok(())
    } else {
        // A symlink is complete once created.
        Ok(())
    }
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)
}

#[cfg(windows)]
fn copy_symlink(from: &Path, to: &Path) -> io::Result<()> {
    let target = fs::read_link(from)?;
    if fs::metadata(from).is_ok_and(|m| m.is_dir()) {
        std::os::windows::fs::symlink_dir(target, to)
    } else {
        std::os::windows::fs::symlink_file(target, to)
    }
}

#[cfg(not(any(unix, windows)))]
fn copy_symlink(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Total bytes and entry count (files, folders, symlinks) under `path`.
/// Symlinks are counted, not followed, and contribute no bytes.
fn measure(path: &Path) -> io::Result<(u64, u64)> {
    let meta = fs::symlink_metadata(path)?;
    let kind = meta.file_type();
    if kind.is_dir() {
        let (mut bytes, mut entries) = (0, 1);
        for entry in fs::read_dir(path)? {
            let (b, e) = measure(&entry?.path())?;
            bytes += b;
            entries += e;
        }
        Ok((bytes, entries))
    } else if kind.is_symlink() {
        Ok((0, 1))
    } else {
        Ok((meta.len(), 1))
    }
}

/// Whether `to` has the same total size and entry count as `from`.
fn copies_match(from: &Path, to: &Path) -> io::Result<bool> {
    Ok(measure(from)? == measure(to)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn unique_destination_counts_up() {
        let d = tempdir().unwrap();
        write(&d.path().join("a.txt"), "");
        write(&d.path().join("a (2).txt"), "");
        assert_eq!(
            unique_destination(d.path(), OsStr::new("a.txt")),
            d.path().join("a (3).txt")
        );
    }

    #[test]
    fn unique_destination_free_name_unchanged() {
        let d = tempdir().unwrap();
        assert_eq!(
            unique_destination(d.path(), OsStr::new("a.txt")),
            d.path().join("a.txt")
        );
    }

    #[test]
    fn unique_destination_names() {
        let d = tempdir().unwrap();
        write(&d.path().join(".env"), "");
        fs::create_dir(d.path().join("f")).unwrap();
        write(&d.path().join("archive.tar.gz"), "");
        assert_eq!(
            unique_destination(d.path(), OsStr::new(".env")),
            d.path().join(".env (2)")
        );
        assert_eq!(
            unique_destination(d.path(), OsStr::new("f")),
            d.path().join("f (2)")
        );
        assert_eq!(
            unique_destination(d.path(), OsStr::new("archive.tar.gz")),
            d.path().join("archive.tar (2).gz")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unique_destination_treats_dangling_symlink_as_occupied() {
        let d = tempdir().unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", d.path().join("a.txt")).unwrap();
        assert_eq!(
            unique_destination(d.path(), OsStr::new("a.txt")),
            d.path().join("a (2).txt")
        );
    }

    #[test]
    fn moves_and_records() {
        let d = tempdir().unwrap();
        let dest = d.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let a = d.path().join("a.txt");
        let b = d.path().join("b.txt");
        write(&a, "A");
        write(&b, "B");
        let out = move_items(&[a.clone(), b.clone()], &dest);
        assert!(out.failed.is_empty());
        assert!(!a.exists() && !b.exists());
        assert_eq!(read(&dest.join("a.txt")), "A");
        assert_eq!(read(&dest.join("b.txt")), "B");
        assert_eq!(out.record.dest, dest);
        assert_eq!(
            out.record.moved,
            vec![(a, dest.join("a.txt")), (b, dest.join("b.txt"))]
        );
    }

    #[test]
    fn never_overwrites() {
        let d = tempdir().unwrap();
        let dest = d.path().join("dest");
        fs::create_dir(&dest).unwrap();
        write(&dest.join("a.txt"), "old");
        let a = d.path().join("a.txt");
        write(&a, "new");
        let out = move_items(std::slice::from_ref(&a), &dest);
        assert!(out.failed.is_empty());
        assert_eq!(read(&dest.join("a.txt")), "old");
        assert_eq!(read(&dest.join("a (2).txt")), "new");
        assert_eq!(out.record.moved, vec![(a, dest.join("a (2).txt"))]);
    }

    #[test]
    fn same_folder_is_noop() {
        let d = tempdir().unwrap();
        let a = d.path().join("a.txt");
        write(&a, "A");
        let out = move_items(std::slice::from_ref(&a), d.path());
        assert!(out.record.moved.is_empty());
        assert!(out.failed.is_empty());
        assert_eq!(read(&a), "A");
        assert!(!d.path().join("a (2).txt").exists());
    }

    #[test]
    fn folder_into_itself_refused() {
        let d = tempdir().unwrap();
        let f = d.path().join("f");
        let sub = f.join("sub");
        fs::create_dir_all(&sub).unwrap();
        write(&f.join("inner.txt"), "I");
        let b = d.path().join("b.txt");
        write(&b, "B");
        let out = move_items(&[f.clone(), b.clone()], &sub);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, f);
        assert!(matches!(out.failed[0].1, FileOpError::IntoItself));
        assert_eq!(read(&sub.join("b.txt")), "B");
        assert!(!b.exists());
        assert_eq!(read(&f.join("inner.txt")), "I");
    }

    #[test]
    fn folder_into_itself_refused_via_dotdot() {
        let d = tempdir().unwrap();
        let f = d.path().join("f");
        fs::create_dir_all(f.join("sub")).unwrap();
        let sneaky = f.join("sub").join("..").join("sub");
        let out = move_items(std::slice::from_ref(&f), &sneaky);
        assert!(matches!(out.failed[0].1, FileOpError::IntoItself));
        assert!(f.join("sub").is_dir());
    }

    #[test]
    fn folder_into_itself_itself_dest_refused() {
        let d = tempdir().unwrap();
        let f = d.path().join("f");
        fs::create_dir(&f).unwrap();
        let out = move_items(std::slice::from_ref(&f), &f);
        assert!(matches!(out.failed[0].1, FileOpError::IntoItself));
        assert!(f.is_dir());
    }

    #[test]
    fn missing_destination_reports_io_not_found() {
        let d = tempdir().unwrap();
        let a = d.path().join("a.txt");
        write(&a, "A");
        let out = move_items(std::slice::from_ref(&a), &d.path().join("nope"));
        assert_eq!(out.failed.len(), 1);
        assert!(
            matches!(&out.failed[0].1, FileOpError::Io(e) if e.kind() == io::ErrorKind::NotFound)
        );
        assert_eq!(read(&a), "A");
    }

    #[test]
    fn partial_failure_keeps_successes() {
        let d = tempdir().unwrap();
        let dest = d.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let a = d.path().join("a.txt");
        let missing = d.path().join("missing.txt");
        write(&a, "A");
        let out = move_items(&[a.clone(), missing.clone()], &dest);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, missing);
        assert!(matches!(out.failed[0].1, FileOpError::NotFound));
        assert_eq!(out.record.moved.len(), 1);
        assert!(!a.exists());

        let back = undo(&out.record);
        assert!(back.failed.is_empty());
        assert_eq!(read(&a), "A");
        assert!(!dest.join("a.txt").exists());
    }

    #[test]
    fn undo_restores_or_renames() {
        let d = tempdir().unwrap();
        let dest = d.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let a = d.path().join("a.txt");
        write(&a, "orig");
        let out = move_items(std::slice::from_ref(&a), &dest);
        write(&a, "newcomer");
        let back = undo(&out.record);
        assert!(back.failed.is_empty());
        assert_eq!(read(&a), "newcomer");
        let renamed = d.path().join("a (2).txt");
        assert_eq!(read(&renamed), "orig");
        assert_eq!(back.record.moved, vec![(dest.join("a.txt"), renamed)]);
    }

    #[test]
    fn undo_of_renamed_item_restores_original_name() {
        let d = tempdir().unwrap();
        let dest = d.path().join("dest");
        fs::create_dir(&dest).unwrap();
        write(&dest.join("a.txt"), "old");
        let a = d.path().join("a.txt");
        write(&a, "new");
        let out = move_items(std::slice::from_ref(&a), &dest);
        let back = undo(&out.record);
        assert!(back.failed.is_empty());
        assert_eq!(read(&a), "new");
        assert_eq!(read(&dest.join("a.txt")), "old");
    }

    #[test]
    fn undo_checks_the_moved_item_not_its_original_name() {
        // `/b/f/x/f` moved up into `/b`, where `/b/f` already exists, lands
        // at `/b/f (2)`. Undoing it moves `/b/f (2)` into `/b/f/x`, which is
        // inside `/b/f` but not inside `/b/f (2)`: it must not be refused.
        let d = tempdir().unwrap();
        let b = d.path().join("b");
        let x = b.join("f").join("x");
        fs::create_dir_all(&x).unwrap();
        let inner = x.join("f");
        write(&inner, "inner");
        let out = move_items(std::slice::from_ref(&inner), &b);
        assert!(out.failed.is_empty());
        let renamed = b.join("f (2)");
        assert_eq!(out.record.moved, vec![(inner.clone(), renamed.clone())]);

        let back = undo(&out.record);
        assert!(back.failed.is_empty(), "undo failed: {:?}", back.failed);
        assert_eq!(read(&inner), "inner");
        assert!(!renamed.exists());
    }

    #[test]
    fn undo_reports_missing_item_as_failed() {
        let d = tempdir().unwrap();
        let record = MoveRecord {
            dest: d.path().to_path_buf(),
            moved: vec![(d.path().join("x.txt"), d.path().join("gone.txt"))],
        };
        let back = undo(&record);
        assert!(back.record.moved.is_empty());
        assert_eq!(back.failed.len(), 1);
        assert!(matches!(back.failed[0].1, FileOpError::NotFound));
    }

    fn make_tree(root: &Path) {
        fs::create_dir_all(root.join("sub")).unwrap();
        write(&root.join("one.txt"), "one");
        write(&root.join("sub").join("two.txt"), "two!");
    }

    /// Sorted names in `dir`, hidden ones included.
    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    /// A source item and an empty destination folder in one temp dir.
    fn src_and_dest(d: &Path) -> (PathBuf, PathBuf) {
        let dest = d.join("dst");
        fs::create_dir(&dest).unwrap();
        (d.join("src"), dest)
    }

    #[test]
    fn move_by_copy_verifies_then_removes() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        make_tree(&from);
        let to = move_by_copy(&from, &dest, OsStr::new("src")).unwrap();
        assert_eq!(to, dest.join("src"));
        assert!(!from.exists());
        assert_eq!(read(&to.join("one.txt")), "one");
        assert_eq!(read(&to.join("sub").join("two.txt")), "two!");
        assert_eq!(names(&dest), ["src"], "no partial copy left behind");
    }

    #[test]
    fn move_by_copy_moves_single_file() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        write(&from, "A");
        let to = move_by_copy(&from, &dest, OsStr::new("b.txt")).unwrap();
        assert_eq!(to, dest.join("b.txt"));
        assert!(!from.exists());
        assert_eq!(read(&to), "A");
        assert_eq!(names(&dest), ["b.txt"]);
    }

    #[test]
    fn move_by_copy_never_replaces_an_existing_name() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        write(&from, "new");
        write(&dest.join("src"), "old");
        let to = move_by_copy(&from, &dest, OsStr::new("src")).unwrap();
        assert_eq!(to, dest.join("src (2)"));
        assert_eq!(read(&dest.join("src")), "old");
        assert_eq!(read(&to), "new");
        assert!(!from.exists());
    }

    #[test]
    fn failed_verify_removes_only_the_partial_copy() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        make_tree(&from);
        write(&dest.join("keep.txt"), "keep");
        let err = move_by_copy_with(&from, &dest, OsStr::new("src"), |_, _| Ok(false)).unwrap_err();
        assert!(matches!(err, FileOpError::VerifyFailed));
        assert_eq!(
            names(&dest),
            ["keep.txt"],
            "partial copy removed, final name absent"
        );
        assert_eq!(read(&from.join("one.txt")), "one");
        assert_eq!(read(&from.join("sub").join("two.txt")), "two!");
    }

    #[test]
    fn existing_partial_name_is_skipped_and_kept() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        write(&from, "mine");
        let theirs = dest.join(partial_name(OsStr::new("a.txt"), 1));
        write(&theirs, "theirs");
        let to = move_by_copy(&from, &dest, OsStr::new("a.txt")).unwrap();
        assert_eq!(to, dest.join("a.txt"));
        assert_eq!(read(&to), "mine");
        assert_eq!(read(&theirs), "theirs");
        assert_eq!(names(&dest).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn failed_copy_never_touches_an_item_it_did_not_create() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        make_tree(&from);
        set_mode(&from.join("one.txt"), 0o000); // copying it fails
        let theirs = dest.join(partial_name(OsStr::new("src"), 1));
        write(&theirs, "theirs");
        let err = move_by_copy(&from, &dest, OsStr::new("src")).unwrap_err();
        set_mode(&from.join("one.txt"), 0o644);
        assert!(matches!(err, FileOpError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied));
        assert_eq!(read(&theirs), "theirs");
        assert_eq!(names(&dest).len(), 1, "only the pre-existing item remains");
        assert_eq!(read(&from.join("one.txt")), "one");
        assert_eq!(read(&from.join("sub").join("two.txt")), "two!");
    }

    #[test]
    fn partial_names() {
        let pid = std::process::id();
        assert_eq!(
            partial_name(OsStr::new("a.txt"), 1),
            OsString::from(format!(".a.txt.vento-partial-{pid}"))
        );
        assert_eq!(
            partial_name(OsStr::new("a.txt"), 2),
            OsString::from(format!(".a.txt.vento-partial-{pid}-2"))
        );
        let long = "x".repeat(250);
        let name = partial_name(OsStr::new(&long), 1);
        assert_eq!(name, OsString::from(format!(".vento-partial-{pid}")));
    }

    #[test]
    fn move_by_copy_keeps_a_very_long_name() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        write(&from, "L");
        let long = "y".repeat(250);
        let to = move_by_copy(&from, &dest, OsStr::new(&long)).unwrap();
        assert_eq!(to, dest.join(&long));
        assert_eq!(read(&to), "L");
    }

    #[test]
    fn copies_match_detects_size_and_entry_mismatch() {
        let d = tempdir().unwrap();
        let a = d.path().join("a");
        let b = d.path().join("b");
        make_tree(&a);
        make_tree(&b);
        assert!(copies_match(&a, &b).unwrap());
        write(&b.join("one.txt"), "longer content");
        assert!(!copies_match(&a, &b).unwrap());
        write(&b.join("one.txt"), "one");
        write(&b.join("extra.txt"), "");
        assert!(!copies_match(&a, &b).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn move_by_copy_preserves_symlinks() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        fs::create_dir(&from).unwrap();
        write(&d.path().join("outside.txt"), "outside");
        std::os::unix::fs::symlink(d.path().join("outside.txt"), from.join("link")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", from.join("dangling")).unwrap();
        let to = move_by_copy(&from, &dest, OsStr::new("src")).unwrap();
        assert!(!from.exists());
        let meta = fs::symlink_metadata(to.join("link")).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(to.join("link")).unwrap(),
            d.path().join("outside.txt")
        );
        assert!(
            fs::symlink_metadata(to.join("dangling"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn move_by_copy_moves_a_top_level_symlink_as_a_link() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        std::os::unix::fs::symlink("/nonexistent/target", &from).unwrap();
        let to = move_by_copy(&from, &dest, OsStr::new("src")).unwrap();
        assert!(fs::symlink_metadata(&from).is_err());
        assert_eq!(
            fs::read_link(&to).unwrap(),
            Path::new("/nonexistent/target")
        );
        assert_eq!(names(&dest), ["src"]);
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn move_by_copy_refuses_read_only_subfolder_before_copying() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        make_tree(&from);
        set_mode(&from.join("sub"), 0o555);
        let err = move_by_copy(&from, &dest, OsStr::new("src")).unwrap_err();
        set_mode(&from.join("sub"), 0o755);
        assert!(matches!(err, FileOpError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied));
        assert!(names(&dest).is_empty());
        assert_eq!(read(&from.join("sub").join("two.txt")), "two!");
        assert_eq!(read(&from.join("one.txt")), "one");
    }

    #[cfg(unix)]
    #[test]
    fn failed_source_removal_keeps_verified_copy_and_reports_it() {
        let d = tempdir().unwrap();
        let from = d.path().join("src");
        let to = d.path().join("dst");
        make_tree(&from);
        make_tree(&to); // stands in for the verified copy
        set_mode(&from.join("sub"), 0o555); // removal fails part-way
        let err = remove_source_after_copy(&from, &to).unwrap_err();
        set_mode(&from.join("sub"), 0o755);
        match err {
            FileOpError::SourceNotRemoved { copy, .. } => assert_eq!(copy, to),
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(read(&to.join("sub").join("two.txt")), "two!");
    }

    #[cfg(unix)]
    #[test]
    fn move_by_copy_refuses_special_files_before_copying() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        make_tree(&from);
        let _sock = std::os::unix::net::UnixListener::bind(from.join("sock")).unwrap();
        // Refused by the up-front walk, before any copy (a FIFO would block a copy).
        let err = ensure_removable(&from).unwrap_err();
        assert!(matches!(err, FileOpError::Io(e) if e.kind() == io::ErrorKind::Unsupported));
        let err = move_by_copy(&from, &dest, OsStr::new("src")).unwrap_err();
        assert!(matches!(err, FileOpError::Io(e) if e.kind() == io::ErrorKind::Unsupported));
        assert!(names(&dest).is_empty());
        assert_eq!(read(&from.join("one.txt")), "one");
    }

    #[test]
    fn source_not_removed_is_recorded_as_moved_and_failed() {
        let src = PathBuf::from("/x/src");
        let copy = PathBuf::from("/y/src (2)");
        let (mut moved, mut failed) = (Vec::new(), Vec::new());
        let err = FileOpError::SourceNotRemoved {
            copy: copy.clone(),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        record_step(&src, Err(err), &mut moved, &mut failed);
        assert_eq!(moved, vec![(src.clone(), copy)]);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, src);
        assert!(matches!(failed[0].1, FileOpError::SourceNotRemoved { .. }));
    }

    #[test]
    fn copy_into_copies_file_and_folder_without_touching_source() {
        let d = tempdir().unwrap();
        let (src_dir, dest) = src_and_dest(d.path());
        make_tree(&src_dir);
        let file = d.path().join("f.txt");
        write(&file, "F");

        let copied_file = copy_into(&file, &dest).unwrap();
        let copied_dir = copy_into(&src_dir, &dest).unwrap();

        assert_eq!(copied_file, dest.join("f.txt"));
        assert_eq!(read(&copied_file), "F");
        assert_eq!(copied_dir, dest.join("src"));
        assert_eq!(read(&copied_dir.join("one.txt")), "one");
        assert_eq!(read(&copied_dir.join("sub").join("two.txt")), "two!");
        assert_eq!(read(&file), "F");
        assert_eq!(read(&src_dir.join("one.txt")), "one");
        assert_eq!(read(&src_dir.join("sub").join("two.txt")), "two!");
        assert_eq!(
            names(&dest),
            ["f.txt", "src"],
            "no partial copy left behind"
        );
    }

    #[test]
    fn copy_into_never_replaces_existing_name() {
        let d = tempdir().unwrap();
        let (_, dest) = src_and_dest(d.path());
        write(&dest.join("a.txt"), "old");
        let a = d.path().join("a.txt");
        write(&a, "new");
        let copy = copy_into(&a, &dest).unwrap();
        assert_eq!(copy, dest.join("a (2).txt"));
        assert_eq!(read(&dest.join("a.txt")), "old");
        assert_eq!(read(&copy), "new");
        assert_eq!(read(&a), "new");
    }

    #[test]
    fn copy_into_same_folder_makes_a_numbered_copy() {
        let d = tempdir().unwrap();
        let a = d.path().join("a.txt");
        write(&a, "A");
        let copy = copy_into(&a, d.path()).unwrap();
        assert_eq!(copy, d.path().join("a (2).txt"));
        assert_eq!(read(&a), "A");
    }

    #[test]
    fn copy_into_refuses_a_folder_inside_itself() {
        let d = tempdir().unwrap();
        let f = d.path().join("f");
        fs::create_dir_all(f.join("sub")).unwrap();
        let err = copy_into(&f, &f.join("sub")).unwrap_err();
        assert!(matches!(err, FileOpError::IntoItself));
        assert_eq!(names(&f.join("sub")), Vec::<String>::new());
    }

    #[test]
    fn copy_into_missing_source_is_not_found() {
        let d = tempdir().unwrap();
        let err = copy_into(&d.path().join("nope"), d.path()).unwrap_err();
        assert!(matches!(err, FileOpError::NotFound));
    }

    #[cfg(unix)]
    #[test]
    fn copy_into_keeps_symlinks_as_links() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        fs::create_dir(&from).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", from.join("dangling")).unwrap();
        let copy = copy_into(&from, &dest).unwrap();
        assert_eq!(
            fs::read_link(copy.join("dangling")).unwrap(),
            Path::new("/nonexistent/target")
        );
        assert!(fs::symlink_metadata(from.join("dangling")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn copy_into_failure_removes_only_its_partial_and_keeps_source() {
        let d = tempdir().unwrap();
        let (from, dest) = src_and_dest(d.path());
        make_tree(&from);
        write(&dest.join("keep.txt"), "keep");
        set_mode(&from.join("one.txt"), 0o000); // copying it fails
        let err = copy_into(&from, &dest).unwrap_err();
        set_mode(&from.join("one.txt"), 0o644);
        assert!(matches!(err, FileOpError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied));
        assert_eq!(names(&dest), ["keep.txt"]);
        assert_eq!(read(&from.join("one.txt")), "one");
    }

    /// Paths handed to the trash closure by `undo_copies`, in call order.
    fn undo_into_log(record: &CopyRecord) -> (CopyUndoOutcome, Vec<PathBuf>) {
        let log = std::cell::RefCell::new(Vec::new());
        let out = undo_copies(record, |p: &Path| -> Result<(), String> {
            log.borrow_mut().push(p.to_path_buf());
            Ok(())
        });
        (out, log.into_inner())
    }

    /// One file and one folder copied into a fresh `dst`.
    fn copied_file_and_folder(d: &Path) -> (PathBuf, PathBuf, PathBuf, CopyOutcome) {
        let (src_dir, dest) = src_and_dest(d);
        make_tree(&src_dir);
        let file = d.join("f.txt");
        write(&file, "F");
        let out = copy_items(&[file.clone(), src_dir.clone()], &dest);
        (file, src_dir, dest, out)
    }

    #[test]
    fn copy_items_records_fingerprints() {
        let d = tempdir().unwrap();
        let (file, src_dir, dest, out) = copied_file_and_folder(d.path());

        assert!(out.failed.is_empty());
        assert_eq!(out.record.dest, dest);
        assert_eq!(out.record.copies.len(), 2);
        let (s0, c0, f0) = &out.record.copies[0];
        assert_eq!((s0, c0), (&file, &dest.join("f.txt")));
        assert_eq!(f0, &Fingerprint::of(c0).unwrap());
        assert!(matches!(f0, Fingerprint::File { len: 1, .. }));
        let (s1, c1, f1) = &out.record.copies[1];
        assert_eq!((s1, c1), (&src_dir, &dest.join("src")));
        assert_eq!(f1, &Fingerprint::of(c1).unwrap());
        let Fingerprint::Dir { entries, .. } = f1 else {
            panic!("a folder copy gets a Dir fingerprint");
        };
        let rel: Vec<&Path> = entries.iter().map(|e| e.path.as_path()).collect();
        assert_eq!(
            rel,
            [
                Path::new("one.txt"),
                Path::new("sub"),
                Path::new("sub").join("two.txt").as_path()
            ]
        );
    }

    #[test]
    fn copy_items_reports_a_failed_item_and_keeps_the_rest() {
        let d = tempdir().unwrap();
        let (_, dest) = src_and_dest(d.path());
        let ok = d.path().join("ok.txt");
        write(&ok, "ok");
        let missing = d.path().join("missing.txt");
        let out = copy_items(&[missing.clone(), ok], &dest);
        assert_eq!(out.record.copies.len(), 1);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, missing);
        assert!(matches!(out.failed[0].1, FileOpError::NotFound));
    }

    #[test]
    fn undo_trashes_unchanged_copies() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.trashed, 2);
        assert!(undone.kept_changed.is_empty() && undone.failed.is_empty());
        assert_eq!(undone.gone, 0);
        assert_eq!(trashed, [dest.join("f.txt"), dest.join("src")]);
    }

    #[test]
    fn undo_keeps_an_edited_copy() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());
        let copy = dest.join("f.txt");
        let mut f = fs::OpenOptions::new().append(true).open(&copy).unwrap();
        io::Write::write_all(&mut f, b"more").unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        drop(f);

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, std::slice::from_ref(&copy));
        assert_eq!(undone.trashed, 1);
        assert_eq!(trashed, [dest.join("src")]);
        assert_eq!(read(&copy), "Fmore");
    }

    #[test]
    fn undo_keeps_a_same_size_edit_with_a_later_mtime() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());
        let copy = dest.join("f.txt");
        write(&copy, "G");
        let f = fs::OpenOptions::new().write(true).open(&copy).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        drop(f);

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, [copy]);
        assert_eq!(trashed, [dest.join("src")]);
    }

    #[test]
    fn undo_keeps_a_folder_with_a_new_file() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());
        let copy = dest.join("src");
        write(&copy.join("sub").join("new.txt"), "n");

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, std::slice::from_ref(&copy));
        assert_eq!(trashed, [dest.join("f.txt")]);
        assert_eq!(read(&copy.join("sub").join("new.txt")), "n");
    }

    #[test]
    fn undo_keeps_a_folder_with_a_removed_file() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());
        fs::remove_file(dest.join("src").join("one.txt")).unwrap();

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, [dest.join("src")]);
        assert_eq!(trashed, [dest.join("f.txt")]);
    }

    #[test]
    fn undo_counts_a_removed_copy_as_gone() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());
        fs::remove_file(dest.join("f.txt")).unwrap();

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.gone, 1);
        assert_eq!(undone.trashed, 1);
        assert!(undone.kept_changed.is_empty() && undone.failed.is_empty());
        assert_eq!(trashed, [dest.join("src")]);
    }

    #[test]
    fn undo_never_touches_sources() {
        let d = tempdir().unwrap();
        let (file, src_dir, _, out) = copied_file_and_folder(d.path());
        let before = (
            Fingerprint::of(&file).unwrap(),
            Fingerprint::of(&src_dir).unwrap(),
        );

        let (_, trashed) = undo_into_log(&out.record);

        assert!(trashed.iter().all(|p| p != &file && p != &src_dir));
        assert_eq!(Fingerprint::of(&file).unwrap(), before.0);
        assert_eq!(Fingerprint::of(&src_dir).unwrap(), before.1);
        assert_eq!(read(&file), "F");
    }

    #[test]
    fn undo_reports_a_trash_failure_and_continues() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());

        let undone = undo_copies(&out.record, |p: &Path| -> Result<(), String> {
            if p.ends_with("f.txt") {
                Err("no trash".to_string())
            } else {
                Ok(())
            }
        });

        assert_eq!(undone.trashed, 1);
        assert_eq!(
            undone.failed,
            [(dest.join("f.txt"), "no trash".to_string())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn fingerprint_of_a_symlink_is_its_target_and_is_not_followed() {
        let d = tempdir().unwrap();
        let link = d.path().join("l");
        std::os::unix::fs::symlink("/nonexistent/target", &link).unwrap();
        assert_eq!(
            Fingerprint::of(&link).unwrap(),
            Fingerprint::Link {
                target: PathBuf::from("/nonexistent/target"),
                id: file_id(&fs::symlink_metadata(&link).unwrap()),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn undo_keeps_a_copy_it_cannot_fingerprint() {
        let d = tempdir().unwrap();
        let (_, _, dest, out) = copied_file_and_folder(d.path());
        set_mode(&dest.join("src").join("sub"), 0o000); // cannot be walked
        let walk_fails = Fingerprint::of(&dest.join("src")).is_err();

        let (undone, trashed) = undo_into_log(&out.record);
        set_mode(&dest.join("src").join("sub"), 0o755);
        assert!(walk_fails, "the walk must really fail for this test");

        assert_eq!(undone.kept_changed, [dest.join("src")]);
        assert_eq!(trashed, [dest.join("f.txt")]);
        assert_eq!(read(&dest.join("src").join("sub").join("two.txt")), "two!");
    }

    #[cfg(unix)]
    #[test]
    fn undo_keeps_a_source_moved_into_the_copys_place() {
        let d = tempdir().unwrap();
        let (_, dest) = src_and_dest(d.path());
        let file = d.path().join("report.pdf");
        write(&file, "R");
        let out = copy_items(std::slice::from_ref(&file), &dest);
        let copy = dest.join("report.pdf");
        assert_eq!(out.record.copies[0].1, copy);
        // The copy shares its source's size and mtime; only its identity differs.
        assert_eq!(fs::metadata(&copy).unwrap().len(), 1);
        fs::remove_file(&copy).unwrap();
        fs::rename(&file, &copy).unwrap();

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, std::slice::from_ref(&copy));
        assert_eq!(undone.trashed, 0);
        assert!(trashed.is_empty(), "trash must not be called");
        assert_eq!(read(&copy), "R");
    }

    #[cfg(unix)]
    #[test]
    fn undo_keeps_a_source_folder_moved_into_the_copys_place() {
        let d = tempdir().unwrap();
        let (src_dir, dest) = src_and_dest(d.path());
        fs::create_dir(&src_dir).unwrap();
        write(&src_dir.join("one.txt"), "one"); // flat: no subfolder mtimes to differ
        let out = copy_items(std::slice::from_ref(&src_dir), &dest);
        let copy = dest.join("src");
        fs::remove_dir_all(&copy).unwrap();
        fs::rename(&src_dir, &copy).unwrap();

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, std::slice::from_ref(&copy));
        assert!(trashed.is_empty(), "trash must not be called");
        assert_eq!(read(&copy.join("one.txt")), "one");
    }

    #[cfg(unix)]
    #[test]
    fn undo_keeps_a_symlink_moved_into_the_copys_place() {
        let d = tempdir().unwrap();
        let (_, dest) = src_and_dest(d.path());
        let link = d.path().join("l");
        std::os::unix::fs::symlink("/nonexistent/target", &link).unwrap();
        let out = copy_items(std::slice::from_ref(&link), &dest);
        let copy = dest.join("l");
        fs::remove_file(&copy).unwrap();
        fs::rename(&link, &copy).unwrap();

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, [copy]);
        assert!(trashed.is_empty(), "trash must not be called");
    }

    #[cfg(unix)]
    #[test]
    fn undo_keeps_a_folder_whose_inner_link_was_retargeted() {
        let d = tempdir().unwrap();
        let (src_dir, dest) = src_and_dest(d.path());
        fs::create_dir(&src_dir).unwrap();
        std::os::unix::fs::symlink("/one", src_dir.join("l")).unwrap();
        let out = copy_items(std::slice::from_ref(&src_dir), &dest);
        let inner = dest.join("src").join("l");
        fs::remove_file(&inner).unwrap();
        std::os::unix::fs::symlink("/two", &inner).unwrap();

        let (undone, trashed) = undo_into_log(&out.record);

        assert_eq!(undone.kept_changed, [dest.join("src")]);
        assert!(trashed.is_empty());
    }
}
