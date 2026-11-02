//! Replacing the running `Vento.app` with the downloaded one: the checks
//! made before anything is touched, the unzip, the two renames and the way
//! back.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::SwapError;

/// The `.app` bundle the running executable lives in: three levels above
/// `Vento.app/Contents/MacOS/vento`. `None` when it is not in a bundle (a
/// `cargo run` build, say).
pub fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    is_app(bundle).then(|| bundle.to_path_buf())
}

/// Whether `path` is named `*.app`.
fn is_app(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "app")
}

/// Checks, before anything is touched, that `bundle` can be replaced: not a
/// translocated copy, an `.app` folder, and in a folder Vento may write to.
pub fn check_replaceable(bundle: &Path) -> Result<(), SwapError> {
    if bundle.to_string_lossy().contains("/AppTranslocation/") {
        return Err(SwapError::Translocated);
    }
    if !is_app(bundle) || !bundle.is_dir() {
        return Err(SwapError::NotABundle(bundle.to_path_buf()));
    }
    let parent = bundle
        .parent()
        .ok_or_else(|| SwapError::NotABundle(bundle.to_path_buf()))?;
    let probe = parent.join(".vento-write-test");
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(SwapError::NotWritable(parent.to_path_buf())),
    }
}

/// Unpacks the release `zip` into the folder `into` with `ditto` (which
/// keeps the bundle's symlinks, permissions and signature) and returns the
/// one `*/Vento.app` it holds.
pub fn unpack(zip: &Path, into: &Path) -> Result<PathBuf, SwapError> {
    let status = Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(zip)
        .arg(into)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| SwapError::Unpack(format!("cannot run ditto: {e}")))?;
    if !status.success() {
        return Err(SwapError::Unpack(format!("ditto failed: {status}")));
    }
    let entries = fs::read_dir(into).map_err(|e| SwapError::Unpack(e.to_string()))?;
    let found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("Vento.app"))
        .filter(|app| app.is_dir())
        .collect();
    match found.as_slice() {
        [app] => Ok(app.clone()),
        [] => Err(SwapError::Unpack("no Vento.app in the archive".into())),
        _ => Err(SwapError::Unpack(
            "more than one Vento.app in the archive".into(),
        )),
    }
}

/// Where the old copy waits while the new one starts: `Vento.app.old`
/// beside the bundle.
pub fn old_path(bundle: &Path) -> PathBuf {
    sibling(bundle, "old")
}

/// `<bundle>.<suffix>` beside `bundle`.
fn sibling(bundle: &Path, suffix: &str) -> PathBuf {
    let mut name = bundle.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    bundle.with_file_name(name)
}

/// Puts `new_app` in the place of `bundle` and keeps the old copy at
/// [`old_path`], whose path it returns. A stale `.old` from an earlier
/// update is removed first. If the new copy cannot be moved in, whatever
/// part of it arrived goes and the old copy is put back with [`restore`].
pub fn swap(bundle: &Path, new_app: &Path) -> Result<PathBuf, SwapError> {
    let old = old_path(bundle);
    if old.symlink_metadata().is_ok() {
        remove_any(&old)
            .map_err(|e| SwapError::Io(format!("cannot remove {}: {e}", old.display())))?;
    }
    fs::rename(bundle, &old)
        .map_err(|e| SwapError::Io(format!("cannot move {} aside: {e}", bundle.display())))?;
    if let Err(e) = move_into_place(new_app, bundle) {
        return match restore(bundle, &old) {
            Ok(()) => Err(SwapError::Io(format!("cannot move the new app in: {e}"))),
            Err(back) => Err(SwapError::Io(format!(
                "cannot move the new app in: {e}; and cannot put {} back: {back}",
                old.display()
            ))),
        };
    }
    Ok(old)
}

/// Moves `from` to `to`: a rename, or a copy with `ditto` when the two are
/// on different volumes (a rename cannot cross them).
fn move_into_place(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            let status = Command::new("/usr/bin/ditto")
                .arg(from)
                .arg(to)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("ditto failed: {status}")))
            }
        }
        Err(e) => Err(e),
    }
}

