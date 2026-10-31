//! The watcher: a short-lived process, the old binary run from its `.old`
//! path as `vento --update-watch …`, that sees the update through once the
//! old copy has quit. It starts the new copy, waits for the new copy's
//! "started" marker and, when none comes, puts the old copy back and starts
//! that instead, so a broken download never leaves the user without Vento.
//! It creates no windows and no event loop.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use vento_core::update::state::{FailedUpdate, UpdateFile};
use vento_core::update::version::Version;

use super::swap;

/// The flag that makes `vento` run as the watcher.
pub const FLAG: &str = "--update-watch";

/// The flag the watcher starts the new copy with, so it writes the marker.
pub const AFTER_UPDATE: &str = "--after-update";

/// How long the old copy has to quit after starting the watcher.
const QUIT_WAIT: Duration = Duration::from_secs(10);

/// How long the new copy has to write its marker.
const START_WAIT: Duration = Duration::from_secs(30);

/// How often the marker is looked for.
const POLL: Duration = Duration::from_millis(250);

/// How long a killed copy has to be gone before the old one is started.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// The failure reason when the old copy was still running after
/// [`QUIT_WAIT`].
pub const NOT_QUIT_REASON: &str = "Vento didn't quit";

/// The failure reason when the new copy wrote no marker in time.
pub const NOT_STARTED_REASON: &str = "The new version didn't start.";

/// What the watcher is told on its command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchArgs {
    /// The old copy, which must be gone before the new one starts.
    pub old_pid: u32,
    /// Where the new copy now is (the app's usual place).
    pub bundle: PathBuf,
    /// Where the old copy waits ([`swap::old_path`]).
    pub old: PathBuf,
    /// Vento's data folder, for the marker and `update.json`.
    pub data_dir: PathBuf,
    /// The version being installed, which the marker must name.
    pub version: Version,
}

/// The arguments that start the watcher for `args` (after the program
/// name): `--update-watch <pid> <bundle> <old> <data_dir> <version>`.
pub fn to_args(args: &WatchArgs) -> Vec<String> {
    vec![
        FLAG.to_string(),
        args.old_pid.to_string(),
        args.bundle.to_string_lossy().into_owned(),
        args.old.to_string_lossy().into_owned(),
        args.data_dir.to_string_lossy().into_owned(),
        args.version.to_string(),
    ]
}

/// What a command line asks `vento` to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// The watcher, with these arguments.
    Watch(WatchArgs),
    /// The watcher, but its arguments do not read: nothing runs, since
    /// starting the app here would make a second copy.
    Malformed,
    /// The app, as usual.
    Normal,
}

/// Decides from the whole command line (program name first) whether to run
/// as the watcher: any `--update-watch` makes it a watcher command line.
pub fn dispatch(args: &[String]) -> Dispatch {
    if !args.iter().any(|a| a == FLAG) {
        return Dispatch::Normal;
    }
    match parse_args(args) {
        Some(watch) => Dispatch::Watch(watch),
        None => Dispatch::Malformed,
    }
}

/// Reads the watcher's arguments from a command line, with or without the
/// program name first. `None` for any other command line, and for a pid of
/// 0 or 1 (a signal to 0 would reach the watcher's own process group, and 1
/// is launchd).
pub fn parse_args(args: &[String]) -> Option<WatchArgs> {
    let rest = match args {
        [first, rest @ ..] if first == FLAG => rest,
        [_, first, rest @ ..] if first == FLAG => rest,
        _ => return None,
    };
    let [pid, bundle, old, data_dir, version] = rest else {
        return None;
    };
    let old_pid: u32 = pid.parse().ok()?;
    if old_pid <= 1 {
        return None;
    }
    Some(WatchArgs {
        old_pid,
        bundle: PathBuf::from(bundle),
        old: PathBuf::from(old),
        data_dir: PathBuf::from(data_dir),
        version: Version::parse(version)?,
    })
}

/// Where a copy started with `--after-update` says it is up.
pub fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join("update-started")
}

