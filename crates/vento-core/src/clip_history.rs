//! The clipboard history: the last copies vento remembers, persisted as JSON.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use log::warn;
use serde::{Deserialize, Serialize};

use crate::clip::Summary;

/// Newest on-disk format this build understands.
const FORMAT_VERSION: u32 = 1;

/// How many copies the history keeps; the oldest fall off.
pub const MAX_ENTRIES: usize = 50;

/// One remembered copy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClipEntry {
    /// Unique within the history, never reused (not even after a restart).
    pub id: u64,
    /// When the content was last copied, in seconds since the Unix epoch.
    pub copied_unix: u64,
    /// Hash of the whole snapshot; two entries with the same hash are one
    /// copy. `None` until it has been worked out off the main thread
    /// ([`ClipHistory::set_hash`]); such an entry never matches another.
    /// Not written to disk while unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<u64>,
    /// What the entry is shown as in the list.
    pub summary: Summary,
    /// The pasteboard type names of each item's representations, in order,
    /// so the blobs saved for the entry can be put back as they were.
    pub types: Vec<Vec<String>>,
}

/// How [`ClipHistory::load`] found the history file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// There was none: the history starts empty.
    Missing,
    /// It was read.
    Read,
    /// It could not be used and was moved aside to `<name>.bak` (or could
    /// not even be moved). The blob folders may belong to it.
    SetAside,
}

/// The remembered copies, newest first.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct ClipHistory {
    entries: Vec<ClipEntry>,
    next_id: u64,
}

/// On-disk shape of the history file.
#[derive(Serialize, Deserialize)]
struct RawHistory {
    version: u32,
    next_id: u64,
    entries: Vec<ClipEntry>,
}

/// `path` with `suffix` appended to its file name (`clipboard.json` -> `clipboard.json.tmp`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

impl ClipHistory {
    /// The entries, newest first.
    pub fn entries(&self) -> &[ClipEntry] {
        &self.entries
    }

    /// Whether the history holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many entries the history holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entry with this id, if any.
    pub fn get(&self, id: u64) -> Option<&ClipEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Adds a copy at the top as a new entry whose hash is not known yet
    /// (see [`set_hash`](Self::set_hash)). Returns its id and the entries
    /// the cap pushed out, oldest last; their blobs can go.
    pub fn insert_new(
        &mut self,
        summary: Summary,
        types: Vec<Vec<String>>,
        now_unix: u64,
    ) -> (u64, Vec<ClipEntry>) {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.entries.insert(
            0,
            ClipEntry {
                id,
                copied_unix: now_unix,
                hash: None,
                summary,
                types,
            },
        );
        (id, self.enforce_cap())
    }

    /// Gives entry `id` its hash, worked out after it was added. When
    /// another entry already has that hash the two are one copy: the one
    /// nearer the top (the later copy, whose row the list already shows
    /// there) stays and the other is removed and returned, so its blobs can
    /// go. `None` when nothing was removed, also for an unknown id.
    pub fn set_hash(&mut self, id: u64, hash: u64) -> Option<ClipEntry> {
        let at = self.entries.iter().position(|e| e.id == id)?;
        self.entries[at].hash = Some(hash);
        let twin = self
            .entries
            .iter()
            .position(|e| e.id != id && e.hash == Some(hash))?;
        Some(self.entries.remove(at.max(twin)))
    }

    /// Whether an entry has this hash (an entry whose hash is not known yet
    /// has none).
    pub fn has_hash(&self, hash: u64) -> bool {
        self.entries.iter().any(|e| e.hash == Some(hash))
    }

