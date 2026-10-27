//! The one place settings are read and written: every change goes through
//! here, is saved, and reaches everything that listens.

use std::cell::{Cell, Ref, RefCell};
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::SystemTime;

use log::{debug, warn};
use vento_core::settings::{Settings, SettingsError};

/// Called with the settings before and after a change.
type Listener = Box<dyn Fn(&Settings, &Settings)>;

/// Owns the settings file and the in-memory copy everyone shares.
pub struct SettingsHub {
    path: PathBuf,
    /// The copy the executor and the Pocket read; replaced in place.
    shared: Rc<RefCell<Settings>>,
    /// Why the file could not be loaded, while that is still true.
    load_error: RefCell<Option<SettingsError>>,
    /// Modification time of the file as vento last read or wrote it.
    seen_mtime: Cell<Option<SystemTime>>,
    listeners: RefCell<Vec<Listener>>,
}

impl SettingsHub {
    /// Loads the settings file. A file that cannot be loaded is left alone:
    /// the defaults stay in memory and the reason is kept for
    /// [`SettingsHub::load_error`].
    pub fn new(path: PathBuf) -> Rc<Self> {
        // Before the read, so a write in between counts as not yet seen.
        let seen_mtime = Cell::new(modified(&path));
        let (settings, error) = match Settings::try_load(&path) {
            Ok(settings) => (settings, None),
            Err(e) => {
                warn!("invalid settings {}: {e}", path.display());
                (Settings::default(), Some(e))
            }
        };
        Rc::new(SettingsHub {
            path,
            shared: Rc::new(RefCell::new(settings)),
            load_error: RefCell::new(error),
            seen_mtime,
            listeners: RefCell::new(Vec::new()),
        })
    }

    /// The settings copy shared with the executor and the Pocket.
    pub fn shared(&self) -> Rc<RefCell<Settings>> {
        self.shared.clone()
    }

    /// A snapshot of the current settings.
    pub fn current(&self) -> Settings {
        self.shared.borrow().clone()
    }

    /// Why the settings file could not be loaded; `None` when it loaded, or
    /// after a change was saved over it. The error itself, not its text, so
    /// whoever shows it words it in the language of that moment.
    pub fn load_error(&self) -> Option<Ref<'_, SettingsError>> {
        Ref::filter_map(self.load_error.borrow(), Option::as_ref).ok()
    }

    /// Calls `f` with the old and the new settings after every change. A
    /// listener must not add listeners.
    pub fn on_change(&self, f: Listener) {
        self.listeners.borrow_mut().push(f);
    }

    /// Applies `f` to a copy of the settings, saves it, makes it current and
    /// tells the listeners. A hand edit made since the last read is picked up
    /// first, so it is not overwritten; if that file is invalid, `f` is
    /// applied to the current settings and replaces the file. On a save error
    /// nothing changes.
    ///
    /// # Errors
    /// If the settings file cannot be written.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> io::Result<()> {
        self.refresh_from_disk();
        let old = self.current();
        let mut new = old.clone();
        f(&mut new);
        new.save(&self.path)?;
        // Our own write is not a hand edit.
        self.seen_mtime.set(modified(&self.path));
        *self.load_error.borrow_mut() = None;
        self.replace(old, new);
        Ok(())
    }

    /// Reloads the file when it changed since vento last read or wrote it
    /// (the user edited it by hand). A good file replaces the settings and
    /// notifies the listeners; a bad one keeps the settings and records why.
    pub fn refresh_from_disk(&self) {
        let mtime = modified(&self.path);
        if mtime == self.seen_mtime.get() {
            return;
        }
        self.seen_mtime.set(mtime);
        match Settings::try_load(&self.path) {
            Ok(new) => {
                debug!("settings reloaded from {}", self.path.display());
                *self.load_error.borrow_mut() = None;
                let old = self.current();
                self.replace(old, new);
            }
            Err(e) => {
                warn!("settings not reloaded from {}: {e}", self.path.display());
                *self.load_error.borrow_mut() = Some(e);
            }
        }
    }

    fn replace(&self, old: Settings, new: Settings) {
        *self.shared.borrow_mut() = new.clone();
        for listener in self.listeners.borrow().iter() {
            listener(&old, &new);
        }
    }
}

