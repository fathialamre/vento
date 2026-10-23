//! The clipboard history behind the island: records copies as they happen,
//! keeps their data on disk and puts them back on the clipboard.
//!
//! A copy is noticed by the clipboard's change count, which rides on the
//! wheel's input samples ([`ClipController::observe`]); the clipboard is only
//! read when that count moved. The snapshot is read on the main thread and
//! shown at once as a new entry; a worker hashes it and writes its blobs.
//! Until that is reported back, the snapshot is held in memory so the entry
//! can already be put back, and only then is a copy of content already in
//! the history folded into one entry.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jiff::tz::TimeZone;
use log::{debug, warn};
use vento_core::clip::{
    ClipSnapshot, MAX_SNAPSHOT_BYTES, Summary, drop_missing_files, fit, image, is_file_reference,
    is_secret, plain_text, shown_picture, snapshot_hash, summarize,
};
use vento_core::clip_history::{
    ClipEntry, ClipHistory, Loaded, clipboard_dir, clipboard_path, has_backup, remove_backup,
};
use vento_core::clip_store::{delete_entry, highest_id, read_entry, sweep, write_entry};
use vento_core::settings::Settings;
use vento_platform::{Dropped, Platform};

use super::clip_thumb::{THUMB_FILE, write_file_thumb, write_thumb};
use super::display_name;

thread_local! {
    /// The controller on the main thread, for blob writes finishing on a
    /// worker thread.
    static MAIN_CLIP: RefCell<Weak<ClipController>> = const { RefCell::new(Weak::new()) };
}

/// What a history row shows as its icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClipKind {
    Text,
    Files,
    Link,
    Image,
}

/// Where a copy's thumbnail comes from.
enum Picture {
    /// The image data that was copied.
    Pasted,
    /// The picture file that was copied (in Finder).
    File(PathBuf),
}

/// One row of the Clipboard view.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClipRowData {
    /// The entry's id.
    pub(super) id: u64,
    pub(super) kind: ClipKind,
    /// One line drawn from the entry's summary.
    pub(super) label: String,
    /// How long ago it was copied: `now`, `5m`, `3h`, `Tue`, `12 Oct`.
    pub(super) age: String,
    /// A file entry whose files are all gone (shown dimmed, not put back).
    pub(super) missing: bool,
    /// The stored image of an Image entry, for its thumbnail.
    pub(super) thumb: Option<PathBuf>,
    /// Kept until removed by hand; pinned rows come first.
    pub(super) pinned: bool,
}

/// Records copies into the clipboard history and puts entries back. Main
/// thread only.
pub(super) struct ClipController {
    platform: Rc<dyn Platform>,
    /// Where the history is kept; `None` when the data folder is unknown,
    /// and then the history stays off and empty.
    store: Option<Store>,
    history: RefCell<ClipHistory>,
    /// Snapshots of new entries whose blobs are still being written.
    pending: RefCell<HashMap<u64, Arc<ClipSnapshot>>>,
    /// Ids taken out by [`remove`](Self::remove) or [`clear`](Self::clear)
    /// that an Undo may still put back: neither restored nor discarded yet.
    held: RefCell<Vec<u64>>,
    /// Hashes of held entries, found by the worker after they were taken
    /// out; a [`restore`](Self::restore) gives them back.
    late_hashes: RefCell<HashMap<u64, u64>>,
    /// The change count at the last sample.
    baseline: Cell<i64>,
    /// A sample has set the baseline since the last launch, resume or
    /// switch on; until then nothing is recorded.
    primed: Cell<bool>,
    /// The change count after vento's own last write, which is no copy.
    own_write: Cell<Option<i64>>,
    paused: Cell<bool>,
    enabled: Cell<bool>,
    /// Called after every change to the history.
    on_change: Box<dyn Fn()>,
}

/// Where the clipboard history lives on disk.
struct Store {
    /// `clipboard.json`.
    json: PathBuf,
    /// The folder holding each entry's blobs.
    dir: PathBuf,
}

impl ClipController {
    /// Loads the history from its usual place; `on_change` is called after
    /// every change to it. Without a known data folder the history stays
    /// off: nothing is loaded, swept or recorded.
    pub(super) fn new(
        platform: Rc<dyn Platform>,
        settings: &RefCell<Settings>,
        on_change: Box<dyn Fn()>,
    ) -> Rc<Self> {
        match (clipboard_path(), clipboard_dir()) {
            (Some(json), Some(dir)) => Self::with_paths(json, dir, platform, settings, on_change),
            _ => {
                warn!("no data folder: the clipboard history stays off");
                Self::build(None, ClipHistory::default(), platform, settings, on_change)
            }
        }
    }

    /// Loads the history from `json`, with the blobs under `dir`. Deletes
    /// every folder there no entry names (left by a crash, or by a write
    /// that finished after its entry left), except while a history file set
    /// aside as unusable is kept: the folders may be its. Then drops the
    /// entries whose folder is gone, as they can never be put back.
    pub(super) fn with_paths(
        json: PathBuf,
        dir: PathBuf,
        platform: Rc<dyn Platform>,
        settings: &RefCell<Settings>,
        on_change: Box<dyn Fn()>,
    ) -> Rc<Self> {
        let (mut history, loaded) = ClipHistory::load(&json);
        debug!("clipboard history loaded: {} entries", history.len());
        // Before anything is recorded: a sweep running alongside a write
        // could delete the fresh folder.
        if loaded == Loaded::SetAside || has_backup(&json) {
            debug!("clipboard: no sweep while a set-aside history is kept");
        } else {
            let ids: Vec<u64> = history.entries().iter().map(|e| e.id).collect();
            sweep(&dir, &ids);
        }
        // Folders the sweep kept for a set-aside history still own their ids.
        if let Some(top) = highest_id(&dir) {
            history.skip_ids_through(top);
        }
        // Saved before its blobs were written (vento quit at once), or
        // removed by hand.
        let dead = history.retain(|e| dir.join(e.id.to_string()).is_dir());
        if !dead.is_empty() {
            warn!(
                "clipboard: {} entries without their data dropped",
                dead.len()
            );
        }
        // Entries recorded before Finder's file references were resolved.
        let resolved = history.rewrite_file_paths(|p| {
            is_file_reference(p)
                .then(|| platform.resolve_file_reference(p))
                .flatten()
        });
        if (!dead.is_empty() || resolved)
            && let Err(e) = history.save(&json)
        {
            warn!(
                "cannot save the clipboard history to {}: {e}",
                json.display()
            );
        }
        Self::build(
            Some(Store { json, dir }),
            history,
            platform,
            settings,
            on_change,
        )
    }

