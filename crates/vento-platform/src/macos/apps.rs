//! Opening files, with their default app or a chosen one, revealing them in
//! Finder, and listing the apps that can open a file.

use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSError, NSFileManager, NSString, NSURL};

use super::clipboard::file_url;
use crate::{AppInfo, PlatformError};

/// The apps that can open `path`, its default app first; empty when
/// `path` doesn't exist.
pub(super) fn apps_for(path: &Path) -> Vec<AppInfo> {
    if std::fs::symlink_metadata(path).is_err() {
        return Vec::new();
    }
    let Ok(url) = file_url(path) else {
        return Vec::new();
    };
    autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let default = workspace
            .URLForApplicationToOpenURL(&url)
            .and_then(|u| url_path(&u));
        let all = workspace
            .URLsForApplicationsToOpenURL(&url)
            .iter()
            .filter_map(|u| url_path(&u))
            .collect();
        let files = NSFileManager::defaultManager();
        app_list(default, all, |app| {
            let display = match app.to_str() {
                Some(s) => files.displayNameAtPath(&NSString::from_str(s)).to_string(),
                None => app
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            };
            app_name(&display)
        })
    })
}

/// Opens `paths` each with its default app (`app` `None`), or all with the
/// app bundle `app`. With an app, launch failures are only logged: AppKit
/// reports them later, on another thread.
pub(super) fn open_paths(paths: &[PathBuf], app: Option<&Path>) -> Result<(), PlatformError> {
    if paths.is_empty() {
        return Ok(());
    }
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|p| file_url(p))
        .collect::<Result<_, _>>()?;
    let workspace = NSWorkspace::sharedWorkspace();
    let Some(app) = app else {
        let failed: Vec<String> = paths
            .iter()
            .zip(&urls)
            .filter(|(_, url)| !workspace.openURL(url))
            .map(|(p, _)| display_name(p))
            .collect();
        return if failed.is_empty() {
            Ok(())
        } else {
            Err(PlatformError::Failed(format!(
                "no app opened {}",
                failed.join(", ")
            )))
        };
    };
    if std::fs::metadata(app).is_err() {
        return Err(PlatformError::Failed(format!(
            "{} is no longer there",
            display_name(app)
        )));
    }
    let app_url = file_url(app)?;
    let configuration = NSWorkspaceOpenConfiguration::configuration();
    // Runs on a queue of AppKit's: log only, capture nothing.
    let done = RcBlock::new(|_app: *mut NSRunningApplication, error: *mut NSError| {
        // SAFETY: AppKit passes a valid NSError or null.
        if let Some(error) = unsafe { error.as_ref() } {
            log::warn!(
                "cannot open with the chosen app: {}",
                error.localizedDescription()
            );
        }
    });
    workspace.openURLs_withApplicationAtURL_configuration_completionHandler(
        &NSArray::from_retained_slice(&urls),
        &app_url,
        &configuration,
        Some(&done),
    );
    Ok(())
}

/// Opens a Finder window showing `paths`, selected. Paths that are not
/// valid UTF-8 are left out.
pub(super) fn reveal(paths: &[PathBuf]) {
    let urls: Vec<Retained<NSURL>> = paths.iter().filter_map(|p| file_url(p).ok()).collect();
    if urls.is_empty() {
        return;
    }
    NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
}

fn url_path(url: &NSURL) -> Option<PathBuf> {
    url.path().map(|p| PathBuf::from(p.to_string()))
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The apps of an "Open With" list: `default` first, marked as such, then
/// every other app of `all` in the order given, each path once. `name`
/// gives an app's display name.
fn app_list(
    default: Option<PathBuf>,
    all: Vec<PathBuf>,
    name: impl Fn(&Path) -> String,
) -> Vec<AppInfo> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut apps = Vec::new();
    let marked = default.iter().map(|p| (p.clone(), true));
    for (path, is_default) in marked.chain(all.into_iter().map(|p| (p, false))) {
        if seen.contains(&path) {
            continue;
        }
        apps.push(AppInfo {
            name: name(&path),
            path: path.clone(),
            is_default,
        });
        seen.push(path);
    }
    apps
}

/// An app's Finder display name without its ".app" extension.
fn app_name(display: &str) -> String {
    let n = display.len();
    if n > 4 && display.is_char_boundary(n - 4) && display[n - 4..].eq_ignore_ascii_case(".app") {
        display[..n - 4].to_string()
    } else {
        display.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names_lose_their_extension() {
        assert_eq!(app_name("Preview.app"), "Preview");
        assert_eq!(app_name("Visual Studio Code.APP"), "Visual Studio Code");
        assert_eq!(app_name("TextEdit"), "TextEdit");
        assert_eq!(app_name("My.application"), "My.application");
        assert_eq!(app_name(".app"), ".app");
    }

    #[test]
    fn the_default_comes_first_and_each_app_once() {
        let p = |s: &str| PathBuf::from(s);
        let apps = app_list(
            Some(p("/A/TextEdit.app")),
            vec![
                p("/A/Zed.app"),
                p("/A/TextEdit.app"),
                p("/A/BBEdit.app"),
                p("/A/Zed.app"),
            ],
            |path| path.file_stem().unwrap().to_string_lossy().into_owned(),
        );
        let got: Vec<(&str, bool)> = apps
            .iter()
            .map(|a| (a.name.as_str(), a.is_default))
            .collect();
        assert_eq!(
            got,
            vec![("TextEdit", true), ("Zed", false), ("BBEdit", false)]
        );
        assert_eq!(apps[0].path, p("/A/TextEdit.app"));
    }

    #[test]
    fn a_real_file_lists_its_default_once_and_first() {
        let dir = std::env::temp_dir().join(format!("vento-apps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        std::fs::write(&file, "hi").unwrap();
        let apps = apps_for(&file);
        std::fs::remove_dir_all(&dir).unwrap();
        // Launch Services may know no app on a bare machine; when it
        // knows some, the default leads and nothing repeats.
        assert!(apps.iter().skip(1).all(|a| !a.is_default));
        for (i, a) in apps.iter().enumerate() {
            assert!(!a.name.is_empty() && !a.name.to_lowercase().ends_with(".app"));
            assert!(apps[..i].iter().all(|b| b.path != a.path));
        }
        assert!(apps_for(&dir.join("gone.txt")).is_empty());
    }

    #[test]
    fn without_a_default_none_is_marked() {
        let apps = app_list(None, vec![PathBuf::from("/A/Zed.app")], |_| "Zed".into());
        assert_eq!(apps.len(), 1);
        assert!(!apps[0].is_default);
        assert!(app_list(None, Vec::new(), |_| String::new()).is_empty());
    }
}
