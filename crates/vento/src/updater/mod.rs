//! Self-update for the macOS build: asks GitHub for the latest release,
//! downloads it and swaps the app in place, with a watcher process that
//! starts the new copy and rolls back when it does not start.
//! [`UpdateController`] runs that flow from the event loop for the banner,
//! the About rows and the tray dot.

pub mod island;
pub mod net;
pub mod swap;
pub mod watch;

use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::{debug, info, warn};
use vento_core::i18n::tr;
use vento_core::update::release::{self, AssetPlatform, ReleaseError, Update};
use vento_core::update::state::{self, FailedUpdate, UpdateEvent, UpdateFile, UpdateState};
use vento_core::update::version::Version;

use crate::settings_hub::SettingsHub;
use net::NetError;
use swap::SwapError;

/// The first check runs this long after start, never at launch.
const FIRST_CHECK: Duration = Duration::from_secs(3 * 60);

/// After the first, whether a check is due is asked this often.
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

/// Quits Vento for an update ([`crate::quit::quit_for_update`] bound to the
/// executor and the PDF windows): the swap to run once the quit is agreed,
/// what to do on a Cancel, and what to do with a swap's failure.
pub type QuitForUpdate = Box<
    dyn Fn(Box<dyn FnOnce() -> Result<(), String>>, Box<dyn FnOnce()>, Box<dyn FnOnce(String)>),
>;

/// Told the new state after every change.
pub type Listener = Box<dyn Fn(&UpdateState)>;

/// Whether the daily check runs now: it is on, and a day has passed since
/// `last_check` (Unix seconds), or it never ran.
pub fn should_check(enabled: bool, last_check: Option<i64>, now: i64) -> bool {
    enabled && state::check_due(last_check, now)
}

/// The version and release notes of the update `state` holds out, for the
/// What's new preview.
fn notes_of(state: &UpdateState) -> Option<(Version, String)> {
    match state {
        UpdateState::Available(u)
        | UpdateState::Downloading { update: u, .. }
        | UpdateState::Ready(u) => Some((u.version, u.notes.clone())),
        _ => None,
    }
}

/// The state a failure the watcher recorded starts in. The watcher writes
/// its reasons in English; the two it knows are shown in the interface
/// language, anything else as it was written.
fn failed_state(failed: &FailedUpdate) -> UpdateState {
    let reason = match failed.reason.as_str() {
        watch::NOT_STARTED_REASON => tr("The new version didn't start."),
        watch::NOT_QUIT_REASON => tr("Vento didn't quit"),
        other => other.to_owned(),
    };
    UpdateState::Failed {
        update: None,
        version: failed.version,
        reason,
    }
}

/// What Try again does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Retry {
    /// Download the failed update again.
    Download,
    /// Check again: the failed update is not known.
    Check,
    /// Nothing failed.
    Nothing,
}

/// What Try again does from `state`.
fn retry_for(state: &UpdateState) -> Retry {
    match state {
        UpdateState::Failed {
            update: Some(_), ..
        } => Retry::Download,
        UpdateState::Failed { update: None, .. } => Retry::Check,
        _ => Retry::Nothing,
    }
}

/// The old copy an update to `running` left beside `bundle`, when the
/// marker in `data_dir` says this version started after that update and the
/// old copy is still there. A start with `--after-update` removes it a
/// minute in; a copy that quit or was killed sooner leaves it behind.
pub fn left_old_copy(data_dir: &Path, bundle: &Path, running: Version) -> Option<PathBuf> {
    let old = swap::old_path(bundle);
    (watch::read_marker(data_dir) == Some(running) && old.exists()).then_some(old)
}

/// Where the downloads of every version go: removed at start.
fn updates_root(data_dir: &Path) -> PathBuf {
    data_dir.join("updates")
}

/// Where one version's archive and checksum list are downloaded.
fn version_dir(data_dir: &Path, version: Version) -> PathBuf {
    updates_root(data_dir).join(version.to_string())
}

/// Where one version's archive is unpacked.
fn unpack_dir(data_dir: &Path, version: Version) -> PathBuf {
    version_dir(data_dir, version).join("unpacked")
}

/// The share of a download done, 0.0 to 1.0: of `total` as the server says
/// it, else of `expected` as the release says it, else 0.
fn fraction(done: u64, total: u64, expected: u64) -> f32 {
    let of = if total > 0 { total } else { expected };
    if of == 0 {
        return 0.0;
    }
    (done as f64 / of as f64).clamp(0.0, 1.0) as f32
}

/// The binary the watcher runs from: the old copy's, at its `.old` path.
fn watcher_program(old: &Path) -> PathBuf {
    old.join("Contents/MacOS/vento")
}

/// The time now in Unix seconds.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The build releases carry for this Mac: Apple silicon only, so an Intel
/// Mac is offered no update.
fn mac_platform() -> Option<AssetPlatform> {
    cfg!(target_arch = "aarch64").then_some(AssetPlatform::MacArm64)
}

/// Logs once per run why an Intel Mac looks for no update.
static NO_BUILD_LOGGED: Once = Once::new();

/// Why a check, a download or a restart failed.
#[derive(Debug)]
enum Failure {
    /// The request or the download did not complete.
    Net(NetError),
    /// The app could not be unpacked or replaced.
    Swap(SwapError),
    /// The archive does not match its line in the checksum list.
    Checksum,
    /// GitHub's answer is not a release Vento can read.
    Unreadable,
}

impl Failure {
    /// The reason in the interface language, for the banner and About.
    fn user_text(&self) -> String {
        match self {
            Failure::Net(e) => e.user_text(),
            Failure::Swap(e) => e.user_text(),
            Failure::Checksum => tr("The download didn't match its checksum"),
            Failure::Unreadable => tr("GitHub's answer couldn't be read"),
        }
    }
}