    /// The controller over `history`, kept in `store`.
    fn build(
        store: Option<Store>,
        history: ClipHistory,
        platform: Rc<dyn Platform>,
        settings: &RefCell<Settings>,
        on_change: Box<dyn Fn()>,
    ) -> Rc<Self> {
        let controller = Rc::new(Self {
            platform,
            store,
            history: RefCell::new(history),
            pending: RefCell::new(HashMap::new()),
            held: RefCell::new(Vec::new()),
            late_hashes: RefCell::new(HashMap::new()),
            baseline: Cell::new(0),
            primed: Cell::new(false),
            own_write: Cell::new(None),
            paused: Cell::new(false),
            enabled: Cell::new(settings.borrow().clipboard_history),
            on_change,
        });
        MAIN_CLIP.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        controller
    }

    /// Takes the clipboard's change count from an input sample. While off
    /// or paused, and on the first sample after either, it only becomes the
    /// baseline; otherwise a count that moved, and is not vento's own write,
    /// is a copy to record.
    pub(super) fn observe(&self, count: i64) {
        let before = self.baseline.replace(count);
        if !self.enabled() || self.paused.get() || !self.primed.replace(true) {
            return;
        }
        if count == before || self.own_write.get() == Some(count) {
            return;
        }
        self.record_clipboard(count);
    }

    /// Reads the clipboard, whose change count was `count` when sampled,
    /// and records it, unless it is secret, too big or holds nothing worth
    /// showing.
    fn record_clipboard(&self, count: i64) {
        // Type names only: a secret's data is never read.
        if is_secret(&self.platform.clipboard_types()) {
            debug!("clipboard: secret copy not recorded");
            return;
        }
        let read = self.platform.read_clipboard();
        // The two reads are not one: a copy landing in between, or an app
        // adding its marker after its text, must not slip through.
        if self.platform.clipboard_change_count() != count {
            debug!("clipboard: changed while read, left to the next sample");
            return;
        }
        let types: Vec<String> = read.items.iter().flatten().map(|r| r.ty.clone()).collect();
        if is_secret(&types) {
            debug!("clipboard: secret copy not recorded");
            return;
        }
        let Some(snapshot) = fit(read, MAX_SNAPSHOT_BYTES) else {
            debug!("clipboard: copy too big, not recorded");
            return;
        };
        let Some(summary) = summarize(&snapshot) else {
            debug!("clipboard: nothing to show in the copy, not recorded");
            return;
        };
        // Finder copies files as references: keep their real paths, which
        // the list names and the Pocket takes.
        let summary = match summary {
            Summary::Files { paths } => Summary::Files {
                paths: paths.iter().map(|p| self.real_path(p)).collect(),
            },
            other => other,
        };
        let picture = match &summary {
            Summary::Image { .. } => Some(Picture::Pasted),
            other => shown_picture(other).map(|p| Picture::File(p.to_path_buf())),
        };
        let types = snapshot
            .items
            .iter()
            .map(|item| item.iter().map(|r| r.ty.clone()).collect())
            .collect();
        // Added without its hash: hashing scales with the copy, so it is
        // the worker's job, and a duplicate is folded in once it is known.
        let (id, evicted) = self
            .history
            .borrow_mut()
            .insert_new(summary, types, now_unix());
        debug!("clipboard: recorded entry {id}");
        let snapshot = Arc::new(snapshot);
        self.pending.borrow_mut().insert(id, snapshot.clone());
        self.spawn_write(id, snapshot, evicted, picture);
        (self.on_change)();
    }

