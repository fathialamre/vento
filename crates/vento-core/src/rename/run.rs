//! The rename itself: a re-check against the disk, then every item to a temporary
//! name and on to its final name, with every step reversed on any failure.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fileops::{FileId, file_id};

use super::name::{Os, name_key};
use super::preview::{Batch, Preview};

/// Prefix of the temporary names used between the two phases.
const TMP_PREFIX: &str = ".vento-rename-";

/// Fresh tokens tried when a temporary name is already taken.
const TOKEN_TRIES: u32 = 10;

/// The file system as the rename sees it; faked in tests.
pub trait Disk: Sync {
    /// Whether anything is at `p`; a symlink is not followed.
    fn exists(&self, p: &Path) -> bool;
    /// Renames `from` to `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// The UTF-8 names in `dir`; other names are skipped.
    fn names_in(&self, dir: &Path) -> io::Result<Vec<String>>;
    /// Which item is at `p`, without following a symlink.
    fn print(&self, p: &Path) -> io::Result<ItemPrint>;
}

/// The real disk, through `std::fs`.
///
/// On macOS and Linux a rename fails with [`io::ErrorKind::AlreadyExists`] instead
/// of replacing an item at the target; elsewhere [`rename_new`]'s check is the guard.
pub struct RealDisk;

impl Disk for RealDisk {
    fn exists(&self, p: &Path) -> bool {
        fs::symlink_metadata(p).is_ok()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            rename_excl_mac(from, to)
        }
        #[cfg(target_os = "linux")]
        {
            rename_noreplace_linux(from, to)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            fs::rename(from, to)
        }
    }

    fn names_in(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(dir)? {
            if let Ok(name) = entry?.file_name().into_string() {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn print(&self, p: &Path) -> io::Result<ItemPrint> {
        let meta = fs::symlink_metadata(p)?;
        let is_dir = meta.is_dir();
        Ok(ItemPrint {
            id: file_id(&meta),
            len: if is_dir { 0 } else { meta.len() },
            is_dir,
        })
    }
}

/// `p` as a C string; a path holding a NUL byte can't be one.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn c_path(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// Renames without replacing: `EEXIST` when something is at `to`. A file system
/// that can't refuse (`ENOTSUP`) gets a plain rename, guarded by [`rename_new`].
#[cfg(target_os = "macos")]
fn rename_excl_mac(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (c_path(from)?, c_path(to)?);
    // SAFETY: both pointers are NUL-terminated strings that live until the call returns.
    if unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) } == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOTSUP) {
        return fs::rename(from, to);
    }
    Err(err)
}

/// Renames without replacing: `EEXIST` when something is at `to`. A file system
/// or kernel that can't refuse (`EINVAL`, `ENOSYS`) gets a plain rename, guarded
/// by [`rename_new`].
#[cfg(target_os = "linux")]
fn rename_noreplace_linux(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (c_path(from)?, c_path(to)?);
    // SAFETY: both pointers are NUL-terminated strings that live until the call
    // returns; `AT_FDCWD` makes them relative to the working folder like `fs::rename`.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
        return fs::rename(from, to);
    }
    Err(err)
}

/// What an item looked like right after it was renamed, so Undo can tell it is the same one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemPrint {
    /// Which file this is.
    pub id: FileId,
    /// Size in bytes; 0 for folders.
    pub len: u64,
    /// Whether it is a real folder.
    pub is_dir: bool,
}

/// One item's rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Where the item is now.
    pub from: PathBuf,
    /// Where it goes, in the same folder.
    pub to: PathBuf,
}

/// The steps for every changing row; empty unless the preview allows Rename.
pub fn steps(batch: &Batch, preview: &Preview) -> Vec<Step> {
    if !preview.can_rename() {
        return Vec::new();
    }
    batch
        .items
        .iter()
        .zip(&preview.rows)
        .filter_map(|(item, row)| {
            let new = row.new_name.as_deref()?;
            Some(Step {
                from: item.path.clone(),
                to: item.path.with_file_name(new),
            })
        })
        .collect()
}

/// What a finished rename did, for Undo.
#[derive(Debug, Clone)]
pub struct RenameRecord {
    /// The new path of every renamed item, in step order, whether or not it
    /// can be undone.
    pub renamed: Vec<PathBuf>,
    /// `(old path, new path, print taken right after)` per renamed item that
    /// Undo can put back: an item whose print could not be read is left out.
    pub items: Vec<(PathBuf, PathBuf, ItemPrint)>,
}

/// How a rename ended.
#[derive(Debug, Clone)]
pub enum RenameOutcome {
    /// Every item has its new name.
    Done(RenameRecord),
    /// The disk no longer matches the preview; nothing was touched.
    Changed,
    /// An item could not be renamed; every step done was reversed, except as `stuck` says.
    Failed {
        /// The failing item's old path.
        path: PathBuf,
        /// Why it failed.
        reason: String,
        /// `(where it is now, old path)` per item the reversal could not put back;
        /// empty normally. "Where it is now" is a temporary name except for an item
        /// whose final rename could not be reversed.
        stuck: Vec<(PathBuf, PathBuf)>,
    },
}

/// Runs `steps` in two phases, reversing everything done on any failure.
///
/// `done` goes up once per finished rename of either phase, for the footer's progress.
pub fn execute(steps: &[Step], disk: &dyn Disk, os: Os, done: &AtomicUsize) -> RenameOutcome {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    execute_with(steps, disk, os, done, nanos ^ std::process::id())
}