impl std::fmt::Display for Failure {
    /// The reason in English, for the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Net(e) => write!(f, "{e}"),
            Failure::Swap(e) => write!(f, "{e}"),
            Failure::Checksum => write!(f, "checksum mismatch"),
            Failure::Unreadable => write!(f, "unreadable release"),
        }
    }
}

/// What a worker thread tells the controller, on the main thread.
enum Report {
    /// A check ended.
    Checked(Result<Option<Update>, Failure>),
    /// Download `job` got this far.
    Progress { job: u64, fraction: f32 },
    /// Download `job` ended: the unpacked app, or why not.
    Prepared {
        job: u64,
        result: Result<PathBuf, Failure>,
    },
}

thread_local! {
    /// The controller the workers report to; set on the main thread, which
    /// owns the non-`Send` controller.
    static MAIN_UPDATER: RefCell<Weak<UpdateController>> = const { RefCell::new(Weak::new()) };
}

/// One download at a time on disk: a job that was cancelled may still be
/// in its last read when the next one starts, in the same folder.
static ON_DISK: Mutex<()> = Mutex::new(());

/// Whether a download began in this run; set under [`ON_DISK`], so the
/// clean-up at start never removes a download that began before it ran.
static DOWNLOAD_BEGAN: AtomicBool = AtomicBool::new(false);

/// Removes the downloads an earlier run left in `root`, on a short thread.
fn remove_downloads_later(root: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("vento-update-clean".into())
        .spawn(move || {
            let _one = ON_DISK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if DOWNLOAD_BEGAN.load(Ordering::Relaxed) {
                return;
            }
            if let Err(e) = fs::remove_dir_all(&root)
                && e.kind() != io::ErrorKind::NotFound
            {
                warn!("cannot remove {}: {e}", root.display());
            }
        });
    if let Err(e) = spawned {
        warn!("cannot remove the old downloads: {e}");
    }
}

/// Hands `report` to the controller on the main thread.
fn report(report: Report) {
    let sent = slint::invoke_from_event_loop(move || {
        if let Some(controller) = MAIN_UPDATER.with(|slot| slot.borrow().upgrade()) {
            controller.on_report(report);
        }
    });
    if let Err(e) = sent {
        warn!("cannot hand an update result back: {e}");
    }
}

/// The running bundle, once it is known to be replaceable.
fn replaceable_bundle() -> Result<PathBuf, SwapError> {
    let bundle = swap::running_bundle()
        .ok_or_else(|| SwapError::NotABundle(std::env::current_exe().unwrap_or_default()))?;
    swap::check_replaceable(&bundle)?;
    Ok(bundle)
}

