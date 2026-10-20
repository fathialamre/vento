//! macOS [`Platform`] built on AppKit, with no Accessibility or Input
//! Monitoring permission needed.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use vento_core::geometry::Rect;
use vento_core::trigger::InputSample;

use crate::{DropCatcher, IslandHome, Platform, PlatformError, RgbaImage, TerminalApp};

mod catcher;
mod clipboard;
mod icons;
mod input;
mod notify;
mod overlay;
mod patch;
mod screens;
mod terminal;
mod trash;

/// AppKit-backed [`Platform`].
pub struct MacPlatform {
    drag: input::DragPasteboardCache,
    catcher: OnceCell<Rc<catcher::MacDropCatcher>>,
    /// Listens for screen changes and termination while vento runs.
    _observer: Retained<notify::AppObserver>,
}

impl Default for MacPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl MacPlatform {
    pub fn new() -> Self {
        Self {
            drag: input::DragPasteboardCache::default(),
            catcher: OnceCell::new(),
            _observer: notify::AppObserver::install(),
        }
    }
}

impl Platform for MacPlatform {
    fn prepare_app(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("prepare_app called off the main thread; ignored");
            return;
        };
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }

    fn sample_input(&self) -> InputSample {
        input::sample(&self.drag)
    }

    fn screens(&self) -> Vec<Rect> {
        screens::screens()
    }

    fn copy_files(&self, paths: &[PathBuf]) -> Result<(), PlatformError> {
        clipboard::copy_files(paths)
    }

    fn copy_text(&self, text: &str) -> Result<(), PlatformError> {
        clipboard::copy_text(text)
    }

    fn terminals(&self) -> Vec<TerminalApp> {
        terminal::installed()
    }

    fn open_terminal(&self, terminal: Option<&str>, dir: &Path) -> Result<(), PlatformError> {
        terminal::open(terminal, dir)
    }

    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf> {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("pick_folder called off the main thread; ignored");
            return None;
        };
        screens::activate_app(mtm);
        let mut dialog = rfd::FileDialog::new();
        if let Some(start) = start {
            dialog = dialog.set_directory(start);
        }
        dialog.pick_folder()
    }

    fn pocket_home(&self) -> Option<IslandHome> {
        screens::pocket_home()
    }

    fn make_overlay(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        overlay::make_overlay(window)
    }

    fn make_non_key(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        patch::make_non_key(window)
    }

    fn file_icon(&self, path: &Path, px: u32) -> Option<RgbaImage> {
        icons::file_icon(path, px)
    }

    fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>> {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("drop_catcher called off the main thread; ignored");
            return None;
        };
        let catcher = self
            .catcher
            .get_or_init(|| Rc::new(catcher::MacDropCatcher::new(mtm)));
        Some(catcher.clone())
    }

    fn trash(&self, path: &Path) -> Result<(), PlatformError> {
        trash::trash(path)
    }

    fn screens_changed(&self) -> bool {
        notify::screens_changed()
    }

    fn on_terminate(&self, f: Box<dyn Fn()>) {
        notify::on_terminate(f);
    }
}
