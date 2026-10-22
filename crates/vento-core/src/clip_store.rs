//! Blob storage for clipboard history entries.
//!
//! Each entry's snapshot lives in its own folder `<dir>/<id>/`, one file per
//! representation named `<item>-<rep>` (both zero-based). A folder is first
//! written as `<dir>/<id>.tmp/` and renamed, so a half-written entry never
//! carries a bare id name. Deletion is outright (never the Trash) and only
//! ever touches direct children of `dir`.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use log::warn;

use crate::clip::{ClipRep, ClipSnapshot};
use crate::clip_history::ClipEntry;

/// Writes `s` to `<dir>/<id>/`, creating `dir` if needed.
///
/// Fails with [`io::ErrorKind::AlreadyExists`] if the entry's folder is
/// already there; a leftover `<id>.tmp` from a crashed write is replaced.
pub fn write_entry(dir: &Path, id: u64, s: &ClipSnapshot) -> io::Result<()> {
    let target = dir.join(id.to_string());
    let tmp = dir.join(format!("{id}.tmp"));
    fs::create_dir_all(dir)?;
    if fs::symlink_metadata(&target).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("clipboard entry {id} already exists"),
        ));
    }
    if fs::symlink_metadata(&tmp).is_ok() {
        remove_child(&tmp)?;
    }
    let result = write_blobs(&tmp, s).and_then(|()| fs::rename(&tmp, &target));
    if result.is_err() {
        // Best effort: don't leave a half-written folder behind.
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

/// Writes every representation of `s` into the fresh folder `folder`.
fn write_blobs(folder: &Path, s: &ClipSnapshot) -> io::Result<()> {
    fs::create_dir(folder)?;
    for (i, item) in s.items.iter().enumerate() {
        for (j, rep) in item.iter().enumerate() {
            let mut f = File::create(folder.join(format!("{i}-{j}")))?;
            f.write_all(&rep.data)?;
            f.sync_all()?;
        }
    }
    Ok(())
}

/// Rebuilds the snapshot of entry `e` from its folder under `dir`.
///
/// `e.types` says which items and representations exist; a missing or
/// unreadable blob file is an error.
pub fn read_entry(dir: &Path, e: &ClipEntry) -> io::Result<ClipSnapshot> {
    let folder = dir.join(e.id.to_string());
    let mut items = Vec::with_capacity(e.types.len());
    for (i, types) in e.types.iter().enumerate() {
        let mut reps = Vec::with_capacity(types.len());
        for (j, ty) in types.iter().enumerate() {
            let data = fs::read(folder.join(format!("{i}-{j}")))?;
            reps.push(ClipRep {
                ty: ty.clone(),
                data,
            });
        }
        items.push(reps);
    }
    Ok(ClipSnapshot { items })
}

/// Deletes entry `id`'s folder outright. Failures are logged, not returned;
/// a folder that is already gone is fine.
pub fn delete_entry(dir: &Path, id: u64) {
    let path = dir.join(id.to_string());
    if path.parent() != Some(dir) {
        warn!(
            "refusing to delete {}: not directly inside {}",
            path.display(),
            dir.display()
        );
        return;
    }
    if let Err(err) = remove_child(&path)
        && err.kind() != io::ErrorKind::NotFound
    {
        warn!("could not delete clipboard entry {}: {err}", path.display());
    }
}

/// Deletes every direct child of `dir` that is not the folder of an id in
/// `keep`: removed entries, stray `.tmp` folders and loose files alike.
/// Symlinks are removed as links, never followed. Failures are logged.
pub fn sweep(dir: &Path, keep: &[u64]) {
    let keep: HashSet<String> = keep.iter().map(u64::to_string).collect();
    let children = match fs::read_dir(dir) {
        Ok(children) => children,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                warn!("could not scan {}: {err}", dir.display());
            }
            return;
        }
    };
    for child in children {
        let child = match child {
            Ok(child) => child,
            Err(err) => {
                warn!("could not read an entry of {}: {err}", dir.display());
                continue;
            }
        };
        let path = child.path();
        let is_kept_folder = child.file_name().to_str().is_some_and(|n| keep.contains(n))
            && fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        if is_kept_folder {
            continue;
        }
        if let Err(err) = remove_child(&path) {
            warn!("could not remove {}: {err}", path.display());
        }
    }
}

/// The highest id that names anything directly in `dir` (an entry folder,
/// or whatever else took that name: [`write_entry`] refuses it either way),
/// `None` when there is none or `dir` can't be read.
pub fn highest_id(dir: &Path) -> Option<u64> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|c| c.file_name().to_str()?.parse::<u64>().ok())
        .max()
}