    /// Keeps only the entries `keep` accepts and returns the others, in
    /// order. Only the entries go; no blob is touched.
    pub fn retain(&mut self, mut keep: impl FnMut(&ClipEntry) -> bool) -> Vec<ClipEntry> {
        let (kept, dropped) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|e| keep(e));
        self.entries = kept;
        dropped
    }

    /// Never hands out `id`, or any id below it, from now on: blobs of an
    /// entry the history no longer knows may still sit under that id.
    pub fn skip_ids_through(&mut self, id: u64) {
        self.next_id = self.next_id.max(id.saturating_add(1));
    }

    /// Moves the entry to the top and sets its time to `now_unix` (it was
    /// pasted again). `false` when there is no such entry.
    pub fn touch(&mut self, id: u64, now_unix: u64) -> bool {
        let Some(at) = self.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        let mut entry = self.entries.remove(at);
        entry.copied_unix = now_unix;
        self.entries.insert(0, entry);
        true
    }

    /// Removes the entries with these ids and returns them (unknown ids are
    /// ignored). Only the entries go; no blob is touched.
    pub fn remove(&mut self, ids: &[u64]) -> Vec<ClipEntry> {
        let (removed, kept) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|e| ids.contains(&e.id));
        self.entries = kept;
        removed
    }

    /// Removes every entry and returns them.
    pub fn clear(&mut self) -> Vec<ClipEntry> {
        std::mem::take(&mut self.entries)
    }

    /// Puts entries back (an undone removal): each lands by `copied_unix`
    /// so the list stays newest first, entries whose id is already present
    /// are skipped. Returns what the cap then pushes out, oldest last.
    pub fn restore(&mut self, entries: Vec<ClipEntry>) -> Vec<ClipEntry> {
        for entry in entries {
            if self.entries.iter().any(|e| e.id == entry.id) {
                continue;
            }
            // After every entry at least as new, so ties keep the given order.
            let at = self
                .entries
                .partition_point(|e| e.copied_unix >= entry.copied_unix);
            // Never hand out an id that a restored entry already has.
            self.next_id = self.next_id.max(entry.id.saturating_add(1));
            self.entries.insert(at, entry);
        }
        self.enforce_cap()
    }

    /// Loads the history from `path`, and says how. Never fails: a missing
    /// file gives an empty history; an unreadable one, or one written by a
    /// newer version, gives an empty history and is moved aside to
    /// `<name>.bak` (replacing an older backup) so the next save cannot
    /// destroy it.
    pub fn load(path: &Path) -> (ClipHistory, Loaded) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return (ClipHistory::default(), Loaded::Missing);
            }
            Err(e) => {
                warn!("cannot read clipboard history {}: {e}", path.display());
                Self::set_aside(path);
                return (ClipHistory::default(), Loaded::SetAside);
            }
        };
        match Self::parse(&text) {
            Ok(history) => (history, Loaded::Read),
            Err(why) => {
                warn!("ignoring clipboard history {}: {why}", path.display());
                Self::set_aside(path);
                (ClipHistory::default(), Loaded::SetAside)
            }
        }
    }

    /// Writes the history to `path`, creating the parent folder if needed.
    /// The content goes to `<name>.tmp` first and is renamed into place, so
    /// a crash never leaves a truncated file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let raw = RawHistory {
            version: FORMAT_VERSION,
            next_id: self.next_id,
            entries: self.entries.clone(),
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

    /// Drops everything beyond [`MAX_ENTRIES`] and returns it, oldest last.
    fn enforce_cap(&mut self) -> Vec<ClipEntry> {
        if self.entries.len() > MAX_ENTRIES {
            self.entries.split_off(MAX_ENTRIES)
        } else {
            Vec::new()
        }
    }

    fn parse(text: &str) -> Result<ClipHistory, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        match version {
            Some(v) if v > u64::from(FORMAT_VERSION) => {
                return Err(format!("written by a newer version (format {v})"));
            }
            Some(_) => {}
            None => return Err("no format version".to_owned()),
        }
        let raw: RawHistory = serde_json::from_value(value).map_err(|e| e.to_string())?;
        // Never hand out an id that an existing entry already has.
        let floor = raw
            .entries
            .iter()
            .map(|e| e.id.saturating_add(1))
            .max()
            .unwrap_or(0);
        // An entry without a hash was still being stored when the file was
        // written, and vento quit before it was done: its blobs may never
        // have been written. Hashing them here would read up to 20 MB per
        // entry on the main thread at launch, so the copy is let go (the
        // launch sweep removes any folder it has).
        let entries = raw
            .entries
            .into_iter()
            .filter(|e| e.hash.is_some())
            .collect();
        Ok(ClipHistory {
            next_id: raw.next_id.max(floor),
            entries,
        })
    }

    /// Moves a file we cannot use to `<name>.bak`.
    fn set_aside(path: &Path) {
        let bak = with_suffix(path, ".bak");
        if let Err(e) = std::fs::rename(path, &bak) {
            warn!("cannot move {} to {}: {e}", path.display(), bak.display());
        }
    }
}

