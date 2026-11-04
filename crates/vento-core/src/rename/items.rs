//! The dropped paths' metadata and the Order menu's sorting.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::name::natural_cmp;

/// Most items one rename window takes.
pub const MAX_ITEMS: usize = 5_000;

/// One dropped file or folder.
#[derive(Debug, Clone)]
pub struct Item {
    /// Where it lives (trailing separator stripped).
    pub path: PathBuf,
    /// The file name; `None` when it is not valid UTF-8.
    pub name: Option<String>,
    /// Whether it is a real folder; a symlink is never one.
    pub is_dir: bool,
    /// Size in bytes; 0 for folders.
    pub len: u64,
    /// Last modification time.
    pub modified: SystemTime,
    /// `modified` as a local `YYYY-MM-DD`, for the `{date}` tag.
    pub modified_day: String,
}

/// What the list is ordered by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderBy {
    /// natural name order
    #[default]
    Name,
    /// oldest first
    Modified,
    /// smallest first
    Size,
}

/// The Order menu's choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Order {
    /// the key
    pub by: OrderBy,
    /// flip each group's order
    pub reverse: bool,
}

/// Reads one `symlink_metadata` per path (links are never followed).
///
/// Paths that fail are left out; a path given twice (a trailing separator does not
/// make it different) is kept once.
pub fn load_items(paths: &[PathBuf]) -> Vec<Item> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for raw in paths {
        // Re-collecting the components drops a trailing separator.
        let path: PathBuf = raw.components().collect();
        if !seen.insert(path.clone()) {
            continue;
        }
        // `/`, `.`, `..` and `foo/..` have no name to change.
        if path.file_name().is_none() {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let is_dir = meta.is_dir();
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        items.push(Item {
            name: path.file_name().and_then(|n| n.to_str()).map(str::to_owned),
            is_dir,
            len: if is_dir { 0 } else { meta.len() },
            modified_day: day_of(modified),
            modified,
            path,
        });
    }
    items
}

/// `t` as a local `YYYY-MM-DD`; empty when out of range.
fn day_of(t: SystemTime) -> String {
    match jiff::Timestamp::try_from(t) {
        Ok(ts) => ts
            .to_zoned(jiff::tz::TimeZone::system())
            .date()
            .strftime("%Y-%m-%d")
            .to_string(),
        Err(_) => String::new(),
    }
}

