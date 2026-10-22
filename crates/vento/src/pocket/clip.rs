//! The clipboard history behind the island: records copies as they happen,
//! keeps their data on disk and puts them back on the clipboard.
//!
//! A copy is noticed by the clipboard's change count, which rides on the
//! wheel's input samples ([`ClipController::observe`]); the clipboard is only
//! read when that count moved. The snapshot is read on the main thread and
//! its blobs are written by a worker; until that write is reported back, the
//! snapshot is held in memory so the entry can already be put back.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jiff::tz::TimeZone;
use log::{debug, warn};
use vento_core::clip::{
    ClipSnapshot, MAX_SNAPSHOT_BYTES, Summary, drop_missing_files, fit, image, is_secret,
    plain_text, snapshot_hash, summarize,
};
use vento_core::clip_history::{ClipEntry, ClipHistory, Recorded, clipboard_dir, clipboard_path};
use vento_core::clip_store::{delete_entry, read_entry, sweep, write_entry};
use vento_core::settings::Settings;
use vento_platform::{Dropped, Platform};

use super::clip_thumb::{THUMB_FILE, write_thumb};
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
}

/// Records copies into the clipboard history and puts entries back. Main
/// thread only.
pub(super) struct ClipController {
    platform: Rc<dyn Platform>,
    /// `clipboard.json`.
    json: PathBuf,
    /// The folder holding each entry's blobs.
    dir: PathBuf,
    history: RefCell<ClipHistory>,
    /// Snapshots of new entries whose blobs are still being written.
    pending: RefCell<HashMap<u64, Arc<ClipSnapshot>>>,
    /// Ids taken out by [`remove`](Self::remove) or [`clear`](Self::clear)
    /// that an Undo may still put back: neither restored nor discarded yet.
    held: RefCell<Vec<u64>>,
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

impl ClipController {
    /// Loads the history from its usual place; `on_change` is called after
    /// every change to it.
    pub(super) fn new(
        platform: Rc<dyn Platform>,
        settings: &RefCell<Settings>,
        on_change: Box<dyn Fn()>,
    ) -> Rc<Self> {
        Self::with_paths(
            clipboard_path(),
            clipboard_dir(),
            platform,
            settings,
            on_change,
        )
    }

