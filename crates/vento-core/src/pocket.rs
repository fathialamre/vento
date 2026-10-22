//! The Pocket: files vento holds temporarily, persisted as JSON.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use log::warn;
use serde::{Deserialize, Serialize};

/// Newest on-disk format this build understands.
const FORMAT_VERSION: u32 = 1;

/// Where a Pocket item's file comes from, and so who owns it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemSource {
    /// The user's own file, held by reference. vento never deletes it.
    Reference,
    /// Content vento saved into its own folder (text, images, links).
    Saved,
    /// A copy vento made and keeps in its own folder.
    Kept,
}

/// One entry in the Pocket.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PocketItem {
    /// Unique within the Pocket, never reused (not even after a restart).
    pub id: u64,
    /// The file or folder this item stands for.
    pub path: PathBuf,
    /// Who owns the file at `path`.
    pub source: ItemSource,
    /// When the item was added, in seconds since the Unix epoch.
    pub added_unix: u64,
    /// For a kept copy: the file it was copied from, so the same file is not
    /// copied in again. Absent in files written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PathBuf>,
}

/// The items currently in the Pocket, oldest first.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Pocket {
    items: Vec<PocketItem>,
    next_id: u64,
}

/// On-disk shape of the Pocket file.
#[derive(Serialize, Deserialize)]
struct RawPocket {
    version: u32,
    next_id: u64,
    items: Vec<PocketItem>,
}

/// The path used to decide whether two items are the same file.
fn identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `item` is the file `wanted` (an [`identity`]), or a kept copy of it.
fn stands_for(item: &PocketItem, wanted: &Path) -> bool {
    identity(&item.path) == wanted
        || item
            .origin
            .as_deref()
            .is_some_and(|o| identity(o) == wanted)
}

/// `path` with `suffix` appended to its file name (`pocket.json` -> `pocket.json.tmp`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

impl Pocket {
    /// The items, oldest first.
    pub fn items(&self) -> &[PocketItem] {
        &self.items
    }

    /// Whether the Pocket holds nothing.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds `path` and returns its new id, or `None` when an item already
    /// stands for the same file (paths are compared after canonicalizing, so
    /// symlinked parents count as the same place).
    pub fn add(&mut self, path: PathBuf, source: ItemSource, now_unix: u64) -> Option<u64> {
        self.add_with_origin(path, source, None, now_unix)
    }

    /// Like [`add`](Self::add), also recording the file a kept copy was made
    /// from. `None` when `path` or `origin` is already held (see
    /// [`contains`](Self::contains)).
    pub fn add_with_origin(
        &mut self,
        path: PathBuf,
        source: ItemSource,
        origin: Option<PathBuf>,
        now_unix: u64,
    ) -> Option<u64> {
        if self.contains(&path) || origin.as_deref().is_some_and(|o| self.contains(o)) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(PocketItem {
            id,
            path,
            source,
            added_unix: now_unix,
            origin,
        });
        Some(id)
    }

    /// Whether an item stands for `path`: its own file, or the file it was
    /// copied from. Paths are compared as in [`add`](Self::add).
    pub fn contains(&self, path: &Path) -> bool {
        let wanted = identity(path);
        self.items.iter().any(|i| stands_for(i, &wanted))
    }

    /// The ids of the items that stand for `path`, as in
    /// [`contains`](Self::contains).
    pub fn holding(&self, path: &Path) -> Vec<u64> {
        let wanted = identity(path);
        self.items
            .iter()
            .filter(|i| stands_for(i, &wanted))
            .map(|i| i.id)
            .collect()
    }

    /// Removes the items with these ids and returns them (unknown ids are
    /// ignored). Only the entries go; no file is touched.
    pub fn remove(&mut self, ids: &[u64]) -> Vec<PocketItem> {
        let (removed, kept) = std::mem::take(&mut self.items)
            .into_iter()
            .partition(|i| ids.contains(&i.id));
        self.items = kept;
        removed
    }

    /// Removes every item and returns them.
    pub fn clear(&mut self) -> Vec<PocketItem> {
        std::mem::take(&mut self.items)
    }

    /// The item with this id, if any.
    pub fn get(&self, id: u64) -> Option<&PocketItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Ids of items whose file no longer exists (deleted or moved away).
    /// A dangling symlink still counts as present.
    pub fn missing(&self) -> Vec<u64> {
        self.items
            .iter()
            .filter(|i| std::fs::symlink_metadata(&i.path).is_err())
            .map(|i| i.id)
            .collect()
    }