/// Asks GitHub for the latest release and picks the update for this copy.
/// A latest release that is not a final one is no update, and a Mac with no
/// build is never offered one (nothing is asked).
fn check(url: &str, running: Option<Version>) -> Result<Option<Update>, Failure> {
    let Some(platform) = mac_platform() else {
        NO_BUILD_LOGGED.call_once(|| {
            info!("releases are built for Apple silicon only: no update is looked for");
        });
        return Ok(None);
    };
    let Some(running) = running else {
        warn!(
            "cannot read this copy's version {:?}: no update is looked for",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(None);
    };
    let body = net::fetch_latest(url).map_err(Failure::Net)?;
    match release::parse_latest(&body) {
        Ok(latest) => Ok(latest.update_for(running, platform)),
        Err(ReleaseError::NotARelease) => Ok(None),
        Err(e @ ReleaseError::Json(_)) => {
            warn!("cannot read the latest release: {e}");
            Err(Failure::Unreadable)
        }
    }
}

/// Removes `dir` (if there) and makes it again, empty.
fn fresh_dir(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    fs::create_dir_all(dir)
}

/// Downloads `update`, checks it against its checksum and unpacks it, on a
/// worker thread: the unpacked `Vento.app`. On any failure, a cancel
/// included, the version's folder is removed.
fn prepare(
    data_dir: &Path,
    update: &Update,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<PathBuf, Failure> {
    let _one = ON_DISK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    DOWNLOAD_BEGAN.store(true, Ordering::Relaxed);
    let dir = version_dir(data_dir, update.version);
    let result = prepare_in(
        &dir,
        &unpack_dir(data_dir, update.version),
        update,
        cancel,
        progress,
    );
    if result.is_err()
        && let Err(e) = fs::remove_dir_all(&dir)
        && e.kind() != io::ErrorKind::NotFound
    {
        warn!("cannot remove {}: {e}", dir.display());
    }
    result
}

fn prepare_in(
    dir: &Path,
    unpacked: &Path,
    update: &Update,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<PathBuf, Failure> {
    let io_failure = |e: io::Error| Failure::Net(NetError::Io(e.to_string()));
    fresh_dir(dir).map_err(io_failure)?;
    // The two names are exactly `SHA256SUMS.txt` and the platform's
    // archive name (`Release::update_for` picks them by name), so they
    // stay inside `dir`.
    let sums_path = dir.join(&update.sums.name);
    net::download(&update.sums.url, &sums_path, cancel, &mut |_, _| {}).map_err(Failure::Net)?;
    let archive = dir.join(&update.archive.name);
    let expected = update.archive.size;
    // One report per whole percent, not one per chunk.
    let mut shown = -1i32;
    net::download(&update.archive.url, &archive, cancel, &mut |done, total| {
        let f = fraction(done, total, expected);
        let percent = (f * 100.0) as i32;
        if percent != shown {
            shown = percent;
            progress(f);
        }
    })
    .map_err(Failure::Net)?;
    let sums = fs::read_to_string(&sums_path).map_err(io_failure)?;
    release::verify(&archive, &update.archive.name, &sums).map_err(|e| {
        warn!("update {} failed its check: {e}", update.version);
        Failure::Checksum
    })?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Failure::Net(NetError::Cancelled));
    }
    // `unpack` wants an empty folder of its own.
    fresh_dir(unpacked).map_err(io_failure)?;
    swap::unpack(&archive, unpacked).map_err(Failure::Swap)
}

/// Starts the watcher `program` with `args` in a session of its own, so it
/// outlives this copy, with no stdio tied to it, and does not wait for it.
fn spawn_watcher(program: &Path, args: &[String]) -> io::Result<()> {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // `setsid` also gives the child a process group of its own; `process_group`
    // is not used with it, as a group leader cannot start a session.
    // SAFETY: `setsid` is async-signal-safe and touches only the child.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn().map(drop)
}

/// Runs the update flow on the event loop: the daily check, Check now, the
/// download and the restart. Work runs on worker threads; their results
/// come back to the main thread, which alone reads and writes
/// `update.json`.
pub struct UpdateController {
    hub: Rc<SettingsHub>,
    data_dir: PathBuf,
    quit_for_update: QuitForUpdate,
    had_screen_access: Box<dyn Fn() -> bool>,
    /// Whether a Move, Copy, Extract, Convert or other job is running, which
    /// a quit would cancel.
    jobs_running: Box<dyn Fn() -> bool>,
    state: RefCell<UpdateState>,
    listeners: RefCell<Vec<Listener>>,
    /// The download whose reports count; any other job's are dropped.
    job: Cell<u64>,
    /// The running download's cancel flag; `None` when none runs.
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    /// The unpacked app of the update that is ready.
    unpacked: RefCell<Option<PathBuf>>,
    /// Whether a quit for the update is asking or under way.
    quitting: Cell<bool>,
    /// The hourly timer, after the first check.
    timer: slint::Timer,
    this: Weak<UpdateController>,
}

impl UpdateController {
    /// A controller with nothing checked yet; [`start`](Self::start) reads
    /// `update.json` and arms the daily check. `jobs_running` says whether a
    /// job is running: the restart then waits for the user. Call on the main
    /// thread.
    pub fn new(
        hub: Rc<SettingsHub>,
        data_dir: PathBuf,
        quit_for_update: QuitForUpdate,
        had_screen_access: Box<dyn Fn() -> bool>,
        jobs_running: Box<dyn Fn() -> bool>,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| UpdateController {
            hub,
            data_dir,
            quit_for_update,
            had_screen_access,
            jobs_running,
            state: RefCell::new(UpdateState::UpToDate { checked_at: None }),
            listeners: RefCell::new(Vec::new()),
            job: Cell::new(0),
            cancel: RefCell::new(None),
            unpacked: RefCell::new(None),
            quitting: Cell::new(false),
            timer: slint::Timer::default(),
            this: this.clone(),
        });
        MAIN_UPDATER.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        controller
    }

    /// Where the flow stands.
    pub fn state(&self) -> UpdateState {
        self.state.borrow().clone()
    }

    /// Reads `update.json`: a failure the watcher recorded becomes the
    /// state, and is cleared from the file. Removes the downloads left from
    /// an earlier run, then arms the daily check: first in 3 minutes, then
    /// asked every hour.
    pub fn start(&self) {
        let path = UpdateFile::path(&self.data_dir);
        let mut file = UpdateFile::load(&path);
        let state = match file.failed.take() {
            Some(failed) => {
                if let Err(e) = file.save(&path) {
                    warn!("cannot clear the failed update: {e}");
                }
                failed_state(&failed)
            }
            None => UpdateState::UpToDate {
                checked_at: file.last_check,
            },
        };
        remove_downloads_later(updates_root(&self.data_dir));
        self.set_state(state);
        let this = self.this.clone();
        slint::Timer::single_shot(FIRST_CHECK, move || {
            let Some(controller) = this.upgrade() else {
                return;
            };
            controller.daily_tick();
            let this = controller.this.clone();
            controller
                .timer
                .start(slint::TimerMode::Repeated, CHECK_EVERY, move || {
                    if let Some(controller) = this.upgrade() {
                        controller.daily_tick();
                    }
                });
        });
    }

    /// Checks when the daily check is on and due (a check that ended,
    /// failed or not, a day ago), and nothing else runs.
    fn daily_tick(&self) {
        let last = UpdateFile::load(&UpdateFile::path(&self.data_dir)).last_check;
        if should_check(self.hub.current().check_for_updates, last, unix_now())
            && !self.state().busy()
        {
            self.begin_check();
        }
    }

    /// Starts a check on a worker, when the state allows one.
    fn begin_check(&self) {
        let next = state::reduce(&self.state(), UpdateEvent::CheckStarted);
        if next != UpdateState::Checking {
            return;
        }
        self.set_state(next);
        let url = net::api_url();
        let spawned = std::thread::Builder::new()
            .name("vento-update-check".into())
            .spawn(move || report(Report::Checked(check(&url, crate::running_version()))));
        if let Err(e) = spawned {
            warn!("cannot start the update check: {e}");
            let failure = Failure::Net(NetError::Offline(e.to_string()));
            self.apply(UpdateEvent::CheckFailed(failure.user_text()));
        }
    }

    /// Remembers in `update.json` when a check ended (`None`: forgets it).
    fn save_last_check(&self, at: Option<i64>) {
        let path = UpdateFile::path(&self.data_dir);
        let mut file = UpdateFile::load(&path);
        file.last_check = at;
        if let Err(e) = file.save(&path) {
            warn!("cannot save the last check: {e}");
        }
    }

    /// A worker's result, on the main thread.
    fn on_report(&self, report: Report) {
        match report {
            // An update found is not kept across a restart, so that check
            // is not counted: the time is forgotten, the first check after
            // the next start is due and finds the update again, and About
            // claims no check meanwhile.
            Report::Checked(Ok(found)) => {
                let at = unix_now();
                self.save_last_check(found.is_none().then_some(at));
                self.apply(UpdateEvent::Checked { found, at });
            }
            // A failed check counts too: the next automatic try is a day
            // later (Check now works any time).
            Report::Checked(Err(failure)) => {
                warn!("update check failed: {failure}");
                self.save_last_check(Some(unix_now()));
                self.apply(UpdateEvent::CheckFailed(failure.user_text()));
            }
            Report::Progress { job, fraction } => {
                if self.is_current(job) {
                    self.apply(UpdateEvent::Progress(fraction));
                }
            }
            Report::Prepared { job, result } => {
                if !self.is_current(job) {
                    debug!("dropped the result of a cancelled download");
                    return;
                }
                self.cancel.borrow_mut().take();
                match result {
                    Ok(app) => {
                        self.apply(UpdateEvent::Progress(1.0));
                        *self.unpacked.borrow_mut() = Some(app.clone());
                        self.ask_restart(app);
                    }
                    Err(failure) => {
                        warn!("update download failed: {failure}");
                        self.apply(UpdateEvent::Failed(failure.user_text()));
                    }
                }
            }
        }
    }

    /// Makes `downloading` the state with a new download job: its number
    /// and cancel flag. Reports of any earlier job no longer count.
    fn start_job(&self, downloading: UpdateState) -> (u64, Arc<AtomicBool>) {
        let job = self.job.get() + 1;
        self.job.set(job);
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.borrow_mut() = Some(cancel.clone());
        *self.unpacked.borrow_mut() = None;
        self.set_state(downloading);
        (job, cancel)
    }

    /// Whether `job` is the download running now.
    fn is_current(&self, job: u64) -> bool {
        job == self.job.get() && self.cancel.borrow().is_some()
    }

    /// Quits for the update with the unpacked `app`, once nothing objects:
    /// a Cancel leaves it Ready, a failed swap Failed. While a job runs it
    /// does not ask at all (the quit would cancel the job): the update is
    /// Ready, and the user's Restart asks again.
    fn ask_restart(&self, app: PathBuf) {
        let version = match &*self.state.borrow() {
            UpdateState::Downloading { update, .. } | UpdateState::Ready(update) => update.version,
            _ => return,
        };
        if self.quitting.get() {
            return;
        }
        if (self.jobs_running)() {
            info!("update to {version}: a job is running, so the restart waits");
            self.apply(UpdateEvent::QuitCancelled);
            return;
        }
        self.quitting.set(true);
        let (swapper, cancelled, failed) =
            (self.this.clone(), self.this.clone(), self.this.clone());
        (self.quit_for_update)(
            Box::new(move || match swapper.upgrade() {
                Some(controller) => controller.swap_and_watch(&app, version),
                None => Err(SwapError::Io("no update controller".into()).user_text()),
            }),
            Box::new(move || {
                if let Some(controller) = cancelled.upgrade() {
                    controller.quitting.set(false);
                    controller.apply(UpdateEvent::QuitCancelled);
                }
            }),
            Box::new(move |reason| {
                if let Some(controller) = failed.upgrade() {
                    controller.quitting.set(false);
                    controller.apply(UpdateEvent::Failed(reason));
                }
            }),
        );
    }

    /// The quit was agreed: saves whether Screen Recording was granted,
    /// moves the new copy in and starts the watcher from the old one. When
    /// the watcher cannot start, the old copy is put back.
    fn swap_and_watch(&self, new_app: &Path, version: Version) -> Result<(), String> {
        let path = UpdateFile::path(&self.data_dir);
        let mut file = UpdateFile::load(&path);
        file.had_screen_access = (self.had_screen_access)();
        if let Err(e) = file.save(&path) {
            warn!("cannot save the Screen Recording grant: {e}");
        }
        self.apply(UpdateEvent::Restarting);
        let fail = |e: SwapError| {
            warn!("update to {version}: {e}");
            e.user_text()
        };
        let bundle = replaceable_bundle().map_err(fail)?;
        let old = swap::swap(&bundle, new_app).map_err(fail)?;
        let args = watch::to_args(&watch::WatchArgs {
            old_pid: std::process::id(),
            bundle: bundle.clone(),
            old: old.clone(),
            data_dir: self.data_dir.clone(),
            version,
        });
        if let Err(e) = spawn_watcher(&watcher_program(&old), &args) {
            if let Err(back) = swap::restore(&bundle, &old) {
                warn!("cannot put {} back: {back}", old.display());
            }
            return Err(fail(SwapError::Io(format!(
                "cannot start the watcher: {e}"
            ))));
        }
        Ok(())
    }

    /// Applies `event` and tells the listeners when the state changed.
    fn apply(&self, event: UpdateEvent) {
        self.set_state(state::reduce(&self.state(), event));
    }

    /// Makes `next` the state and tells the listeners, when it differs.
    fn set_state(&self, next: UpdateState) {
        if *self.state.borrow() == next {
            return;
        }
        *self.state.borrow_mut() = next.clone();
        for listener in self.listeners.borrow().iter() {
            listener(&next);
        }
    }
}

