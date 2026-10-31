//! The screenshots the island lists: a watch on the screenshot folder, its
//! listings on a worker thread, whether vento may read the folder, and which
//! screenshots are new.
//!
//! Folder events are gathered for [`COALESCE`] into one listing; a file
//! still being written is listed again after [`SETTLE`]. Only plain data
//! crosses from the worker to the main thread, and a listing started before
//! the folder moved or the feature went off is dropped when it comes back.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{debug, warn};
use slint::{Timer, TimerMode};
use vento_core::screenshots::{
    FileFacts, SETTLE, SHOT_DAYS, Settler, Shot, dims_of, grant_path, is_shot_candidate,
    load_grant, new_shots, recent, save_grant, wait_for_open,
};
use vento_core::settings::Settings;
use vento_platform::{
    FolderWatch, is_protected_folder, is_screen_capture, needs_screen_capture_mark,
    screenshot_folder, watch_folder,
};

/// How long folder events are gathered into one listing.
const COALESCE: Duration = Duration::from_millis(250);
/// How long a listing may run before a [`touch`](ShotsController::touch)
/// gives up on its answer and lists again.
const STUCK: Duration = Duration::from_secs(10);

thread_local! {
    /// The controller on the main thread, for listings and folder events
    /// arriving from other threads.
    static MAIN_SHOTS: RefCell<Weak<ShotsController>> = const { RefCell::new(Weak::new()) };
}

/// Whether vento lists the screenshot folder, and how that went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The feature is off: no watch, no list.
    Off,
    /// A protected folder this build was not let into yet: nothing is
    /// listed until the user opens the tab or the Settings page, so the
    /// system asks then and not at launch.
    WaitForOpen,
    /// A listing is under way whose answer is not known yet.
    Asking,
    /// The last listing worked.
    Granted,
    /// The last listing failed; tried again on the next
    /// [`touch`](ShotsController::touch) or switch on.
    Denied,
}

/// How a listing tells screenshots from other files.
#[derive(Clone, Copy)]
struct Probe {
    /// Whether the system's mark is required.
    needs_mark: bool,
    /// Whether a file carries the mark.
    mark: fn(&Path) -> bool,
}

/// What a listing found: each screenshot (possibly still being written),
/// with its dimensions when it is inside the window.
type Found = io::Result<Vec<(FileFacts, Option<(u32, u32)>)>>;

/// One listing of the folder, made on the worker.
struct Job {
    dir: PathBuf,
    probe: Probe,
    /// The grant file and this build's stamp, to record once the listing
    /// works; for a protected folder only.
    record: Option<(PathBuf, String)>,
}

impl Job {
    /// Lists the folder: the entries that may be screenshots, with their
    /// dimensions. The mark is read only for files that pass every other
    /// test, the dimensions only for those inside the window.
    fn run(self) -> Found {
        let oldest = unix_now().saturating_sub(SHOT_DAYS * 86_400);
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let Ok(entry) = entry else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            let facts = FileFacts {
                path: entry.path(),
                len: meta.len(),
                modified_unix: meta.modified().map_or(0, unix_secs),
                is_file: meta.is_file(),
                screen_capture: true,
            };
            if !is_shot_candidate(&facts, self.probe.needs_mark)
                || (self.probe.needs_mark && !(self.probe.mark)(&facts.path))
            {
                continue;
            }
            let dims = if facts.modified_unix >= oldest {
                dims_of(&facts.path)
            } else {
                None
            };
            found.push((facts, dims));
        }
        if let Some((file, build)) = self.record
            && load_grant(&file).as_deref() != Some(build.as_str())
            && let Err(e) = save_grant(&file, &build)
        {
            warn!("cannot record the screenshot folder grant: {e}");
        }
        Ok(found)
    }
}

/// Runs `job`, turning a panic in it (a library choking on a broken file)
/// into an error, so the listing always has an answer.
fn run_guarded(job: Job) -> Found {
    std::panic::catch_unwind(move || job.run())
        .unwrap_or_else(|_| Err(io::Error::other("the listing stopped on a panic")))
}

/// Seconds since the Unix epoch at `t`; 0 before it.
fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Seconds since the Unix epoch now.
fn unix_now() -> u64 {
    unix_secs(SystemTime::now())
}

/// What tells this build from any other: the version, and the size and
/// modification time of the executable. Just the version when the
/// executable cannot be looked at.
pub(super) fn build_stamp() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe()
        .and_then(std::fs::metadata)
        .and_then(|m| Ok((m.len(), m.modified()?)));
    match exe {
        Ok((len, modified)) => format!("{version}-{len}-{}", unix_secs(modified)),
        Err(e) => {
            debug!("no executable to stamp the build with: {e}");
            version.to_owned()
        }
    }
}

