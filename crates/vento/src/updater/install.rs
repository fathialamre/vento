//! Installing an update on Windows with the release's setup: the checks
//! made before anything is downloaded, the copy of the old `vento.exe` the
//! watcher runs from, the silent setup and the way back.
//!
//! Nothing is replaced while Vento runs: the watcher runs the setup once
//! this copy has quit, from a copy of it outside the folder the setup
//! writes to.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::SwapError;

/// The uninstaller Inno Setup leaves beside the `vento.exe` it installed.
const UNINSTALLER: &str = "unins000.exe";

/// The setup's switches for an update: no window, no question, no restart
/// of Windows. The folder and the tasks of the first install (start at
/// sign-in, the desktop icon) are kept, as Inno Setup does by default.
const SETUP_ARGS: &[&str] = &[
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/NOCANCEL",
    "/SP-",
];

/// How long the setup may run before it is given up on.
const SETUP_WAIT: Duration = Duration::from_secs(10 * 60);

/// How often the setup is asked whether it is done.
const SETUP_POLL: Duration = Duration::from_millis(250);

/// The running `vento.exe`, once it is known to be a copy the setup
/// installed, in a folder Vento may write to.
pub fn installed_exe() -> Result<PathBuf, SwapError> {
    let exe = std::env::current_exe().map_err(|e| SwapError::Io(e.to_string()))?;
    check_installed(&exe)?;
    Ok(exe)
}

/// Checks, before anything is downloaded, that `exe` was installed by the
/// setup (its uninstaller is beside it) in a folder Vento may write to. A
/// copy run from anywhere else (a build, a copy on a USB stick) would be
/// left as it is by the setup, which installs to its own folder.
pub fn check_installed(exe: &Path) -> Result<(), SwapError> {
    let not_installed = || SwapError::NotABundle(exe.to_path_buf());
    let dir = exe.parent().ok_or_else(not_installed)?;
    if !dir.join(UNINSTALLER).is_file() {
        return Err(not_installed());
    }
    let probe = dir.join(".vento-write-test");
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(SwapError::NotWritable(dir.to_path_buf())),
    }
}

/// Where the copy of the old `vento.exe` waits, in the data folder `data_dir`.
pub fn old_path(data_dir: &Path) -> PathBuf {
    data_dir.join("vento-old.exe")
}

/// Copies `exe` to [`old_path`]: the watcher runs from that copy, and it is
/// what [`restore`] puts back. A copy left from an earlier update is
/// replaced.
pub fn keep_old_copy(exe: &Path, data_dir: &Path) -> Result<PathBuf, SwapError> {
    let old = old_path(data_dir);
    fs::create_dir_all(data_dir)
        .and_then(|()| fs::copy(exe, &old))
        .map_err(|e| SwapError::Io(format!("cannot copy {} aside: {e}", exe.display())))?;
    Ok(old)
}

/// Puts the old copy `old` back over `exe` (the new one, which did not
/// start, or whatever a failed setup left). Without `old` nothing is
/// touched.
pub fn restore(exe: &Path, old: &Path) -> io::Result<()> {
    old.metadata()?;
    fs::copy(old, exe).map(drop)
}

/// Runs the setup `setup` silently, with its log written to `log`, and
/// waits for it. `Err` says why when it could not start, failed, or was
/// still running after [`SETUP_WAIT`] (it is then killed). Inno Setup
/// undoes a failed install itself.
pub fn run_setup(setup: &Path, log: &Path) -> Result<(), String> {
    let mut log_arg = std::ffi::OsString::from("/LOG=");
    log_arg.push(log);
    let mut child = Command::new(setup)
        .args(SETUP_ARGS)
        .arg(log_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", setup.display()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("the setup ended with {status}")),
            Ok(None) if start.elapsed() < SETUP_WAIT => std::thread::sleep(SETUP_POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the setup was still running after 10 minutes".into());
            }
            Err(e) => return Err(format!("cannot wait for the setup: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_without_the_uninstaller_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("vento.exe");
        fs::write(&exe, "exe").unwrap();
        assert_eq!(
            check_installed(&exe),
            Err(SwapError::NotABundle(exe.clone()))
        );
        fs::write(dir.path().join(UNINSTALLER), "uninstaller").unwrap();
        assert_eq!(check_installed(&exe), Ok(()));
        assert!(
            !dir.path().join(".vento-write-test").exists(),
            "the probe is gone"
        );
    }

    #[test]
    fn the_test_binary_is_not_installed() {
        assert!(matches!(installed_exe(), Err(SwapError::NotABundle(_))));
    }

    #[test]
    fn the_old_copy_waits_in_the_data_folder_and_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Vento").join("vento.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "old").unwrap();
        let data = dir.path().join("data");
        // A copy left from an earlier update is replaced.
        fs::create_dir_all(&data).unwrap();
        fs::write(old_path(&data), "older").unwrap();
        let old = keep_old_copy(&exe, &data).unwrap();
        assert_eq!(old, data.join("vento-old.exe"));
        assert_eq!(fs::read_to_string(&old).unwrap(), "old");
        fs::write(&exe, "new").unwrap();
        restore(&exe, &old).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
    }

    #[test]
    fn restore_without_the_old_copy_leaves_the_exe_alone() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("vento.exe");
        fs::write(&exe, "new").unwrap();
        assert!(restore(&exe, &dir.path().join("vento-old.exe")).is_err());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
    }

    #[test]
    fn a_setup_that_cannot_start_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("vento-9.9.9-windows-setup.exe");
        let err = run_setup(&missing, &dir.path().join("setup.log")).unwrap_err();
        assert!(err.starts_with("cannot start"), "{err}");
    }
}
