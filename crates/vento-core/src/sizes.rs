//! Read-only size counting for files and folders, with progress and cancel.
//!
//! The walk is pure `std::fs`: it never writes and never follows symlinks.
//! Platform knowledge (what counts as a package, which volumes belong together)
//! comes in through [`CountOptions`], so this module stays portable.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How many of the largest parts a [`Progress`] reports.
pub const BIGGEST: usize = 3;

/// Which folders below a root the walk may enter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeScope {
    /// Device ids that are entered even though they differ from the root's
    /// (e.g. the pieces of one logical volume).
    pub also_same: Vec<u64>,
    /// Folders that are counted as a folder but never entered.
    pub skip: Vec<PathBuf>,
}

/// Inputs to [`count`].
pub struct CountOptions {
    /// Whether a directory is an opaque package, counted as a single file.
    pub is_package: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
    /// Which folders may be entered.
    pub scope: VolumeScope,
    /// Set from any thread to stop the walk.
    pub cancel: Arc<AtomicBool>,
    /// Minimum time between progress callbacks while walking.
    pub interval: Duration,
}

/// Running totals of a walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    /// Sum of the sizes of all files and symlinks seen.
    pub bytes: u64,
    /// Files, symlinks and packages.
    pub files: u64,
    /// Folders, not counting a folder whose contents are being listed.
    pub folders: u64,
    /// Something could not be read, so the totals are a lower bound.
    pub skipped: bool,
}

/// One top-level piece of what is being measured, with its running size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Where the part lives.
    pub path: PathBuf,
    /// Bytes counted inside it so far.
    pub bytes: u64,
}

/// A snapshot of the walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Totals so far.
    pub tally: Tally,
    /// The [`BIGGEST`] parts, largest first, ties by path.
    pub biggest: Vec<Part>,
    /// Whether the walk has finished.
    pub done: bool,
}

/// Where an item sits in the walk, which decides how it is counted.
#[derive(Clone, Copy)]
struct Ctx {
    /// Device of the root the item belongs to.
    root_dev: u64,
    /// The item is a dropped path: exempt from the device and skip rules.
    top: bool,
    /// The item lies inside a package: it adds bytes but no files or folders.
    in_package: bool,
}

struct Walker<'a> {
    opts: &'a CountOptions,
    tally: Tally,
    parts: Vec<Part>,
    seen_links: HashSet<(u64, u64)>,
    /// Directories still to list: path, index into `parts`, context.
    stack: Vec<(PathBuf, usize, Ctx)>,
    last_report: Instant,
}

#[cfg(unix)]
fn dev_of(md: &Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::dev(md)
}

#[cfg(not(unix))]
fn dev_of(_: &Metadata) -> u64 {
    0
}

/// `(dev, ino)` of a multiply-linked non-directory, so it is counted once.
#[cfg(unix)]
fn link_key(md: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (!md.is_dir() && md.nlink() > 1).then(|| (md.dev(), md.ino()))
}

#[cfg(not(unix))]
fn link_key(_: &Metadata) -> Option<(u64, u64)> {
    None
}

impl<'a> Walker<'a> {
    fn cancelled(&self) -> bool {
        self.opts.cancel.load(Ordering::Relaxed)
    }

    fn snapshot(&self, done: bool) -> Progress {
        let mut biggest = self.parts.clone();
        biggest.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        biggest.truncate(BIGGEST);
        Progress {
            tally: self.tally.clone(),
            biggest,
            done,
        }
    }

    /// Checked before every entry. Returns `false` when the walk was cancelled.
    fn poll(&mut self, on_progress: &mut dyn FnMut(&Progress)) -> bool {
        if self.cancelled() {
            return false;
        }
        if self.last_report.elapsed() >= self.opts.interval {
            on_progress(&self.snapshot(false));
            self.last_report = Instant::now();
            // The callback is how a caller cancels, so look again.
            if self.cancelled() {
                return false;
            }
        }
        true
    }

    fn add_bytes(&mut self, part: usize, md: &Metadata) {
        if let Some(key) = link_key(md)
            && !self.seen_links.insert(key)
        {
            return;
        }
        self.tally.bytes += md.len();
        self.parts[part].bytes += md.len();
    }

    /// Count one item; directories to descend into are pushed on the stack.
    fn visit(&mut self, path: PathBuf, part: usize, ctx: Ctx) {
        let md = match fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(_) => {
                self.tally.skipped = true;
                return;
            }
        };
        if !md.is_dir() {
            if !ctx.in_package {
                self.tally.files += 1;
            }
            self.add_bytes(part, &md);
            return;
        }