fn name_cmp(a: &Item, b: &Item) -> Ordering {
    let text = |i: &Item| match &i.name {
        Some(n) => n.clone(),
        None => i
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    natural_cmp(&text(a), &text(b))
}

/// Orders `items`: folders first, then by `order`; ties fall back to natural name order.
///
/// Folders sort among themselves the same way, except under Size, where they sort
/// by name (a folder's size is not known); Reverse flips each group on its own.
pub fn sort_items(items: &mut [Item], order: Order) {
    items.sort_by(|a, b| {
        // Folders first, in either direction.
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        let by = if a.is_dir && order.by == OrderBy::Size {
            OrderBy::Name
        } else {
            order.by
        };
        let ord = match by {
            OrderBy::Name => Ordering::Equal,
            OrderBy::Modified => a.modified.cmp(&b.modified),
            OrderBy::Size => a.len.cmp(&b.len),
        }
        .then_with(|| name_cmp(a, b));
        if order.reverse { ord.reverse() } else { ord }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    fn names(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_deref().unwrap()).collect()
    }

    fn file(dir: &std::path::Path, name: &str, bytes: usize, age_secs: u64) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, vec![b'x'; bytes]).unwrap();
        let f = File::options().write(true).open(&p).unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 - age_secs);
        f.set_modified(t).unwrap();
        p
    }

    #[test]
    fn load_items_dedupes_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let a = file(tmp.path(), "a.txt", 1, 0);
        let d = tmp.path().join("d");
        fs::create_dir(&d).unwrap();
        let mut slash = d.clone().into_os_string();
        slash.push("/");
        let paths = vec![a.clone(), d.clone(), a, PathBuf::from(slash)];
        assert_eq!(load_items(&paths).len(), 2);
    }

    #[test]
    fn load_items_skips_missing_and_keeps_links_unfollowed() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("d");
        fs::create_dir(&d).unwrap();
        let missing = tmp.path().join("nope");
        let a = file(tmp.path(), "a.txt", 3, 0);
        // Only the Unix block below adds to it.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut paths = vec![missing, a];
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&d, &link).unwrap();
            paths.push(link);
        }
        let items = load_items(&paths);
        #[cfg(unix)]
        {
            assert_eq!(items.len(), 2);
            let link = items.iter().find(|i| i.name.as_deref() == Some("link"));
            assert!(!link.unwrap().is_dir);
        }
        let a = items.iter().find(|i| i.name.as_deref() == Some("a.txt"));
        assert_eq!(a.unwrap().len, 3);
        assert_eq!(a.unwrap().modified_day.len(), 10);
    }

    #[test]
    fn folders_first_then_natural() {
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = vec![
            file(tmp.path(), "IMG_10", 1, 0),
            file(tmp.path(), "IMG_2", 1, 0),
        ];
        for d in ["b", "A"] {
            let p = tmp.path().join(d);
            fs::create_dir(&p).unwrap();
            paths.push(p);
        }
        let mut items = load_items(&paths);
        sort_items(&mut items, Order::default());
        assert_eq!(names(&items), ["A", "b", "IMG_2", "IMG_10"]);
    }

    #[test]
    fn by_date_and_size_with_reverse_and_ties() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("d");
        fs::create_dir(&d).unwrap();
        let paths = vec![
            file(tmp.path(), "x", 5, 1000),
            file(tmp.path(), "y", 1, 0),
            file(tmp.path(), "z", 5, 1000),
            d,
        ];
        let mut items = load_items(&paths);
        let order = |by, reverse| Order { by, reverse };
        sort_items(&mut items, order(OrderBy::Modified, false));
        assert_eq!(names(&items), ["d", "x", "z", "y"]);
        sort_items(&mut items, order(OrderBy::Size, false));
        assert_eq!(names(&items), ["d", "y", "x", "z"]);
        sort_items(&mut items, order(OrderBy::Size, true));
        assert_eq!(names(&items), ["d", "z", "x", "y"]);
    }

    #[test]
    fn load_items_leaves_out_paths_without_a_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let a = file(tmp.path(), "a.txt", 1, 0);
        let paths = vec![
            PathBuf::from("/"),
            PathBuf::from(".."),
            PathBuf::from("."),
            tmp.path().join(".."),
            a,
        ];
        let items = load_items(&paths);
        assert_eq!(names(&items), ["a.txt"]);
    }

    #[test]
    fn name_order_reversed_keeps_folders_first() {
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = vec![
            file(tmp.path(), "IMG_10", 1, 0),
            file(tmp.path(), "IMG_2", 1, 0),
        ];
        for d in ["b", "A"] {
            let p = tmp.path().join(d);
            fs::create_dir(&p).unwrap();
            paths.push(p);
        }
        let mut items = load_items(&paths);
        let order = Order {
            by: OrderBy::Name,
            reverse: true,
        };
        sort_items(&mut items, order);
        assert_eq!(names(&items), ["b", "A", "IMG_10", "IMG_2"]);
    }

    #[test]
    fn folders_sort_by_name_under_size() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        let a = tmp.path().join("a");
        fs::create_dir(&b).unwrap();
        fs::create_dir(&a).unwrap();
        file(&b, "big", 10_000, 0);
        let mut items = load_items(&[b, a]);
        sort_items(
            &mut items,
            Order {
                by: OrderBy::Size,
                reverse: false,
            },
        );
        assert_eq!(names(&items), ["a", "b"]);
    }

    #[test]
    fn folders_sort_by_date_under_date() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let dir = |name: &str, secs| Item {
            path: PathBuf::from("/t").join(name),
            name: Some(name.into()),
            is_dir: true,
            len: 0,
            modified: at(secs),
            modified_day: String::new(),
        };
        // "a" is the newer folder, so by date it comes after "b".
        let mut items = vec![dir("a", 2_000), dir("b", 1_000)];
        let order = |reverse| Order {
            by: OrderBy::Modified,
            reverse,
        };
        sort_items(&mut items, order(false));
        assert_eq!(names(&items), ["b", "a"]);
        sort_items(&mut items, order(true));
        assert_eq!(names(&items), ["a", "b"]);
    }
}