/// A temporary folder standing in for the system's, in tests.
#[cfg(test)]
struct Fake {
    folder: PathBuf,
    protected: bool,
    grant: PathBuf,
    mark: fn(&Path) -> bool,
}

/// Watches the screenshot folder and keeps the list of recent screenshots.
/// Main thread only.
pub(super) struct ShotsController {
    access: Cell<Access>,
    /// The folder listed, as last read from the system.
    folder: RefCell<PathBuf>,
    /// The running watch and the folder it is on.
    watch: RefCell<Option<(PathBuf, FolderWatch)>>,
    /// Goes up when the folder moves or the feature goes off: a listing or
    /// a folder event of an older generation is dropped.
    generation: Cell<u64>,
    /// A listing runs on the worker, since then.
    listing: Cell<Option<Instant>>,
    /// Another listing was asked for while one ran.
    again: Cell<bool>,
    settler: RefCell<Settler>,
    /// The folder's files already settled, or there at its first listing,
    /// which are never new; `None` before the first listing.
    seen: RefCell<Option<HashSet<PathBuf>>>,
    /// The recent settled shots, newest first.
    shots: RefCell<Vec<Shot>>,
    /// Shots left out of the list while their Trash waits for an Undo.
    hidden: RefCell<HashSet<PathBuf>>,
    /// Starts the next listing: after folder events, or to see a file
    /// settle.
    timer: Timer,
    /// Called when a listing changed the list or the access; not for the
    /// changes callers make ([`apply`](Self::apply), [`touch`](Self::touch),
    /// [`hide`](Self::hide)), after which they refresh themselves.
    on_change: Box<dyn Fn()>,
    /// Called with the newest of the screenshots that just settled.
    on_new: Box<dyn Fn(PathBuf)>,
    this: Weak<Self>,
    #[cfg(test)]
    fake: RefCell<Option<Fake>>,
    /// Listings asked for, which tests run with [`scan_sync`](Self::scan_sync).
    #[cfg(test)]
    scans_asked: Cell<u32>,
    /// The delay of the last listing asked for by a timer.
    #[cfg(test)]
    followup: Cell<Option<Duration>>,
}

impl ShotsController {
    /// Starts watching the folder if `settings` have the feature on, unless
    /// it must wait to be opened after an update. `on_change` is called
    /// when a listing changes the list or the access, `on_new` with each
    /// new screenshot once it is saved.
    pub(super) fn new(
        settings: &RefCell<Settings>,
        on_change: Box<dyn Fn()>,
        on_new: Box<dyn Fn(PathBuf)>,
    ) -> Rc<Self> {
        let controller = Self::build(on_change, on_new);
        MAIN_SHOTS.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        controller.launch(settings.borrow().screenshots);
        controller
    }

