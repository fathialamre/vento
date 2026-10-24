//! Opening files: default apps, "Open With", showing files in the file
//! manager, and the kind a file manager shows.

use std::path::{Path, PathBuf};

use gio::prelude::*;
use gio_unix::DesktopAppInfo;

use crate::{AppInfo, PlatformError};

/// The content type of `path` itself (`inode/directory` for a folder), or
/// `None` if it can't be read.
fn content_type(path: &Path) -> Option<String> {
    let info = gio::File::for_path(path)
        .query_info(
            gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok()?;
    info.content_type().map(|t| t.to_string())
}

/// The kind Files shows for `path` ("PNG image", "Folder").
pub(super) fn kind_description(path: &Path) -> Option<String> {
    let ty = content_type(path)?;
    Some(gio::content_type_get_description(&ty).to_string())
}

/// The `.desktop` file behind `app`; only those can be listed, since
/// [`AppInfo::path`] is what [`open_with`] opens again.
fn desktop_file(app: &gio::AppInfo) -> Option<PathBuf> {
    app.downcast_ref::<DesktopAppInfo>()?.filename()
}

/// The apps that can open `path`, its default first, each once.
pub(super) fn apps_for(path: &Path) -> Vec<AppInfo> {
    let Some(ty) = content_type(path) else {
        return Vec::new();
    };
    let default = gio::AppInfo::default_for_type(&ty, false);
    let default_id = default.as_ref().and_then(|a| a.id());
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for app in default.iter().chain(&gio::AppInfo::all_for_type(&ty)) {
        let Some(path) = desktop_file(app) else {
            continue;
        };
        let id = app.id();
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        out.push(AppInfo {
            path,
            name: app.display_name().to_string(),
            is_default: id.is_some() && id == default_id,
        });
    }
    out
}

/// Open each of `paths` with its default app.
pub(super) fn open_paths(paths: &[PathBuf]) -> Result<(), PlatformError> {
    let mut failed = Vec::new();
    for path in paths {
        let uri = gio::File::for_path(path).uri();
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE) {
            failed.push(format!("{}: {}", path.display(), e.message()));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(PlatformError::Failed(failed.join("; ")))
    }
}

/// Open all of `paths` with the app of the `.desktop` file `app`. gio
/// starts the app before it returns, so every failure is returned here.
pub(super) fn open_with(paths: &[PathBuf], app: &Path) -> Result<(), PlatformError> {
    let info = DesktopAppInfo::from_filename(app)
        .ok_or_else(|| PlatformError::Failed(format!("not an app: {}", app.display())))?;
    let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
    info.launch(&files, gio::AppLaunchContext::NONE)
        .map_err(|e| PlatformError::Failed(e.message().to_owned()))
}

/// Show `paths` selected in the file manager (`FileManager1.ShowItems`);
/// if none answers, open the folder of the first one.
pub(super) fn reveal(paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let uris: Vec<String> = paths
        .iter()
        .map(|p| gio::File::for_path(p).uri().to_string())
        .collect();
    let shown = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).and_then(|bus| {
        bus.call_sync(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            Some(&(uris, "").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
    });
    if let Err(e) = shown {
        log::warn!("ShowItems failed ({}); opening the folder", e.message());
        let folder = paths[0].parent().unwrap_or(Path::new("/"));
        if let Err(e) = open_paths(&[folder.to_path_buf()]) {
            log::warn!("could not show {}: {e}", paths[0].display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_has_no_kind_and_no_apps() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("never-created");
        assert_eq!(kind_description(&gone), None);
        assert!(apps_for(&gone).is_empty());
    }

    #[test]
    fn a_folder_has_a_kind() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(content_type(dir.path()).as_deref(), Some("inode/directory"));
        assert!(kind_description(dir.path()).is_some_and(|k| !k.is_empty()));
    }

    #[test]
    fn apps_are_listed_once_with_at_most_one_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        std::fs::write(&file, "hello").unwrap();
        let apps = apps_for(&file);
        assert!(apps.iter().filter(|a| a.is_default).count() <= 1);
        if let Some(i) = apps.iter().position(|a| a.is_default) {
            assert_eq!(i, 0, "the default app comes first");
        }
        let mut paths: Vec<_> = apps.iter().map(|a| &a.path).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), apps.len());
    }

    #[test]
    fn opening_with_a_file_that_is_no_app_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let not_app = dir.path().join("not-an-app.desktop");
        std::fs::write(&not_app, "nonsense").unwrap();
        assert!(matches!(
            open_with(&[], &not_app),
            Err(PlatformError::Failed(_))
        ));
    }
}