// What the banner, the About rows, the tray dot and What's new call.
impl UpdateController {
    /// Calls `f` with the new state after every change. A listener must not
    /// add listeners.
    pub fn on_change(&self, f: Listener) {
        self.listeners.borrow_mut().push(f);
    }

    /// Checks now, whether or not the daily check is on.
    pub fn check_now(&self) {
        if !self.state().busy() {
            self.begin_check();
        }
    }

    /// Downloads the available update (or the failed one again), checks
    /// it, unpacks it and quits to install it. A copy that cannot be
    /// replaced fails before anything is downloaded. Does nothing while a
    /// download runs or a quit for the update is asking (a Ready update
    /// goes through [`restart`](Self::restart)).
    pub fn update(&self) {
        let can_start = matches!(
            self.state(),
            UpdateState::Available(_)
                | UpdateState::Failed {
                    update: Some(_),
                    ..
                }
        );
        if !can_start || self.quitting.get() {
            return;
        }
        let start = state::reduce(&self.state(), UpdateEvent::DownloadStarted);
        let UpdateState::Downloading { update, .. } = start.clone() else {
            return;
        };
        if let Err(e) = replaceable_bundle() {
            warn!("update to {}: {e}", update.version);
            let failed = state::reduce(&start, UpdateEvent::Failed(e.user_text()));
            self.set_state(failed);
            return;
        }
        let (job, cancel) = self.start_job(start);
        let data_dir = self.data_dir.clone();
        let spawned = std::thread::Builder::new()
            .name("vento-update-download".into())
            .spawn(move || {
                let mut progress = |fraction| report(Report::Progress { job, fraction });
                let result = prepare(&data_dir, &update, &cancel, &mut progress);
                report(Report::Prepared { job, result });
            });
        if let Err(e) = spawned {
            warn!("cannot start the update download: {e}");
            self.cancel.borrow_mut().take();
            let failure = Failure::Net(NetError::Io(e.to_string()));
            self.apply(UpdateEvent::Failed(failure.user_text()));
        }
    }