    /// Hashes entry `id`'s snapshot and writes its blobs on a worker
    /// thread, with a thumbnail of its `picture` beside them, then deletes
    /// the folders of `evicted`; the hash, or the failure, is reported back
    /// on the main thread.
    fn spawn_write(
        &self,
        id: u64,
        snapshot: Arc<ClipSnapshot>,
        evicted: Vec<ClipEntry>,
        picture: Option<Picture>,
    ) {
        let evicted: Vec<u64> = evicted.iter().map(|e| e.id).collect();
        {
            let mut pending = self.pending.borrow_mut();
            for old in &evicted {
                pending.remove(old);
            }
        }
        // Always there: nothing is recorded without a store.
        let Some(dir) = self.dir().map(Path::to_path_buf) else {
            return;
        };
        let gone = evicted.clone();
        let spawned = std::thread::Builder::new()
            .name("vento-clip-store".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let hash = snapshot_hash(&snapshot);
                    let written = write_entry(&dir, id, &snapshot)
                        .map(|()| hash)
                        .map_err(|e| e.to_string());
                    // Here, not on the main thread: decoding scales with
                    // the picture. Without a thumbnail the row shows an icon.
                    if written.is_ok()
                        && let Some(picture) = &picture
                    {
                        let folder = dir.join(id.to_string());
                        let thumb = match picture {
                            Picture::Pasted => write_thumb(&folder, &snapshot),
                            Picture::File(path) => write_file_thumb(&folder, path),
                        };
                        if let Err(e) = thumb {
                            debug!("clipboard: no thumbnail for entry {id}: {e}");
                        }
                    }
                    for old in gone {
                        delete_entry(&dir, old);
                    }
                    written
                }))
                .unwrap_or_else(|_| Err("internal error".into()));
                let delivered = slint::invoke_from_event_loop(move || {
                    if let Some(c) = MAIN_CLIP.with(|slot| slot.borrow().upgrade()) {
                        c.stored(id, result);
                    }
                });
                if let Err(e) = delivered {
                    debug!("cannot report stored clipboard entry {id}: {e}");
                }
            });
        if let Err(e) = spawned {
            for old in evicted {
                self.delete(old);
            }
            self.stored(id, Err(format!("cannot start writing: {e}")));
        }
    }

    /// Entry `id`'s snapshot was hashed and its blobs written, or they
    /// failed to be: an entry whose data could not be stored can never be
    /// put back, so it goes. A stored entry whose content the history
    /// already held is folded into one with it ([`ClipHistory::set_hash`]),
    /// and the folder of the one that goes is deleted. A stored image is
    /// reported as a change: its thumbnail can be read now.
    fn stored(&self, id: u64, result: Result<u64, String>) {
        self.pending.borrow_mut().remove(&id);
        let hash = match result {
            Ok(hash) => hash,
            Err(e) => {
                warn!("cannot store clipboard entry {id}: {e}");
                let removed = self.history.borrow_mut().remove(&[id]);
                self.save();
                if !removed.is_empty() {
                    (self.on_change)();
                }
                return;
            }
        };
        if self.history.borrow().get(id).is_none() {
            if self.held.borrow().contains(&id) {
                // Taken out meanwhile; an Undo may still bring it back.
                self.late_hashes.borrow_mut().insert(id, hash);
            } else {
                // Gone for good while it was being written.
                self.delete(id);
            }
            return;
        }
        let twin = self.history.borrow_mut().set_hash(id, hash);
        if let Some(twin) = &twin {
            debug!("clipboard: entry {} was copied again", twin.id);
            self.delete(twin.id);
        }
        self.save();
        let image = self.history.borrow().get(id).is_some_and(|e| {
            matches!(e.summary, Summary::Image { .. }) || shown_picture(&e.summary).is_some()
        });
        // A picture's thumbnail is there now.
        if twin.is_some() || image {
            (self.on_change)();
        }
    }

    /// vento paused or resumed. Either way the next sample only sets the
    /// baseline, so the copy made while paused is not recorded.
    pub(super) fn set_paused(&self, paused: bool) {
        self.paused.set(paused);
        self.primed.set(false);
    }

    /// The history was switched on or off. Either way the next sample only
    /// sets the baseline, so the copy made while off is not recorded.
    pub(super) fn set_enabled(&self, on: bool) {
        self.enabled.set(on);
        self.primed.set(false);
    }

    /// Whether the history records copies: switched on, and with a place
    /// to keep them.
    pub(super) fn enabled(&self) -> bool {
        self.enabled.get() && self.store.is_some()
    }

    /// How many entries the history holds.
    pub(super) fn len(&self) -> usize {
        self.history.borrow().len()
    }

    /// Whether the history holds nothing.
    pub(super) fn is_empty(&self) -> bool {
        self.history.borrow().is_empty()
    }

    /// Puts entry `id` back on the clipboard as it was copied (files that
    /// are gone left out) and moves it to the top. Returns whether anything
    /// was written: not for an unknown entry, one whose data can't be read,
    /// or one whose files are all gone.
    pub(super) fn put_back(&self, id: u64) -> bool {
        let Some((entry, snapshot)) = self.snapshot(id) else {
            return false;
        };
        let snapshot = match entry.summary {
            Summary::Files { .. } => {
                Arc::new(drop_missing_files(ClipSnapshot::clone(&snapshot), |p| {
                    self.real_path(p).exists()
                }))
            }
            _ => snapshot,
        };
        if snapshot.items.is_empty() {
            debug!("clipboard: entry {id} has nothing left to put back");
            return false;
        }
        match self.platform.write_clipboard(&snapshot) {
            Ok(count) => self.own_write.set(Some(count)),
            Err(e) => {
                warn!("cannot put clipboard entry {id} back: {e}");
                return false;
            }
        }
        self.history.borrow_mut().touch(id, now_unix());
        self.save();
        (self.on_change)();
        true
    }

    /// What entry `id` adds to the Pocket: its files still there, its whole
    /// text, its link or its image. `None` for an unknown entry, one whose
    /// files are all gone, or when its data can't be read.
    pub(super) fn to_pocket(&self, id: u64) -> Option<Dropped> {
        let entry = self.history.borrow().get(id).cloned()?;
        match entry.summary {
            Summary::Files { paths } => {
                let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
                (!paths.is_empty()).then_some(Dropped::Files(paths))
            }
            Summary::Link { url } => Some(Dropped::Url(url)),
            Summary::Text { .. } => Some(Dropped::Text(plain_text(&self.snapshot(id)?.1)?)),
            Summary::Image { .. } => {
                let (bytes, ext) = image(&self.snapshot(id)?.1)?;
                Some(Dropped::Image { bytes, ext })
            }
        }
    }

    /// Entry `id` with its snapshot: the one in memory while its blobs are
    /// being written, otherwise read from disk.
    fn snapshot(&self, id: u64) -> Option<(ClipEntry, Arc<ClipSnapshot>)> {
        let entry = self.history.borrow().get(id).cloned()?;
        if let Some(s) = self.pending.borrow().get(&id) {
            return Some((entry, s.clone()));
        }
        match read_entry(self.dir()?, &entry) {
            Ok(s) => Some((entry, Arc::new(s))),
            Err(e) => {
                warn!("cannot read clipboard entry {id}: {e}");
                None
            }
        }
    }

    /// Takes the entries with these ids out and returns them, for an Undo
    /// to [`restore`](Self::restore) or [`discard`](Self::discard).
    pub(super) fn remove(&self, ids: &[u64]) -> Vec<ClipEntry> {
        let removed = self.history.borrow_mut().remove(ids);
        self.took(&removed);
        removed
    }

    /// Empties the history except for pinned entries and returns what it
    /// took, as [`remove`](Self::remove) does.
    pub(super) fn clear(&self) -> Vec<ClipEntry> {
        let removed = self.history.borrow_mut().clear_unpinned();
        self.took(&removed);
        removed
    }

    /// Pins entry `id` (kept until removed by hand) or unpins it, saves and
    /// reports the change.
    pub(super) fn set_pinned(&self, id: u64, pinned: bool) {
        let changed = {
            let mut history = self.history.borrow_mut();
            history.get(id).is_some_and(|e| e.pinned != pinned) && history.set_pinned(id, pinned)
        };
        if changed {
            self.save();
            (self.on_change)();
        }
    }

    /// Holds `removed` for an Undo, saves and reports the change.
    fn took(&self, removed: &[ClipEntry]) {
        if removed.is_empty() {
            return;
        }
        debug!("clipboard: removed {} entries", removed.len());
        self.held.borrow_mut().extend(removed.iter().map(|e| e.id));
        self.save();
        (self.on_change)();
    }

    /// Puts removed entries back (an Undo). An entry whose content was
    /// copied again meanwhile is already there, so it is dropped instead;
    /// it and whatever the cap then pushes out lose their blobs.
    pub(super) fn restore(&self, entries: Vec<ClipEntry>) {
        let entries: Vec<ClipEntry> = {
            let mut late = self.late_hashes.borrow_mut();
            entries
                .into_iter()
                .map(|mut e| {
                    if let Some(hash) = late.remove(&e.id) {
                        e.hash = Some(hash);
                    }
                    e
                })
                .collect()
        };
        self.release(&entries);
        // An entry whose hash is still unknown matches nothing.
        let (copied_again, back): (Vec<ClipEntry>, Vec<ClipEntry>) = {
            let history = self.history.borrow();
            entries
                .into_iter()
                .partition(|e| e.hash.is_some_and(|h| history.has_hash(h)))
        };
        debug!(
            "clipboard: restored {} entries, {} copied again meanwhile",
            back.len(),
            copied_again.len()
        );
        let evicted = self.history.borrow_mut().restore(back);
        self.delete_unless_back(copied_again.iter().chain(&evicted).map(|e| e.id));
        self.save();
        (self.on_change)();
    }

    /// Lets go of removed entries for good (their Undo ended): their blobs
    /// are deleted, unless an entry is back in the history. Letting go of
    /// the last ones (a Clear, or × on the last row) forgets the history
    /// file set aside, if any, too.
    pub(super) fn discard(&self, entries: Vec<ClipEntry>) {
        self.release(&entries);
        let any = !entries.is_empty();
        self.delete_unless_back(entries.iter().map(|e| e.id));
        if any && self.is_empty() {
            self.forget_backup();
        }
    }

    /// Discards whatever an Undo still holds. Call when vento quits.
    pub(super) fn flush(&self) {
        self.late_hashes.borrow_mut().clear();
        let held = self.held.take();
        if held.is_empty() {
            return;
        }
        debug!("clipboard: {} held entries deleted at quit", held.len());
        self.delete_unless_back(held);
        if self.is_empty() {
            self.forget_backup();
        }
    }

    /// `entries` are no longer held for an Undo.
    fn release(&self, entries: &[ClipEntry]) {
        self.held
            .borrow_mut()
            .retain(|id| !entries.iter().any(|e| e.id == *id));
        let mut late = self.late_hashes.borrow_mut();
        for e in entries {
            late.remove(&e.id);
        }
    }

    /// Deletes the blobs (on disk and in memory) of each of `ids` that is
    /// not in the history.
    fn delete_unless_back(&self, ids: impl IntoIterator<Item = u64>) {
        for id in ids {
            if self.history.borrow().get(id).is_some() {
                continue;
            }
            self.pending.borrow_mut().remove(&id);
            self.delete(id);
        }
    }

    /// The rows of the Clipboard view, newest first, aged against
    /// `now_unix` in the local time zone.
    pub(super) fn rows(&self, now_unix: u64) -> Vec<ClipRowData> {
        let Some(dir) = self.dir() else {
            return Vec::new();
        };
        build_rows(
            self.history.borrow().entries(),
            now_unix,
            |p| self.real_path(p).exists(),
            dir,
        )
    }

    /// `path`, or the file it refers to when it is a file reference.
    fn real_path(&self, path: &Path) -> PathBuf {
        if is_file_reference(path) {
            self.platform
                .resolve_file_reference(path)
                .unwrap_or_else(|| path.to_path_buf())
        } else {
            path.to_path_buf()
        }
    }

    /// Deletes the history file an unusable one was set aside as, if any
    /// (it holds previews of old copies), so the next launch sweeps again.
    pub(super) fn forget_backup(&self) {
        if let Some(store) = &self.store {
            remove_backup(&store.json);
        }
    }

    /// The folder holding each entry's blobs.
    fn dir(&self) -> Option<&Path> {
        self.store.as_ref().map(|s| s.dir.as_path())
    }

    /// Deletes entry `id`'s blobs.
    fn delete(&self, id: u64) {
        if let Some(dir) = self.dir() {
            delete_entry(dir, id);
        }
    }

    fn save(&self) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(e) = self.history.borrow().save(&store.json) {
            warn!(
                "cannot save the clipboard history to {}: {e}",
                store.json.display()
            );
        }
    }
}