/// Writes the marker for `version`, once the tray and shortcuts are up.
pub fn write_marker(data_dir: &Path, version: Version) -> io::Result<()> {
    fs::create_dir_all(data_dir)?;
    fs::write(marker_path(data_dir), version.to_string())
}

/// The version the marker names, if there is a readable one.
pub fn read_marker(data_dir: &Path) -> Option<Version> {
    let text = fs::read_to_string(marker_path(data_dir)).ok()?;
    Version::parse(text.trim())
}

/// The executable inside the bundle `app`.
fn binary(app: &Path) -> PathBuf {
    app.join("Contents/MacOS/vento")
}

/// Starts a program with arguments, detached: its pid, or `None` when it
/// could not be started.
pub type Spawn = Box<dyn FnMut(&Path, &[&str]) -> Option<u32>>;

/// The effects the watcher has on the system, injected so the order can be
/// tested.
pub struct WatchDeps {
    /// Waits up to the given time for the process to be gone; `true` once it
    /// is.
    pub wait_gone: Box<dyn FnMut(u32, Duration) -> bool>,
    /// Starts the program with the arguments, detached; its pid, or `None`
    /// when it could not be started.
    pub spawn: Spawn,
    /// The version the marker names, if there is a readable one.
    pub marker_says: Box<dyn FnMut() -> Option<Version>>,
    /// Kills the process.
    pub kill: Box<dyn FnMut(u32)>,
    /// Puts the old copy back at the bundle path.
    pub restore: Box<dyn FnMut() -> io::Result<()>>,
    /// Remembers the failed update for the old copy's banner.
    pub record_failure: Box<dyn FnMut(FailedUpdate)>,
    /// Waits.
    pub sleep: Box<dyn FnMut(Duration)>,
}

/// How the watch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The new copy wrote its marker.
    Started,
    /// The old copy was put back and started.
    RolledBack,
}

/// The watcher's steps. The stale marker is removed by the caller before
/// this runs.
///
/// 1. Wait for the old copy to be gone. If it is still running after
///    [`QUIT_WAIT`] it is killed (it already agreed to quit, and two copies
///    must never run at once), and the update is rolled back.
/// 2. Start the new copy with `--after-update` and look for its marker every
///    [`POLL`] for [`START_WAIT`].
/// 3. No marker: kill the new copy, put the old one back, record why and
///    start the old one.
///
/// When a killed copy is still running after [`KILL_WAIT`], the old copy is
/// put back and the failure recorded, but nothing is started: that would
/// be a second copy.
pub fn watch_with(args: &WatchArgs, mut deps: WatchDeps) -> WatchOutcome {
    if !(deps.wait_gone)(args.old_pid, QUIT_WAIT) {
        (deps.kill)(args.old_pid);
        let gone = (deps.wait_gone)(args.old_pid, KILL_WAIT);
        return roll_back(args, &mut deps, NOT_QUIT_REASON, gone);
    }
    let Some(pid) = (deps.spawn)(&binary(&args.bundle), &[AFTER_UPDATE]) else {
        return roll_back(args, &mut deps, NOT_STARTED_REASON, true);
    };
    let polls = START_WAIT.as_millis() / POLL.as_millis();
    for _ in 0..polls {
        if (deps.marker_says)() == Some(args.version) {
            return WatchOutcome::Started;
        }
        (deps.sleep)(POLL);
    }
    if (deps.marker_says)() == Some(args.version) {
        return WatchOutcome::Started;
    }
    (deps.kill)(pid);
    let gone = (deps.wait_gone)(pid, KILL_WAIT);
    roll_back(args, &mut deps, NOT_STARTED_REASON, gone)
}