/// Removes a folder (with everything in it) or a file.
fn remove_any(path: &Path) -> io::Result<()> {
    if path.symlink_metadata()?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Puts the old copy `old` back at `bundle`, and deletes the copy that was
/// at `bundle` (the new one that did not start), if any.
///
/// Without `old` nothing is touched. The copy at `bundle` is renamed aside
/// to `<bundle>.failed` first and deleted only once the old copy is back,
/// so a deletion that fails halfway never stands in the old copy's way;
/// if the old copy then cannot be renamed back, the aside copy returns to
/// `bundle`. Only when the aside rename fails is it deleted in place.
pub fn restore(bundle: &Path, old: &Path) -> io::Result<()> {
    old.symlink_metadata()?;
    if bundle.symlink_metadata().is_err() {
        return fs::rename(old, bundle);
    }
    let failed = sibling(bundle, "failed");
    let aside = (failed.symlink_metadata().is_err() || remove_any(&failed).is_ok())
        && fs::rename(bundle, &failed).is_ok();
    if !aside {
        remove_any(bundle)?;
    }
    if let Err(e) = fs::rename(old, bundle) {
        if aside {
            let _ = fs::rename(&failed, bundle);
        }
        return Err(e);
    }
    if aside && let Err(e) = remove_any(&failed) {
        log::warn!("cannot remove {}: {e}", failed.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A fake `Vento.app` at `path` holding the file `file` with `text`.
    fn fake_app(path: &Path, text: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("file"), text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path.join("file")).unwrap()
    }

    #[test]
    fn translocated_path_is_refused() {
        assert!(matches!(
            check_replaceable(Path::new(
                "/private/var/folders/x/AppTranslocation/ABC/d/Vento.app"
            )),
            Err(SwapError::Translocated)
        ));
    }

    #[test]
    fn a_path_that_is_not_an_app_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("target/debug");
        fs::create_dir_all(&bin).unwrap();
        assert_eq!(check_replaceable(&bin), Err(SwapError::NotABundle(bin)));
    }

    #[test]
    fn a_writable_bundle_is_replaceable_and_the_probe_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        fake_app(&bundle, "v1");
        assert_eq!(check_replaceable(&bundle), Ok(()));
        assert!(!dir.path().join(".vento-write-test").exists());
    }

    /// Gives a folder its write permission back when dropped, so the
    /// tempdir can be cleaned even when an assert fails.
    struct Writable<'a>(&'a Path);

    impl Drop for Writable<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn read_only_parent_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("Apps");
        let bundle = parent.join("Vento.app");
        fake_app(&bundle, "v1");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
        let _guard = Writable(&parent);
        assert_eq!(
            check_replaceable(&bundle),
            Err(SwapError::NotWritable(parent.clone()))
        );
    }

    #[test]
    fn swap_puts_new_in_place_and_keeps_old() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        let new = dir.path().join("download/vento-0.3.0/Vento.app");
        fake_app(&bundle, "v1");
        fake_app(&new, "v2");
        let old = swap(&bundle, &new).unwrap();
        assert_eq!(old, dir.path().join("Vento.app.old"));
        assert_eq!(read(&bundle), "v2");
        assert_eq!(read(&old), "v1");
        assert!(!new.exists());
    }

    #[test]
    fn swap_removes_a_stale_old_copy_first() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        let new = dir.path().join("new/Vento.app");
        fake_app(&bundle, "v1");
        fake_app(&old_path(&bundle), "v0");
        fake_app(&new, "v2");
        let old = swap(&bundle, &new).unwrap();
        assert_eq!(read(&old), "v1");
        assert_eq!(read(&bundle), "v2");
    }

    #[test]
    fn a_failed_move_puts_the_old_copy_back() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        fake_app(&bundle, "v1");
        let missing = dir.path().join("nowhere/Vento.app");
        assert!(matches!(swap(&bundle, &missing), Err(SwapError::Io(_))));
        assert_eq!(read(&bundle), "v1");
        assert!(!old_path(&bundle).exists());
    }

    #[test]
    fn restore_brings_old_back() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        let new = dir.path().join("new/Vento.app");
        fake_app(&bundle, "v1");
        fake_app(&new, "v2");
        let old = swap(&bundle, &new).unwrap();
        restore(&bundle, &old).unwrap();
        assert_eq!(read(&bundle), "v1");
        assert!(!old.exists());
        assert!(!dir.path().join("Vento.app.failed").exists());
    }

    #[test]
    fn restore_works_when_the_new_copy_is_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        let old = old_path(&bundle);
        fake_app(&old, "v1");
        restore(&bundle, &old).unwrap();
        assert_eq!(read(&bundle), "v1");
    }

    #[test]
    fn restore_without_the_old_copy_leaves_the_bundle_alone() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        fake_app(&bundle, "v2");
        assert!(restore(&bundle, &old_path(&bundle)).is_err());
        assert_eq!(read(&bundle), "v2");
    }

    #[test]
    fn restore_replaces_a_stale_failed_copy() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Vento.app");
        fake_app(&bundle, "v2");
        fake_app(&old_path(&bundle), "v1");
        fake_app(&dir.path().join("Vento.app.failed"), "v-1");
        restore(&bundle, &old_path(&bundle)).unwrap();
        assert_eq!(read(&bundle), "v1");
        assert!(!dir.path().join("Vento.app.failed").exists());
    }

    #[test]
    fn unpack_finds_the_app_inside_the_release_zip() {
        let src = tempfile::tempdir().unwrap();
        let tree = src.path().join("vento-0.3.0");
        fake_app(&tree.join("Vento.app"), "v2");
        let zip = src.path().join("x.zip");
        let status = Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&tree)
            .arg(&zip)
            .status()
            .unwrap();
        assert!(status.success());
        let into = tempfile::tempdir().unwrap();
        let app = unpack(&zip, into.path()).unwrap();
        assert!(app.ends_with("vento-0.3.0/Vento.app"), "{}", app.display());
        assert_eq!(read(&app), "v2");
    }

    #[test]
    fn a_broken_zip_is_an_unpack_error() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("x.zip");
        fs::write(&zip, b"not a zip").unwrap();
        let into = dir.path().join("out");
        fs::create_dir(&into).unwrap();
        assert!(matches!(unpack(&zip, &into), Err(SwapError::Unpack(_))));
    }

    #[test]
    fn errors_show_the_folder_and_share_the_move_text() {
        assert_eq!(
            SwapError::NotWritable(PathBuf::from("/Applications")).user_text(),
            "Can't replace Vento in /Applications"
        );
        assert_eq!(
            SwapError::Translocated.user_text(),
            SwapError::NotABundle(PathBuf::from("/x")).user_text()
        );
    }
}