    /// Stops the download: the banner goes back to the update at once, and
    /// whatever the download still reports is dropped. Once the download is
    /// done and the quit question is open (PDF windows with unsaved edits),
    /// there is nothing left to cancel and this does nothing.
    pub fn cancel(&self) {
        let Some(flag) = self.cancel.borrow_mut().take() else {
            return;
        };
        flag.store(true, Ordering::Relaxed);
        self.job.set(self.job.get() + 1);
        self.apply(UpdateEvent::Cancelled);
    }

    /// From Ready (a quit that was cancelled, or one that waited for a
    /// running job): quits again with the app already unpacked. While a job
    /// still runs it stays Ready.
    pub fn restart(&self) {
        if !matches!(self.state(), UpdateState::Ready(_)) {
            return;
        }
        let app = self.unpacked.borrow().clone();
        match app {
            Some(app) if app.is_dir() => self.ask_restart(app),
            _ => {
                let gone = SwapError::Unpack("the unpacked app is gone".into());
                warn!("restart for the update: {gone}");
                self.apply(UpdateEvent::Failed(gone.user_text()));
            }
        }
    }

    /// Try again after a failure: downloads the failed update again when it
    /// is known, and checks again when it is not (a failure the watcher
    /// recorded before this start).
    pub fn try_again(&self) {
        match retry_for(&self.state()) {
            Retry::Download => self.update(),
            Retry::Check => self.check_now(),
            Retry::Nothing => {}
        }
    }

