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
    /// Hash of the whole snapshot; two copies with the same hash are one entry.
    pub hash: u64,
    /// What the entry is shown as in the list.
    pub summary: Summary,
    /// The pasteboard type names of each item's representations, in order,
    /// so the blobs saved for the entry can be put back as they were.
    pub types: Vec<Vec<String>>,
}

/// What [`ClipHistory::record`] did with a copy.
#[derive(Clone, Debug, PartialEq)]
pub enum Recorded {
    /// A new entry was added at the top.
    New {
        /// Id of the new entry.
        id: u64,
        /// Entries pushed out by the cap, oldest last; their blobs can go.
        evicted: Vec<ClipEntry>,
    },
    /// An entry with the same hash already existed and moved to the top.
    Bumped {
        /// Id of the entry that moved.
        id: u64,
    },
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

    /// Records a copy. An entry with the same `hash` moves to the top with
    /// its time updated and keeps its id; otherwise a new entry is added at
    /// the top and anything beyond [`MAX_ENTRIES`] is evicted.
    pub fn record(
        &mut self,
        hash: u64,
        summary: Summary,
        types: Vec<Vec<String>>,
        now_unix: u64,
    ) -> Recorded {
        if let Some(at) = self.entries.iter().position(|e| e.hash == hash) {
            let mut entry = self.entries.remove(at);
            entry.copied_unix = now_unix;
            let id = entry.id;
            self.entries.insert(0, entry);
            return Recorded::Bumped { id };
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(
            0,
            ClipEntry {
                id,
                copied_unix: now_unix,
                hash,
                summary,
                types,
            },
        );
        Recorded::New {
            id,
            evicted: self.enforce_cap(),
        }
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
            self.next_id = self.next_id.max(entry.id + 1);
            self.entries.insert(at, entry);
        }
        self.enforce_cap()
    }

    /// Loads the history from `path`. Never fails: a missing file gives an
    /// empty history; an unreadable one, or one written by a newer version,
    /// gives an empty history and is moved aside to `<name>.bak` (replacing
    /// an older backup) so the next save cannot destroy it.
    pub fn load(path: &Path) -> ClipHistory {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return ClipHistory::default(),
            Err(e) => {
                warn!("cannot read clipboard history {}: {e}", path.display());
                Self::set_aside(path);
                return ClipHistory::default();
            }
        };
        match Self::parse(&text) {
            Ok(history) => history,
            Err(why) => {
                warn!("ignoring clipboard history {}: {why}", path.display());
                Self::set_aside(path);
                ClipHistory::default()
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
        let floor = raw.entries.iter().map(|e| e.id + 1).max().unwrap_or(0);
        Ok(ClipHistory {
            next_id: raw.next_id.max(floor),
            entries: raw.entries,
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

/// Location of the history file.
pub fn clipboard_path() -> PathBuf {
    match ProjectDirs::from("", "", "vento") {
        Some(dirs) => dirs.data_dir().join("clipboard.json"),
        None => PathBuf::from("clipboard.json"),
    }
}

/// Folder holding the blobs of the remembered copies (`<id>/<item>-<rep>`).
pub fn clipboard_dir() -> PathBuf {
    match ProjectDirs::from("", "", "vento") {
        Some(dirs) => dirs.data_dir().join("clipboard"),
        None => PathBuf::from("clipboard"),
    }
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

    /// Records a text copy whose hash is `n` and whose time is `n`.
    fn rec(h: &mut ClipHistory, n: u64) -> Recorded {
        h.record(n, text(&n.to_string()), types(), n)
    }

    fn ids(h: &ClipHistory) -> Vec<u64> {
        h.entries().iter().map(|e| e.id).collect()
    }

    fn new_id(r: Recorded) -> u64 {
        match r {
            Recorded::New { id, .. } => id,
            other => panic!("expected New, got {other:?}"),
        }
    }

    fn entry(id: u64, t: u64) -> ClipEntry {
        ClipEntry {
            id,
            copied_unix: t,
            hash: id,
            summary: text("x"),
            types: types(),
        }
    }

    #[test]
    fn record_puts_newest_first() {
        let mut h = ClipHistory::default();
        assert!(h.is_empty());
        let a = new_id(rec(&mut h, 1));
        let b = new_id(rec(&mut h, 2));
        assert_ne!(a, b);
        assert_eq!(h.len(), 2);
        assert!(!h.is_empty());
        assert_eq!(ids(&h), [b, a]);
        assert_eq!(h.get(a).unwrap().hash, 1);
        assert_eq!(h.get(b).unwrap().copied_unix, 2);
        assert_eq!(h.get(b).unwrap().types, types());
        assert!(h.get(999).is_none());
    }

    #[test]
    fn same_hash_bumps_instead_of_adding() {
        let mut h = ClipHistory::default();
        let a = new_id(rec(&mut h, 1));
        let b = new_id(rec(&mut h, 2));
        assert_eq!(ids(&h), [b, a]);
        let r = h.record(1, text("other"), types(), 99);
        assert_eq!(r, Recorded::Bumped { id: a });
        assert_eq!(ids(&h), [a, b]);
        assert_eq!(h.get(a).unwrap().copied_unix, 99);
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn cap_evicts_oldest() {
        let mut h = ClipHistory::default();
        let first = new_id(rec(&mut h, 0));
        for n in 1..MAX_ENTRIES as u64 {
            assert!(matches!(
                rec(&mut h, n),
                Recorded::New { evicted, .. } if evicted.is_empty()
            ));
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        let Recorded::New { id, evicted } = rec(&mut h, 1000) else {
            panic!("expected New");
        };
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
        let a = new_id(rec(&mut h, 1));
        let b = new_id(rec(&mut h, 2));
        h.remove(&[b]);
        h.save(&path).unwrap();
        let mut loaded = ClipHistory::load(&path);
        let c = new_id(rec(&mut loaded, 3));
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
        let mut h = ClipHistory::load(&path);
        assert!(new_id(rec(&mut h, 2)) > 7);
    }

    #[test]
    fn touch_moves_to_top() {
        let mut h = ClipHistory::default();
        let a = new_id(rec(&mut h, 1));
        let b = new_id(rec(&mut h, 2));
        assert!(h.touch(a, 50));
        assert_eq!(ids(&h), [a, b]);
        assert_eq!(h.get(a).unwrap().copied_unix, 50);
        assert!(!h.touch(999, 60));
        assert_eq!(ids(&h), [a, b]);
    }

    #[test]
    fn remove_and_clear_return_removed_entries() {
        let mut h = ClipHistory::default();
        let all: Vec<u64> = (0..4).map(|n| new_id(rec(&mut h, n))).collect();
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
            rec(&mut h, n);
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
            rec(&mut full, 100 + n);
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
        assert!(new_id(rec(&mut h, 9)) > 22);
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
            let h = ClipHistory::load(&path);
            assert!(h.is_empty());
            assert!(!path.exists());
            let bak = dir.path().join("clipboard.json.bak");
            assert_eq!(std::fs::read_to_string(bak).unwrap(), body);
        }
    }

    #[test]
    fn missing_file_is_an_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        let h = ClipHistory::load(&dir.path().join("clipboard.json"));
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
            let hash = u64::MAX - n as u64; // full-range u64 survives JSON
            h.record(hash, s, types(), n as u64);
        }
        h.save(&path).unwrap();
        assert!(
            !dir.path()
                .join("nested")
                .join("clipboard.json.tmp")
                .exists()
        );
        assert_eq!(ClipHistory::load(&path), h);
    }
}