    /// Loads the Pocket from `path`. Never fails: a missing file gives an
    /// empty Pocket; an unreadable one, or one written by a newer version,
    /// gives an empty Pocket and is moved aside to `<name>.bak` (replacing
    /// an older backup) so the next save cannot destroy it.
    pub fn load(path: &Path) -> Pocket {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Pocket::default(),
            Err(e) => {
                warn!("cannot read pocket {}: {e}", path.display());
                Self::set_aside(path);
                return Pocket::default();
            }
        };
        match Self::parse(&text) {
            Ok(pocket) => pocket,
            Err(why) => {
                warn!("ignoring pocket {}: {why}", path.display());
                Self::set_aside(path);
                Pocket::default()
            }
        }
    }

    fn parse(text: &str) -> Result<Pocket, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        match version {
            Some(v) if v > u64::from(FORMAT_VERSION) => {
                return Err(format!("written by a newer version (format {v})"));
            }
            Some(_) => {}
            None => return Err("no format version".to_owned()),
        }
        let raw: RawPocket = serde_json::from_value(value).map_err(|e| e.to_string())?;
        // Never hand out an id that an existing item already has.
        let floor = raw.items.iter().map(|i| i.id + 1).max().unwrap_or(0);
        Ok(Pocket {
            next_id: raw.next_id.max(floor),
            items: raw.items,
        })
    }

    /// Moves a file we cannot use to `<name>.bak`.
    fn set_aside(path: &Path) {
        let bak = with_suffix(path, ".bak");
        if let Err(e) = std::fs::rename(path, &bak) {
            warn!("cannot move {} to {}: {e}", path.display(), bak.display());
        }
    }

    /// Writes the Pocket to `path`, creating the parent folder if needed.
    /// The content goes to `<name>.tmp` first and is renamed into place, so
    /// a crash never leaves a truncated file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let raw = RawPocket {
            version: FORMAT_VERSION,
            next_id: self.next_id,
            items: self.items.clone(),
        };
        let text = serde_json::to_string_pretty(&raw).map_err(io::Error::other)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = with_suffix(path, ".tmp");
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }
}

/// Location of the Pocket file.
pub fn pocket_path() -> PathBuf {
    match ProjectDirs::from("", "", "vento") {
        Some(dirs) => dirs.data_dir().join("pocket.json"),
        None => PathBuf::from("pocket.json"),
    }
}