/// When `path` was last modified; `None` if it is missing or unreadable.
fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    use vento_core::geometry::Layout;
    use vento_core::settings::Settings;

    use super::*;

    fn temp_file(text: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        if let Some(text) = text {
            fs::write(&path, text).unwrap();
        }
        (dir, path)
    }

    /// Rewrites `path` and gives it a modification time `secs` after now, so
    /// the change is visible however coarse the file system clock is.
    fn rewrite(path: &PathBuf, text: &str, secs: u64) {
        fs::write(path, text).unwrap();
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(secs))
            .unwrap();
    }

    fn record(hub: &SettingsHub) -> Rc<RefCell<Vec<(Layout, Layout)>>> {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        hub.on_change(Box::new(move |old, new| {
            log.borrow_mut().push((old.layout, new.layout));
        }));
        seen
    }

    #[test]
    fn invalid_file_is_kept_and_reported() {
        let (_dir, path) = temp_file(Some("layout = \"honeycomb\"\n\nthis is not toml\n"));
        let before = fs::read(&path).unwrap();
        let hub = SettingsHub::new(path.clone());
        let error = hub.load_error().expect("an error is recorded");
        assert!(
            matches!(*error, SettingsError::Parse { line: Some(3), .. }),
            "{error}"
        );
        assert_eq!(hub.current(), Settings::default());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn update_saves_notifies_and_clears_error() {
        let (_dir, path) = temp_file(Some("not = = toml"));
        let hub = SettingsHub::new(path.clone());
        assert!(hub.load_error().is_some());
        let seen = record(&hub);
        hub.update(|s| s.layout = Layout::Petals).unwrap();
        assert_eq!(*seen.borrow(), vec![(Layout::Honeycomb, Layout::Petals)]);
        assert!(hub.load_error().is_none());
        assert_eq!(hub.current().layout, Layout::Petals);
        assert_eq!(hub.shared().borrow().layout, Layout::Petals);
        assert_eq!(Settings::try_load(&path).unwrap().layout, Layout::Petals);
        // The hub's own write is not a hand edit.
        hub.refresh_from_disk();
        assert_eq!(seen.borrow().len(), 1);
    }

    #[test]
    fn update_changes_the_shared_copy_in_place() {
        let (_dir, path) = temp_file(None);
        let hub = SettingsHub::new(path.clone());
        let shared = hub.shared();
        hub.update(|s| s.move_hold_ms = 600).unwrap();
        assert_eq!(shared.borrow().move_hold_ms, 600);
    }

    #[test]
    fn update_keeps_a_hand_edit_made_since_the_last_read() {
        let (_dir, path) = temp_file(Some("layout = \"honeycomb\"\n"));
        let hub = SettingsHub::new(path.clone());
        rewrite(&path, "layout = \"petals\"\nmove_hold_ms = 800\n", 5);
        hub.update(|s| s.pocket_keep_copies = true).unwrap();
        let saved = Settings::try_load(&path).unwrap();
        assert_eq!(saved.layout, Layout::Petals);
        assert_eq!(saved.move_hold_ms, 800);
        assert!(saved.pocket_keep_copies);
    }

    #[test]
    fn update_over_an_invalid_hand_edit_replaces_it() {
        let (_dir, path) = temp_file(Some("layout = \"petals\"\n"));
        let hub = SettingsHub::new(path.clone());
        rewrite(&path, "layout = \n", 5);
        hub.update(|s| s.pocket_keep_copies = true).unwrap();
        assert!(hub.load_error().is_none());
        let saved = Settings::try_load(&path).unwrap();
        assert_eq!(saved.layout, Layout::Petals, "in-memory settings are kept");
        assert!(saved.pocket_keep_copies);
    }

    #[test]
    fn missing_file_is_not_created_by_new() {
        let (_dir, path) = temp_file(None);
        let hub = SettingsHub::new(path.clone());
        assert!(hub.load_error().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn refresh_picks_up_hand_edits() {
        let (_dir, path) = temp_file(Some("layout = \"honeycomb\"\n"));
        let hub = SettingsHub::new(path.clone());
        let seen = record(&hub);
        hub.refresh_from_disk();
        assert!(seen.borrow().is_empty(), "unchanged file is not reloaded");
        rewrite(&path, "layout = \"petals\"\n", 5);
        hub.refresh_from_disk();
        assert_eq!(hub.current().layout, Layout::Petals);
        assert_eq!(hub.shared().borrow().layout, Layout::Petals);
        assert_eq!(*seen.borrow(), vec![(Layout::Honeycomb, Layout::Petals)]);
        hub.refresh_from_disk();
        assert_eq!(seen.borrow().len(), 1, "listener called once");
    }

    #[test]
    fn refresh_reports_a_bad_hand_edit() {
        let (_dir, path) = temp_file(Some("layout = \"petals\"\n"));
        let hub = SettingsHub::new(path.clone());
        let seen = record(&hub);
        rewrite(&path, "layout = \n", 5);
        hub.refresh_from_disk();
        assert!(hub.load_error().is_some());
        assert_eq!(hub.current().layout, Layout::Petals, "settings are kept");
        assert!(seen.borrow().is_empty());
        // Fixing the file clears the error.
        rewrite(&path, "layout = \"honeycomb\"\n", 10);
        hub.refresh_from_disk();
        assert!(hub.load_error().is_none());
        assert_eq!(hub.current().layout, Layout::Honeycomb);
    }
}
