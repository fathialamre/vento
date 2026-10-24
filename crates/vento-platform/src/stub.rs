//! Platform used where vento has no real implementation yet.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::clip::ClipSnapshot;
use vento_core::convert::{ConvertError, ImageFormat};
use vento_core::geometry::{Point, Rect};
use vento_core::screen_text::{Selection, TextBox};
use vento_core::trigger::{InputSample, Modifiers};

use crate::{
    AppInfo, DropCatcher, IslandHome, MenuEntry, Platform, PlatformError, RgbaImage, ScreenText,
    TerminalApp,
};

/// No quarantine attribute exists here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn quarantine_of(_path: &Path) -> Option<Vec<u8>> {
    None
}

/// Nothing to set here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn set_quarantine(_path: &Path, _value: &[u8]) -> std::io::Result<()> {
    Ok(())
}

/// No packages exist here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn is_package(_path: &Path) -> bool {
    false
}

/// Nothing downloads on demand here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn block_cloud_downloads_on_this_thread() -> Result<(), PlatformError> {
    Ok(())
}

/// Stay on the start's own device.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn volume_scope(_start: &Path) -> vento_core::sizes::VolumeScope {
    vento_core::sizes::VolumeScope::default()
}

/// Message of the image conversion stubs.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const NO_IMAGE_CONVERSION: &str = "image conversion is not available on this system";

/// No image codec here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn probe_image(_path: &Path) -> Result<Option<ImageFormat>, ConvertError> {
    Err(ConvertError::Unsupported(NO_IMAGE_CONVERSION.into()))
}

/// No image codec here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn write_image(
    _src: &Path,
    _dst: &Path,
    _format: ImageFormat,
) -> Result<(), ConvertError> {
    Err(ConvertError::Unsupported(NO_IMAGE_CONVERSION.into()))
}

/// Nothing to capture here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn read_screen_text(_sel: &Selection) -> Result<Vec<TextBox>, PlatformError> {
    Err(PlatformError::Unsupported)
}

/// No text reader here.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn screen_text_languages() -> Vec<String> {
    Vec::new()
}

/// Inert [`ScreenText`]: unsupported, never granted, selects nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubScreenText;

impl ScreenText for StubScreenText {
    fn supported(&self) -> bool {
        false
    }

    fn has_access(&self) -> bool {
        false
    }

    fn request_access(&self) {}

    fn open_access_settings(&self) {}

    fn select_area(&self, done: Box<dyn FnOnce(Option<Selection>)>) {
        done(None);
    }
}

/// Inert [`Platform`]: reports no input and supports nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubPlatform;

impl Platform for StubPlatform {
    fn prepare_app(&self) {}

    fn sample_input(&self) -> InputSample {
        InputSample {
            left_down: false,
            modifiers: Modifiers::default(),
            escape_down: false,
            drag_change_count: 0,
            drag_has_files: false,
            clip_change_count: 0,
            cursor: Point { x: 0.0, y: 0.0 },
        }
    }

    fn drag_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn screens(&self) -> Vec<Rect> {
        vec![Rect {
            x: 0.0,
            y: 0.0,
            w: 1440.0,
            h: 900.0,
        }]
    }