    /// The version and release notes of the update on offer, downloading
    /// or ready, for the What's new preview.
    pub fn pending_notes(&self) -> Option<(Version, String)> {
        notes_of(&self.state())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use vento_core::update::release::{Asset, Update};
    use vento_core::update::state::{self, FailedUpdate, UpdateEvent, UpdateFile, UpdateState};
    use vento_core::update::version::Version;

    use super::net::NetError;
    use super::swap::SwapError;
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn update(ver: &str) -> Update {
        let asset = |name: &str| Asset {
            name: name.to_owned(),
            url: format!("https://example.com/{name}"),
            size: 10,
        };
        Update {
            version: v(ver),
            notes: format!("notes for {ver}"),
            archive: asset("vento.zip"),
            sums: asset("SHA256SUMS.txt"),
        }
    }

    #[test]
    fn no_daily_check_when_turned_off() {
        assert!(!should_check(false, None, 0));
    }

    #[test]
    fn daily_check_when_due() {
        assert!(should_check(true, None, 0));
        assert!(!should_check(true, Some(0), 10));
    }

    #[test]
    fn daily_check_after_a_day_or_when_the_clock_went_back() {
        let day = vento_core::update::state::DAY;
        assert!(should_check(true, Some(100), 100 + day));
        assert!(!should_check(true, Some(100), 100 + day - 1));
        assert!(should_check(true, Some(100), 5));
        assert!(!should_check(false, Some(100), 100 + day));
    }

    #[test]
    fn pending_notes_for_the_states_with_an_update() {
        let u = update("0.4.0");
        let want = Some((v("0.4.0"), "notes for 0.4.0".to_owned()));
        assert_eq!(notes_of(&UpdateState::Available(u.clone())), want);
        assert_eq!(
            notes_of(&UpdateState::Downloading {
                update: u.clone(),
                progress: 0.5
            }),
            want
        );
        assert_eq!(notes_of(&UpdateState::Ready(u.clone())), want);
    }

    #[test]
    fn no_pending_notes_otherwise() {
        let u = update("0.4.0");
        for s in [
            UpdateState::UpToDate { checked_at: None },
            UpdateState::Checking,
            UpdateState::CheckFailed { reason: "x".into() },
            UpdateState::Restarting(u.clone()),
            UpdateState::Failed {
                update: Some(u.clone()),
                version: v("0.4.0"),
                reason: "x".into(),
            },
            UpdateState::Failed {
                update: None,
                version: v("0.4.0"),
                reason: "x".into(),
            },
        ] {
            assert_eq!(notes_of(&s), None, "{s:?}");
        }
    }

    #[test]
    fn a_recorded_failure_becomes_the_failed_state_in_the_language() {
        let failed = |reason: &str| FailedUpdate {
            version: v("0.4.0"),
            reason: reason.to_owned(),
        };
        assert_eq!(
            failed_state(&failed(watch::NOT_STARTED_REASON)),
            UpdateState::Failed {
                update: None,
                version: v("0.4.0"),
                reason: "The new version didn't start.".into(),
            }
        );
        assert_eq!(
            failed_state(&failed(watch::NOT_QUIT_REASON)),
            UpdateState::Failed {
                update: None,
                version: v("0.4.0"),
                reason: "Vento didn't quit".into(),
            }
        );
        // Any other text is shown as it was recorded.
        assert_eq!(
            failed_state(&failed("disk full")),
            UpdateState::Failed {
                update: None,
                version: v("0.4.0"),
                reason: "disk full".into(),
            }
        );
        vento_core::i18n::with_language("ar", || {
            let UpdateState::Failed { reason, .. } =
                failed_state(&failed(watch::NOT_STARTED_REASON))
            else {
                panic!("not failed");
            };
            assert_ne!(reason, watch::NOT_STARTED_REASON);
            let UpdateState::Failed { reason, .. } = failed_state(&failed(watch::NOT_QUIT_REASON))
            else {
                panic!("not failed");
            };
            assert_ne!(reason, watch::NOT_QUIT_REASON);
        });
    }

    #[test]
    fn failure_texts() {
        assert_eq!(
            Failure::Net(NetError::Http(403)).user_text(),
            "GitHub answered 403"
        );
        assert_eq!(
            Failure::Swap(SwapError::Translocated).user_text(),
            "Move Vento to Applications to update it"
        );
        assert_eq!(
            Failure::Checksum.user_text(),
            "The download didn't match its checksum"
        );
        assert_eq!(
            Failure::Unreadable.user_text(),
            "GitHub's answer couldn't be read"
        );
    }

    #[test]
    fn download_paths_under_the_data_folder() {
        let data = Path::new("/data");
        assert_eq!(updates_root(data), PathBuf::from("/data/updates"));
        assert_eq!(
            version_dir(data, v("0.4.0")),
            PathBuf::from("/data/updates/0.4.0")
        );
        assert_eq!(
            unpack_dir(data, v("0.4.0")),
            PathBuf::from("/data/updates/0.4.0/unpacked")
        );
    }

    #[test]
    fn progress_falls_back_to_the_release_size() {
        assert_eq!(fraction(5, 10, 99), 0.5);
        assert_eq!(fraction(5, 0, 20), 0.25);
        assert_eq!(fraction(5, 0, 0), 0.0);
        assert_eq!(fraction(30, 0, 20), 1.0);
    }

    #[test]
    fn the_watcher_is_the_old_binary() {
        assert_eq!(
            watcher_program(Path::new("/Applications/Vento.app.old")),
            PathBuf::from("/Applications/Vento.app.old/Contents/MacOS/vento")
        );
    }

    /// The callbacks one `quit_for_update` call was handed.
    struct Quit {
        swap: Box<dyn FnOnce() -> Result<(), String>>,
        on_cancel: Box<dyn FnOnce()>,
        on_fail: Box<dyn FnOnce(String)>,
    }

    impl Quit {
        /// Every window agreed: the swap runs, and its error goes to
        /// `on_fail`, as `quit_for_update` does.
        fn agree(self) {
            if let Err(reason) = (self.swap)() {
                (self.on_fail)(reason);
            }
        }
    }

    /// A controller over a temporary data folder whose quit only records
    /// what it was handed, with every state it went through.
    struct Harness {
        controller: Rc<UpdateController>,
        quits: Rc<RefCell<Vec<Quit>>>,
        states: Rc<RefCell<Vec<UpdateState>>>,
        /// Whether a job runs, as the controller is told.
        jobs_running: Rc<Cell<bool>>,
        dir: tempfile::TempDir,
    }

    fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let hub = crate::settings_hub::SettingsHub::new(dir.path().join("settings.json"));
        let quits: Rc<RefCell<Vec<Quit>>> = Rc::default();
        let held = quits.clone();
        let jobs_running = Rc::new(Cell::new(false));
        let running = jobs_running.clone();
        let controller = UpdateController::new(
            hub,
            dir.path().join("data"),
            Box::new(move |swap, on_cancel, on_fail| {
                held.borrow_mut().push(Quit {
                    swap,
                    on_cancel,
                    on_fail,
                })
            }),
            Box::new(|| true),
            Box::new(move || running.get()),
        );
        let states: Rc<RefCell<Vec<UpdateState>>> = Rc::default();
        let seen = states.clone();
        controller.on_change(Box::new(move |s| seen.borrow_mut().push(s.clone())));
        Harness {
            controller,
            quits,
            states,
            jobs_running,
            dir,
        }
    }

    impl Harness {
        /// A download of 0.4.0 running as `update()` starts one, without
        /// its worker: its job and cancel flag.
        fn downloading(&self) -> (u64, Arc<AtomicBool>) {
            let c = &self.controller;
            if !matches!(c.state(), UpdateState::Available(_)) {
                c.set_state(UpdateState::Available(update("0.4.0")));
            }
            let start = state::reduce(&c.state(), UpdateEvent::DownloadStarted);
            c.start_job(start)
        }

        /// A folder standing in for the unpacked `Vento.app`.
        fn app(&self) -> PathBuf {
            let app = self.dir.path().join("Vento.app");
            std::fs::create_dir_all(&app).unwrap();
            app
        }

        fn take_quit(&self) -> Quit {
            self.quits.borrow_mut().remove(0)
        }

        fn update_file(&self) -> UpdateFile {
            UpdateFile::load(&UpdateFile::path(&self.dir.path().join("data")))
        }
    }

    fn downloading(progress: f32) -> UpdateState {
        UpdateState::Downloading {
            update: update("0.4.0"),
            progress,
        }
    }

    #[test]
    fn cancel_is_immediate_and_late_reports_are_dropped() {
        let h = harness();
        let (job, flag) = h.downloading();
        h.controller.cancel();
        assert!(flag.load(Ordering::Relaxed), "the worker is told to stop");
        assert_eq!(
            h.controller.state(),
            UpdateState::Available(update("0.4.0"))
        );
        h.controller
            .on_report(Report::Progress { job, fraction: 0.5 });
        h.controller.on_report(Report::Prepared {
            job,
            result: Ok(h.app()),
        });
        h.controller.on_report(Report::Prepared {
            job,
            result: Err(Failure::Net(NetError::Cancelled)),
        });
        assert_eq!(
            h.controller.state(),
            UpdateState::Available(update("0.4.0"))
        );
        assert!(h.quits.borrow().is_empty(), "no quit for a cancelled job");
    }

