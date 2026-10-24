//! Linux [`Platform`] for GNOME.
//!
//! The desktop's own services (Trash, apps, terminals, the folder picker)
//! go through gio. Global input, screens, the clipboard and vento's windows
//! need vento running under XWayland with its GNOME Shell extension; until
//! those land they behave like [`StubPlatform`].

use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::clip::ClipSnapshot;
use vento_core::geometry::{Point, Rect};
use vento_core::trigger::InputSample;

use crate::{
    AppInfo, DropCatcher, IslandHome, MenuEntry, Platform, PlatformError, RgbaImage, StubPlatform,
    TerminalApp,
};

mod apps;
mod icons;
mod portal;
mod terminal;
mod trash;

/// gio-backed [`Platform`] for GNOME.
#[derive(Debug, Default)]
pub struct LinuxPlatform {
    /// Answers what is not done on Linux yet.
    stub: StubPlatform,
}

impl LinuxPlatform {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Platform for LinuxPlatform {
    fn prepare_app(&self) {}

    fn sample_input(&self) -> InputSample {
        self.stub.sample_input()
    }

    fn drag_paths(&self) -> Vec<PathBuf> {
        self.stub.drag_paths()
    }

    fn screens(&self) -> Vec<Rect> {
        self.stub.screens()
    }

    fn copy_files(&self, paths: &[PathBuf]) -> Result<(), PlatformError> {
        self.stub.copy_files(paths)
    }

    fn copy_text(&self, text: &str) -> Result<(), PlatformError> {
        self.stub.copy_text(text)
    }

    fn clipboard_types(&self) -> Vec<String> {
        self.stub.clipboard_types()
    }

    fn clipboard_change_count(&self) -> i64 {
        self.stub.clipboard_change_count()
    }

    fn read_clipboard(&self) -> ClipSnapshot {
        self.stub.read_clipboard()
    }

    fn write_clipboard(&self, snapshot: &ClipSnapshot) -> Result<i64, PlatformError> {
        self.stub.write_clipboard(snapshot)
    }

    /// Linux has no file reference paths: a path is its own file.
    fn resolve_file_reference(&self, path: &Path) -> Option<PathBuf> {
        std::fs::symlink_metadata(path)
            .ok()
            .map(|_| path.to_path_buf())
    }

    fn terminals(&self) -> Vec<TerminalApp> {
        terminal::installed()
    }

    fn open_terminal(&self, terminal: Option<&str>, dir: &Path) -> Result<(), PlatformError> {
        terminal::open(terminal, dir)
    }

    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf> {
        portal::pick_folder(start)
    }

    fn pocket_home(&self) -> Option<IslandHome> {
        self.stub.pocket_home()
    }

    fn make_overlay(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        self.stub.make_overlay(window)
    }

    fn make_non_key(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        self.stub.make_non_key(window)
    }

    fn keep_window_frame(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        self.stub.keep_window_frame(window)
    }

    fn file_icon(&self, path: &Path, px: u32) -> Option<RgbaImage> {
        let scale = self.scale_factor_at(Point { x: 0.0, y: 0.0 });
        icons::file_icon(path, (f64::from(px) * scale).round() as u32)
    }

    fn kind_description(&self, path: &Path) -> Option<String> {
        apps::kind_description(path)
    }

    fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>> {
        self.stub.drop_catcher()
    }

    fn trash(&self, path: &Path) -> Result<(), PlatformError> {
        trash::trash(path)
    }

    fn screens_changed(&self) -> bool {
        self.stub.screens_changed()
    }

    fn on_terminate(&self, f: Box<dyn Fn()>) {
        self.stub.on_terminate(f)
    }

    fn activate_app(&self) {
        self.stub.activate_app()
    }

    fn deactivate_app(&self) {
        self.stub.deactivate_app()
    }

    /// There is no app menu bar on GNOME.
    fn suspend_app_menu(&self) {}

    /// There is no app menu bar on GNOME.
    fn restore_app_menu(&self) {}

    fn watch_outside_clicks(&self, f: Box<dyn Fn()>) {
        self.stub.watch_outside_clicks(f)
    }

    fn stop_watching_outside_clicks(&self) {
        self.stub.stop_watching_outside_clicks()
    }

    fn scale_factor_at(&self, p: Point) -> f64 {
        self.stub.scale_factor_at(p)
    }

    fn apps_for(&self, path: &Path) -> Vec<AppInfo> {
        apps::apps_for(path)
    }

    fn open_paths(&self, paths: &[PathBuf]) -> Result<(), PlatformError> {
        apps::open_paths(paths)
    }

    /// gio starts the app before returning, so `on_error` is never called:
    /// every failure is returned.
    fn open_with(
        &self,
        paths: &[PathBuf],
        app: &Path,
        _on_error: Box<dyn Fn(String) + Send>,
    ) -> Result<(), PlatformError> {
        apps::open_with(paths, app)
    }

    fn reveal_in_finder(&self, paths: &[PathBuf]) {
        apps::reveal(paths)
    }

    fn context_menu(
        &self,
        window: raw_window_handle::RawWindowHandle,
        at: Point,
        entries: &[MenuEntry],
    ) -> Option<u32> {
        self.stub.context_menu(window, at, entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_its_own_file_reference() {
        let dir = tempfile::tempdir().unwrap();
        let p = LinuxPlatform::new();
        assert_eq!(
            p.resolve_file_reference(dir.path()),
            Some(dir.path().to_path_buf())
        );
        assert_eq!(p.resolve_file_reference(&dir.path().join("gone")), None);
    }

    #[test]
    fn a_dangling_link_still_resolves_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("gone"), &link).unwrap();
        assert_eq!(
            LinuxPlatform::new().resolve_file_reference(&link),
            Some(link)
        );
    }
}
