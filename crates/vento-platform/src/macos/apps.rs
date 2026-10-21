//! Opening files, with their default app or a chosen one, revealing them in
//! Finder, and listing the apps that can open a file.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSBundle, NSError, NSFileManager, NSString, NSURL};

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
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let bundle_id = |app: &Path| {
            let url = file_url(app).ok()?;
            Some(
                NSBundle::bundleWithURL(&url)?
                    .bundleIdentifier()?
                    .to_string(),
            )
        };
        app_list(default, all, home.as_deref(), bundle_id, |app| {
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

/// Opens each of `paths` with its default app.
pub(super) fn open_paths(paths: &[PathBuf]) -> Result<(), PlatformError> {
    let urls = file_urls(paths)?;
    let workspace = NSWorkspace::sharedWorkspace();
    let failed: Vec<String> = paths
        .iter()
        .zip(&urls)
        .filter(|(_, url)| !workspace.openURL(url))
        .map(|(p, _)| display_name(p))
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(PlatformError::Failed(format!(
            "no app opened {}",
            failed.join(", ")
        )))
    }
}

/// Opens all of `paths` with the app bundle `app`. AppKit reports a failed
/// launch later, on a queue of its own: `on_error` gets the reason then.
pub(super) fn open_with(
    paths: &[PathBuf],
    app: &Path,
    on_error: Box<dyn Fn(String) + Send>,
) -> Result<(), PlatformError> {
    if paths.is_empty() {
        return Ok(());
    }
    let urls = file_urls(paths)?;
    if std::fs::metadata(app).is_err() {
        return Err(PlatformError::Failed(format!(
            "{} is no longer there",
            display_name(app)
        )));
    }
    let app_url = file_url(app)?;
    let configuration = NSWorkspaceOpenConfiguration::configuration();
    // Called on another thread: the mutex makes the handler shareable.
    let on_error = Mutex::new(on_error);
    let done = RcBlock::new(
        move |_app: *mut NSRunningApplication, error: *mut NSError| {
            // A panic must not unwind into AppKit through the block: that
            // aborts vento.
            let reported = catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: AppKit passes a valid NSError or null.
                let Some(error) = (unsafe { error.as_ref() }) else {
                    return;
                };
                let reason = error.localizedDescription().to_string();
                log::warn!("cannot open with the chosen app: {reason}");
                let f = on_error.lock().unwrap_or_else(PoisonError::into_inner);
                f(reason);
            }));
            if reported.is_err() {
                log::error!("open-with completion handler panicked");
            }
        },
    );
    NSWorkspace::sharedWorkspace().openURLs_withApplicationAtURL_configuration_completionHandler(
        &NSArray::from_retained_slice(&urls),
        &app_url,
        &configuration,
        Some(&done),
    );
    Ok(())
}

fn file_urls(paths: &[PathBuf]) -> Result<Vec<Retained<NSURL>>, PlatformError> {
    paths.iter().map(|p| file_url(p)).collect()
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
/// the other apps of `all` in the order given. Each app appears once: copies
/// with the same bundle id (`bundle_id`) are one app, shown as the copy the
/// system opens by default if it is one of them, otherwise as the copy in
/// /Applications, then in `home`/Applications, then the first other one.
/// `name` gives an app's display name.
fn app_list(
    default: Option<PathBuf>,
    all: Vec<PathBuf>,
    home: Option<&Path>,
    bundle_id: impl Fn(&Path) -> Option<String>,
    name: impl Fn(&Path) -> String,
) -> Vec<AppInfo> {
    let rank = |path: &Path| {
        if path.starts_with("/Applications") {
            0
        } else if home.is_some_and(|h| path.starts_with(h.join("Applications"))) {
            1
        } else {
            2
        }
    };
    // Copies by app, in the order the apps first appear; the default leads.
    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for path in default.iter().cloned().chain(all) {
        let key = bundle_id(&path).unwrap_or_else(|| format!("path:{}", path.display()));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, copies)) if copies.contains(&path) => {}
            Some((_, copies)) => copies.push(path),
            None => groups.push((key, vec![path])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(_, copies)| {
            let is_default = default.as_ref().is_some_and(|d| copies.contains(d));
            let path = if is_default {
                default.clone()
            } else {
                // The first of the best rank.
                copies.iter().min_by_key(|p| rank(p)).cloned()
            }?;
            Some(AppInfo {
                name: name(&path),
                path,
                is_default,
            })
        })
        .collect()
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

    fn stem(path: &Path) -> String {
        path.file_stem().unwrap().to_string_lossy().into_owned()
    }

    fn no_id(_: &Path) -> Option<String> {
        None
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
            None,
            no_id,
            stem,
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
    fn copies_of_one_app_show_once_preferring_applications_then_home() {
        let p = |s: &str| PathBuf::from(s);
        let id = |path: &Path| Some(format!("com.example.{}", stem(path).to_lowercase()));
        let home = p("/Users/me");
        let apps = app_list(
            None,
            vec![
                p("/Volumes/Old/Zed.app"),
                p("/Users/me/Applications/Zed.app"),
                p("/Applications/Zed.app"),
                p("/Volumes/Old/Notes.app"),
                p("/Users/me/Applications/Notes.app"),
                p("/Volumes/Old/Solo.app"),
            ],
            Some(&home),
            id,
            stem,
        );
        let paths: Vec<&Path> = apps.iter().map(|a| a.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![
                Path::new("/Applications/Zed.app"),
                Path::new("/Users/me/Applications/Notes.app"),
                Path::new("/Volumes/Old/Solo.app"),
            ]
        );
    }

    #[test]
    fn the_default_copy_stands_for_its_app() {
        let p = |s: &str| PathBuf::from(s);
        let apps = app_list(
            Some(p("/Users/me/Applications/Zed.app")),
            vec![
                p("/Applications/Zed.app"),
                p("/Users/me/Applications/Zed.app"),
            ],
            Some(Path::new("/Users/me")),
            |_| Some("dev.zed.Zed".into()),
            stem,
        );
        assert_eq!(apps.len(), 1);
        assert!(apps[0].is_default);
        assert_eq!(apps[0].path, p("/Users/me/Applications/Zed.app"));
    }

    #[test]
    fn without_a_default_none_is_marked() {
        let apps = app_list(None, vec![PathBuf::from("/A/Zed.app")], None, no_id, |_| {
            "Zed".into()
        });
        assert_eq!(apps.len(), 1);
        assert!(!apps[0].is_default);
        assert!(app_list(None, Vec::new(), None, no_id, |_| String::new()).is_empty());
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
}