/// Seconds since the Unix epoch, now.
pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The rows for `entries`, pinned ones first, each part newest first:
/// `exists` says whether a file is still there, and `dir` holds the blobs
/// (for image thumbnails). Ages are in the local time zone.
fn build_rows(
    entries: &[ClipEntry],
    now_unix: u64,
    exists: impl Fn(&Path) -> bool,
    dir: &Path,
) -> Vec<ClipRowData> {
    let tz = TimeZone::system();
    let (pinned, rest): (Vec<&ClipEntry>, Vec<&ClipEntry>) = entries.iter().partition(|e| e.pinned);
    pinned
        .into_iter()
        .chain(rest)
        .map(|e| {
            let (kind, label, missing, thumb) = match &e.summary {
                Summary::Text { preview } => (ClipKind::Text, preview.clone(), false, None),
                // Picture files (a photo copied in Finder) show as the
                // picture, with a thumbnail made from the file.
                Summary::Files { paths } => {
                    let picture = shown_picture(&e.summary).is_some();
                    (
                        if picture {
                            ClipKind::Image
                        } else {
                            ClipKind::Files
                        },
                        files_label(paths),
                        !paths.is_empty() && paths.iter().all(|p| !exists(p)),
                        picture.then(|| dir.join(e.id.to_string()).join(THUMB_FILE)),
                    )
                }
                Summary::Link { url } => (ClipKind::Link, link_label(url), false, None),
                Summary::Image { width, height, .. } => (
                    ClipKind::Image,
                    if *width == 0 || *height == 0 {
                        "Image".to_owned()
                    } else {
                        format!("Image {width}×{height}")
                    },
                    false,
                    Some(dir.join(e.id.to_string()).join(THUMB_FILE)),
                ),
            };
            ClipRowData {
                id: e.id,
                kind,
                label,
                age: age_label(e.copied_unix, now_unix, tz.clone()),
                missing,
                thumb,
                pinned: e.pinned,
            }
        })
        .collect()
}

/// The first file's name, then `", <second>"` for exactly two, or `" +N"`
/// for N more.
fn files_label(paths: &[PathBuf]) -> String {
    let Some(first) = paths.first() else {
        return String::new();
    };
    let first = display_name(first);
    match paths {
        [_, second] => format!("{first}, {}", display_name(second)),
        [_, _, ..] => format!("{first} +{}", paths.len() - 1),
        _ => first,
    }
}

/// `url` without an `https://` or `http://` scheme.
fn link_label(url: &str) -> String {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .to_owned()
}