/// Removes one direct child of the history folder: a real folder with its
/// contents, anything else (file or symlink, never followed) as a single file.
fn remove_child(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::Summary;

    fn rep(ty: &str, data: &[u8]) -> ClipRep {
        ClipRep {
            ty: ty.into(),
            data: data.to_vec(),
        }
    }

    fn sample() -> ClipSnapshot {
        ClipSnapshot {
            items: vec![
                vec![
                    rep("public.utf8-plain-text", b"hello"),
                    rep("public.png", &[0, 159, 146, 150, 255, 0]),
                ],
                vec![rep("public.tiff", &[1, 2, 3, 0, 4])],
            ],
        }
    }

    fn entry_for(id: u64, s: &ClipSnapshot) -> ClipEntry {
        ClipEntry {
            id,
            copied_unix: 0,
            hash: Some(0),
            summary: Summary::Text {
                preview: String::new(),
            },
            types: s
                .items
                .iter()
                .map(|item| item.iter().map(|r| r.ty.clone()).collect())
                .collect(),
            pocket_file: None,
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("clipboard");
        let s = sample();
        write_entry(&dir, 3, &s).unwrap();
        assert!(dir.join("3").join("0-1").is_file());
        assert!(!dir.join("3.tmp").exists());
        assert_eq!(read_entry(&dir, &entry_for(3, &s)).unwrap(), s);
    }

    #[test]
    fn write_replaces_a_stale_tmp_but_refuses_an_existing_entry() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("4.tmp")).unwrap();
        fs::write(tmp.path().join("4.tmp").join("junk"), b"x").unwrap();
        let s = sample();
        write_entry(tmp.path(), 4, &s).unwrap();
        assert!(!tmp.path().join("4").join("junk").exists());
        let err = write_entry(tmp.path(), 4, &s).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read_entry(tmp.path(), &entry_for(4, &s)).unwrap(), s);
    }

    #[test]
    fn missing_rep_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let s = sample();
        write_entry(tmp.path(), 1, &s).unwrap();
        fs::remove_file(tmp.path().join("1").join("1-0")).unwrap();
        assert!(read_entry(tmp.path(), &entry_for(1, &s)).is_err());
    }

    #[test]
    fn delete_entry_removes_only_that_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let s = sample();
        write_entry(tmp.path(), 1, &s).unwrap();
        write_entry(tmp.path(), 2, &s).unwrap();
        delete_entry(tmp.path(), 1);
        assert!(!tmp.path().join("1").exists());
        assert!(tmp.path().join("2").join("0-0").is_file());
        delete_entry(tmp.path(), 1); // already gone: no panic
    }

    #[test]
    fn sweep_keeps_listed_ids_and_removes_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let s = sample();
        for id in [1, 2, 3] {
            write_entry(tmp.path(), id, &s).unwrap();
        }
        fs::create_dir_all(tmp.path().join("7.tmp")).unwrap();
        fs::write(tmp.path().join("7.tmp").join("0-0"), b"x").unwrap();
        fs::write(tmp.path().join("stray.txt"), b"x").unwrap();
        sweep(tmp.path(), &[1, 3]);
        let mut left: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|c| c.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["1", "3"]);
        assert!(tmp.path().join("1").join("0-0").is_file());
    }

    #[test]
    fn highest_id_reads_folder_names() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(highest_id(&tmp.path().join("none")), None);
        assert_eq!(highest_id(tmp.path()), None);
        for name in ["3", "12", "40.tmp", "stray", "7x"] {
            fs::create_dir(tmp.path().join(name)).unwrap();
        }
        assert_eq!(highest_id(tmp.path()), Some(12));
        fs::write(tmp.path().join("99"), b"a file still takes the name").unwrap();
        assert_eq!(highest_id(tmp.path()), Some(99));
    }

    #[cfg(unix)]
    #[test]
    fn sweep_does_not_follow_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("precious"), b"keep me").unwrap();
        let dir = tmp.path().join("clipboard");
        fs::create_dir(&dir).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("9")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("stray")).unwrap();
        sweep(&dir, &[9]); // a link named like a kept id is still not a kept folder
        assert!(fs::read_dir(&dir).unwrap().next().is_none());
        assert_eq!(
            fs::read(outside.path().join("precious")).unwrap(),
            b"keep me"
        );
        std::os::unix::fs::symlink(outside.path(), dir.join("5")).unwrap();
        delete_entry(&dir, 5);
        assert!(fs::symlink_metadata(dir.join("5")).is_err());
        assert!(outside.path().join("precious").is_file());
    }
}
