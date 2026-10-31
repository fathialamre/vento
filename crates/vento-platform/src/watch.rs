//! A watch on one folder, for the Screenshots view.

use std::path::Path;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::PlatformError;

/// A running watch on a folder. It stops when dropped.
pub struct FolderWatch {
    /// Kept alive for as long as the watch should run.
    _watcher: RecommendedWatcher,
}

/// Whether a report of the watch should wake the caller: every change and
/// every error, but not a read (Linux reports opens and closes of files
/// that were only read, which the caller's own rescan of the folder causes).
fn wakes(report: &notify::Result<notify::Event>) -> bool {
    match report {
        Ok(event) => !matches!(event.kind, notify::EventKind::Access(_)),
        Err(_) => true,
    }
}

/// Watches `dir` itself (not what is below it) and calls `on_event` for
/// every change in it (not for files only read), and for every error the
/// watch reports, on a thread of the watching library. `on_event` must be quick and must not block.
/// The watch stops when the returned [`FolderWatch`] is dropped.
pub fn watch_folder(
    dir: &Path,
    on_event: Box<dyn Fn() + Send + 'static>,
) -> Result<FolderWatch, PlatformError> {
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Err(e) = &result {
            log::debug!("folder watch error: {e}");
        }
        if wakes(&result) {
            on_event();
        }
    })
    .map_err(|e| PlatformError::Failed(e.to_string()))?;
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .map_err(|e| PlatformError::Failed(e.to_string()))?;
    Ok(FolderWatch { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind};
    use notify::{Event, EventKind};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn watch_folder_reports_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let _watch = watch_folder(
            dir.path(),
            Box::new(move || {
                let _ = tx.send(());
            }),
        )
        .unwrap();
        std::fs::write(dir.path().join("Screenshot.png"), b"x").unwrap();
        rx.recv_timeout(Duration::from_secs(2))
            .expect("the watch reports the new file");
    }

    #[test]
    fn reads_do_not_wake_the_caller_but_changes_and_errors_do() {
        let report = |kind| Ok(Event::new(kind));
        assert!(!wakes(&report(EventKind::Access(AccessKind::Open(
            AccessMode::Any
        )))));
        assert!(!wakes(&report(EventKind::Access(AccessKind::Close(
            AccessMode::Read
        )))));
        assert!(wakes(&report(EventKind::Create(CreateKind::File))));
        assert!(wakes(&report(EventKind::Modify(ModifyKind::Any))));
        assert!(wakes(&report(EventKind::Remove(RemoveKind::File))));
        assert!(wakes(&Err(notify::Error::generic("x"))));
    }

    #[test]
    fn watch_folder_of_a_missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(watch_folder(&dir.path().join("gone"), Box::new(|| {})).is_err());
    }
}