/// Puts the old copy back, records `reason` and starts the old copy: from
/// the bundle path once it is back there. When it could not be put back it
/// is started from where it still is, and if that fails too, whatever copy
/// is at the bundle path is started, so some Vento runs. With `start` false
/// (a killed copy is still running) nothing is started.
fn roll_back(args: &WatchArgs, deps: &mut WatchDeps, reason: &str, start: bool) -> WatchOutcome {
    let restored = (deps.restore)();
    if let Err(e) = &restored {
        log::warn!("cannot put the old copy back: {e}");
    }
    (deps.record_failure)(FailedUpdate {
        version: args.version,
        reason: reason.to_string(),
    });
    if !start {
        log::warn!("a killed copy is still running; not starting another");
        return WatchOutcome::RolledBack;
    }
    let at_bundle = binary(&args.bundle);
    let started = match restored {
        Ok(()) => (deps.spawn)(&at_bundle, &[]),
        Err(_) => (deps.spawn)(&binary(&args.old), &[]).or_else(|| (deps.spawn)(&at_bundle, &[])),
    };
    if started.is_none() {
        log::warn!("cannot start the old copy again");
    }
    WatchOutcome::RolledBack
}

/// Runs the watcher with the real effects. Success when the new copy
/// started.
pub fn run(args: WatchArgs) -> ExitCode {
    let marker = marker_path(&args.data_dir);
    if let Err(e) = fs::remove_file(&marker)
        && e.kind() != io::ErrorKind::NotFound
    {
        log::warn!("cannot remove the old marker: {e}");
    }
    // The children are kept to reap them: a killed child that is not
    // waited for stays a zombie, which still answers `kill(pid, 0)`.
    let children: Rc<RefCell<Vec<Child>>> = Rc::default();
    let (bundle, old, data_dir) = (args.bundle.clone(), args.old.clone(), args.data_dir.clone());
    let deps = WatchDeps {
        wait_gone: Box::new({
            let children = children.clone();
            move |pid, timeout| wait_gone(&children, pid, timeout)
        }),
        spawn: Box::new({
            let children = children.clone();
            move |program, program_args| spawn(&children, program, program_args)
        }),
        marker_says: Box::new({
            let data_dir = data_dir.clone();
            move || read_marker(&data_dir)
        }),
        kill: Box::new(|pid| {
            if let Ok(pid) = i32::try_from(pid) {
                // SAFETY: kill only sends a signal to the process `pid`.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }),
        restore: Box::new(move || swap::restore(&bundle, &old)),
        record_failure: Box::new(move |failed| {
            record_failure(&data_dir, failed, crate::running_version());
        }),
        sleep: Box::new(std::thread::sleep),
    };
    match watch_with(&args, deps) {
        WatchOutcome::Started => ExitCode::SUCCESS,
        WatchOutcome::RolledBack => {
            // A marker the new copy wrote after the last look would make
            // the next update attempt of this version look started.
            if let Err(e) = fs::remove_file(&marker)
                && e.kind() != io::ErrorKind::NotFound
            {
                log::warn!("cannot remove the marker: {e}");
            }
            log::warn!("update to {} rolled back", args.version);
            ExitCode::FAILURE
        }
    }
}

/// Records `failed` in `update.json` for the old copy's banner, with
/// `watcher` (this binary's version: the old copy, which runs next) as the
/// version that ran last. The new copy may have saved itself as that
/// before it was killed, which would cost What's new once the update does
/// start.
fn record_failure(data_dir: &Path, failed: FailedUpdate, watcher: Option<Version>) {
    let path = UpdateFile::path(data_dir);
    let mut file = UpdateFile::load(&path);
    file.failed = Some(failed);
    file.last_run_version = watcher.or(file.last_run_version);
    if let Err(e) = file.save(&path) {
        log::warn!("cannot record the failed update: {e}");
    }
}

/// Polls every 100 ms until `pid` is gone or `timeout` has passed.
fn wait_gone(children: &RefCell<Vec<Child>>, pid: u32, timeout: Duration) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return true;
    };
    let start = Instant::now();
    loop {
        let reaped = children
            .borrow_mut()
            .iter_mut()
            .find(|child| child.id() == pid)
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))));
        // SAFETY: signal 0 only asks whether the process exists.
        let gone = reaped
            || unsafe { libc::kill(raw, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Starts `program` in its own process group, with no terminal and no
/// stdio tied to the watcher.
fn spawn(children: &RefCell<Vec<Child>>, program: &Path, args: &[&str]) -> Option<u32> {
    use std::os::unix::process::CommandExt;

    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            children.borrow_mut().push(child);
            Some(pid)
        }
        Err(e) => {
            log::warn!("cannot start {}: {e}", program.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn args() -> WatchArgs {
        WatchArgs {
            old_pid: 41,
            bundle: PathBuf::from("/Applications/Vento.app"),
            old: PathBuf::from("/Applications/Vento.app.old"),
            data_dir: PathBuf::from("/Users/me/Library/Application Support/Vento"),
            version: Version::parse("0.3.0").unwrap(),
        }
    }

    /// The calls the fake effects saw, in order.
    type Log = Rc<RefCell<Vec<String>>>;

    /// The failures the fake effects recorded.
    type Failures = Rc<RefCell<Vec<FailedUpdate>>>;

    /// How a fake run behaves.
    #[derive(Clone, Copy)]
    struct Script {
        /// Whether the old copy quits in time.
        old_quits: bool,
        /// The poll (counted in sleeps) from which the marker names the
        /// version; `None` for never.
        marker_after: Option<u32>,
        /// Whether starting the new copy works.
        spawn_works: bool,
        /// Whether a killed copy is gone in time.
        kill_works: bool,
    }

    /// Fake effects that record each call into the returned log, and the
    /// failures recorded.
    fn fake(script: Script) -> (WatchDeps, Log, Failures) {
        let log: Log = Rc::default();
        let failures: Failures = Rc::default();
        let sleeps = Rc::new(Cell::new(0u32));
        let deps = WatchDeps {
            wait_gone: Box::new({
                let log = log.clone();
                move |pid, timeout| {
                    log.borrow_mut()
                        .push(format!("wait_gone {pid} {}s", timeout.as_secs()));
                    if timeout == KILL_WAIT {
                        script.kill_works
                    } else {
                        pid != 41 || script.old_quits
                    }
                }
            }),
            spawn: Box::new({
                let log = log.clone();
                move |program, program_args| {
                    log.borrow_mut().push(format!(
                        "spawn {} {}",
                        program.display(),
                        program_args.join(" ")
                    ));
                    let first = program_args == [AFTER_UPDATE];
                    (!first || script.spawn_works).then_some(if first { 77 } else { 78 })
                }
            }),
            marker_says: Box::new({
                let sleeps = sleeps.clone();
                move || {
                    script
                        .marker_after
                        .filter(|&at| sleeps.get() >= at)
                        .map(|_| Version::parse("0.3.0").unwrap())
                }
            }),
            kill: Box::new({
                let log = log.clone();
                move |pid| log.borrow_mut().push(format!("kill {pid}"))
            }),
            restore: Box::new({
                let log = log.clone();
                move || {
                    log.borrow_mut().push("restore".into());
                    Ok(())
                }
            }),
            record_failure: Box::new({
                let (log, failures) = (log.clone(), failures.clone());
                move |failed| {
                    log.borrow_mut().push("record_failure".into());
                    failures.borrow_mut().push(failed);
                }
            }),
            sleep: Box::new(move |d| {
                assert_eq!(d, Duration::from_millis(250));
                sleeps.set(sleeps.get() + 1);
            }),
        };
        (deps, log, failures)
    }

    const NEW: &str = "spawn /Applications/Vento.app/Contents/MacOS/vento --after-update";
    const OLD_BACK: &str = "spawn /Applications/Vento.app/Contents/MacOS/vento ";

    #[test]
    fn args_round_trip() {
        let a = args();
        let mut argv = vec!["vento".to_string()];
        argv.extend(to_args(&a));
        assert_eq!(parse_args(&argv), Some(a.clone()));
        assert_eq!(parse_args(&to_args(&a)), Some(a));
        assert_eq!(parse_args(&["vento".into()]), None);
        assert_eq!(parse_args(&["vento".into(), "--after-update".into()]), None);
        assert_eq!(
            parse_args(&["vento".into(), FLAG.into(), "41".into()]),
            None
        );
    }

    #[test]
    fn pid_0_and_1_are_refused() {
        for pid in [0, 1] {
            let argv = to_args(&WatchArgs {
                old_pid: pid,
                ..args()
            });
            assert_eq!(parse_args(&argv), None, "pid {pid}");
        }
    }

    #[test]
    fn a_malformed_watch_line_never_starts_the_app() {
        let a = args();
        let mut argv = vec!["vento".to_string()];
        argv.extend(to_args(&a));
        assert_eq!(dispatch(&argv), Dispatch::Watch(a));
        let normal = ["vento".to_string(), AFTER_UPDATE.to_string()];
        assert_eq!(dispatch(&normal), Dispatch::Normal);
        assert_eq!(dispatch(&["vento".to_string()]), Dispatch::Normal);
        let mut bad_version = argv.clone();
        *bad_version.last_mut().unwrap() = "0.3".into();
        assert_eq!(dispatch(&bad_version), Dispatch::Malformed);
        let short = ["vento".to_string(), FLAG.to_string(), "41".to_string()];
        assert_eq!(dispatch(&short), Dispatch::Malformed);
        let late = ["vento".to_string(), "-x".into(), "-y".into(), FLAG.into()];
        assert_eq!(dispatch(&late), Dispatch::Malformed);
        let mut pid_0 = argv;
        pid_0[2] = "0".into();
        assert_eq!(dispatch(&pid_0), Dispatch::Malformed);
    }

    #[test]
    fn marker_path_is_in_the_data_folder() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Vento");
        write_marker(&data, Version::parse("0.3.0").unwrap()).unwrap();
        let text = fs::read_to_string(data.join("update-started")).unwrap();
        assert_eq!(text.trim(), "0.3.0");
        assert_eq!(read_marker(&data), Version::parse("0.3.0"));
        fs::write(marker_path(&data), "junk").unwrap();
        assert_eq!(read_marker(&data), None);
        assert_eq!(read_marker(&dir.path().join("missing")), None);
    }

    #[test]
    fn a_recorded_failure_makes_the_watcher_the_last_that_ran() {
        let dir = tempfile::tempdir().unwrap();
        let path = UpdateFile::path(dir.path());
        let v = |s: &str| Version::parse(s).unwrap();
        // The new copy saved itself before it was killed.
        UpdateFile {
            last_run_version: Some(v("0.4.0")),
            last_check: Some(9),
            ..UpdateFile::default()
        }
        .save(&path)
        .unwrap();
        let failed = FailedUpdate {
            version: v("0.4.0"),
            reason: NOT_STARTED_REASON.into(),
        };
        record_failure(dir.path(), failed.clone(), Some(v("0.3.0")));
        let file = UpdateFile::load(&path);
        assert_eq!(file.failed, Some(failed.clone()));
        assert_eq!(file.last_run_version, Some(v("0.3.0")));
        assert_eq!(file.last_check, Some(9), "the rest is kept");
        // An unreadable own version leaves the last run as it was.
        record_failure(dir.path(), failed, None);
        assert_eq!(UpdateFile::load(&path).last_run_version, Some(v("0.3.0")));
    }

    #[test]
    fn marker_in_time_means_started_and_no_restore() {
        let (deps, log, failures) = fake(Script {
            old_quits: true,
            marker_after: Some(119),
            spawn_works: true,
            kill_works: true,
        });
        assert_eq!(watch_with(&args(), deps), WatchOutcome::Started);
        assert_eq!(*log.borrow(), ["wait_gone 41 10s", NEW]);
        assert!(failures.borrow().is_empty());
    }

    #[test]
    fn a_marker_at_the_last_poll_still_counts() {
        let (deps, _, _) = fake(Script {
            old_quits: true,
            marker_after: Some(120),
            spawn_works: true,
            kill_works: true,
        });
        assert_eq!(watch_with(&args(), deps), WatchOutcome::Started);
    }

    #[test]
    fn no_marker_rolls_back_and_records_failure() {
        let (deps, log, failures) = fake(Script {
            old_quits: true,
            marker_after: None,
            spawn_works: true,
            kill_works: true,
        });
        assert_eq!(watch_with(&args(), deps), WatchOutcome::RolledBack);
        assert_eq!(
            *log.borrow(),
            [
                "wait_gone 41 10s",
                NEW,
                "kill 77",
                "wait_gone 77 5s",
                "restore",
                "record_failure",
                OLD_BACK,
            ]
        );
        assert_eq!(
            *failures.borrow(),
            [FailedUpdate {
                version: Version::parse("0.3.0").unwrap(),
                reason: "The new version didn't start.".into(),
            }]
        );
    }

    #[test]
    fn a_marker_for_another_version_does_not_count() {
        let (mut deps, log, _) = fake(Script {
            old_quits: true,
            marker_after: None,
            spawn_works: true,
            kill_works: true,
        });
        deps.marker_says = Box::new(|| Version::parse("0.2.0"));
        assert_eq!(watch_with(&args(), deps), WatchOutcome::RolledBack);
        assert!(log.borrow().contains(&"restore".to_string()));
    }

    #[test]
    fn a_new_copy_that_cannot_start_rolls_back_at_once() {
        let (deps, log, failures) = fake(Script {
            old_quits: true,
            marker_after: Some(0),
            spawn_works: false,
            kill_works: true,
        });
        assert_eq!(watch_with(&args(), deps), WatchOutcome::RolledBack);
        assert_eq!(
            *log.borrow(),
            [
                "wait_gone 41 10s",
                NEW,
                "restore",
                "record_failure",
                OLD_BACK
            ]
        );
        assert_eq!(failures.borrow()[0].reason, NOT_STARTED_REASON);
    }

    #[test]
    fn old_copy_never_quitting_rolls_back_without_starting_new() {
        let (deps, log, failures) = fake(Script {
            old_quits: false,
            marker_after: Some(0),
            spawn_works: true,
            kill_works: true,
        });
        assert_eq!(watch_with(&args(), deps), WatchOutcome::RolledBack);
        assert_eq!(
            *log.borrow(),
            [
                "wait_gone 41 10s",
                "kill 41",
                "wait_gone 41 5s",
                "restore",
                "record_failure",
                OLD_BACK,
            ]
        );
        assert_eq!(failures.borrow()[0].reason, "Vento didn't quit");
    }

    #[test]
    fn an_old_copy_that_survives_the_kill_is_not_started_twice() {
        let (deps, log, failures) = fake(Script {
            old_quits: false,
            marker_after: Some(0),
            spawn_works: true,
            kill_works: false,
        });
        assert_eq!(watch_with(&args(), deps), WatchOutcome::RolledBack);
        assert_eq!(
            *log.borrow(),
            [
                "wait_gone 41 10s",
                "kill 41",
                "wait_gone 41 5s",
                "restore",
                "record_failure",
            ]
        );
        assert_eq!(failures.borrow()[0].reason, NOT_QUIT_REASON);
    }

    #[test]
    fn a_new_copy_that_survives_the_kill_is_not_joined_by_the_old() {
        let (deps, log, failures) = fake(Script {
            old_quits: true,
            marker_after: None,
            spawn_works: true,
            kill_works: false,
        });
        assert_eq!(watch_with(&args(), deps), WatchOutcome::RolledBack);
        assert_eq!(
            *log.borrow(),
            [
                "wait_gone 41 10s",
                NEW,
                "kill 77",
                "wait_gone 77 5s",
                "restore",
                "record_failure",
            ]
        );
        assert_eq!(failures.borrow()[0].reason, NOT_STARTED_REASON);
    }

    #[test]
    fn a_failed_restore_starts_the_old_copy_where_it_is() {
        let (mut deps, log, _) = fake(Script {
            old_quits: true,
            marker_after: None,
            spawn_works: true,
            kill_works: true,
        });
        deps.restore = Box::new(|| Err(io::Error::other("busy")));
        assert_eq!(watch_with(&args(), deps), WatchOutcome::RolledBack);
        assert_eq!(
            log.borrow().last().unwrap(),
            "spawn /Applications/Vento.app.old/Contents/MacOS/vento "
        );
    }
}