        let mut ctx = ctx;
        if !ctx.in_package {
            if (self.opts.is_package)(&path) {
                self.tally.files += 1;
                ctx.in_package = true;
            } else {
                self.tally.folders += 1;
            }
        }
        if !ctx.top {
            let dev = dev_of(&md);
            let same_volume = dev == ctx.root_dev || self.opts.scope.also_same.contains(&dev);
            if !same_volume || self.opts.scope.skip.contains(&path) {
                return;
            }
        }
        // Whatever sits inside is no longer a dropped path.
        ctx.top = false;
        self.stack.push((path, part, ctx));
    }

    /// List the stacked directories until none are left. `false` on cancel.
    fn drain(&mut self, on_progress: &mut dyn FnMut(&Progress)) -> bool {
        while let Some((dir, part, ctx)) = self.stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => {
                    self.tally.skipped = true;
                    continue;
                }
            };
            for entry in entries {
                if !self.poll(on_progress) {
                    return false;
                }
                match entry {
                    Ok(entry) => self.visit(entry.path(), part, ctx),
                    Err(_) => self.tally.skipped = true,
                }
            }
        }
        true
    }

    /// Inside mode: the root's direct children are the parts.
    fn walk_inside(
        &mut self,
        root: &Path,
        root_md: &Metadata,
        on_progress: &mut dyn FnMut(&Progress),
    ) -> bool {
        let root_dev = dev_of(root_md);
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(_) => {
                self.tally.skipped = true;
                return true;
            }
        };
        let ctx = Ctx {
            root_dev,
            top: false,
            in_package: false,
        };
        for entry in entries {
            if !self.poll(on_progress) {
                return false;
            }
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(_) => {
                    self.tally.skipped = true;
                    continue;
                }
            };
            self.parts.push(Part {
                path: path.clone(),
                bytes: 0,
            });
            let part = self.parts.len() - 1;
            self.visit(path, part, ctx);
            // Finish this child before the next so its part total is exact.
            if !self.drain(on_progress) {
                return false;
            }
        }
        true
    }

    /// Otherwise: each dropped path is a part.
    fn walk_items(&mut self, paths: &[PathBuf], on_progress: &mut dyn FnMut(&Progress)) -> bool {
        for path in paths {
            if !self.poll(on_progress) {
                return false;
            }
            self.parts.push(Part {
                path: path.clone(),
                bytes: 0,
            });
            let part = self.parts.len() - 1;
            // Each dropped path is its own root for the device rule.
            let root_dev = fs::symlink_metadata(path)
                .map(|md| dev_of(&md))
                .unwrap_or(0);
            let ctx = Ctx {
                root_dev,
                top: true,
                in_package: false,
            };
            self.visit(path.clone(), part, ctx);
            if !self.drain(on_progress) {
                return false;
            }
        }
        true
    }
}