/// Whether a history file set aside by [`ClipHistory::load`] is still
/// beside `path`.
pub fn has_backup(path: &Path) -> bool {
    with_suffix(path, ".bak").exists()
}

/// Deletes the history file set aside by [`ClipHistory::load`] beside
/// `path`, if there is one: it holds previews of old copies. Failures are
/// logged.
pub fn remove_backup(path: &Path) {
    let bak = with_suffix(path, ".bak");
    if let Err(e) = std::fs::remove_file(&bak)
        && e.kind() != io::ErrorKind::NotFound
    {
        warn!("cannot delete {}: {e}", bak.display());
    }
}

/// Location of the history file; `None` when the user's data folder is
/// unknown (no home directory), and then there is no history.
pub fn clipboard_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "vento").map(|d| d.data_dir().join("clipboard.json"))
}

/// Folder holding the blobs of the remembered copies (`<id>/<item>-<rep>`);
/// `None` when the user's data folder is unknown. Never a relative path:
/// the launch sweep deletes what it finds there.
pub fn clipboard_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "vento").map(|d| d.data_dir().join("clipboard"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Summary {
        Summary::Text {
            preview: s.to_owned(),
        }
    }

    fn types() -> Vec<Vec<String>> {
        vec![vec!["public.utf8-plain-text".to_owned()]]
    }

    /// Records a unique text copy whose hash and time are `n`; returns its
    /// id and what the cap evicted.
    fn rec_evicting(h: &mut ClipHistory, n: u64) -> (u64, Vec<ClipEntry>) {
        let (id, evicted) = h.insert_new(text(&n.to_string()), types(), n);
        assert_eq!(h.set_hash(id, n), None);
        (id, evicted)
    }

    /// [`rec_evicting`], the id alone.
    fn new_id(h: &mut ClipHistory, n: u64) -> u64 {
        rec_evicting(h, n).0
    }

    fn ids(h: &ClipHistory) -> Vec<u64> {
        h.entries().iter().map(|e| e.id).collect()
    }

    fn entry(id: u64, t: u64) -> ClipEntry {
        ClipEntry {
            id,
            copied_unix: t,
            hash: Some(id),
            summary: text("x"),
            types: types(),
        }
    }

    #[test]
    fn insert_puts_newest_first() {
        let mut h = ClipHistory::default();
        assert!(h.is_empty());
        let a = new_id(&mut h, 1);
        let b = new_id(&mut h, 2);
        assert_ne!(a, b);
        assert_eq!(h.len(), 2);
        assert!(!h.is_empty());
        assert_eq!(ids(&h), [b, a]);
        assert_eq!(h.get(a).unwrap().hash, Some(1));
        assert_eq!(h.get(b).unwrap().copied_unix, 2);
        assert_eq!(h.get(b).unwrap().types, types());
        assert!(h.get(999).is_none());
    }

    #[test]
    fn duplicate_resolves_to_one_row_at_the_top() {
        let mut h = ClipHistory::default();
        let a = new_id(&mut h, 1);
        let b = new_id(&mut h, 2);
        let (c, _) = h.insert_new(text("1 again"), types(), 99);
        assert_eq!(ids(&h), [c, b, a]);
        // Its hash is not known yet: it matches nothing.
        assert_eq!(h.get(c).unwrap().hash, None);
        let gone = h.set_hash(c, 1).expect("a duplicate");
        assert_eq!(gone.id, a);
        assert_eq!(ids(&h), [c, b]);
        let top = h.get(c).unwrap();
        assert_eq!((top.hash, top.copied_unix), (Some(1), 99));
    }

    #[test]
    fn duplicate_resolved_out_of_order_keeps_the_later_copy() {
        // Two copies of the same content whose hashes come back in the
        // reverse order: the later copy, on top, stays.
        let mut h = ClipHistory::default();
        let (x, _) = h.insert_new(text("x"), types(), 1);
        let (y, _) = h.insert_new(text("x"), types(), 2);
        assert_eq!(h.set_hash(y, 5), None);
        assert_eq!(h.set_hash(x, 5).map(|e| e.id), Some(x));
        assert_eq!(ids(&h), [y]);
    }

    #[test]
    fn unique_copy_keeps_its_row_and_unknown_hashes_never_match() {
        let mut h = ClipHistory::default();
        let (x, _) = h.insert_new(text("x"), types(), 1);
        let (y, _) = h.insert_new(text("y"), types(), 2);
        assert!(!h.has_hash(7));
        assert_eq!(h.set_hash(y, 7), None);
        assert!(h.has_hash(7));
        assert_eq!(ids(&h), [y, x]);
        assert_eq!(h.get(x).unwrap().hash, None);
        assert_eq!(h.set_hash(999, 7), None);
        assert_eq!(ids(&h), [y, x]);
    }

    #[test]
    fn cap_evicts_oldest() {
        let mut h = ClipHistory::default();
        let first = new_id(&mut h, 0);
        for n in 1..MAX_ENTRIES as u64 {
            assert!(rec_evicting(&mut h, n).1.is_empty());
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        let (id, evicted) = rec_evicting(&mut h, 1000);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, first);
        assert_eq!(h.len(), MAX_ENTRIES);
        assert_eq!(h.entries()[0].id, id);
        assert!(h.get(first).is_none());
    }

    #[test]
    fn ids_are_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.json");
        let mut h = ClipHistory::default();
        let a = new_id(&mut h, 1);
        let b = new_id(&mut h, 2);
        h.remove(&[b]);
        h.save(&path).unwrap();
        let (mut loaded, _) = ClipHistory::load(&path);
        let c = new_id(&mut loaded, 3);
        assert!(c > a && c > b);
    }

    #[test]
    fn next_id_never_falls_below_an_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.json");
        let raw = RawHistory {
            version: 1,
            next_id: 0,
            entries: vec![entry(7, 1)],
        };
        std::fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();
        let (mut h, _) = ClipHistory::load(&path);
        assert!(new_id(&mut h, 2) > 7);
    }

    #[test]
    fn retain_returns_what_it_drops() {
        let mut h = ClipHistory::default();
        let all: Vec<u64> = (0..4).map(|n| new_id(&mut h, n)).collect();
        let dropped = h.retain(|e| e.id % 2 == 0);
        let dropped: Vec<u64> = dropped.iter().map(|e| e.id).collect();
        assert_eq!(dropped, [all[3], all[1]]);
        assert_eq!(ids(&h), [all[2], all[0]]);
    }

    #[test]
    fn skipped_ids_are_never_handed_out() {
        let mut h = ClipHistory::default();
        h.skip_ids_through(41);
        assert_eq!(new_id(&mut h, 1), 42);
        h.skip_ids_through(3); // never lowers
        assert_eq!(new_id(&mut h, 2), 43);
    }

    #[test]
    fn remove_backup_deletes_only_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.json");
        remove_backup(&path); // none yet: fine
        std::fs::write(&path, "{}").unwrap();
        std::fs::write(dir.path().join("clipboard.json.bak"), "old").unwrap();
        assert!(has_backup(&path));
        remove_backup(&path);
        assert!(!has_backup(&path));
        assert!(path.exists());
    }

    #[test]
    fn touch_moves_to_top() {
        let mut h = ClipHistory::default();
        let a = new_id(&mut h, 1);
        let b = new_id(&mut h, 2);
        assert!(h.touch(a, 50));
        assert_eq!(ids(&h), [a, b]);
        assert_eq!(h.get(a).unwrap().copied_unix, 50);
        assert!(!h.touch(999, 60));
        assert_eq!(ids(&h), [a, b]);
    }

    #[test]
    fn remove_and_clear_return_removed_entries() {
        let mut h = ClipHistory::default();
        let all: Vec<u64> = (0..4).map(|n| new_id(&mut h, n)).collect();
        let removed = h.remove(&[all[1], all[3], 999]);
        let removed_ids: Vec<u64> = removed.iter().map(|e| e.id).collect();
        assert_eq!(removed_ids, [all[3], all[1]]);
        assert_eq!(ids(&h), [all[2], all[0]]);
        let cleared = h.clear();
        assert_eq!(cleared.len(), 2);
        assert!(h.is_empty());
    }

    #[test]
    fn restore_reinserts_by_time_and_caps() {
        let mut h = ClipHistory::default();
        for n in [10, 20, 30, 40] {
            new_id(&mut h, n);
        }
        let removed = h.remove(&[1, 2]); // the entries copied at 20 and 30
        assert_eq!(h.len(), 2);
        let evicted = h.restore(removed);
        assert!(evicted.is_empty());
        let times: Vec<u64> = h.entries().iter().map(|e| e.copied_unix).collect();
        assert_eq!(times, [40, 30, 20, 10]);

        // Already-present ids are skipped.
        let again = h.entries()[1].clone();
        assert!(h.restore(vec![again]).is_empty());
        assert_eq!(h.len(), 4);

        // Overflow comes back: fill to the cap, then restore two older ones.
        let mut full = ClipHistory::default();
        for n in 0..MAX_ENTRIES as u64 {
            new_id(&mut full, 100 + n);
        }
        let evicted = full.restore(vec![entry(1000, 1), entry(1001, 2)]);
        assert_eq!(full.len(), MAX_ENTRIES);
        let evicted_ids: Vec<u64> = evicted.iter().map(|e| e.id).collect();
        assert_eq!(evicted_ids, [1001, 1000]); // oldest last
        assert!(full.get(1000).is_none());
    }

    #[test]
    fn restore_keeps_ties_in_given_order_and_ids_stay_fresh() {
        let mut h = ClipHistory::default();
        h.restore(vec![entry(20, 5), entry(21, 5), entry(22, 5)]);
        assert_eq!(ids(&h), [20, 21, 22]);
        assert!(new_id(&mut h, 9) > 22);
    }

    #[test]
    fn corrupt_or_newer_file_is_set_aside() {
        for body in [
            "this is not json",
            r#"{"version": 2, "next_id": 1, "entries": []}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("clipboard.json");
            std::fs::write(&path, body).unwrap();
            let (h, loaded) = ClipHistory::load(&path);
            assert_eq!(loaded, Loaded::SetAside);
            assert!(h.is_empty());
            assert!(!path.exists());
            let bak = dir.path().join("clipboard.json.bak");
            assert_eq!(std::fs::read_to_string(bak).unwrap(), body);
        }
    }

    #[test]
    fn entries_without_a_hash_are_written_without_one_and_dropped_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.json");
        let mut h = ClipHistory::default();
        let kept = new_id(&mut h, 1);
        let (pending, _) = h.insert_new(text("still storing"), types(), 2);
        h.save(&path).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        assert_eq!(json.matches("\"hash\"").count(), 1);
        let (mut loaded, _) = ClipHistory::load(&path);
        assert_eq!(ids(&loaded), [kept]);
        // Its id is still never handed out again.
        assert!(new_id(&mut loaded, 3) > pending);
    }

    #[test]
    fn missing_file_is_an_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        let (h, loaded) = ClipHistory::load(&dir.path().join("clipboard.json"));
        assert_eq!(loaded, Loaded::Missing);
        assert!(h.is_empty());
        assert!(!dir.path().join("clipboard.json.bak").exists());
    }

    #[test]
    fn round_trip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("clipboard.json");
        let mut h = ClipHistory::default();
        let summaries = [
            Summary::Files {
                paths: vec![PathBuf::from("/a/b.txt"), PathBuf::from("/a/c")],
            },
            text("hello"),
            Summary::Link {
                url: "https://example.com/".to_owned(),
            },
            Summary::Image {
                ext: "png".to_owned(),
                width: 640,
                height: 480,
            },
        ];
        for (n, s) in summaries.into_iter().enumerate() {
            let (id, _) = h.insert_new(s, types(), n as u64);
            let hash = u64::MAX - n as u64; // full-range u64 survives JSON
            h.set_hash(id, hash);
        }
        h.save(&path).unwrap();
        assert!(
            !dir.path()
                .join("nested")
                .join("clipboard.json.tmp")
                .exists()
        );
        assert_eq!(ClipHistory::load(&path), (h, Loaded::Read));
    }
}