/// How long ago `copied_unix` was at `now_unix`: `now` under a minute,
/// then `5m`, `3h`, the weekday in `tz` within a week, else the date
/// (`12 Oct`).
fn age_label(copied_unix: u64, now_unix: u64, tz: TimeZone) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let secs = now_unix.saturating_sub(copied_unix);
    if secs < MINUTE {
        return "now".to_owned();
    }
    if secs < HOUR {
        return format!("{}m", secs / MINUTE);
    }
    if secs < DAY {
        return format!("{}h", secs / HOUR);
    }
    let Some(at) = i64::try_from(copied_unix)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
    else {
        return String::new();
    };
    let format = if secs < 7 * DAY { "%a" } else { "%-d %b" };
    at.to_zoned(tz).strftime(format).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use raw_window_handle::RawWindowHandle;
    use vento_core::clip::{ClipRep, TYPE_FILE_URL, TYPE_PNG, TYPE_TEXT, TYPE_URL, snapshot_hash};
    use vento_core::clip_history::MAX_ENTRIES;
    use vento_core::geometry::{Point, Rect};
    use vento_platform::{
        AppInfo, DropCatcher, IslandHome, MenuEntry, PlatformError, RgbaImage, TerminalApp,
    };

    /// A clipboard the test sets, with a log of what was written to it.
    #[derive(Default)]
    struct FakeClipPlatform {
        types: RefCell<Vec<String>>,
        snapshot: RefCell<ClipSnapshot>,
        written: RefCell<Vec<ClipSnapshot>>,
        count: Cell<i64>,
        /// The change count the clipboard reports now.
        live: Cell<i64>,
        /// Another copy lands while the data is read: the count moves by one.
        copy_during_read: Cell<bool>,
        /// How often the data (not just the type names) was read.
        data_reads: Cell<usize>,
        /// File references (`/.file/id=…`) and the files they lead to.
        references: RefCell<HashMap<PathBuf, PathBuf>>,
    }

    impl FakeClipPlatform {
        /// Puts `s` on the clipboard, with its own type names plus `extra`.
        fn set(&self, s: ClipSnapshot, extra: &[&str]) {
            let mut types: Vec<String> = s.items.iter().flatten().map(|r| r.ty.clone()).collect();
            types.extend(extra.iter().map(|t| (*t).to_owned()));
            *self.types.borrow_mut() = types;
            *self.snapshot.borrow_mut() = s;
        }
    }

    impl Platform for FakeClipPlatform {
        fn prepare_app(&self) {
            unreachable!()
        }
        fn sample_input(&self) -> vento_core::trigger::InputSample {
            unreachable!()
        }
        fn drag_paths(&self) -> Vec<PathBuf> {
            unreachable!()
        }
        fn screens(&self) -> Vec<Rect> {
            unreachable!()
        }
        fn copy_files(&self, _: &[PathBuf]) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn copy_text(&self, _: &str) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn clipboard_types(&self) -> Vec<String> {
            self.types.borrow().clone()
        }
        fn clipboard_change_count(&self) -> i64 {
            self.live.get()
        }
        fn resolve_file_reference(&self, path: &Path) -> Option<PathBuf> {
            self.references.borrow().get(path).cloned()
        }
        fn read_clipboard(&self) -> ClipSnapshot {
            self.data_reads.set(self.data_reads.get() + 1);
            if self.copy_during_read.get() {
                self.live.set(self.live.get() + 1);
            }
            self.snapshot.borrow().clone()
        }
        fn write_clipboard(&self, snapshot: &ClipSnapshot) -> Result<i64, PlatformError> {
            self.written.borrow_mut().push(snapshot.clone());
            self.count.set(self.count.get() + 100);
            Ok(self.count.get())
        }
        fn terminals(&self) -> Vec<TerminalApp> {
            unreachable!()
        }
        fn open_terminal(&self, _: Option<&str>, _: &Path) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn pick_folder(&self, _: Option<&Path>) -> Option<PathBuf> {
            unreachable!()
        }
        fn pocket_home(&self) -> Option<IslandHome> {
            unreachable!()
        }
        fn make_overlay(&self, _: RawWindowHandle) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn make_non_key(&self, _: RawWindowHandle) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn keep_window_frame(&self, _: RawWindowHandle) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn file_icon(&self, _: &Path, _: u32) -> Option<RgbaImage> {
            unreachable!()
        }
        fn kind_description(&self, _: &Path) -> Option<String> {
            unreachable!()
        }
        fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>> {
            unreachable!()
        }
        fn trash(&self, _: &Path) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn screens_changed(&self) -> bool {
            unreachable!()
        }
        fn on_terminate(&self, _: Box<dyn Fn()>) {
            unreachable!()
        }
        fn activate_app(&self) {
            unreachable!()
        }
        fn deactivate_app(&self) {
            unreachable!()
        }
        fn suspend_app_menu(&self) {
            unreachable!()
        }
        fn restore_app_menu(&self) {
            unreachable!()
        }
        fn watch_outside_clicks(&self, _: Box<dyn Fn()>) {
            unreachable!()
        }
        fn stop_watching_outside_clicks(&self) {
            unreachable!()
        }
        fn scale_factor_at(&self, _: Point) -> f64 {
            unreachable!()
        }
        fn apps_for(&self, _: &Path) -> Vec<AppInfo> {
            unreachable!()
        }
        fn open_paths(&self, _: &[PathBuf]) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn open_with(
            &self,
            _: &[PathBuf],
            _: &Path,
            _: Box<dyn Fn(String) + Send>,
        ) -> Result<(), PlatformError> {
            unreachable!()
        }
        fn reveal_in_finder(&self, _: &[PathBuf]) {
            unreachable!()
        }
        fn context_menu(&self, _: RawWindowHandle, _: Point, _: &[MenuEntry]) -> Option<u32> {
            unreachable!()
        }
    }

    fn rep(ty: &str, data: &[u8]) -> ClipRep {
        ClipRep {
            ty: ty.to_owned(),
            data: data.to_vec(),
        }
    }

    fn text(s: &str) -> ClipSnapshot {
        ClipSnapshot {
            items: vec![vec![rep(TYPE_TEXT, s.as_bytes())]],
        }
    }

    fn files(paths: &[&str]) -> ClipSnapshot {
        ClipSnapshot {
            items: paths
                .iter()
                .map(|p| vec![rep(TYPE_FILE_URL, format!("file://{p}").as_bytes())])
                .collect(),
        }
    }

    /// A controller over a fresh temp folder whose blob folder does not
    /// exist yet; history on unless `enabled` is false.
    fn setup(enabled: bool) -> (tempfile::TempDir, Rc<FakeClipPlatform>, Rc<ClipController>) {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Rc::new(FakeClipPlatform::default());
        let c = launch(tmp.path(), &fake, enabled);
        (tmp, fake, c)
    }

    /// A controller over `clipboard.json` and `clipboard/` in `at`, as
    /// vento launching there.
    fn launch(at: &Path, fake: &Rc<FakeClipPlatform>, enabled: bool) -> Rc<ClipController> {
        let settings = RefCell::new(Settings {
            clipboard_history: enabled,
            ..Settings::default()
        });
        ClipController::with_paths(
            at.join("clipboard.json"),
            at.join("clipboard"),
            fake.clone(),
            &settings,
            Box::new(|| {}),
        )
    }

    /// An input sample carrying change count `n`, which the clipboard
    /// reports too.
    fn tick(fake: &FakeClipPlatform, c: &ClipController, n: i64) {
        fake.live.set(n);
        c.observe(n);
    }

    fn top_id(c: &ClipController) -> u64 {
        c.history.borrow().entries()[0].id
    }

    fn ids(c: &ClipController) -> Vec<u64> {
        c.history.borrow().entries().iter().map(|e| e.id).collect()
    }

    /// Waits up to five seconds for a store worker to make `done` true.
    fn wait_until(done: impl Fn() -> bool) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < until, "store worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Plays the event loop for entry `id`'s store worker: waits for its
    /// folder, then reports the result as the worker would. (Tests have no
    /// event loop, so the worker's own report never arrives.)
    fn report(c: &ClipController, id: u64) {
        let folder = c.dir().unwrap().join(id.to_string());
        wait_until(|| folder.is_dir());
        let snapshot = c.pending.borrow()[&id].clone();
        c.stored(id, Ok(snapshot_hash(&snapshot)));
    }

    fn entry(id: u64, copied_unix: u64, summary: Summary, types: Vec<Vec<String>>) -> ClipEntry {
        ClipEntry {
            id,
            copied_unix,
            hash: Some(id),
            summary,
            types,
            pinned: false,
        }
    }

    #[test]
    fn first_sample_only_primes() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("hello"), &[]);
        tick(&fake, &c, 5);
        assert_eq!(c.len(), 0);
        assert_eq!(fake.data_reads.get(), 0);
        tick(&fake, &c, 6);
        assert_eq!(c.len(), 1);
        // The same count again is no new copy.
        tick(&fake, &c, 6);
        assert_eq!(fake.data_reads.get(), 1);
    }

    #[test]
    fn resume_reprimes() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 7);
        tick(&fake, &c, 8);
        assert_eq!(c.len(), 1);
        c.set_paused(true);
        c.set_paused(false);
        fake.set(text("b"), &[]);
        tick(&fake, &c, 9);
        assert_eq!(c.len(), 1);
        tick(&fake, &c, 10);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn secret_copy_is_not_recorded() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("hunter2"), &["org.nspasteboard.ConcealedType"]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        assert!(c.is_empty());
        // Decided from the type names alone: the data is never read.
        assert_eq!(fake.data_reads.get(), 0);
    }

    #[test]
    fn marker_seen_only_in_the_read_data_is_not_recorded() {
        // The marker arrives between the type check and the read.
        let (_tmp, fake, c) = setup(true);
        let mut s = text("hunter2");
        s.items[0].push(rep("org.nspasteboard.ConcealedType", b""));
        fake.set(s, &[]);
        *fake.types.borrow_mut() = vec![TYPE_TEXT.to_owned()];
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        assert!(c.is_empty());
    }

    #[test]
    fn copy_landing_during_the_read_is_left_to_the_next_tick() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        fake.copy_during_read.set(true);
        tick(&fake, &c, 2);
        assert!(c.is_empty());
        // The next sample carries the newer count and records it.
        fake.copy_during_read.set(false);
        fake.set(text("b"), &[]);
        tick(&fake, &c, 3);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn duplicate_copy_resolves_to_one_row_at_the_top() {
        let (tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let a = top_id(&c);
        report(&c, a);
        fake.set(text("b"), &[]);
        tick(&fake, &c, 3);
        let b = top_id(&c);
        report(&c, b);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 4);
        // Shown at once, before its hash is known.
        let again = top_id(&c);
        assert_eq!(ids(&c), [again, b, a]);
        report(&c, again);
        assert_eq!(ids(&c), [again, b]);
        let dir = tmp.path().join("clipboard");
        assert!(!dir.join(a.to_string()).exists());
        assert!(dir.join(again.to_string()).is_dir());
        let (saved, _) = ClipHistory::load(&tmp.path().join("clipboard.json"));
        assert_eq!(saved.len(), 2);
        assert_eq!(
            saved.get(again).unwrap().hash,
            Some(snapshot_hash(&text("a")))
        );
    }

    #[test]
    fn unique_copy_keeps_its_row() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let a = top_id(&c);
        assert_eq!(c.history.borrow().get(a).unwrap().hash, None);
        report(&c, a);
        assert_eq!(ids(&c), [a]);
        assert_eq!(
            c.history.borrow().get(a).unwrap().hash,
            Some(snapshot_hash(&text("a")))
        );
        assert!(c.pending.borrow().is_empty());
    }

    #[test]
    fn eviction_still_deletes_folders() {
        let (tmp, fake, c) = setup(true);
        tick(&fake, &c, 0);
        let mut first = None;
        for n in 1..=MAX_ENTRIES as i64 + 1 {
            fake.set(text(&format!("n{n}")), &[]);
            tick(&fake, &c, n);
            let id = top_id(&c);
            first.get_or_insert(id);
            report(&c, id);
        }
        assert_eq!(c.len(), MAX_ENTRIES);
        let gone = tmp
            .path()
            .join("clipboard")
            .join(first.unwrap().to_string());
        wait_until(|| !gone.exists());
    }

    #[test]
    fn hash_found_while_held_for_undo_comes_back_with_the_entry() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let a = top_id(&c);
        let removed = c.remove(&[a]);
        report(&c, a);
        c.restore(removed);
        assert_eq!(
            c.history.borrow().get(a).unwrap().hash,
            Some(snapshot_hash(&text("a")))
        );
    }

    #[test]
    fn folder_of_an_entry_gone_while_stored_is_deleted() {
        let (tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let a = top_id(&c);
        // Pushed out while its blobs were being written (not held).
        c.history.borrow_mut().remove(&[a]);
        report(&c, a);
        assert!(!tmp.path().join("clipboard").join(a.to_string()).exists());
    }

    #[test]
    fn set_aside_history_keeps_its_blobs_until_its_backup_goes() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Rc::new(FakeClipPlatform::default());
        let json = tmp.path().join("clipboard.json");
        let bak = tmp.path().join("clipboard.json.bak");
        let old = tmp.path().join("clipboard").join("3");
        std::fs::write(&json, "not json").unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("0-0"), b"old copy").unwrap();

        let c = launch(tmp.path(), &fake, true);
        assert!(c.is_empty());
        assert!(bak.exists());
        assert!(old.join("0-0").exists(), "the set-aside file's blobs stay");
        // A new copy never takes a kept folder's id.
        fake.set(text("new"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let id = top_id(&c);
        assert!(id > 3);
        report(&c, id);
        assert_eq!(c.len(), 1);

        // Next launch: the file reads fine, but its backup is still there.
        let c = launch(tmp.path(), &fake, true);
        assert_eq!(c.len(), 1);
        assert!(old.exists());
        c.forget_backup();
        assert!(!bak.exists());
        // Once the backup is gone the sweep runs again.
        let c = launch(tmp.path(), &fake, true);
        assert_eq!(c.len(), 1);
        assert!(!old.exists());
    }

    #[test]
    fn finder_file_references_become_real_paths() {
        let (tmp, fake, c) = setup(true);
        let real = tmp.path().join("photo.png");
        std::fs::write(&real, b"x").unwrap();
        let reference = PathBuf::from("/.file/id=6571367.6922287");
        fake.references
            .borrow_mut()
            .insert(reference.clone(), real.clone());
        let copy = files(&[reference.to_str().unwrap()]);
        fake.set(copy.clone(), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let id = top_id(&c);
        assert_eq!(
            c.history.borrow().get(id).unwrap().summary,
            Summary::Files {
                paths: vec![real.clone()]
            }
        );
        let row = &c.rows(0)[0];
        assert!(!row.missing);
        assert_eq!(row.label, "photo.png");
        // Put back as Finder wrote it: the reference itself.
        assert!(c.put_back(id));
        assert_eq!(*fake.written.borrow(), vec![copy]);
        // Once the file is gone, the reference leads nowhere.
        fake.references.borrow_mut().clear();
        std::fs::remove_file(&real).unwrap();
        assert!(c.rows(0)[0].missing);
        assert!(!c.put_back(id));
    }

    #[test]
    fn copied_picture_files_are_shown_as_pictures() {
        let dir = Path::new("/data/clipboard");
        let entries = vec![
            entry(
                1,
                0,
                Summary::Files {
                    paths: vec![PathBuf::from("/x/Photo.jpg")],
                },
                vec![],
            ),
            entry(
                2,
                0,
                Summary::Files {
                    paths: vec![PathBuf::from("/x/a.png"), PathBuf::from("/x/b.txt")],
                },
                vec![],
            ),
        ];
        let rows = build_rows(&entries, 0, |_| true, dir);
        assert_eq!(rows[0].kind, ClipKind::Image);
        assert_eq!(rows[0].label, "Photo.jpg");
        assert_eq!(rows[0].thumb, Some(dir.join("1").join("thumb.png")));
        assert_eq!(rows[1].kind, ClipKind::Files);
        assert_eq!(rows[1].thumb, None);
        // Gone, it is dimmed like any file.
        assert!(build_rows(&entries, 0, |_| false, dir)[0].missing);
    }

    #[test]
    fn pinned_entries_survive_clear_and_lead_the_rows() {
        let (_tmp, fake, c) = setup(true);
        tick(&fake, &c, 1);
        for (n, s) in ["a", "b", "c"].into_iter().enumerate() {
            fake.set(text(s), &[]);
            tick(&fake, &c, n as i64 + 2);
        }
        let ids: Vec<u64> = c.history.borrow().entries().iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 3);
        let (newest, middle) = (ids[0], ids[1]);
        c.set_pinned(middle, true);
        let rows = c.rows(0);
        let order: Vec<(u64, bool)> = rows.iter().map(|r| (r.id, r.pinned)).collect();
        assert_eq!(order, [(middle, true), (newest, false), (ids[2], false)]);

        let removed: Vec<u64> = c.clear().iter().map(|e| e.id).collect();
        assert_eq!(removed, [newest, ids[2]]);
        assert_eq!(c.len(), 1);
        c.set_pinned(middle, false);
        assert!(!c.rows(0)[0].pinned);
    }

    #[test]
    fn letting_go_of_the_last_entries_removes_the_backup() {
        let (tmp, fake, c) = setup(true);
        let bak = tmp.path().join("clipboard.json.bak");
        std::fs::write(&bak, "old previews").unwrap();
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        report(&c, top_id(&c));
        let removed = c.clear();
        // Held for the Undo: nothing is forgotten yet.
        assert!(bak.exists());
        c.discard(removed);
        assert!(!bak.exists());
    }

    #[test]
    fn entry_without_its_folder_is_dropped_at_launch() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Rc::new(FakeClipPlatform::default());
        let json = tmp.path().join("clipboard.json");
        let text_entry = |id| {
            entry(
                id,
                id,
                Summary::Text {
                    preview: "x".into(),
                },
                vec![vec![TYPE_TEXT.to_owned()]],
            )
        };
        let mut h = ClipHistory::default();
        h.restore(vec![text_entry(1), text_entry(2)]);
        h.save(&json).unwrap();
        std::fs::create_dir_all(tmp.path().join("clipboard").join("1")).unwrap();

        let c = launch(tmp.path(), &fake, true);
        assert_eq!(ids(&c), [1]);
        // Saved, so the dead row is gone from the file too.
        assert_eq!(ClipHistory::load(&json).0.len(), 1);
    }

    #[test]
    fn unknown_data_folder_keeps_the_history_off() {
        let fake = Rc::new(FakeClipPlatform::default());
        let settings = RefCell::new(Settings::default());
        let c = ClipController::build(
            None,
            ClipHistory::default(),
            fake.clone(),
            &settings,
            Box::new(|| {}),
        );
        assert!(!c.enabled());
        c.set_enabled(true);
        assert!(!c.enabled());
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        assert!(c.is_empty());
        assert_eq!(fake.data_reads.get(), 0);
        c.forget_backup();
        c.discard(c.clear());
        c.flush();
        assert!(c.rows(0).is_empty());
    }

    #[test]
    fn to_pocket_leaves_missing_files_out() {
        let (tmp, fake, c) = setup(true);
        let here = tmp.path().join("here.txt");
        std::fs::write(&here, b"x").unwrap();
        fake.set(
            files(&[here.to_str().unwrap(), "/nonexistent/gone.txt"]),
            &[],
        );
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let id = top_id(&c);
        assert_eq!(c.to_pocket(id), Some(Dropped::Files(vec![here.clone()])));
        std::fs::remove_file(&here).unwrap();
        assert_eq!(c.to_pocket(id), None);
    }

    #[test]
    fn own_write_is_ignored() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let a = top_id(&c);
        fake.set(text("b"), &[]);
        tick(&fake, &c, 3);
        assert_ne!(top_id(&c), a);

        assert!(c.put_back(a));
        let count = fake.count.get();
        assert_eq!(*fake.written.borrow(), vec![text("a")]);
        let reads = fake.data_reads.get();
        fake.set(text("c"), &[]);
        tick(&fake, &c, count);
        assert_eq!(fake.data_reads.get(), reads);
        assert_eq!(c.len(), 2);
        assert_eq!(top_id(&c), a);
    }

    #[test]
    fn put_back_uses_pending_snapshot() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("hello"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let id = top_id(&c);
        // No event loop in tests: the finished write is never reported, so
        // the snapshot stays pending.
        assert!(c.pending.borrow().contains_key(&id));
        assert!(c.put_back(id));
        assert_eq!(*fake.written.borrow(), vec![text("hello")]);
    }

    #[test]
    fn all_missing_files_put_back_nothing() {
        let (_tmp, fake, c) = setup(true);
        fake.set(files(&["/nonexistent/a"]), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let id = top_id(&c);
        assert!(!c.put_back(id));
        assert!(fake.written.borrow().is_empty());
        assert!(c.rows(0)[0].missing);
    }

    #[test]
    fn rows_labels_and_ages() {
        let now = 1_000_000;
        let dir = Path::new("/data/clipboard");
        let paths = |ps: &[&str]| ps.iter().map(PathBuf::from).collect::<Vec<_>>();
        let entries = vec![
            entry(
                1,
                now - 30,
                Summary::Files {
                    paths: paths(&["/x/a.txt", "/x/b.txt"]),
                },
                vec![],
            ),
            entry(
                2,
                now - 300,
                Summary::Files {
                    paths: paths(&["/x/a.txt", "/x/b.txt", "/x/c.txt"]),
                },
                vec![],
            ),
            entry(
                3,
                now - 3 * 3600,
                Summary::Link {
                    url: "https://example.com/x".into(),
                },
                vec![],
            ),
            entry(
                4,
                now - 59,
                Summary::Text {
                    preview: "hello".into(),
                },
                vec![],
            ),
            entry(
                5,
                now - 60,
                Summary::Image {
                    ext: "png".into(),
                    width: 640,
                    height: 480,
                },
                vec![vec![TYPE_URL.into(), TYPE_PNG.into()]],
            ),
            entry(
                6,
                now,
                Summary::Image {
                    ext: "png".into(),
                    width: 0,
                    height: 0,
                },
                vec![vec![TYPE_PNG.into()]],
            ),
        ];
        // Only b.txt is still there.
        let rows = build_rows(&entries, now, |p| p == Path::new("/x/b.txt"), dir);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "a.txt, b.txt",
                "a.txt +2",
                "example.com/x",
                "hello",
                "Image 640×480",
                "Image"
            ]
        );
        let ages: Vec<&str> = rows.iter().map(|r| r.age.as_str()).collect();
        assert_eq!(ages, ["now", "5m", "3h", "now", "1m", "now"]);
        let kinds: Vec<ClipKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                ClipKind::Files,
                ClipKind::Files,
                ClipKind::Link,
                ClipKind::Text,
                ClipKind::Image,
                ClipKind::Image
            ]
        );
        assert!(rows.iter().all(|r| !r.missing));
        assert_eq!(rows[4].thumb, Some(dir.join("5").join("thumb.png")));
        assert_eq!(rows[0].thumb, None);
        assert_eq!(rows[0].id, 1);
        // Every file gone: missing.
        assert!(build_rows(&entries[..1], now, |_| false, dir)[0].missing);

        let utc = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_second() as u64;
        // A Thursday.
        let now = utc("2026-10-22T12:00:00Z");
        assert_eq!(age_label(now - 2 * 86400, now, TimeZone::UTC), "Tue");
        assert_eq!(age_label(now - 10 * 86400, now, TimeZone::UTC), "12 Oct");
        // A time ahead of now (a clock set back) reads as now.
        assert_eq!(age_label(now + 50, now, TimeZone::UTC), "now");
    }

    #[test]
    fn disabled_records_nothing() {
        let (_tmp, fake, c) = setup(false);
        assert!(!c.enabled());
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        assert!(c.is_empty());
        assert_eq!(fake.data_reads.get(), 0);
        // Turned on: the copy made while off is not recorded.
        c.set_enabled(true);
        assert!(c.enabled());
        tick(&fake, &c, 3);
        assert!(c.is_empty());
        tick(&fake, &c, 4);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn restore_drops_content_copied_again() {
        let (tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        tick(&fake, &c, 1);
        tick(&fake, &c, 2);
        let kept = top_id(&c);
        report(&c, kept);
        // An undone removal of the same content, whose blobs are on disk.
        let folder = tmp.path().join("clipboard").join("40");
        std::fs::create_dir_all(&folder).unwrap();
        let old = ClipEntry {
            hash: Some(snapshot_hash(&text("a"))),
            ..entry(
                40,
                0,
                Summary::Text {
                    preview: "a".into(),
                },
                vec![vec![TYPE_TEXT.into()]],
            )
        };
        c.restore(vec![old]);
        assert_eq!(c.len(), 1);
        assert_eq!(top_id(&c), kept);
        assert!(!folder.exists());
    }

    #[test]
    fn flush_deletes_what_undo_holds() {
        let (tmp, _fake, c) = setup(true);
        let folder = tmp.path().join("clipboard").join("40");
        std::fs::create_dir_all(&folder).unwrap();
        c.restore(vec![entry(
            40,
            0,
            Summary::Text {
                preview: "a".into(),
            },
            vec![vec![TYPE_TEXT.into()]],
        )]);
        assert_eq!(c.len(), 1);
        let removed = c.remove(&[40]);
        assert_eq!(removed.len(), 1);
        assert!(c.is_empty());
        // Still held for Undo.
        assert!(folder.exists());
        c.flush();
        assert!(!folder.exists());
    }
}