    #[test]
    fn reports_of_an_older_job_are_dropped() {
        let h = harness();
        let (old, _) = h.downloading();
        h.controller.cancel();
        let (new, _) = h.downloading();
        h.controller.on_report(Report::Progress {
            job: old,
            fraction: 0.9,
        });
        assert_eq!(h.controller.state(), downloading(0.0));
        h.controller.on_report(Report::Progress {
            job: new,
            fraction: 0.3,
        });
        assert_eq!(h.controller.state(), downloading(0.3));
    }

    #[test]
    fn a_second_update_does_nothing_while_downloading() {
        let h = harness();
        let (job, flag) = h.downloading();
        h.controller.update();
        assert_eq!(h.controller.job.get(), job, "no second job");
        let kept = h.controller.cancel.borrow().clone().unwrap();
        assert!(Arc::ptr_eq(&kept, &flag), "the running job's flag is kept");
        assert_eq!(h.controller.state(), downloading(0.0));
    }

    #[test]
    fn update_does_nothing_while_the_quit_question_is_open() {
        let h = harness();
        let (job, _) = h.downloading();
        h.controller.on_report(Report::Prepared {
            job,
            result: Ok(h.app()),
        });
        assert_eq!(h.controller.state(), downloading(1.0));
        assert_eq!(h.quits.borrow().len(), 1);
        h.controller.update();
        h.controller.cancel();
        h.controller.restart();
        assert_eq!(h.controller.job.get(), job, "no new job");
        assert_eq!(h.controller.state(), downloading(1.0));
        assert_eq!(h.quits.borrow().len(), 1, "asked once");
        assert!(h.app().is_dir(), "the unpacked app is left alone");
    }

    #[test]
    fn update_fails_before_downloading_when_vento_cannot_be_replaced() {
        // The test binary is not inside an `.app` bundle.
        let h = harness();
        h.controller
            .set_state(UpdateState::Available(update("0.4.0")));
        h.controller.update();
        assert_eq!(
            h.controller.state(),
            UpdateState::Failed {
                update: Some(update("0.4.0")),
                version: v("0.4.0"),
                reason: "Move Vento to Applications to update it".into(),
            }
        );
        assert_eq!(h.controller.job.get(), 0, "nothing downloaded");
        assert!(h.controller.cancel.borrow().is_none());
    }

    #[test]
    fn a_cancelled_quit_is_ready_and_restart_reuses_the_app() {
        let h = harness();
        let (job, _) = h.downloading();
        let app = h.app();
        h.controller.on_report(Report::Prepared {
            job,
            result: Ok(app.clone()),
        });
        (h.take_quit().on_cancel)();
        assert_eq!(h.controller.state(), UpdateState::Ready(update("0.4.0")));
        assert!(
            !h.states
                .borrow()
                .iter()
                .any(|s| matches!(s, UpdateState::Restarting(_))),
            "no Restarting before the quit was agreed"
        );
        h.controller.restart();
        assert_eq!(h.quits.borrow().len(), 1, "asked again");
        assert_eq!(*h.controller.unpacked.borrow(), Some(app));
        assert_eq!(h.controller.state(), UpdateState::Ready(update("0.4.0")));
    }