    fn copy_files(&self, _paths: &[PathBuf]) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn copy_text(&self, _text: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn clipboard_types(&self) -> Vec<String> {
        Vec::new()
    }

    fn clipboard_change_count(&self) -> i64 {
        0
    }

    fn read_clipboard(&self) -> ClipSnapshot {
        ClipSnapshot::default()
    }

    fn write_clipboard(&self, _snapshot: &ClipSnapshot) -> Result<i64, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn resolve_file_reference(&self, _path: &Path) -> Option<PathBuf> {
        None
    }

    fn terminals(&self) -> Vec<TerminalApp> {
        Vec::new()
    }

    fn open_terminal(
        &self,
        _terminal: Option<&str>,
        _dir: &Path,
        _on_error: Box<dyn Fn(String) + Send>,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn pick_folder(&self, _start: Option<&Path>) -> Option<PathBuf> {
        None
    }

    fn pocket_home(&self) -> Option<IslandHome> {
        None
    }

    fn make_overlay(
        &self,
        _window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn make_non_key(
        &self,
        _window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn keep_window_frame(
        &self,
        _window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn file_icon(&self, _path: &Path, _px: u32) -> Option<RgbaImage> {
        None
    }

    fn kind_description(&self, _path: &Path) -> Option<String> {
        None
    }

    fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>> {
        None
    }

    fn trash(&self, _path: &Path) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn screens_changed(&self) -> bool {
        false
    }

    fn on_terminate(&self, _f: Box<dyn Fn()>) {}

    fn activate_app(&self) {}

    fn deactivate_app(&self) {}

    fn suspend_app_menu(&self) {}

    fn restore_app_menu(&self) {}

    fn watch_outside_clicks(&self, _f: Box<dyn Fn()>) {}

    fn stop_watching_outside_clicks(&self) {}

    fn scale_factor_at(&self, _p: Point) -> f64 {
        1.0
    }

    fn apps_for(&self, _path: &Path) -> Vec<AppInfo> {
        Vec::new()
    }

    fn open_paths(&self, _paths: &[PathBuf]) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn open_with(
        &self,
        _paths: &[PathBuf],
        _app: &Path,
        _on_error: Box<dyn Fn(String) + Send>,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn reveal_in_finder(&self, _paths: &[PathBuf]) {}

    fn context_menu(
        &self,
        _window: raw_window_handle::RawWindowHandle,
        _at: Point,
        _entries: &[MenuEntry],
    ) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_idle_input_and_one_screen() {
        let p = StubPlatform;
        let s = p.sample_input();
        assert!(!s.left_down && !s.escape_down && !s.drag_has_files);
        assert_eq!(s.drag_change_count, 0);
        assert_eq!(s.clip_change_count, 0);
        assert_eq!(p.screens().len(), 1);
    }

    #[test]
    fn stub_has_no_pocket_home() {
        let p = StubPlatform;
        assert!(p.pocket_home().is_none());
        assert!(p.file_icon(Path::new("/"), 32).is_none());
        assert!(p.kind_description(Path::new("/")).is_none());
        assert!(!is_package(Path::new("/")));
        assert!(block_cloud_downloads_on_this_thread().is_ok());
        assert_eq!(volume_scope(Path::new("/")), Default::default());
        assert!(p.drop_catcher().is_none());
        let web = raw_window_handle::WebWindowHandle::new(1);
        assert!(matches!(
            p.make_overlay(web.into()),
            Err(PlatformError::Unsupported)
        ));
        assert!(matches!(
            p.make_non_key(web.into()),
            Err(PlatformError::Unsupported)
        ));
    }

    #[test]
    fn stub_operations_are_unsupported() {
        let p = StubPlatform;
        assert!(matches!(p.copy_text("x"), Err(PlatformError::Unsupported)));
        assert!(matches!(p.copy_files(&[]), Err(PlatformError::Unsupported)));
        assert!(p.clipboard_types().is_empty());
        assert_eq!(p.clipboard_change_count(), 0);
        assert!(p.read_clipboard().items.is_empty());
        assert!(matches!(
            p.write_clipboard(&ClipSnapshot::default()),
            Err(PlatformError::Unsupported)
        ));
        assert!(matches!(
            p.open_terminal(
                None,
                Path::new("/"),
                Box::new(|_| panic!("the stub opens nothing"))
            ),
            Err(PlatformError::Unsupported)
        ));
        assert!(p.terminals().is_empty());
        assert!(p.pick_folder(None).is_none());
        assert!(matches!(
            p.trash(Path::new("/tmp/vento-stub-never")),
            Err(PlatformError::Unsupported)
        ));
        assert!(!p.screens_changed());
        p.suspend_app_menu();
        p.restore_app_menu();
        p.watch_outside_clicks(Box::new(|| panic!("the stub reports no clicks")));
        p.stop_watching_outside_clicks();
        assert_eq!(p.scale_factor_at(Point { x: 10.0, y: 10.0 }), 1.0);
    }

    #[test]
    fn stub_screen_text_does_nothing() {
        let s = StubScreenText;
        assert!(!s.supported());
        assert!(!s.has_access());
        s.request_access();
        s.open_access_settings();
        let got = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = got.clone();
        s.select_area(Box::new(move |sel| seen.set(Some(sel))));
        assert_eq!(got.get(), Some(None));
        let sel = Selection {
            display_id: 1,
            display: Rect {
                x: 0.0,
                y: 0.0,
                w: 1440.0,
                h: 900.0,
            },
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
        };
        assert!(matches!(
            read_screen_text(&sel),
            Err(PlatformError::Unsupported)
        ));
        assert!(screen_text_languages().is_empty());
    }

    #[test]
    fn stub_has_no_quarantine() {
        assert_eq!(quarantine_of(Path::new("/")), None);
        assert!(set_quarantine(Path::new("/"), b"x").is_ok());
    }

    #[test]
    fn stub_has_no_apps_and_shows_no_menu() {
        let p = StubPlatform;
        assert!(p.apps_for(Path::new("/")).is_empty());
        assert!(matches!(
            p.open_paths(&[PathBuf::from("/")]),
            Err(PlatformError::Unsupported)
        ));
        assert!(matches!(
            p.open_with(
                &[PathBuf::from("/")],
                Path::new("/Applications/TextEdit.app"),
                Box::new(|_| panic!("the stub opens nothing"))
            ),
            Err(PlatformError::Unsupported)
        ));
        p.reveal_in_finder(&[PathBuf::from("/")]);
        let web = raw_window_handle::WebWindowHandle::new(1);
        let entries = [MenuEntry::Item {
            id: 1,
            title: "Open".into(),
            enabled: true,
            icon: None,
        }];
        assert_eq!(
            p.context_menu(web.into(), Point { x: 0.0, y: 0.0 }, &entries),
            None
        );
    }
}