/// Measure `paths`, calling `on_progress` at most once per `opts.interval`
/// while walking and exactly once more with `done: true`.
///
/// A single real folder that is not a package is measured by its contents,
/// with its direct children as the parts; anything else is measured as the
/// dropped items themselves. Returns `None` when cancelled, without a final
/// callback.
pub fn count(
    paths: &[PathBuf],
    opts: &CountOptions,
    on_progress: &mut dyn FnMut(&Progress),
) -> Option<Progress> {
    let mut walker = Walker {
        opts,
        tally: Tally::default(),
        parts: Vec::new(),
        seen_links: HashSet::new(),
        stack: Vec::new(),
        last_report: Instant::now(),
    };
    if walker.cancelled() {
        return None;
    }

    let inside_root = match paths {
        [only] => fs::symlink_metadata(only)
            .ok()
            .filter(|md| md.is_dir() && !(opts.is_package)(only))
            .map(|md| (only, md)),
        _ => None,
    };
    let finished = match inside_root {
        Some((root, md)) => walker.walk_inside(root, &md, on_progress),
        None => walker.walk_items(paths, on_progress),
    };
    if !finished || walker.cancelled() {
        return None;
    }

    let last = walker.snapshot(true);
    on_progress(&last);
    Some(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, n_bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; n_bytes]).unwrap();
    }

    fn opts() -> CountOptions {
        CountOptions {
            is_package: Arc::new(|p| p.extension() == Some("app".as_ref())),
            scope: VolumeScope::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            interval: Duration::ZERO,
        }
    }

    fn run(paths: &[PathBuf]) -> Progress {
        count(paths, &opts(), &mut |_| {}).expect("not cancelled")
    }

    fn part(path: PathBuf, bytes: u64) -> Part {
        Part { path, bytes }
    }

    #[test]
    fn counts_files_inside_a_folder() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&d.join("a"), 10);
        write(&d.join("sub/b"), 20);
        write(&d.join("sub/c"), 5);
        let p = run(std::slice::from_ref(&d));
        assert_eq!(
            p.tally,
            Tally {
                bytes: 35,
                files: 3,
                folders: 1,
                skipped: false
            }
        );
        assert_eq!(
            p.biggest,
            vec![part(d.join("sub"), 25), part(d.join("a"), 10)]
        );
        assert!(p.done);
    }

    #[test]
    fn hidden_files_count() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&d.join(".h"), 7);
        let p = run(&[d]);
        assert_eq!(p.tally.bytes, 7);
        assert_eq!(p.tally.files, 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        use std::os::unix::fs::symlink;
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&t.path().join("t/big"), 1000);
        fs::create_dir_all(&d).unwrap();
        symlink(t.path().join("t/big"), d.join("link")).unwrap();
        symlink(&d, d.join("loop")).unwrap();
        let expected = fs::symlink_metadata(d.join("link")).unwrap().len()
            + fs::symlink_metadata(d.join("loop")).unwrap().len();
        let p = run(&[d]);
        assert_eq!(p.tally.bytes, expected);
        assert_eq!(p.tally.files, 2);
        assert_eq!(p.tally.folders, 0);
    }

    #[test]
    fn hard_links_count_once() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&d.join("a"), 100);
        fs::hard_link(d.join("a"), d.join("b")).unwrap();
        let p = run(&[d]);
        assert_eq!(p.tally.bytes, 100);
        assert_eq!(p.tally.files, 2);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folder_marks_skipped() {
        use std::os::unix::fs::PermissionsExt;
        if std::env::var("USER") == Ok("root".into()) {
            return;
        }
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&d.join("ok"), 5);
        write(&d.join("locked/x"), 9);
        let locked = d.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let p = run(&[d]);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(p.tally.skipped);
        assert_eq!(p.tally.bytes, 5);
        assert_eq!(p.tally.folders, 1);
    }

    #[test]
    fn packages_count_as_one_file() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&d.join("Foo.app/Contents/x"), 50);
        write(&d.join("y"), 5);
        let p = run(std::slice::from_ref(&d));
        assert_eq!(p.tally.files, 2);
        assert_eq!(p.tally.folders, 0);
        assert_eq!(p.tally.bytes, 55);
        assert_eq!(
            p.biggest,
            vec![part(d.join("Foo.app"), 50), part(d.join("y"), 5)]
        );
    }

    #[test]
    fn several_items_use_the_items_as_parts() {
        let t = tempdir().unwrap();
        let a = t.path().join("a");
        let d = t.path().join("d");
        write(&a, 10);
        write(&d.join("f"), 30);
        let p = run(&[a.clone(), d.clone()]);
        assert_eq!(p.tally.files, 2);
        assert_eq!(p.tally.folders, 1);
        assert_eq!(p.tally.bytes, 40);
        assert_eq!(p.biggest, vec![part(d, 30), part(a, 10)]);
    }

    #[test]
    fn single_file() {
        let t = tempdir().unwrap();
        let a = t.path().join("a");
        write(&a, 10);
        let p = run(std::slice::from_ref(&a));
        assert_eq!(p.tally.files, 1);
        assert_eq!(p.tally.folders, 0);
        assert_eq!(p.biggest, vec![part(a, 10)]);
    }

    #[test]
    fn empty_folder() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir_all(&d).unwrap();
        let p = run(&[d]);
        assert_eq!(p.tally, Tally::default());
        assert!(p.biggest.is_empty());
        assert!(p.done);
    }

    #[test]
    fn skip_paths_are_not_entered() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&d.join("sub/x"), 10);
        let mut o = opts();
        o.scope.skip = vec![d.join("sub")];
        let p = count(&[d], &o, &mut |_| {}).unwrap();
        assert_eq!(p.tally.folders, 1);
        assert_eq!(p.tally.bytes, 0);
    }

    #[test]
    fn a_dropped_path_in_skip_is_still_entered() {
        let t = tempdir().unwrap();
        let a = t.path().join("a");
        let d = t.path().join("d");
        write(&a, 1);
        write(&d.join("x"), 10);
        let mut o = opts();
        o.scope.skip = vec![d.clone()];
        let p = count(&[a, d], &o, &mut |_| {}).unwrap();
        assert_eq!(p.tally.bytes, 11);
    }

    #[test]
    fn cancelled_before_start_returns_none() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        write(&d.join("a"), 1);
        let o = opts();
        o.cancel.store(true, Ordering::Relaxed);
        let mut calls = 0;
        assert!(count(&[d], &o, &mut |_| calls += 1).is_none());
        assert_eq!(calls, 0);
    }

    #[test]
    fn cancel_from_progress_stops() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        for i in 0..200 {
            write(&d.join(format!("f{i}")), 1);
        }
        let o = opts();
        let mut calls = 0;
        let cancel = o.cancel.clone();
        let r = count(&[d], &o, &mut |_| {
            calls += 1;
            cancel.store(true, Ordering::Relaxed);
        });
        assert!(r.is_none());
        assert_eq!(calls, 1);
    }

    #[test]
    fn progress_is_throttled() {
        let t = tempdir().unwrap();
        let d = t.path().join("d");
        for i in 0..50 {
            write(&d.join(format!("f{i}")), 1);
        }
        let mut o = opts();
        o.interval = Duration::from_secs(3600);
        let mut seen = Vec::new();
        let r = count(&[d], &o, &mut |p| seen.push(p.done)).unwrap();
        assert_eq!(seen, vec![true]);
        assert_eq!(r.tally.files, 50);
    }

    #[test]
    fn missing_path_marks_skipped() {
        let t = tempdir().unwrap();
        let p = run(&[t.path().join("d/nope")]);
        assert!(p.tally.skipped);
        assert_eq!(p.tally.bytes, 0);
    }
}