    /// The controller, off, over the system's folder.
    fn build(on_change: Box<dyn Fn()>, on_new: Box<dyn Fn(PathBuf)>) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            access: Cell::new(Access::Off),
            folder: RefCell::new(PathBuf::new()),
            watch: RefCell::new(None),
            generation: Cell::new(0),
            listing: Cell::new(None),
            again: Cell::new(false),
            settler: RefCell::new(Settler::new()),
            seen: RefCell::new(None),
            shots: RefCell::new(Vec::new()),
            hidden: RefCell::new(HashSet::new()),
            timer: Timer::default(),
            on_change,
            on_new,
            this: this.clone(),
            #[cfg(test)]
            fake: RefCell::new(None),
            #[cfg(test)]
            scans_asked: Cell::new(0),
            #[cfg(test)]
            followup: Cell::new(None),
        })
    }

    /// At launch: reads the folder, and with the feature `on` starts, or
    /// waits to be opened when the folder is protected and this build was
    /// not let into it yet.
    fn launch(&self, on: bool) {
        let folder = self.system_folder();
        *self.folder.borrow_mut() = folder.clone();
        if !on {
            return;
        }
        let granted = self.grant_file().and_then(|p| load_grant(&p));
        if wait_for_open(self.protected(&folder), granted.as_deref(), &build_stamp()) {
            debug!(
                "screenshots: {} waits to be opened after an update",
                folder.display()
            );
            self.access.set(Access::WaitForOpen);
        } else {
            self.start();
        }
    }

    /// Follows the screenshots setting of `s`. Switched off, the watch and
    /// the list go; switched on, the folder is read and listed at once, so
    /// the system asks for access right then.
    pub(super) fn apply(self: &Rc<Self>, s: &Settings) {
        let on = s.screenshots;
        if on == (self.access.get() != Access::Off) {
            return;
        }
        debug!("screenshots {}", if on { "on" } else { "off" });
        if on {
            *self.folder.borrow_mut() = self.system_folder();
            self.start();
        } else {
            self.reset();
            self.access.set(Access::Off);
        }
    }

    /// Reads the system's folder again (the tab or the Settings page
    /// opens): a new folder moves the watch, and a folder waiting to be
    /// opened or denied, or whose listing never answered, is listed again.
    pub(super) fn touch(self: &Rc<Self>) {
        let folder = self.system_folder();
        let moved = *self.folder.borrow() != folder;
        if moved {
            debug!("screenshot folder now {}", folder.display());
            *self.folder.borrow_mut() = folder;
        }
        match self.access.get() {
            Access::Off => {}
            Access::WaitForOpen | Access::Denied => self.start(),
            Access::Asking | Access::Granted => {
                let stuck = self
                    .listing
                    .get()
                    .is_some_and(|since| since.elapsed() >= STUCK);
                if stuck {
                    warn!("screenshots: a listing never answered, listing again");
                }
                if moved || stuck {
                    self.start();
                }
            }
        }
    }

    pub(super) fn access(&self) -> Access {
        self.access.get()
    }

    /// Whether the folder could be read.
    pub(super) fn enabled(&self) -> bool {
        self.access.get() == Access::Granted
    }

    /// The screenshot folder.
    pub(super) fn folder(&self) -> PathBuf {
        self.folder.borrow().clone()
    }

    /// The recent screenshots, newest first, without the hidden ones.
    pub(super) fn shots(&self) -> Vec<Shot> {
        let hidden = self.hidden.borrow();
        self.shots
            .borrow()
            .iter()
            .filter(|s| !hidden.contains(&s.path))
            .cloned()
            .collect()
    }

    /// How many screenshots [`shots`](Self::shots) holds.
    pub(super) fn count(&self) -> usize {
        let hidden = self.hidden.borrow();
        self.shots
            .borrow()
            .iter()
            .filter(|s| !hidden.contains(&s.path))
            .count()
    }

    /// Leaves `path` out of the list, while its Trash waits for an Undo.
    pub(super) fn hide(&self, path: &Path) {
        self.hidden.borrow_mut().insert(path.to_path_buf());
    }

    /// Lists `path` again: its Trash was undone.
    pub(super) fn unhide(&self, path: &Path) {
        self.hidden.borrow_mut().remove(path);
    }

    /// Watches and lists the folder from scratch.
    fn start(&self) {
        self.reset();
        let folder = self.folder.borrow().clone();
        // GNOME and Windows make the folder only with the first screenshot:
        // made now, it can be watched from the start. Should that fail, the
        // listing fails too and the folder counts as denied.
        if let Err(e) = std::fs::metadata(&folder)
            && e.kind() == io::ErrorKind::NotFound
        {
            match std::fs::create_dir_all(&folder) {
                Ok(()) => debug!("screenshot folder {} made", folder.display()),
                Err(e) => warn!("cannot make {}: {e}", folder.display()),
            }
        }
        let generation = self.generation.get();
        match watch_folder(&folder, Box::new(move || folder_event(generation))) {
            Ok(watch) => *self.watch.borrow_mut() = Some((folder, watch)),
            Err(e) => warn!("cannot watch {}: {e}", folder.display()),
        }
        self.access.set(Access::Asking);
        self.scan();
    }

    /// Drops the watch, the list and what listings learned, and makes any
    /// listing still running stale.
    fn reset(&self) {
        self.generation.set(self.generation.get() + 1);
        self.watch.borrow_mut().take();
        self.timer.stop();
        self.listing.set(None);
        self.again.set(false);
        *self.settler.borrow_mut() = Settler::new();
        self.seen.borrow_mut().take();
        self.shots.borrow_mut().clear();
    }

    /// A folder event of `generation` came in: lists the folder once the
    /// events calm down.
    fn folder_changed(&self, generation: u64) {
        if generation == self.generation.get() {
            self.schedule(COALESCE);
        }
    }

    /// Lists the folder after `after`, unless a listing is already due.
    fn schedule(&self, after: Duration) {
        #[cfg(test)]
        if self.fake.borrow().is_some() {
            self.followup.set(Some(after));
            return;
        }
        if self.timer.running() {
            return;
        }
        let this = self.this.clone();
        self.timer.start(TimerMode::SingleShot, after, move || {
            if let Some(c) = this.upgrade() {
                c.scan();
            }
        });
    }

    /// Lists the folder on the worker, or once more after the listing
    /// running now.
    fn scan(&self) {
        if !matches!(self.access.get(), Access::Asking | Access::Granted) {
            return;
        }
        if self.listing.get().is_some() {
            self.again.set(true);
            return;
        }
        #[cfg(test)]
        if self.fake.borrow().is_some() {
            self.scans_asked.set(self.scans_asked.get() + 1);
            return;
        }
        let job = self.job();
        let generation = self.generation.get();
        self.listing.set(Some(Instant::now()));
        let spawned = std::thread::Builder::new()
            .name("vento-shots".into())
            .spawn(move || {
                // Before the listing, so a file is never thought settled
                // sooner than it is.
                let at = Instant::now();
                let found = run_guarded(job);
                let posted = slint::invoke_from_event_loop(move || {
                    if let Some(c) = MAIN_SHOTS.with(|slot| slot.borrow().upgrade()) {
                        c.take(generation, found, at);
                    }
                });
                if let Err(e) = posted {
                    // The controller still thinks it runs, until a touch
                    // finds it stuck.
                    warn!("cannot hand the screenshot listing over: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start listing the screenshot folder: {e}");
            self.listing.set(None);
        }
    }

    /// The next listing of the folder.
    fn job(&self) -> Job {
        let dir = self.folder.borrow().clone();
        // Recorded on the first listing that works, not on every one.
        let record = (self.access.get() != Access::Granted && self.protected(&dir))
            .then(|| self.grant_file())
            .flatten()
            .map(|file| (file, build_stamp()));
        Job {
            dir,
            probe: self.probe(),
            record,
        }
    }

    /// Takes a listing of `generation` made at `at`; drops it when the
    /// folder moved or the feature went off since.
    fn take(&self, generation: u64, found: Found, at: Instant) {
        if generation != self.generation.get() {
            debug!("screenshots: a stale listing dropped");
            return;
        }
        self.listing.set(None);
        let before = (self.access.get(), self.shots());
        let newest = match found {
            Ok(found) => self.listed(found, at),
            Err(e) => {
                // Any failure, not only a refusal: which error a refusal
                // gives is not known for sure.
                warn!(
                    "cannot read the screenshot folder {}: {e}",
                    self.folder.borrow().display()
                );
                self.reset();
                self.access.set(Access::Denied);
                None
            }
        };
        let current = self.generation.get();
        if before != (self.access.get(), self.shots()) {
            (self.on_change)();
        }
        // The refresh may have moved the folder or switched off: what this
        // listing found is no longer the list's.
        if self.generation.get() != current {
            return;
        }
        if let Some(path) = newest {
            (self.on_new)(path);
        }
        if self.again.replace(false) {
            self.schedule(COALESCE);
        }
    }

    /// Takes a listing that worked, made at `at`. Returns the newest shot
    /// that settled since the folder's first listing and was written just
    /// now, if any.
    fn listed(&self, found: Vec<(FileFacts, Option<(u32, u32)>)>, at: Instant) -> Option<PathBuf> {
        self.access.set(Access::Granted);
        let facts: Vec<FileFacts> = found.iter().map(|(f, _)| f.clone()).collect();
        let (done, waiting) = self.settler.borrow_mut().settled(&facts, at);
        let settled: HashSet<&PathBuf> = done.iter().collect();
        let shots: Vec<Shot> = found
            .into_iter()
            .filter(|(f, _)| settled.contains(&f.path))
            .map(|(f, dims)| Shot {
                path: f.path,
                modified_unix: f.modified_unix,
                len: f.len,
                dims,
            })
            .collect();
        let now_unix = unix_now();
        let shots = recent(shots, now_unix);
        let newest = match self.seen.borrow_mut().as_mut() {
            Some(seen) => {
                // A path not seen before whose file was written a while ago
                // was renamed, copied or moved in: not new.
                let fresh = new_shots(seen, &shots, now_unix);
                seen.extend(done.iter().cloned());
                fresh.into_iter().next()
            }
            None => None,
        };
        // The first listing: nothing there now is new, settled or not.
        self.seen
            .borrow_mut()
            .get_or_insert_with(|| facts.iter().map(|f| f.path.clone()).collect());
        // A hidden shot that left the folder (its Trash went through).
        self.hidden
            .borrow_mut()
            .retain(|p| facts.iter().any(|f| &f.path == p));
        *self.shots.borrow_mut() = shots;
        if waiting {
            self.schedule(SETTLE);
        }
        newest
    }

    /// The folder the system saves screenshots in.
    fn system_folder(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(fake) = self.fake.borrow().as_ref() {
            return fake.folder.clone();
        }
        screenshot_folder()
    }

    /// Whether the system asks before vento reads `folder`.
    fn protected(&self, folder: &Path) -> bool {
        #[cfg(test)]
        if let Some(fake) = self.fake.borrow().as_ref() {
            return fake.protected;
        }
        is_protected_folder(folder)
    }

    /// Where the build let into a protected folder is recorded.
    fn grant_file(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(fake) = self.fake.borrow().as_ref() {
            return Some(fake.grant.clone());
        }
        grant_path()
    }

    /// How a listing tells screenshots from other files.
    fn probe(&self) -> Probe {
        #[cfg(test)]
        if let Some(fake) = self.fake.borrow().as_ref() {
            return Probe {
                needs_mark: true,
                mark: fake.mark,
            };
        }
        Probe {
            needs_mark: needs_screen_capture_mark(),
            mark: is_screen_capture,
        }
    }
}

/// Test seams: a fake folder, listings run on the calling thread.
#[cfg(test)]
impl ShotsController {
    /// [`new`](Self::new) over a fake folder.
    fn with_fake(
        settings: &RefCell<Settings>,
        fake: Fake,
        on_change: Box<dyn Fn()>,
        on_new: Box<dyn Fn(PathBuf)>,
    ) -> Rc<Self> {
        let controller = Self::build(on_change, on_new);
        *controller.fake.borrow_mut() = Some(fake);
        controller.launch(settings.borrow().screenshots);
        controller
    }

    /// Points the fake at another folder, as the user moving it would.
    fn set_fake_folder(&self, folder: &Path) {
        if let Some(fake) = self.fake.borrow_mut().as_mut() {
            fake.folder = folder.to_path_buf();
        }
    }

    /// The folder the watch is on.
    fn watched(&self) -> Option<PathBuf> {
        self.watch.borrow().as_ref().map(|(dir, _)| dir.clone())
    }

    /// Runs one listing here and takes it as made at `now`.
    fn scan_sync(&self, now: Instant) {
        self.followup.set(None);
        let generation = self.generation.get();
        let found = run_guarded(self.job());
        self.take(generation, found, now);
    }
}

/// Hands a folder event of `generation`, on the watch's thread, to the
/// controller on the main thread.
fn folder_event(generation: u64) {
    let posted = slint::invoke_from_event_loop(move || {
        if let Some(c) = MAIN_SHOTS.with(|slot| slot.borrow().upgrade()) {
            c.folder_changed(generation);
        }
    });
    if let Err(e) = posted {
        debug!("screenshots: folder event not handed over: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The system's mark, faked: files named `Screenshot…` carry it.
    fn fake_mark(path: &Path) -> bool {
        path.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("Screenshot"))
    }

    /// A controller over `folder`, launched with the feature `on`, and what
    /// its callbacks were called with.
    struct Rig {
        c: Rc<ShotsController>,
        changes: Rc<Cell<u32>>,
        news: Rc<RefCell<Vec<PathBuf>>>,
    }

    fn rig(on: bool, folder: &Path, protected: bool, grant: &Path) -> Rig {
        let settings = RefCell::new(Settings {
            screenshots: on,
            ..Settings::default()
        });
        let changes = Rc::new(Cell::new(0));
        let news = Rc::new(RefCell::new(Vec::new()));
        let (ch, nw) = (changes.clone(), news.clone());
        let c = ShotsController::with_fake(
            &settings,
            Fake {
                folder: folder.to_path_buf(),
                protected,
                grant: grant.to_path_buf(),
                mark: fake_mark,
            },
            Box::new(move || ch.set(ch.get() + 1)),
            Box::new(move |p| nw.borrow_mut().push(p)),
        );
        Rig { c, changes, news }
    }

    fn paths(c: &ShotsController) -> Vec<PathBuf> {
        c.shots().into_iter().map(|s| s.path).collect()
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn rename_into_place_gives_one_new_shot() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        let r = rig(true, &folder, false, &grant);
        assert_eq!(r.c.access(), Access::Asking);
        assert_eq!(r.c.scans_asked.get(), 1);
        let t0 = Instant::now();
        r.c.scan_sync(t0);
        assert_eq!(r.c.access(), Access::Granted);
        assert!(r.c.enabled());

        // Written hidden, then renamed into place, as the system does.
        let tmp = folder.join(".Screenshot tmp.png");
        fs::write(&tmp, b"picture").unwrap();
        r.c.scan_sync(t0 + 100 * MS);
        assert!(r.c.shots().is_empty());
        assert_eq!(r.c.followup.get(), None, "a hidden file asks for nothing");
        let shot = folder.join("Screenshot A.png");
        fs::rename(&tmp, &shot).unwrap();
        r.c.scan_sync(t0 + 1000 * MS);
        assert!(r.c.shots().is_empty(), "not settled yet");
        assert_eq!(r.c.followup.take(), Some(SETTLE));
        assert!(r.news.borrow().is_empty());
        r.c.scan_sync(t0 + 1300 * MS);
        assert_eq!(paths(&r.c), std::slice::from_ref(&shot));
        assert_eq!(*r.news.borrow(), std::slice::from_ref(&shot));
        assert_eq!(r.c.followup.get(), None);
        // Once per path.
        r.c.scan_sync(t0 + 2000 * MS);
        assert_eq!(*r.news.borrow(), [shot]);
        assert_eq!(r.c.count(), 1);
    }

    #[test]
    fn existing_files_at_start_are_not_new() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        for name in ["Screenshot 1.png", "Screenshot 2.png", "photo.png"] {
            fs::write(folder.join(name), b"picture").unwrap();
        }
        fs::create_dir(folder.join("Screenshot folder.png")).unwrap();
        let r = rig(true, &folder, false, &grant);
        let t0 = Instant::now();
        r.c.scan_sync(t0);
        let changes = r.changes.get();
        assert!(changes >= 1, "Granted is a change");
        r.c.scan_sync(t0 + SETTLE);
        r.c.scan_sync(t0 + SETTLE * 3);
        let mut listed = paths(&r.c);
        listed.sort();
        assert_eq!(
            listed,
            [
                folder.join("Screenshot 1.png"),
                folder.join("Screenshot 2.png")
            ]
        );
        assert!(r.news.borrow().is_empty());
        assert_eq!(r.changes.get(), changes + 1, "only the settling changed");

        // Two at once (two screens): one call, for the newest.
        let (a, b) = (
            folder.join("Screenshot 3.png"),
            folder.join("Screenshot 4.png"),
        );
        fs::write(&a, b"picture").unwrap();
        fs::write(&b, b"picture").unwrap();
        let old = fs::File::options().write(true).open(&a).unwrap();
        old.set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        let t1 = t0 + Duration::from_secs(5);
        r.c.scan_sync(t1);
        r.c.scan_sync(t1 + SETTLE);
        assert_eq!(*r.news.borrow(), [b]);
        assert_eq!(r.c.count(), 4);
    }

    #[test]
    fn renamed_or_copied_old_shots_are_not_new() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3_600);
        let old = folder.join("Screenshot old.png");
        fs::write(&old, b"picture").unwrap();
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        let r = rig(true, &folder, false, &grant);
        let t0 = Instant::now();
        r.c.scan_sync(t0);
        r.c.scan_sync(t0 + SETTLE);
        assert_eq!(paths(&r.c), std::slice::from_ref(&old));

        // Renamed: a path the folder never had, with the old file time.
        let renamed = folder.join("Screenshot renamed.png");
        fs::rename(&old, &renamed).unwrap();
        // Duplicated (Finder keeps the file time too).
        let copy = folder.join("Screenshot renamed copy.png");
        fs::copy(&renamed, &copy).unwrap();
        fs::File::options()
            .write(true)
            .open(&copy)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        let t1 = t0 + Duration::from_secs(1);
        r.c.scan_sync(t1);
        r.c.scan_sync(t1 + SETTLE);
        assert_eq!(r.c.count(), 2, "both are listed");
        assert!(r.news.borrow().is_empty(), "neither is new");

        // A screenshot just taken still is.
        let fresh = folder.join("Screenshot fresh.png");
        fs::write(&fresh, b"picture").unwrap();
        let t2 = t1 + Duration::from_secs(1);
        r.c.scan_sync(t2);
        r.c.scan_sync(t2 + SETTLE);
        assert_eq!(*r.news.borrow(), [fresh]);
    }

    #[test]
    fn folder_change_moves_the_watch() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(a.join("Screenshot a.png"), b"picture").unwrap();
        fs::write(b.join("Screenshot b.png"), b"picture").unwrap();
        let r = rig(true, &a, false, &grant);
        assert_eq!(r.c.watched(), Some(a.clone()));
        let t0 = Instant::now();
        r.c.scan_sync(t0);
        r.c.scan_sync(t0 + SETTLE);
        assert_eq!(paths(&r.c), [a.join("Screenshot a.png")]);

        // Unchanged: nothing happens.
        r.c.touch();
        assert_eq!(r.c.access(), Access::Granted);
        assert_eq!(r.c.scans_asked.get(), 1);

        r.c.set_fake_folder(&b);
        r.c.touch();
        assert_eq!(r.c.folder(), b);
        assert_eq!(r.c.watched(), Some(b.clone()));
        assert_eq!(r.c.access(), Access::Asking);
        assert!(r.c.shots().is_empty());
        assert_eq!(r.c.scans_asked.get(), 2);
        let t1 = t0 + Duration::from_secs(1);
        r.c.scan_sync(t1);
        r.c.scan_sync(t1 + SETTLE);
        assert_eq!(paths(&r.c), [b.join("Screenshot b.png")]);
        // B's files were there at B's first listing.
        assert!(r.news.borrow().is_empty());
    }

    #[test]
    fn stale_listing_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(a.join("Screenshot a.png"), b"picture").unwrap();
        let r = rig(true, &a, false, &grant);
        let t0 = Instant::now();
        // A listing of A that comes back after the folder moved to B.
        let generation = r.c.generation.get();
        let late = r.c.job().run();
        r.c.set_fake_folder(&b);
        r.c.touch();
        r.c.take(generation, late, t0);
        r.c.take(generation, Ok(Vec::new()), t0 + SETTLE);
        assert_eq!(r.c.access(), Access::Asking);
        assert!(r.c.shots().is_empty());
        assert_eq!(r.changes.get(), 0);
    }

    #[test]
    fn off_drops_everything() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("Screenshot 1.png"), b"picture").unwrap();
        let r = rig(true, &folder, false, &grant);
        let t0 = Instant::now();
        r.c.scan_sync(t0);
        r.c.scan_sync(t0 + SETTLE);
        assert_eq!(r.c.count(), 1);

        let off = Settings {
            screenshots: false,
            ..Settings::default()
        };
        r.c.apply(&off);
        assert_eq!(r.c.access(), Access::Off);
        assert!(!r.c.enabled());
        assert!(r.c.shots().is_empty());
        assert_eq!(r.c.watched(), None);
        // Off, a tab or page open lists nothing.
        r.c.touch();
        assert_eq!(r.c.access(), Access::Off);
        assert_eq!(r.c.scans_asked.get(), 1);

        // On again: listed at once, so the system asks now.
        r.c.apply(&Settings {
            screenshots: true,
            ..off
        });
        assert_eq!(r.c.access(), Access::Asking);
        assert_eq!(r.c.watched(), Some(folder.clone()));
        assert_eq!(r.c.scans_asked.get(), 2);
        r.c.scan_sync(t0 + SETTLE * 2);
        r.c.scan_sync(t0 + SETTLE * 3);
        assert_eq!(r.c.count(), 1);
        assert!(r.news.borrow().is_empty(), "a new first listing");
    }

    #[test]
    fn wait_for_open_after_a_new_build() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("Desktop");
        fs::create_dir(&folder).unwrap();
        save_grant(&grant, "0.0.1-old").unwrap();
        let r = rig(true, &folder, true, &grant);
        assert_eq!(r.c.access(), Access::WaitForOpen);
        assert_eq!(r.c.scans_asked.get(), 0);
        assert_eq!(r.c.watched(), None);
        assert_eq!(r.c.folder(), folder);

        r.c.touch();
        assert_eq!(r.c.access(), Access::Asking);
        assert_eq!(r.c.scans_asked.get(), 1);
        r.c.scan_sync(Instant::now());
        assert_eq!(r.c.access(), Access::Granted);
        assert_eq!(load_grant(&grant), Some(build_stamp()));

        // The next launch of this build lists at once.
        let again = rig(true, &folder, true, &grant);
        assert_eq!(again.c.access(), Access::Asking);
        // An unprotected folder never waits.
        let other = dir.path().join("other.json");
        assert_eq!(rig(true, &folder, false, &other).c.access(), Access::Asking);
        assert_eq!(load_grant(&other), None);
        assert_eq!(rig(false, &folder, true, &other).c.access(), Access::Off);
    }

    #[test]
    fn unreadable_folder_is_denied_until_touched() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        // A file where a folder should be: it can be neither made nor read.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"file").unwrap();
        let folder = blocker.join("shots");
        let r = rig(true, &folder, true, &grant);
        r.c.touch();
        r.c.scan_sync(Instant::now());
        assert_eq!(r.c.access(), Access::Denied);
        assert!(!r.c.enabled());
        assert_eq!(load_grant(&grant), None);
        assert_eq!(r.changes.get(), 1);

        fs::remove_file(&blocker).unwrap();
        r.c.touch();
        assert_eq!(r.c.access(), Access::Asking);
        r.c.scan_sync(Instant::now());
        assert_eq!(r.c.access(), Access::Granted);
    }

    #[test]
    fn missing_folder_is_created_and_watched() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        // As on a fresh system, where the folder comes with the first shot.
        let folder = dir.path().join("Pictures").join("Screenshots");
        let r = rig(true, &folder, false, &grant);
        assert!(folder.is_dir());
        assert_eq!(r.c.watched(), Some(folder.clone()));
        r.c.scan_sync(Instant::now());
        assert_eq!(r.c.access(), Access::Granted);
        assert!(r.c.shots().is_empty());
        // Off, nothing is made.
        let other = dir.path().join("Other");
        rig(false, &other, false, &grant);
        assert!(!other.exists());
    }

    #[test]
    fn listing_that_panics_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("Screenshot 1.png"), b"picture").unwrap();
        let r = rig(true, &folder, false, &grant);
        if let Some(fake) = r.c.fake.borrow_mut().as_mut() {
            fake.mark = |_| panic!("a broken file");
        }
        r.c.scan_sync(Instant::now());
        assert_eq!(r.c.access(), Access::Denied);
        assert_eq!(r.c.listing.get(), None);
    }

    #[test]
    fn stuck_listing_restarts_on_touch() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        let r = rig(true, &folder, false, &grant);
        r.c.scan_sync(Instant::now());
        assert_eq!(r.c.scans_asked.get(), 1);

        // A listing that is still young is left to finish.
        r.c.listing.set(Some(Instant::now()));
        let generation = r.c.generation.get();
        r.c.touch();
        assert_eq!(r.c.generation.get(), generation);
        assert_eq!(r.c.scans_asked.get(), 1);

        // One whose answer never came is given up.
        let long_ago = Instant::now().checked_sub(STUCK * 2).unwrap();
        r.c.listing.set(Some(long_ago));
        r.c.touch();
        assert_ne!(r.c.generation.get(), generation);
        assert_eq!(r.c.listing.get(), None);
        assert_eq!(r.c.scans_asked.get(), 2);
        assert_eq!(r.c.access(), Access::Asking);
        assert_eq!(r.c.watched(), Some(folder));
    }

    #[test]
    fn move_during_refresh_drops_the_new_shot() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        let settings = RefCell::new(Settings {
            screenshots: true,
            ..Settings::default()
        });
        // The refresh opens the tab, which finds the folder moved to `to`.
        let slot: Rc<RefCell<Weak<ShotsController>>> = Rc::new(RefCell::new(Weak::new()));
        let to: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
        let news = Rc::new(RefCell::new(Vec::new()));
        let (s, t, nw) = (slot.clone(), to.clone(), news.clone());
        let c = ShotsController::with_fake(
            &settings,
            Fake {
                folder: a.clone(),
                protected: false,
                grant,
                mark: fake_mark,
            },
            Box::new(move || {
                let moved = t.borrow_mut().take();
                if let (Some(c), Some(dir)) = (s.borrow().upgrade(), moved) {
                    c.set_fake_folder(&dir);
                    c.touch();
                }
            }),
            Box::new(move |p| nw.borrow_mut().push(p)),
        );
        *slot.borrow_mut() = Rc::downgrade(&c);
        let t0 = Instant::now();
        c.scan_sync(t0);
        fs::write(a.join("Screenshot new.png"), b"picture").unwrap();
        c.scan_sync(t0 + SETTLE);
        *to.borrow_mut() = Some(b.clone());
        c.scan_sync(t0 + SETTLE * 2);
        assert_eq!(c.folder(), b);
        assert_eq!(c.access(), Access::Asking);
        assert!(news.borrow().is_empty(), "the old folder's shot is not new");
    }

    #[test]
    fn hidden_shots_leave_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        let (p, q) = (
            folder.join("Screenshot 1.png"),
            folder.join("Screenshot 2.png"),
        );
        fs::write(&p, b"picture").unwrap();
        fs::write(&q, b"picture").unwrap();
        let r = rig(true, &folder, false, &grant);
        let t0 = Instant::now();
        r.c.scan_sync(t0);
        r.c.scan_sync(t0 + SETTLE);
        assert_eq!(r.c.count(), 2);

        r.c.hide(&p);
        assert_eq!(paths(&r.c), std::slice::from_ref(&q));
        assert_eq!(r.c.count(), 1);
        // Still hidden after a listing that finds it.
        r.c.scan_sync(t0 + SETTLE * 2);
        assert_eq!(r.c.count(), 1);
        r.c.unhide(&p);
        assert_eq!(r.c.count(), 2);

        // Gone from the folder: forgotten.
        r.c.hide(&p);
        fs::remove_file(&p).unwrap();
        r.c.scan_sync(t0 + SETTLE * 3);
        assert!(r.c.hidden.borrow().is_empty());
    }

    #[test]
    fn listing_reads_the_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant.json");
        let folder = dir.path().join("shots");
        fs::create_dir(&folder).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        fs::write(folder.join("Screenshot 1.png"), &png).unwrap();
        let r = rig(true, &folder, false, &grant);
        let t0 = Instant::now();
        r.c.scan_sync(t0);
        r.c.scan_sync(t0 + SETTLE);
        let shots = r.c.shots();
        assert_eq!(shots[0].dims, Some((640, 480)));
        assert_eq!(shots[0].len, png.len() as u64);
    }

    #[test]
    fn build_stamp_starts_with_the_version() {
        let stamp = build_stamp();
        assert!(stamp.starts_with(concat!(env!("CARGO_PKG_VERSION"), "-")));
        assert_eq!(stamp, build_stamp());
    }
}