    /// Loads the history from `json`, with the blobs under `dir`, and
    /// deletes every folder there no entry names (left by a crash, or by a
    /// write that finished after its entry left).
    pub(super) fn with_paths(
        json: PathBuf,
        dir: PathBuf,
        platform: Rc<dyn Platform>,
        settings: &RefCell<Settings>,
        on_change: Box<dyn Fn()>,
    ) -> Rc<Self> {
        let history = ClipHistory::load(&json);
        debug!("clipboard history loaded: {} entries", history.len());
        // Before anything is recorded: a sweep running alongside a write
        // could delete the fresh folder.
        let ids: Vec<u64> = history.entries().iter().map(|e| e.id).collect();
        sweep(&dir, &ids);
        let controller = Rc::new(Self {
            platform,
            json,
            dir,
            history: RefCell::new(history),
            pending: RefCell::new(HashMap::new()),
            held: RefCell::new(Vec::new()),
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
        if !self.enabled.get() || self.paused.get() || !self.primed.replace(true) {
            return;
        }
        if count == before || self.own_write.get() == Some(count) {
            return;
        }
        self.record_clipboard();
    }

    /// Reads the clipboard and records it, unless it is secret, too big or
    /// holds nothing worth showing.
    fn record_clipboard(&self) {
        // Type names only: a secret's data is never read.
        if is_secret(&self.platform.clipboard_types()) {
            debug!("clipboard: secret copy not recorded");
            return;
        }
        let Some(snapshot) = fit(self.platform.read_clipboard(), MAX_SNAPSHOT_BYTES) else {
            debug!("clipboard: copy too big, not recorded");
            return;
        };
        let Some(summary) = summarize(&snapshot) else {
            debug!("clipboard: nothing to show in the copy, not recorded");
            return;
        };
        let hash = snapshot_hash(&snapshot);
        let picture = matches!(summary, Summary::Image { .. });
        let types = snapshot
            .items
            .iter()
            .map(|item| item.iter().map(|r| r.ty.clone()).collect())
            .collect();
        let recorded = self
            .history
            .borrow_mut()
            .record(hash, summary, types, now_unix());
        match recorded {
            Recorded::New { id, evicted } => {
                debug!("clipboard: recorded entry {id}");
                let snapshot = Arc::new(snapshot);
                self.pending.borrow_mut().insert(id, snapshot.clone());
                self.spawn_write(id, snapshot, evicted, picture);
            }
            Recorded::Bumped { id } => {
                debug!("clipboard: entry {id} copied again");
                self.save();
            }
        }
        (self.on_change)();
    }

    /// Writes entry `id`'s blobs on a worker thread, with a thumbnail beside
    /// them for a `picture`, then deletes the folders of `evicted`; the
    /// result is reported back on the main thread.
    fn spawn_write(
        &self,
        id: u64,
        snapshot: Arc<ClipSnapshot>,
        evicted: Vec<ClipEntry>,
        picture: bool,
    ) {
        let evicted: Vec<u64> = evicted.iter().map(|e| e.id).collect();
        {
            let mut pending = self.pending.borrow_mut();
            for old in &evicted {
                pending.remove(old);
            }
        }
        let dir = self.dir.clone();
        let gone = evicted.clone();
        let spawned = std::thread::Builder::new()
            .name("vento-clip-store".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let written = write_entry(&dir, id, &snapshot).map_err(|e| e.to_string());
                    // Here, not on the main thread: decoding scales with
                    // the picture. Without a thumbnail the row shows an icon.
                    if written.is_ok()
                        && picture
                        && let Err(e) = write_thumb(&dir.join(id.to_string()), &snapshot)
                    {
                        debug!("clipboard: no thumbnail for entry {id}: {e}");
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
                delete_entry(&self.dir, old);
            }
            self.stored(id, Err(format!("cannot start writing: {e}")));
        }
    }

    /// Entry `id`'s blobs were written, or failed to be: an entry whose
    /// data could not be stored can never be put back, so it goes. A stored
    /// image is reported as a change: its thumbnail can be read now.
    fn stored(&self, id: u64, result: Result<(), String>) {
        self.pending.borrow_mut().remove(&id);
        match result {
            Ok(()) => {
                self.save();
                let image = self
                    .history
                    .borrow()
                    .get(id)
                    .is_some_and(|e| matches!(e.summary, Summary::Image { .. }));
                if image {
                    (self.on_change)();
                }
            }
            Err(e) => {
                warn!("cannot store clipboard entry {id}: {e}");
                let removed = self.history.borrow_mut().remove(&[id]);
                self.save();
                if !removed.is_empty() {
                    (self.on_change)();
                }
            }
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
    // Wired to the settings in Task 9 (`apply_clip_settings`).
    #[allow(dead_code)]
    pub(super) fn set_enabled(&self, on: bool) {
        self.enabled.set(on);
        self.primed.set(false);
    }

    /// Whether the history records copies.
    pub(super) fn enabled(&self) -> bool {
        self.enabled.get()
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
            Summary::Files { .. } => Arc::new(drop_missing_files(
                ClipSnapshot::clone(&snapshot),
                Path::exists,
            )),
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

    /// What entry `id` adds to the Pocket: its files, its whole text, its
    /// link or its image. `None` for an unknown entry, or when its data
    /// can't be read.
    pub(super) fn to_pocket(&self, id: u64) -> Option<Dropped> {
        let entry = self.history.borrow().get(id).cloned()?;
        match entry.summary {
            Summary::Files { paths } => Some(Dropped::Files(paths)),
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
        match read_entry(&self.dir, &entry) {
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

    /// Empties the history and returns what it held, as
    /// [`remove`](Self::remove) does.
    pub(super) fn clear(&self) -> Vec<ClipEntry> {
        let removed = self.history.borrow_mut().clear();
        self.took(&removed);
        removed
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
        self.release(&entries);
        let (copied_again, back): (Vec<ClipEntry>, Vec<ClipEntry>) = {
            let history = self.history.borrow();
            entries
                .into_iter()
                .partition(|e| history.entries().iter().any(|h| h.hash == e.hash))
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
    /// are deleted, unless an entry is back in the history.
    pub(super) fn discard(&self, entries: Vec<ClipEntry>) {
        self.release(&entries);
        self.delete_unless_back(entries.iter().map(|e| e.id));
    }

    /// Discards whatever an Undo still holds. Call when vento quits.
    pub(super) fn flush(&self) {
        let held = self.held.take();
        if !held.is_empty() {
            debug!("clipboard: {} held entries deleted at quit", held.len());
        }
        self.delete_unless_back(held);
    }

    /// `entries` are no longer held for an Undo.
    fn release(&self, entries: &[ClipEntry]) {
        self.held
            .borrow_mut()
            .retain(|id| !entries.iter().any(|e| e.id == *id));
    }

    /// Deletes the blobs (on disk and in memory) of each of `ids` that is
    /// not in the history.
    fn delete_unless_back(&self, ids: impl IntoIterator<Item = u64>) {
        for id in ids {
            if self.history.borrow().get(id).is_some() {
                continue;
            }
            self.pending.borrow_mut().remove(&id);
            delete_entry(&self.dir, id);
        }
    }

    /// The rows of the Clipboard view, newest first, aged against
    /// `now_unix` in the local time zone.
    pub(super) fn rows(&self, now_unix: u64) -> Vec<ClipRowData> {
        build_rows(
            self.history.borrow().entries(),
            now_unix,
            Path::exists,
            &self.dir,
        )
    }

    fn save(&self) {
        if let Err(e) = self.history.borrow().save(&self.json) {
            warn!(
                "cannot save the clipboard history to {}: {e}",
                self.json.display()
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

/// The rows for `entries`: `exists` says whether a file is still there, and
/// `dir` holds the blobs (for image thumbnails). Ages are in the local time
/// zone.
fn build_rows(
    entries: &[ClipEntry],
    now_unix: u64,
    exists: impl Fn(&Path) -> bool,
    dir: &Path,
) -> Vec<ClipRowData> {
    let tz = TimeZone::system();
    entries
        .iter()
        .map(|e| {
            let (kind, label, missing, thumb) = match &e.summary {
                Summary::Text { preview } => (ClipKind::Text, preview.clone(), false, None),
                Summary::Files { paths } => (
                    ClipKind::Files,
                    files_label(paths),
                    !paths.is_empty() && paths.iter().all(|p| !exists(p)),
                    None,
                ),
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
        /// How often the data (not just the type names) was read.
        data_reads: Cell<usize>,
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
        fn read_clipboard(&self) -> ClipSnapshot {
            self.data_reads.set(self.data_reads.get() + 1);
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
        let settings = RefCell::new(Settings {
            clipboard_history: enabled,
            ..Settings::default()
        });
        let c = ClipController::with_paths(
            tmp.path().join("clipboard.json"),
            tmp.path().join("clipboard"),
            fake.clone(),
            &settings,
            Box::new(|| {}),
        );
        (tmp, fake, c)
    }

    fn top_id(c: &ClipController) -> u64 {
        c.history.borrow().entries()[0].id
    }

    fn entry(id: u64, copied_unix: u64, summary: Summary, types: Vec<Vec<String>>) -> ClipEntry {
        ClipEntry {
            id,
            copied_unix,
            hash: id,
            summary,
            types,
        }
    }

    #[test]
    fn first_sample_only_primes() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("hello"), &[]);
        c.observe(5);
        assert_eq!(c.len(), 0);
        assert_eq!(fake.data_reads.get(), 0);
        c.observe(6);
        assert_eq!(c.len(), 1);
        // The same count again is no new copy.
        c.observe(6);
        assert_eq!(fake.data_reads.get(), 1);
    }

    #[test]
    fn resume_reprimes() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        c.observe(7);
        c.observe(8);
        assert_eq!(c.len(), 1);
        c.set_paused(true);
        c.set_paused(false);
        fake.set(text("b"), &[]);
        c.observe(9);
        assert_eq!(c.len(), 1);
        c.observe(10);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn secret_copy_is_not_recorded() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("hunter2"), &["org.nspasteboard.ConcealedType"]);
        c.observe(1);
        c.observe(2);
        assert!(c.is_empty());
        // Decided from the type names alone: the data is never read.
        assert_eq!(fake.data_reads.get(), 0);
    }

    #[test]
    fn own_write_is_ignored() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        c.observe(1);
        c.observe(2);
        let a = top_id(&c);
        fake.set(text("b"), &[]);
        c.observe(3);
        assert_ne!(top_id(&c), a);

        assert!(c.put_back(a));
        let count = fake.count.get();
        assert_eq!(*fake.written.borrow(), vec![text("a")]);
        let reads = fake.data_reads.get();
        fake.set(text("c"), &[]);
        c.observe(count);
        assert_eq!(fake.data_reads.get(), reads);
        assert_eq!(c.len(), 2);
        assert_eq!(top_id(&c), a);
    }

    #[test]
    fn put_back_uses_pending_snapshot() {
        let (_tmp, fake, c) = setup(true);
        fake.set(text("hello"), &[]);
        c.observe(1);
        c.observe(2);
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
        c.observe(1);
        c.observe(2);
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
        c.observe(1);
        c.observe(2);
        assert!(c.is_empty());
        assert_eq!(fake.data_reads.get(), 0);
        // Turned on: the copy made while off is not recorded.
        c.set_enabled(true);
        assert!(c.enabled());
        c.observe(3);
        assert!(c.is_empty());
        c.observe(4);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn restore_drops_content_copied_again() {
        let (tmp, fake, c) = setup(true);
        fake.set(text("a"), &[]);
        c.observe(1);
        c.observe(2);
        let kept = top_id(&c);
        // An undone removal of the same content, whose blobs are on disk.
        let folder = tmp.path().join("clipboard").join("40");
        std::fs::create_dir_all(&folder).unwrap();
        let old = ClipEntry {
            hash: snapshot_hash(&text("a")),
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