/// [`execute`] with the first temporary-name token given.
fn execute_with(
    steps: &[Step],
    disk: &dyn Disk,
    os: Os,
    done: &AtomicUsize,
    seed: u32,
) -> RenameOutcome {
    if !still_matches(steps, disk, os) {
        return RenameOutcome::Changed;
    }
    let Some(tmps) = temp_names(steps, disk, seed) else {
        log::warn!("rename: no free temporary name");
        return RenameOutcome::Failed {
            path: steps[0].from.clone(),
            reason: "no free temporary name".into(),
            stuck: Vec::new(),
        };
    };

    // Where each item is, for the items phase one has moved.
    let mut at: Vec<PathBuf> = Vec::with_capacity(steps.len());
    let moves = steps
        .iter()
        .zip(&tmps)
        .map(|(s, tmp)| (&s.from, tmp))
        .chain(steps.iter().zip(&tmps).map(|(s, tmp)| (tmp, &s.to)));
    for (i, (from, to)) in moves.enumerate() {
        let ix = i % steps.len();
        if let Err(e) = rename_new(disk, from, to) {
            let failed = &steps[ix].from;
            log::warn!("rename: {} -> {}: {e}", from.display(), to.display());
            return RenameOutcome::Failed {
                path: failed.clone(),
                reason: crate::i18n::errors::io_reason(&e),
                stuck: roll_back(steps, &tmps, at, disk),
            };
        }
        if i < steps.len() {
            at.push(to.clone());
        } else {
            at[ix] = to.clone();
        }
        done.fetch_add(1, Ordering::Relaxed);
    }

    let items = steps
        .iter()
        .filter_map(|s| match disk.print(&s.to) {
            Ok(print) => Some((s.from.clone(), s.to.clone(), print)),
            Err(e) => {
                // The rename stands; this item just cannot be undone.
                log::warn!("rename: can't read {} for undo: {e}", s.to.display());
                None
            }
        })
        .collect();
    let renamed = steps.iter().map(|s| s.to.clone()).collect();
    RenameOutcome::Done(RenameRecord { renamed, items })
}

/// Whether the disk still looks as the preview saw it: every item is there, and
/// every target is free or held by an item that is being renamed away.
fn still_matches(steps: &[Step], disk: &dyn Disk, os: Os) -> bool {
    let key = |p: &Path| p.file_name().map(|n| name_key(&n.to_string_lossy(), os));
    let leaving: HashSet<(&Path, String)> = steps
        .iter()
        .filter_map(|s| Some((s.from.parent()?, key(&s.from)?)))
        .collect();
    steps.iter().all(|s| {
        if !disk.exists(&s.from) {
            log::warn!("rename: {} is gone", s.from.display());
            return false;
        }
        let free = !disk.exists(&s.to)
            || s.to
                .parent()
                .zip(key(&s.to))
                .is_some_and(|(dir, k)| leaving.contains(&(dir, k)));
        if !free {
            log::warn!("rename: {} appeared", s.to.display());
        }
        free
    })
}

/// A free temporary name per step, `.vento-rename-<token>-<index>` in the item's
/// own folder; the token goes up when one of its names is taken.
fn temp_names(steps: &[Step], disk: &dyn Disk, seed: u32) -> Option<Vec<PathBuf>> {
    (0..TOKEN_TRIES).find_map(|n| {
        let t = token(seed.wrapping_add(n));
        let tmps: Vec<PathBuf> = steps
            .iter()
            .enumerate()
            .map(|(i, s)| s.from.with_file_name(format!("{TMP_PREFIX}{t}-{i}")))
            .collect();
        tmps.iter().all(|p| !disk.exists(p)).then_some(tmps)
    })
}

/// Eight lowercase hex digits.
fn token(n: u32) -> String {
    format!("{n:08x}")
}

/// Renames `from` to `to` unless something is already at `to`.
fn rename_new(disk: &dyn Disk, from: &Path, to: &Path) -> io::Result<()> {
    if disk.exists(to) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "an item with this name already exists",
        ));
    }
    disk.rename(from, to)
}

/// Puts every moved item back, newest step first; `at[i]` is where item `i` is.
///
/// Stops at the first reversal that fails and returns `(where it is now, old path)`
/// for every item not back under its old name, in list order.
fn roll_back(
    steps: &[Step],
    tmps: &[PathBuf],
    mut at: Vec<PathBuf>,
    disk: &dyn Disk,
) -> Vec<(PathBuf, PathBuf)> {
    // Final names back to temporary ones, then temporary names back to the old ones.
    let finals = (0..at.len()).rev().filter(|&i| at[i] != tmps[i]);
    let back: Vec<(usize, PathBuf)> = finals
        .map(|i| (i, tmps[i].clone()))
        .chain((0..at.len()).rev().map(|i| (i, steps[i].from.clone())))
        .collect();
    for (i, to) in back {
        if let Err(e) = rename_new(disk, &at[i], &to) {
            log::error!(
                "rename: can't put back {} (now {}): {e}",
                steps[i].from.display(),
                at[i].display()
            );
            return at
                .into_iter()
                .zip(steps)
                .filter(|(now, s)| *now != s.from)
                .map(|(now, s)| (now, s.from.clone()))
                .collect();
        }
        at[i] = to;
    }
    Vec::new()
}