/// Folder holding the content vento owns for the Pocket (`saved/`, `kept/`).
pub fn pocket_dir() -> PathBuf {
    match ProjectDirs::from("", "", "vento") {
        Some(dirs) => dirs.data_dir().join("pocket"),
        None => PathBuf::from("pocket"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bak_of(p: &Path) -> PathBuf {
        p.with_file_name("pocket.json.bak")
    }

    #[test]
    fn add_returns_ids_and_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let mut p = Pocket::default();
        assert!(p.is_empty());
        let ia = p.add(a.clone(), ItemSource::Reference, 10).unwrap();
        let ib = p.add(b.clone(), ItemSource::Saved, 11).unwrap();
        assert_ne!(ia, ib);
        assert!(!p.is_empty());
        let paths: Vec<_> = p.items().iter().map(|i| i.path.clone()).collect();
        assert_eq!(paths, [a, b]);
        assert_eq!(p.get(ib).unwrap().source, ItemSource::Saved);
        assert_eq!(p.get(ia).unwrap().added_unix, 10);
        assert!(p.get(999).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn same_path_added_twice_is_one_item() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let f = real.join("f.txt");
        std::fs::write(&f, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut p = Pocket::default();
        assert!(p.add(f.clone(), ItemSource::Reference, 1).is_some());
        assert!(p.add(f, ItemSource::Reference, 2).is_none());
        assert!(
            p.add(link.join("f.txt"), ItemSource::Reference, 3)
                .is_none()
        );
        assert_eq!(p.items().len(), 1);
    }

    #[test]
    fn remove_and_clear_return_removed_items() {
        let mut p = Pocket::default();
        let ids: Vec<u64> = (0..4)
            .map(|n| {
                p.add(PathBuf::from(format!("/no/such/{n}")), ItemSource::Kept, n)
                    .unwrap()
            })
            .collect();
        let removed = p.remove(&[ids[1], ids[2], 999]);
        assert_eq!(
            removed.iter().map(|i| i.id).collect::<Vec<_>>(),
            [ids[1], ids[2]]
        );
        assert_eq!(p.items().len(), 2);
        let cleared = p.clear();
        assert_eq!(
            cleared.iter().map(|i| i.id).collect::<Vec<_>>(),
            [ids[0], ids[3]]
        );
        assert!(p.is_empty());
    }

    #[test]
    fn missing_reports_deleted_and_moved_files() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for f in [&a, &b, &c] {
            std::fs::write(f, "x").unwrap();
        }
        let mut p = Pocket::default();
        let ia = p.add(a.clone(), ItemSource::Reference, 1).unwrap();
        let ib = p.add(b.clone(), ItemSource::Reference, 1).unwrap();
        p.add(c, ItemSource::Reference, 1).unwrap();
        assert!(p.missing().is_empty());
        std::fs::remove_file(&a).unwrap();
        std::fs::rename(&b, dir.path().join("b-moved")).unwrap();
        assert_eq!(p.missing(), [ia, ib]);
    }

    #[test]
    fn roundtrip_keeps_items_and_next_id() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("pocket.json");
        let mut p = Pocket::default();
        let i0 = p
            .add(PathBuf::from("/no/such/0"), ItemSource::Reference, 5)
            .unwrap();
        let i1 = p
            .add(PathBuf::from("/no/such/1"), ItemSource::Kept, 6)
            .unwrap();
        // The highest id is removed: it must not be handed out again.
        p.remove(&[i1]);
        p.save(&file).unwrap();

        let mut loaded = Pocket::load(&file);
        assert_eq!(loaded, p);
        assert_eq!(loaded.items()[0].id, i0);
        let next = loaded
            .add(PathBuf::from("/no/such/2"), ItemSource::Saved, 7)
            .unwrap();
        assert!(next > i0 && next > i1, "{next} vs {i0}, {i1}");
    }

    #[test]
    fn contains_matches_path_and_origin() {
        let dir = tempfile::tempdir().unwrap();
        let (orig, copy, other) = (
            dir.path().join("orig.txt"),
            dir.path().join("kept-orig.txt"),
            dir.path().join("other.txt"),
        );
        for f in [&orig, &copy, &other] {
            std::fs::write(f, "x").unwrap();
        }
        let mut p = Pocket::default();
        assert!(!p.contains(&orig));
        let id = p
            .add_with_origin(copy.clone(), ItemSource::Kept, Some(orig.clone()), 1)
            .unwrap();
        assert_eq!(p.get(id).unwrap().origin.as_deref(), Some(orig.as_path()));
        assert!(p.contains(&copy));
        assert!(p.contains(&orig));
        // Same identity rule as dedupe: a `.` detour is the same file.
        assert!(p.contains(&dir.path().join("./orig.txt")));
        assert!(!p.contains(&other));
        // Neither the copy nor the original can come in a second time.
        assert!(p.add(orig.clone(), ItemSource::Reference, 2).is_none());
        assert!(
            p.add_with_origin(other.clone(), ItemSource::Kept, Some(orig), 3)
                .is_none()
        );
        assert!(p.add(other, ItemSource::Reference, 4).is_some());
    }

    #[test]
    fn holding_finds_items_by_path_and_origin() {
        let dir = tempfile::tempdir().unwrap();
        let (orig, copy, other) = (
            dir.path().join("orig.txt"),
            dir.path().join("kept-orig.txt"),
            dir.path().join("other.txt"),
        );
        for f in [&orig, &copy, &other] {
            std::fs::write(f, "x").unwrap();
        }
        let mut p = Pocket::default();
        let kept = p
            .add_with_origin(copy.clone(), ItemSource::Kept, Some(orig.clone()), 1)
            .unwrap();
        let referenced = p.add(other.clone(), ItemSource::Reference, 2).unwrap();
        assert_eq!(p.holding(&orig), vec![kept]);
        assert_eq!(p.holding(&copy), vec![kept]);
        assert_eq!(p.holding(&dir.path().join("./other.txt")), vec![referenced]);
        assert!(p.holding(&dir.path().join("gone.txt")).is_empty());
    }

    #[test]
    fn origin_roundtrips_and_old_files_load() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pocket.json");
        let mut p = Pocket::default();
        p.add_with_origin(
            PathBuf::from("/no/such/kept/a"),
            ItemSource::Kept,
            Some(PathBuf::from("/no/such/a")),
            1,
        )
        .unwrap();
        p.add(PathBuf::from("/no/such/b"), ItemSource::Reference, 2)
            .unwrap();
        p.save(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.matches("origin").count(), 1, "{text}");
        assert!(text.contains("\"version\": 1") || text.contains("\"version\":1"));
        assert_eq!(Pocket::load(&file), p);

        // A version 1 file from before origins existed.
        std::fs::write(
            &file,
            r#"{"version":1,"next_id":1,"items":[{"id":0,"path":"/no/such/c","source":{"kind":"reference"},"added_unix":0}]}"#,
        )
        .unwrap();
        let old = Pocket::load(&file);
        assert_eq!(old.items().len(), 1);
        assert_eq!(old.items()[0].origin, None);
        assert!(!bak_of(&file).exists());
    }

    #[test]
    fn load_missing_is_empty_and_silent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pocket.json");
        assert!(Pocket::load(&file).is_empty());
        assert!(!bak_of(&file).exists());
    }

    #[test]
    fn corrupt_file_loads_empty_and_is_kept_as_bak() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pocket.json");
        std::fs::write(bak_of(&file), "older").unwrap();
        std::fs::write(&file, "{nope").unwrap();
        assert!(Pocket::load(&file).is_empty());
        assert_eq!(std::fs::read_to_string(bak_of(&file)).unwrap(), "{nope");
        assert!(!file.exists());
    }

    #[test]
    fn newer_version_loads_empty_and_is_kept_as_bak() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pocket.json");
        let text = r#"{"version": 2, "next_id": 3, "items": []}"#;
        std::fs::write(&file, text).unwrap();
        assert!(Pocket::load(&file).is_empty());
        assert_eq!(std::fs::read_to_string(bak_of(&file)).unwrap(), text);
        assert!(!file.exists());
    }

    #[test]
    fn save_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pocket.json");
        let mut p = Pocket::default();
        p.add(PathBuf::from("/no/such/0"), ItemSource::Reference, 1)
            .unwrap();
        p.save(&file).unwrap();
        p.save(&file).unwrap();
        assert!(!dir.path().join("pocket.json.tmp").exists());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
    }
}
