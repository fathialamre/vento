//! Helpers shared by the tests of the archive modules.

use std::fs;
use std::io;
use std::path::Path;

/// The names directly inside `dir`, sorted.
pub(crate) fn listing(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Every path below `dir`, relative to it, sorted.
pub(crate) fn tree(dir: &Path) -> Vec<String> {
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

/// A `mark` callback that accepts everything.
pub(crate) fn no_mark(_: &Path) -> io::Result<()> {
    Ok(())
}