/// How an Undo ended.
#[derive(Debug, Clone, Default)]
pub struct UndoOutcome {
    /// Items back under their old names.
    pub put_back: usize,
    /// Items left as they are because they changed since the rename.
    pub skipped: usize,
    /// `(path, reason)` when the put-back failed and was reversed.
    pub failed: Option<(PathBuf, String)>,
    /// As in [`RenameOutcome::Failed`]: items the reversal could not put back.
    pub stuck: Vec<(PathBuf, PathBuf)>,
}

/// Puts back the names of a finished rename, with the same two phases and rollback.
///
/// An item is skipped when its new path is gone, it is no longer the same item
/// (its print differs), or its old name is held by something that is not
/// itself being put back.
pub fn undo(record: &RenameRecord, disk: &dyn Disk, os: Os) -> UndoOutcome {
    let key = |p: &Path| {
        p.parent()
            .zip(p.file_name())
            .map(|(dir, n)| (dir.to_path_buf(), name_key(&n.to_string_lossy(), os)))
    };
    let mut keep: Vec<&(PathBuf, PathBuf, ItemPrint)> = record
        .items
        .iter()
        .filter(|(_, new, print)| disk.exists(new) && disk.print(new).ok().as_ref() == Some(print))
        .collect();
    // An old name held by an item being put back is freed by the undo itself;
    // dropping one item can take another's old name, so repeat until settled.
    loop {
        let leaving: HashSet<_> = keep.iter().filter_map(|(_, new, _)| key(new)).collect();
        let before = keep.len();
        keep.retain(|(old, _, _)| {
            !disk.exists(old) || key(old).is_some_and(|k| leaving.contains(&k))
        });
        if keep.len() == before {
            break;
        }
    }

    let skipped = record.items.len() - keep.len();
    if keep.is_empty() {
        return UndoOutcome {
            skipped,
            ..UndoOutcome::default()
        };
    }
    let steps: Vec<Step> = keep
        .iter()
        .map(|(old, new, _)| Step {
            from: new.clone(),
            to: old.clone(),
        })
        .collect();
    let done = AtomicUsize::new(0);
    match execute(&steps, disk, os, &done) {
        RenameOutcome::Done(_) => UndoOutcome {
            put_back: steps.len(),
            skipped,
            ..UndoOutcome::default()
        },
        RenameOutcome::Changed => UndoOutcome {
            skipped,
            failed: Some((steps[0].from.clone(), "something changed".into())),
            ..UndoOutcome::default()
        },
        RenameOutcome::Failed {
            path,
            reason,
            stuck,
        } => UndoOutcome {
            put_back: 0,
            skipped,
            failed: Some((path, reason)),
            stuck,
        },
    }
}