    #[test]
    fn restart_without_the_unpacked_app_fails_and_keeps_the_update() {
        let h = harness();
        let (job, _) = h.downloading();
        let app = h.app();
        h.controller.on_report(Report::Prepared {
            job,
            result: Ok(app.clone()),
        });
        (h.take_quit().on_cancel)();
        std::fs::remove_dir(&app).unwrap();
        h.controller.restart();
        assert!(h.quits.borrow().is_empty());
        assert!(matches!(
            h.controller.state(),
            UpdateState::Failed {
                update: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn a_failed_swap_is_failed_after_restarting_and_keeps_the_update() {
        // The swap itself fails at once here: the test binary is not in a
        // bundle, so nothing on disk is touched but `update.json`.
        let h = harness();
        let (job, _) = h.downloading();
        h.controller.on_report(Report::Prepared {
            job,
            result: Ok(h.app()),
        });
        h.take_quit().agree();
        let failed = UpdateState::Failed {
            update: Some(update("0.4.0")),
            version: v("0.4.0"),
            reason: "Move Vento to Applications to update it".into(),
        };
        assert_eq!(h.controller.state(), failed);
        {
            let states = h.states.borrow();
            let n = states.len();
            assert_eq!(states[n - 2], UpdateState::Restarting(update("0.4.0")));
        }
        assert!(h.update_file().had_screen_access, "saved before the swap");
        assert_eq!(retry_for(&failed), Retry::Download);
        // Try again downloads it again: here it fails before downloading,
        // as the test binary still cannot be replaced.
        h.controller.try_again();
        assert_eq!(h.controller.state(), failed);
        assert!(h.quits.borrow().is_empty(), "no second quit");
    }

    #[test]
    fn try_again_checks_again_when_the_update_is_not_known() {
        let recorded = UpdateState::Failed {
            update: None,
            version: v("0.4.0"),
            reason: "Vento didn't quit".into(),
        };
        assert_eq!(retry_for(&recorded), Retry::Check);
        assert_eq!(
            state::reduce(&recorded, UpdateEvent::CheckStarted),
            UpdateState::Checking
        );
        for other in [
            UpdateState::UpToDate { checked_at: None },
            UpdateState::Checking,
            UpdateState::Available(update("0.4.0")),
            UpdateState::Ready(update("0.4.0")),
            UpdateState::Restarting(update("0.4.0")),
        ] {
            assert_eq!(retry_for(&other), Retry::Nothing, "{other:?}");
        }
    }

    #[test]
    fn a_download_done_while_a_job_runs_waits_as_ready() {
        let h = harness();
        h.jobs_running.set(true);
        let (job, _) = h.downloading();
        let app = h.app();
        h.controller.on_report(Report::Prepared {
            job,
            result: Ok(app.clone()),
        });
        assert_eq!(h.controller.state(), UpdateState::Ready(update("0.4.0")));
        assert!(h.quits.borrow().is_empty(), "no quit over a running job");
        assert!(!h.controller.quitting.get());
        // Restart while the job still runs: still Ready, no quit.
        h.controller.restart();
        assert_eq!(h.controller.state(), UpdateState::Ready(update("0.4.0")));
        assert!(h.quits.borrow().is_empty());
        // Once the job is done, Restart asks to quit with the same app.
        h.jobs_running.set(false);
        h.controller.restart();
        assert_eq!(h.quits.borrow().len(), 1);
        assert_eq!(*h.controller.unpacked.borrow(), Some(app));
    }

    #[test]
    fn a_failed_download_keeps_the_update_for_try_again() {
        let h = harness();
        let (job, _) = h.downloading();
        h.controller.on_report(Report::Prepared {
            job,
            result: Err(Failure::Checksum),
        });
        assert_eq!(
            h.controller.state(),
            UpdateState::Failed {
                update: Some(update("0.4.0")),
                version: v("0.4.0"),
                reason: "The download didn't match its checksum".into(),
            }
        );
        assert!(h.controller.cancel.borrow().is_none());
        assert!(h.quits.borrow().is_empty());
    }

    #[test]
    fn a_check_saves_the_last_check_unless_it_found_an_update() {
        let h = harness();
        h.controller.set_state(UpdateState::Checking);
        h.controller
            .on_report(Report::Checked(Err(Failure::Net(NetError::Offline(
                "x".into(),
            )))));
        assert_eq!(
            h.controller.state(),
            UpdateState::CheckFailed {
                reason: "You're offline".into()
            }
        );
        let failed_at = h.update_file().last_check.expect("saved on failure");
        assert!(
            !should_check(true, Some(failed_at), unix_now()),
            "not again today"
        );
        h.controller.set_state(UpdateState::Checking);
        h.controller.on_report(Report::Checked(Ok(None)));
        let none_at = h.update_file().last_check.expect("saved with no update");
        assert_eq!(
            h.controller.state(),
            UpdateState::UpToDate {
                checked_at: Some(none_at)
            }
        );
        // An update found is not kept across a restart: the time is
        // forgotten, so the first check after the next start is due.
        h.controller.set_state(UpdateState::Checking);
        h.controller
            .on_report(Report::Checked(Ok(Some(update("0.4.0")))));
        assert_eq!(
            h.controller.state(),
            UpdateState::Available(update("0.4.0"))
        );
        let last = h.update_file().last_check;
        assert_eq!(last, None);
        assert!(should_check(true, last, unix_now()));
    }

    #[test]
    fn a_start_after_an_update_was_found_claims_no_check() {
        let h = harness();
        h.controller.set_state(UpdateState::Checking);
        h.controller
            .on_report(Report::Checked(Ok(Some(update("0.4.0")))));
        let next = harness();
        std::fs::create_dir_all(next.dir.path().join("data")).unwrap();
        std::fs::copy(
            UpdateFile::path(&h.dir.path().join("data")),
            UpdateFile::path(&next.dir.path().join("data")),
        )
        .unwrap();
        next.controller.start();
        assert_eq!(
            next.controller.state(),
            UpdateState::UpToDate { checked_at: None }
        );
    }

    #[test]
    fn a_start_keeps_the_time_of_the_last_check() {
        let h = harness();
        let path = UpdateFile::path(&h.dir.path().join("data"));
        UpdateFile {
            last_check: Some(7),
            ..UpdateFile::default()
        }
        .save(&path)
        .unwrap();
        h.controller.start();
        assert_eq!(
            h.controller.state(),
            UpdateState::UpToDate {
                checked_at: Some(7)
            }
        );
    }

    #[test]
    fn updates_are_looked_for_on_apple_silicon_only() {
        assert_eq!(
            mac_platform(),
            cfg!(target_arch = "aarch64").then_some(AssetPlatform::MacArm64)
        );
    }

    /// An Intel Mac asks nothing: the unreachable address is never tried.
    #[cfg(not(target_arch = "aarch64"))]
    #[test]
    fn an_intel_mac_is_offered_no_update() {
        assert!(matches!(
            check("http://127.0.0.1:9/never", Some(v("0.1.0"))),
            Ok(None)
        ));
    }

    #[test]
    fn an_old_copy_is_left_only_after_a_finished_update_to_this_version() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let bundle = dir.path().join("Vento.app");
        let old = swap::old_path(&bundle);
        // No marker, no old copy.
        assert_eq!(left_old_copy(&data, &bundle, v("0.4.0")), None);
        watch::write_marker(&data, v("0.4.0")).unwrap();
        assert_eq!(
            left_old_copy(&data, &bundle, v("0.4.0")),
            None,
            "none there"
        );
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(left_old_copy(&data, &bundle, v("0.4.0")), Some(old));
        // A marker of another version: the update to it did not finish here.
        assert_eq!(left_old_copy(&data, &bundle, v("0.3.0")), None);
    }

    #[test]
    fn start_turns_a_recorded_failure_into_the_state_and_clears_it() {
        let h = harness();
        let path = UpdateFile::path(&h.dir.path().join("data"));
        UpdateFile {
            last_check: Some(7),
            failed: Some(FailedUpdate {
                version: v("0.4.0"),
                reason: watch::NOT_STARTED_REASON.into(),
            }),
            ..UpdateFile::default()
        }
        .save(&path)
        .unwrap();
        h.controller.start();
        assert_eq!(
            h.controller.state(),
            UpdateState::Failed {
                update: None,
                version: v("0.4.0"),
                reason: "The new version didn't start.".into(),
            }
        );
        let file = h.update_file();
        assert_eq!(file.failed, None);
        assert_eq!(file.last_check, Some(7));
    }
}
