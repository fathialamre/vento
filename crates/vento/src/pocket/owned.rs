//! Disposing of the files vento owns: they go to the Trash, never anything
//! outside the Pocket's `saved/` and `kept/` folders, never a user's file.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};

use log::{debug, warn};
use vento_core::pocket::{ItemSource, PocketItem};
use vento_platform::PlatformError;

/// The folders under the Pocket's content folder that hold vento's files.
const OWNED_FOLDERS: [&str; 2] = ["saved", "kept"];

/// `path` as it really is on disk if that is strictly inside one of
/// `folders` of `dir` (never one of those folders itself): the parent is
/// canonicalized (so `..` and symlinked folders are seen through), the last
/// part is kept as is (so a link is itself, not what it points to).
fn inside_any(path: &Path, dir: &Path, folders: &[&str]) -> Option<PathBuf> {
    let dir = fs::canonicalize(dir).ok()?;
    let name = path.file_name()?;
    let target = fs::canonicalize(path.parent()?).ok()?.join(name);
    folders
        .iter()
        .map(|f| dir.join(f))
        .any(|base| target.starts_with(&base) && target != base)
        .then_some(target)
}

/// `path` on disk if it is strictly inside `<dir>/saved` or `<dir>/kept`.
pub(super) fn inside(path: &Path, dir: &Path) -> Option<PathBuf> {
    inside_any(path, dir, &OWNED_FOLDERS)
}

/// Moves `target` to the Trash with `trash`, if it is still there. Returns
/// whether it went. Without a Trash, the file stays where it is.
fn trash_target(target: &Path, trash: &dyn Fn(&Path) -> Result<(), PlatformError>) -> bool {
    // Already gone (the item was missing): nothing to do.
    if fs::symlink_metadata(target).is_err() {
        return false;
    }
    match trash(target) {
        Ok(()) => {
            debug!("pocket moved {} to the Trash", target.display());
            true
        }
        Err(PlatformError::Unsupported) => {
            warn!("no Trash here: leaving {}", target.display());
            false
        }
        Err(e) => {
            warn!("cannot move {} to the Trash: {e}", target.display());
            false
        }
    }
}

/// Moves a copy vento just made in `<dir>/kept` to the Trash; anything
/// else is left alone.
pub(super) fn trash_kept(
    path: &Path,
    dir: &Path,
    trash: &dyn Fn(&Path) -> Result<(), PlatformError>,
) -> bool {
    match inside_any(path, dir, &["kept"]) {
        Some(target) => trash_target(&target, trash),
        None => {
            warn!(
                "not removing the copy {}: it is not in kept/",
                path.display()
            );
            false
        }
    }
}

/// Moves the file or folder of `item` to the Trash if vento owns it: a
/// `Saved` or `Kept` item strictly inside `<dir>/saved` or `<dir>/kept`
/// (`dir` is the Pocket's content folder). `Reference` items are the user's
/// and are never touched. Returns whether something went.
pub(super) fn trash_owned(
    item: &PocketItem,
    dir: &Path,
    trash: &dyn Fn(&Path) -> Result<(), PlatformError>,
) -> bool {
    if item.source == ItemSource::Reference {
        return false;
    }
    let Some(target) = inside(&item.path, dir) else {
        warn!(
            "not removing {}: it is not inside {}/saved or kept",
            item.path.display(),
            dir.display()
        );
        return false;
    };
    trash_target(&target, trash)
}

/// Items taken out of the Pocket whose files wait to go to the Trash: until
/// their Undo is no longer offered, or for a grace period after a drop. A
/// job is taken once: by its Undo, its timer, or the flush at quit.
#[derive(Default)]
pub(super) struct PendingTrash {
    jobs: RefCell<Vec<(u64, Vec<PocketItem>)>>,
    next: Cell<u64>,
}

impl PendingTrash {
    /// Holds `items`; returns the job to take them back by.
    pub(super) fn add(&self, items: Vec<PocketItem>) -> u64 {
        let job = self.next.get();
        self.next.set(job.wrapping_add(1));
        self.jobs.borrow_mut().push((job, items));
        job
    }

    /// The items of `job`, unless it was already taken.
    pub(super) fn take(&self, job: u64) -> Option<Vec<PocketItem>> {
        let mut jobs = self.jobs.borrow_mut();
        let i = jobs.iter().position(|(j, _)| *j == job)?;
        Some(jobs.swap_remove(i).1)
    }