/// A file system in memory that fails on demand, for this module's and Undo's tests.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    /// One entry of the fake disk; `id` moves with it on rename.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FakeEntry {
        /// Which item this is.
        pub id: u64,
        /// Size in bytes.
        pub len: u64,
        /// Whether it is a folder.
        pub is_dir: bool,
    }

    /// Paths in memory, compared exactly or ignoring case. Like a real rename,
    /// `rename` replaces whatever is at the target, so a test can catch an overwrite.
    #[derive(Default)]
    pub struct FakeDisk {
        entries: Mutex<BTreeMap<PathBuf, FakeEntry>>,
        insensitive: bool,
        next_id: Mutex<u64>,
        calls: Mutex<usize>,
        /// Rename calls (1-based, counted over the disk's life) that fail.
        fail_calls: Mutex<BTreeSet<usize>>,
        /// Renames whose target is one of these fail.
        fail_to: Mutex<BTreeSet<PathBuf>>,
        /// After this rename call, this path appears (as another program would make it).
        appear: Mutex<Option<(usize, PathBuf)>>,
    }

    impl FakeDisk {
        /// A disk holding `paths` as files, compared exactly.
        pub fn new(paths: &[&str]) -> Self {
            let disk = FakeDisk::default();
            for p in paths {
                disk.add(p);
            }
            disk
        }

        /// A disk holding `paths` as files, ignoring case like macOS and Windows.
        pub fn insensitive(paths: &[&str]) -> Self {
            let disk = FakeDisk {
                insensitive: true,
                ..FakeDisk::default()
            };
            for p in paths {
                disk.add(p);
            }
            disk
        }

        /// Adds a file at `p`, whose length is its path's length.
        pub fn add(&self, p: &str) {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            let entry = FakeEntry {
                id: *next,
                len: p.len() as u64,
                is_dir: false,
            };
            self.entries.lock().unwrap().insert(PathBuf::from(p), entry);
        }

        /// Removes whatever is at `p`.
        pub fn remove(&self, p: &str) {
            let key = self.find(Path::new(p));
            if let Some(k) = key {
                self.entries.lock().unwrap().remove(&k);
            }
        }

        /// Every path, spelled as stored.
        pub fn paths(&self) -> BTreeSet<PathBuf> {
            self.entries.lock().unwrap().keys().cloned().collect()
        }

        /// The entry at `p`, if any.
        pub fn entry(&self, p: &str) -> Option<FakeEntry> {
            let key = self.find(Path::new(p))?;
            self.entries.lock().unwrap().get(&key).copied()
        }

        /// Makes rename call number `n` (1-based) fail.
        pub fn fail_call(&self, n: usize) {
            self.fail_calls.lock().unwrap().insert(n);
        }

        /// Makes every rename onto `p` fail.
        pub fn fail_to(&self, p: &str) {
            self.fail_to.lock().unwrap().insert(PathBuf::from(p));
        }

        /// Makes a file appear at `p` right after rename call number `n`.
        pub fn appear_after(&self, n: usize, p: &str) {
            *self.appear.lock().unwrap() = Some((n, PathBuf::from(p)));
        }

        /// Rename calls so far.
        pub fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }

        fn key(&self, p: &Path) -> String {
            let s = p.to_string_lossy();
            if self.insensitive {
                s.to_lowercase()
            } else {
                s.into_owned()
            }
        }

        /// The stored spelling of `p`.
        fn find(&self, p: &Path) -> Option<PathBuf> {
            let key = self.key(p);
            let entries = self.entries.lock().unwrap();
            entries.keys().find(|k| self.key(k) == key).cloned()
        }
    }

    fn fake_id(n: u64) -> FileId {
        #[cfg(unix)]
        {
            (0, n)
        }
        #[cfg(not(unix))]
        {
            let _ = n;
        }
    }

    impl Disk for FakeDisk {
        fn exists(&self, p: &Path) -> bool {
            self.find(p).is_some()
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            let n = {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            if self.fail_calls.lock().unwrap().contains(&n)
                || self.fail_to.lock().unwrap().contains(to)
            {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let src = self.find(from).ok_or(io::ErrorKind::NotFound)?;
            let dst = self.find(to);
            {
                let mut entries = self.entries.lock().unwrap();
                let entry = entries.remove(&src).unwrap();
                if let Some(dst) = dst {
                    entries.remove(&dst);
                }
                entries.insert(to.to_path_buf(), entry);
            }
            let appear = self.appear.lock().unwrap().clone();
            if let Some((after, p)) = appear
                && after == n
            {
                self.add(p.to_str().unwrap());
            }
            Ok(())
        }

        fn names_in(&self, dir: &Path) -> io::Result<Vec<String>> {
            let key = self.key(dir);
            let entries = self.entries.lock().unwrap();
            let names: Vec<String> = entries
                .keys()
                .filter(|p| p.parent().is_some_and(|d| self.key(d) == key))
                .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
                .collect();
            // A folder with nothing in it and no entry of its own is not there.
            if names.is_empty() && !entries.keys().any(|p| self.key(p) == key) {
                return Err(io::ErrorKind::NotFound.into());
            }
            Ok(names)
        }

        fn print(&self, p: &Path) -> io::Result<ItemPrint> {
            let key = self.find(p).ok_or(io::ErrorKind::NotFound)?;
            let e = self.entries.lock().unwrap()[&key];
            Ok(ItemPrint {
                id: fake_id(e.id),
                len: if e.is_dir { 0 } else { e.len },
                is_dir: e.is_dir,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeDisk;
    use super::*;
    use std::collections::BTreeSet;

    fn step(from: &str, to: &str) -> Step {
        Step {
            from: from.into(),
            to: to.into(),
        }
    }

    fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn no_temp_left(disk: &FakeDisk) -> bool {
        !disk
            .paths()
            .iter()
            .any(|p| p.to_string_lossy().contains(TMP_PREFIX))
    }

    fn run(steps: &[Step], disk: &dyn Disk, os: Os) -> (RenameOutcome, usize) {
        let done = AtomicUsize::new(0);
        let out = execute(steps, disk, os, &done);
        (out, done.load(Ordering::Relaxed))
    }

    #[test]
    fn swap_two_names() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "was a").unwrap();
        fs::write(&b, "was b").unwrap();
        let steps = [
            Step {
                from: a.clone(),
                to: b.clone(),
            },
            Step {
                from: b.clone(),
                to: a.clone(),
            },
        ];
        let (out, done) = run(&steps, &RealDisk, Os::HOST);
        assert!(matches!(out, RenameOutcome::Done(_)), "{out:?}");
        assert_eq!(done, 4);
        assert_eq!(fs::read_to_string(&a).unwrap(), "was b");
        assert_eq!(fs::read_to_string(&b).unwrap(), "was a");
        let mut left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["a", "b"]);
    }

    #[test]
    fn cycle_of_three() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in ["a", "b", "c"] {
            fs::write(p(n), n).unwrap();
        }
        let steps = [
            Step {
                from: p("a"),
                to: p("b"),
            },
            Step {
                from: p("b"),
                to: p("c"),
            },
            Step {
                from: p("c"),
                to: p("a"),
            },
        ];
        let (out, _) = run(&steps, &RealDisk, Os::HOST);
        assert!(matches!(out, RenameOutcome::Done(_)), "{out:?}");
        assert_eq!(fs::read_to_string(p("b")).unwrap(), "a");
        assert_eq!(fs::read_to_string(p("c")).unwrap(), "b");
        assert_eq!(fs::read_to_string(p("a")).unwrap(), "c");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);

        // The same on the fake disk, where the items' ids show who went where.
        let disk = FakeDisk::new(&["/t/a", "/t/b", "/t/c"]);
        let ids: Vec<_> = ["/t/a", "/t/b", "/t/c"]
            .iter()
            .map(|p| disk.entry(p).unwrap().id)
            .collect();
        let steps = [
            step("/t/a", "/t/b"),
            step("/t/b", "/t/c"),
            step("/t/c", "/t/a"),
        ];
        let (out, done) = run(&steps, &disk, Os::Linux);
        assert!(matches!(out, RenameOutcome::Done(_)), "{out:?}");
        assert_eq!(done, 6);
        assert_eq!(disk.entry("/t/b").unwrap().id, ids[0]);
        assert_eq!(disk.entry("/t/c").unwrap().id, ids[1]);
        assert_eq!(disk.entry("/t/a").unwrap().id, ids[2]);
        assert_eq!(disk.paths(), set(&["/t/a", "/t/b", "/t/c"]));
    }

    #[test]
    fn case_only_rename_on_insensitive_disk() {
        let disk = FakeDisk::insensitive(&["/t/A.JPG"]);
        let (out, _) = run(&[step("/t/A.JPG", "/t/A.jpg")], &disk, Os::Mac);
        assert!(matches!(out, RenameOutcome::Done(_)), "{out:?}");
        assert_eq!(disk.paths(), set(&["/t/A.jpg"]));
    }

    #[test]
    fn phase_two_failure_rolls_everything_back() {
        let before = ["/t/a", "/t/b", "/t/c"];
        let disk = FakeDisk::new(&before);
        disk.fail_to("/t/y");
        let steps = [
            step("/t/a", "/t/x"),
            step("/t/b", "/t/y"),
            step("/t/c", "/t/z"),
        ];
        let (out, _) = run(&steps, &disk, Os::Linux);
        match out {
            RenameOutcome::Failed {
                path,
                reason,
                stuck,
            } => {
                assert_eq!(path, PathBuf::from("/t/b"));
                assert!(!reason.is_empty());
                assert!(stuck.is_empty(), "{stuck:?}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(disk.paths(), set(&before));
        assert!(no_temp_left(&disk));
    }

    #[test]
    fn phase_one_failure_rolls_back() {
        let before = ["/t/a", "/t/b", "/t/c", "/t/d"];
        let disk = FakeDisk::new(&before);
        disk.fail_call(3);
        let steps = [
            step("/t/a", "/t/w"),
            step("/t/b", "/t/x"),
            step("/t/c", "/t/y"),
            step("/t/d", "/t/z"),
        ];
        let (out, done) = run(&steps, &disk, Os::Linux);
        match out {
            RenameOutcome::Failed { path, stuck, .. } => {
                assert_eq!(path, PathBuf::from("/t/c"));
                assert!(stuck.is_empty());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(done, 2);
        assert_eq!(disk.paths(), set(&before));
        // Two renames out, the failed one, two back.
        assert_eq!(disk.calls(), 5);
    }

    #[test]
    fn failed_rollback_reports_stuck_items() {
        let disk = FakeDisk::new(&["/t/a", "/t/b"]);
        // Calls: 1–2 phase one, 3 a's final rename, 4 b's final rename (fails),
        // 5 a back to its temporary name, 6 b back to "b", 7 a back to "a" (fails).
        disk.fail_call(4);
        disk.fail_call(7);
        let steps = [step("/t/a", "/t/x"), step("/t/b", "/t/y")];
        let (out, _) = run(&steps, &disk, Os::Linux);
        let RenameOutcome::Failed { path, stuck, .. } = out else {
            panic!("{out:?}");
        };
        assert_eq!(path, PathBuf::from("/t/b"));
        assert_eq!(stuck.len(), 1);
        let (tmp, old) = &stuck[0];
        assert_eq!(old, &PathBuf::from("/t/a"));
        assert!(tmp.to_string_lossy().starts_with("/t/.vento-rename-"));
        assert!(tmp.to_string_lossy().ends_with("-0"));
        assert_eq!(disk.paths(), [tmp.clone(), "/t/b".into()].into());
    }

    #[test]
    fn stuck_includes_an_item_left_at_its_final_name() {
        let disk = FakeDisk::new(&["/t/a", "/t/b"]);
        // 4: b's final rename fails; 5: a back to its temporary name fails.
        disk.fail_call(4);
        disk.fail_call(5);
        let steps = [step("/t/a", "/t/x"), step("/t/b", "/t/y")];
        let (out, _) = run(&steps, &disk, Os::Linux);
        let RenameOutcome::Failed { stuck, .. } = out else {
            panic!("{out:?}");
        };
        assert_eq!(stuck.len(), 2);
        assert_eq!(stuck[0], ("/t/x".into(), "/t/a".into()));
        assert_eq!(stuck[1].1, PathBuf::from("/t/b"));
        assert!(stuck[1].0.to_string_lossy().contains(TMP_PREFIX));
        assert_eq!(disk.calls(), 5);
    }

    #[test]
    fn vanished_item_is_changed() {
        let disk = FakeDisk::new(&["/t/a", "/t/b"]);
        disk.remove("/t/a");
        let steps = [step("/t/a", "/t/x"), step("/t/b", "/t/y")];
        let (out, done) = run(&steps, &disk, Os::Linux);
        assert!(matches!(out, RenameOutcome::Changed), "{out:?}");
        assert_eq!(done, 0);
        assert_eq!(disk.calls(), 0);
        assert_eq!(disk.paths(), set(&["/t/b"]));
    }

    #[test]
    fn new_file_at_target_is_changed() {
        let disk = FakeDisk::new(&["/t/a"]);
        disk.add("/t/b");
        let (out, _) = run(&[step("/t/a", "/t/b")], &disk, Os::Linux);
        assert!(matches!(out, RenameOutcome::Changed), "{out:?}");
        assert_eq!(disk.calls(), 0);
        // A target held by an item being renamed away is fine, in any case on macOS.
        let disk = FakeDisk::insensitive(&["/t/a", "/t/B"]);
        let steps = [step("/t/a", "/t/b"), step("/t/B", "/t/c")];
        let (out, _) = run(&steps, &disk, Os::Mac);
        assert!(matches!(out, RenameOutcome::Done(_)), "{out:?}");
        assert_eq!(disk.paths(), set(&["/t/b", "/t/c"]));
        // On Linux "b" is a different file from "B", so it is a new file at the target.
        let disk = FakeDisk::new(&["/t/a", "/t/B", "/t/b"]);
        let (out, _) = run(&steps, &disk, Os::Linux);
        assert!(matches!(out, RenameOutcome::Changed), "{out:?}");
    }

    /// The real disk, with a file appearing right after one rename call.
    struct Racing {
        after: usize,
        appear: PathBuf,
        calls: AtomicUsize,
    }

    impl Disk for Racing {
        fn exists(&self, p: &Path) -> bool {
            RealDisk.exists(p)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            RealDisk.rename(from, to)?;
            if self.calls.fetch_add(1, Ordering::Relaxed) + 1 == self.after {
                fs::write(&self.appear, "someone else's").unwrap();
            }
            Ok(())
        }
        fn names_in(&self, dir: &Path) -> io::Result<Vec<String>> {
            RealDisk.names_in(dir)
        }
        fn print(&self, p: &Path) -> io::Result<ItemPrint> {
            RealDisk.print(p)
        }
    }

    #[test]
    fn never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        fs::write(p("a"), "a").unwrap();
        fs::write(p("b"), "b").unwrap();
        // "y" appears once both items hold temporary names.
        let disk = Racing {
            after: 2,
            appear: p("y"),
            calls: AtomicUsize::new(0),
        };
        let steps = [
            Step {
                from: p("a"),
                to: p("x"),
            },
            Step {
                from: p("b"),
                to: p("y"),
            },
        ];
        let (out, _) = run(&steps, &disk, Os::HOST);
        let RenameOutcome::Failed { path, stuck, .. } = out else {
            panic!("{out:?}");
        };
        assert_eq!(path, p("b"));
        assert!(stuck.is_empty());
        assert_eq!(fs::read_to_string(p("a")).unwrap(), "a");
        assert_eq!(fs::read_to_string(p("b")).unwrap(), "b");
        assert_eq!(fs::read_to_string(p("y")).unwrap(), "someone else's");
        let mut left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["a", "b", "y"]);
    }

    #[test]
    fn target_appearing_mid_run_is_left_alone() {
        let disk = FakeDisk::new(&["/t/a", "/t/b"]);
        // "/t/x" appears after phase one, before a's final rename.
        disk.appear_after(2, "/t/x");
        let steps = [step("/t/a", "/t/x"), step("/t/b", "/t/y")];
        let (out, _) = run(&steps, &disk, Os::Linux);
        let RenameOutcome::Failed { path, stuck, .. } = out else {
            panic!("{out:?}");
        };
        assert_eq!(path, PathBuf::from("/t/a"));
        assert!(stuck.is_empty());
        assert_eq!(disk.paths(), set(&["/t/a", "/t/b", "/t/x"]));
        // Nothing was renamed onto it.
        assert_eq!(disk.entry("/t/x").unwrap().id, 3);
    }

    #[test]
    fn taken_temporary_name_is_skipped() {
        let disk = FakeDisk::new(&["/t/a", "/t/.vento-rename-0000002a-0"]);
        let done = AtomicUsize::new(0);
        let out = execute_with(&[step("/t/a", "/t/b")], &disk, Os::Linux, &done, 0x2a);
        assert!(matches!(out, RenameOutcome::Done(_)), "{out:?}");
        assert_eq!(disk.paths(), set(&["/t/.vento-rename-0000002a-0", "/t/b"]));
        // The temporary name went to the next token.
        assert_eq!(disk.calls(), 2);
        assert_eq!(token(0x2a), "0000002a");
    }

    #[test]
    fn record_holds_prints() {
        let disk = FakeDisk::new(&["/t/a", "/t/b"]);
        let steps = [step("/t/a", "/t/x"), step("/t/b", "/t/y")];
        let (out, done) = run(&steps, &disk, Os::Linux);
        let RenameOutcome::Done(record) = out else {
            panic!("{out:?}");
        };
        assert_eq!(done, 4);
        assert_eq!(record.items.len(), 2);
        assert_eq!(record.items[0].0, PathBuf::from("/t/a"));
        assert_eq!(record.items[0].1, PathBuf::from("/t/x"));
        assert_eq!(record.items[0].2, disk.print(Path::new("/t/x")).unwrap());
        assert_eq!(record.items[1].2, disk.print(Path::new("/t/y")).unwrap());
        assert_ne!(record.items[0].2, record.items[1].2);
    }

    #[test]
    fn real_disk_basics() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f"), "1234").unwrap();
        fs::create_dir(dir.path().join("d")).unwrap();
        let disk = RealDisk;
        assert!(disk.exists(&dir.path().join("f")));
        assert!(!disk.exists(&dir.path().join("nope")));
        let mut names = disk.names_in(dir.path()).unwrap();
        names.sort();
        assert_eq!(names, ["d", "f"]);
        let f = disk.print(&dir.path().join("f")).unwrap();
        assert_eq!((f.len, f.is_dir), (4, false));
        let d = disk.print(&dir.path().join("d")).unwrap();
        assert_eq!((d.len, d.is_dir), (0, true));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_disk_rename_refuses_to_replace() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "A").unwrap();
        fs::write(&b, "B").unwrap();
        let err = RealDisk.rename(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(fs::read_to_string(&a).unwrap(), "A");
        assert_eq!(fs::read_to_string(&b).unwrap(), "B");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_disk_rename_moves_to_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let (a, c) = (dir.path().join("a"), dir.path().join("c"));
        fs::write(&a, "A").unwrap();
        RealDisk.rename(&a, &c).unwrap();
        assert!(!a.exists());
        assert_eq!(fs::read_to_string(&c).unwrap(), "A");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_disk_rename_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (d, e) = (dir.path().join("d"), dir.path().join("e"));
        fs::create_dir(&d).unwrap();
        fs::write(d.join("inside"), "in").unwrap();
        RealDisk.rename(&d, &e).unwrap();
        assert!(!d.exists());
        assert_eq!(fs::read_to_string(e.join("inside")).unwrap(), "in");
    }

    #[test]
    fn steps_follow_the_preview() {
        use super::super::items::Item;
        use super::super::preview::{Problem, Row};
        let item = |p: &str| Item {
            path: p.into(),
            name: Path::new(p).file_name().map(|n| n.to_str().unwrap().into()),
            is_dir: false,
            len: 0,
            modified: SystemTime::UNIX_EPOCH,
            modified_day: String::new(),
        };
        let batch = Batch {
            items: vec![item("/t/a"), item("/t/b"), item("/u/c")],
            taken: Default::default(),
        };
        let row = |n: Option<&str>| Row {
            new_name: n.map(str::to_string),
            problem: None,
        };
        let mut p = Preview {
            rows: vec![row(Some("x")), row(None), row(Some("y"))],
            changing: 2,
            problems: 0,
            unchanged: 1,
        };
        assert_eq!(
            steps(&batch, &p),
            [step("/t/a", "/t/x"), step("/u/c", "/u/y")]
        );
        p.rows[2].problem = Some(Problem::Taken);
        p.changing = 1;
        p.problems = 1;
        assert!(steps(&batch, &p).is_empty());
    }

    /// A [`FakeDisk`] that cannot read the print of `blind`.
    struct Blind {
        disk: FakeDisk,
        blind: PathBuf,
    }

    impl Disk for Blind {
        fn exists(&self, p: &Path) -> bool {
            self.disk.exists(p)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.disk.rename(from, to)
        }
        fn names_in(&self, dir: &Path) -> io::Result<Vec<String>> {
            self.disk.names_in(dir)
        }
        fn print(&self, p: &Path) -> io::Result<ItemPrint> {
            if p == self.blind {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            self.disk.print(p)
        }
    }

    #[test]
    fn record_counts_items_whose_print_failed() {
        let disk = Blind {
            disk: FakeDisk::new(&["/t/a", "/t/b"]),
            blind: PathBuf::from("/t/y"),
        };
        let steps = [
            Step {
                from: "/t/a".into(),
                to: "/t/x".into(),
            },
            Step {
                from: "/t/b".into(),
                to: "/t/y".into(),
            },
        ];
        let record = renamed(&steps, &disk);
        assert_eq!(
            record.renamed,
            [PathBuf::from("/t/x"), PathBuf::from("/t/y")]
        );
        assert_eq!(record.items.len(), 1);
        assert_eq!(crate::rename::done_text(&record), "Renamed 2 items");
    }

    /// Renames by `steps` on `disk` and returns the record.
    fn renamed(steps: &[Step], disk: &dyn Disk) -> RenameRecord {
        match run(steps, disk, Os::Linux).0 {
            RenameOutcome::Done(record) => record,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn undo_puts_every_name_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        fs::write(p("a"), "was a").unwrap();
        fs::write(p("b"), "was b").unwrap();
        let steps = [
            Step {
                from: p("a"),
                to: p("b"),
            },
            Step {
                from: p("b"),
                to: p("a"),
            },
        ];
        let record = renamed(&steps, &RealDisk);
        assert_eq!(fs::read_to_string(p("a")).unwrap(), "was b");
        let out = undo(&record, &RealDisk, Os::HOST);
        assert_eq!(out.put_back, 2);
        assert_eq!(out.skipped, 0);
        assert!(out.failed.is_none(), "{out:?}");
        assert_eq!(fs::read_to_string(p("a")).unwrap(), "was a");
        assert_eq!(fs::read_to_string(p("b")).unwrap(), "was b");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);

        // A chain: c's old name is b's new one.
        let disk = FakeDisk::new(&["/t/b", "/t/c"]);
        let record = renamed(&[step("/t/b", "/t/a"), step("/t/c", "/t/b")], &disk);
        let out = undo(&record, &disk, Os::Linux);
        assert_eq!((out.put_back, out.skipped), (2, 0));
        assert_eq!(disk.paths(), set(&["/t/b", "/t/c"]));
    }

    #[test]
    fn undo_skips_item_changed_since() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in ["a", "b", "c"] {
            fs::write(p(n), n).unwrap();
        }
        let steps: Vec<Step> = ["a", "b", "c"]
            .iter()
            .map(|n| Step {
                from: p(n),
                to: p(&format!("{n}2")),
            })
            .collect();
        let record = renamed(&steps, &RealDisk);
        let mut f = fs::OpenOptions::new().append(true).open(p("b2")).unwrap();
        io::Write::write_all(&mut f, b"xyz").unwrap();
        drop(f);
        let out = undo(&record, &RealDisk, Os::HOST);
        assert_eq!(out.skipped, 1);
        assert_eq!(out.put_back, steps.len() - 1);
        assert!(out.failed.is_none(), "{out:?}");
        assert!(p("a").exists() && p("c").exists());
        assert!(p("b2").exists() && !p("b").exists());
    }

    #[test]
    fn undo_skips_item_renamed_again() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        fs::write(p("a"), "a").unwrap();
        fs::write(p("b"), "b").unwrap();
        let steps = [
            Step {
                from: p("a"),
                to: p("x"),
            },
            Step {
                from: p("b"),
                to: p("y"),
            },
        ];
        let record = renamed(&steps, &RealDisk);
        fs::rename(p("x"), p("by hand")).unwrap();
        let out = undo(&record, &RealDisk, Os::HOST);
        assert_eq!((out.put_back, out.skipped), (1, 1));
        assert!(out.failed.is_none(), "{out:?}");
        assert!(p("by hand").exists() && p("b").exists());
        assert!(!p("a").exists() && !p("y").exists());
    }

    #[test]
    fn undo_skips_when_old_name_was_taken() {
        let disk = FakeDisk::new(&["/t/a", "/t/b"]);
        let record = renamed(&[step("/t/a", "/t/x"), step("/t/b", "/t/y")], &disk);
        disk.add("/t/a");
        let out = undo(&record, &disk, Os::Linux);
        assert_eq!((out.put_back, out.skipped), (1, 1));
        assert!(out.failed.is_none(), "{out:?}");
        assert_eq!(disk.paths(), set(&["/t/a", "/t/b", "/t/x"]));
        // A chain whose first link is skipped skips the rest: b's old name is
        // still held by c, which changed.
        let disk = FakeDisk::new(&["/t/b", "/t/c"]);
        let record = renamed(&[step("/t/b", "/t/a"), step("/t/c", "/t/b")], &disk);
        disk.remove("/t/b");
        disk.add("/t/b");
        let out = undo(&record, &disk, Os::Linux);
        assert_eq!((out.put_back, out.skipped), (0, 2));
        assert_eq!(disk.calls(), 4);
    }

    #[test]
    fn undo_skips_both_sides_of_a_swap_when_one_changed() {
        let disk = FakeDisk::new(&["/t/a", "/t/b"]);
        let record = renamed(&[step("/t/a", "/t/b"), step("/t/b", "/t/a")], &disk);
        // Another item now sits at "a": b can't go back, so a's old name stays held.
        disk.remove("/t/a");
        disk.add("/t/a");
        let after = disk.paths();
        let calls = disk.calls();
        let out = undo(&record, &disk, Os::Linux);
        assert_eq!((out.put_back, out.skipped), (0, 2));
        assert!(out.failed.is_none(), "{out:?}");
        assert_eq!(disk.paths(), after);
        assert_eq!(disk.calls(), calls);
    }

    #[test]
    fn undo_case_only_rename_on_mac() {
        let disk = FakeDisk::insensitive(&["/t/a.JPG"]);
        let (out, _) = run(&[step("/t/a.JPG", "/t/a.jpg")], &disk, Os::Mac);
        let RenameOutcome::Done(record) = out else {
            panic!("{out:?}");
        };
        assert_eq!(disk.paths(), set(&["/t/a.jpg"]));
        let out = undo(&record, &disk, Os::Mac);
        assert_eq!((out.put_back, out.skipped), (1, 0));
        assert!(out.failed.is_none(), "{out:?}");
        assert_eq!(disk.paths(), set(&["/t/a.JPG"]));
    }

    #[test]
    fn undo_failure_rolls_back() {
        let disk = FakeDisk::new(&["/t/a", "/t/b", "/t/c"]);
        let steps = [
            step("/t/a", "/t/x"),
            step("/t/b", "/t/y"),
            step("/t/c", "/t/z"),
        ];
        let record = renamed(&steps, &disk);
        let after = disk.paths();
        // Calls 1–6 were the rename; undo's 7–9 are phase one, 10 is x's final rename.
        disk.fail_call(11);
        let out = undo(&record, &disk, Os::Linux);
        assert_eq!(out.put_back, 0);
        let (path, reason) = out.failed.expect("failed");
        assert_eq!(path, PathBuf::from("/t/y"));
        assert!(!reason.is_empty());
        assert!(out.stuck.is_empty());
        assert_eq!(disk.paths(), after);
        assert!(no_temp_left(&disk));
    }
}