    /// Every item still waiting, oldest job first.
    pub(super) fn take_all(&self) -> Vec<PocketItem> {
        self.jobs
            .borrow_mut()
            .drain(..)
            .flat_map(|(_, items)| items)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(path: &Path, source: ItemSource) -> PocketItem {
        PocketItem {
            id: 1,
            path: path.to_path_buf(),
            source,
            added_unix: 0,
            origin: None,
        }
    }

    /// A Trash in `bin`: moves each item there, and records it.
    struct FakeTrash {
        bin: PathBuf,
        moved: RefCell<Vec<PathBuf>>,
    }

    impl FakeTrash {
        fn new(root: &Path) -> Self {
            let bin = root.join("Trash");
            fs::create_dir_all(&bin).unwrap();
            Self {
                bin,
                moved: RefCell::new(Vec::new()),
            }
        }

        fn trash(&self, path: &Path) -> Result<(), PlatformError> {
            let n = self.moved.borrow().len();
            fs::rename(path, self.bin.join(n.to_string()))
                .map_err(|e| PlatformError::Failed(e.to_string()))?;
            self.moved.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    #[test]
    fn owned_files_go_to_the_trash_and_nothing_else_does() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("pocket");
        fs::create_dir_all(dir.join("saved")).unwrap();
        fs::create_dir_all(dir.join("kept/folder/inner")).unwrap();
        let outside = root.path().join("x");
        fs::write(&outside, "user").unwrap();
        let loose = dir.join("loose.txt");
        fs::write(&loose, "not in saved or kept").unwrap();
        let bin = FakeTrash::new(root.path());
        let trash = |p: &Path| bin.trash(p);
        let owned_gone = |path: &Path, source| trash_owned(&owned(path, source), &dir, &trash);

        // Outside the Pocket's folders: refused, whatever the kind.
        assert!(!owned_gone(Path::new("/tmp/x"), ItemSource::Saved));
        assert!(!owned_gone(&outside, ItemSource::Saved));
        assert!(!owned_gone(&outside, ItemSource::Kept));
        assert!(!owned_gone(&loose, ItemSource::Saved));
        // A detour out of the folder is seen through.
        assert!(!owned_gone(&dir.join("saved/../../x"), ItemSource::Saved));
        assert!(!owned_gone(
            &dir.join("kept/../loose.txt"),
            ItemSource::Kept
        ));
        assert!(outside.exists() && loose.exists());
        // Never the folders themselves.
        for folder in [dir.clone(), dir.join("saved"), dir.join("kept")] {
            assert!(!owned_gone(&folder, ItemSource::Kept), "{folder:?}");
            assert!(folder.exists());
        }
        assert!(!owned_gone(&dir.join("saved/."), ItemSource::Saved));
        assert!(!owned_gone(&dir.join("kept/folder/.."), ItemSource::Kept));
        assert!(dir.join("saved").exists() && dir.join("kept").exists());

        // References are only forgotten, even inside the folder.
        let note = dir.join("saved/note.txt");
        fs::write(&note, "hi").unwrap();
        assert!(!owned_gone(&note, ItemSource::Reference));
        assert!(note.exists());
        assert!(bin.moved.borrow().is_empty());

        // Vento's own files and folders go to the Trash.
        assert!(owned_gone(&note, ItemSource::Saved));
        assert!(!note.exists());
        let folder = dir.join("kept/folder");
        assert!(owned_gone(&folder, ItemSource::Kept));
        assert!(!folder.exists());
        // Already gone: nothing to trash.
        assert!(!owned_gone(&note, ItemSource::Saved));

        let canonical = |p: &Path| {
            fs::canonicalize(p.parent().unwrap())
                .unwrap()
                .join(p.file_name().unwrap())
        };
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut trashed = vec![canonical(&note), canonical(&folder)];

        // A link in the folder goes; what it points to stays. (Windows lets
        // only admins and developer mode make links.)
        #[cfg(unix)]
        {
            let link = dir.join("saved/link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(owned_gone(&link, ItemSource::Saved));
            assert!(fs::symlink_metadata(&link).is_err());
            assert!(outside.exists());
            trashed.push(canonical(&link));
        }

        assert_eq!(*bin.moved.borrow(), trashed);
    }

    #[test]
    fn without_a_trash_files_stay() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("pocket");
        fs::create_dir_all(dir.join("saved")).unwrap();
        let note = dir.join("saved/note.txt");
        fs::write(&note, "hi").unwrap();
        let unsupported = |_: &Path| Err(PlatformError::Unsupported);
        assert!(!trash_owned(
            &owned(&note, ItemSource::Saved),
            &dir,
            &unsupported
        ));
        assert!(note.exists());
    }

    #[test]
    fn unused_copies_are_trashed_only_from_kept() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("pocket");
        fs::create_dir_all(dir.join("kept")).unwrap();
        fs::create_dir_all(dir.join("saved")).unwrap();
        let copy = dir.join("kept/a.txt");
        let saved = dir.join("saved/a.txt");
        fs::write(&copy, "copy").unwrap();
        fs::write(&saved, "saved").unwrap();
        let bin = FakeTrash::new(root.path());
        let trash = |p: &Path| bin.trash(p);

        assert!(!trash_kept(&saved, &dir, &trash));
        assert!(!trash_kept(&dir.join("kept"), &dir, &trash));
        assert!(!trash_kept(&dir.join("kept/../saved/a.txt"), &dir, &trash));
        assert!(saved.exists() && dir.join("kept").exists());
        assert!(trash_kept(&copy, &dir, &trash));
        assert!(!copy.exists());
    }

    #[test]
    fn pending_jobs_are_taken_once() {
        let pending = PendingTrash::default();
        let a = pending.add(vec![owned(Path::new("/a"), ItemSource::Saved)]);
        let b = pending.add(vec![
            owned(Path::new("/b"), ItemSource::Kept),
            owned(Path::new("/c"), ItemSource::Reference),
        ]);
        assert_ne!(a, b);
        // Undo takes its job back; the timer then finds nothing.
        let items = pending.take(a).unwrap();
        assert_eq!(items[0].path, Path::new("/a"));
        assert!(pending.take(a).is_none());
        // The flush at quit takes what is left, once.
        let rest: Vec<PathBuf> = pending.take_all().into_iter().map(|i| i.path).collect();
        assert_eq!(rest, vec![PathBuf::from("/b"), PathBuf::from("/c")]);
        assert!(pending.take(b).is_none());
        assert!(pending.take_all().is_empty());
    }
}
