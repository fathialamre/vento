//! Platform used where vento has no real implementation yet.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::geometry::{Point, Rect};
use vento_core::trigger::{InputSample, Modifiers};

use crate::{DropCatcher, IslandHome, Platform, PlatformError, RgbaImage, TerminalApp};

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
            cursor: Point { x: 0.0, y: 0.0 },
        }
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

    fn terminals(&self) -> Vec<TerminalApp> {
        Vec::new()
    }

    fn open_terminal(&self, _terminal: Option<&str>, _dir: &Path) -> Result<(), PlatformError> {
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

    fn file_icon(&self, _path: &Path, _px: u32) -> Option<RgbaImage> {
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

    fn scale_factor_at(&self, _p: Point) -> f64 {
        1.0
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
        assert_eq!(p.screens().len(), 1);
    }

    #[test]
    fn stub_has_no_pocket_home() {
        let p = StubPlatform;
        assert!(p.pocket_home().is_none());
        assert!(p.file_icon(Path::new("/"), 32).is_none());
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
        assert!(matches!(
            p.open_terminal(None, Path::new("/")),
            Err(PlatformError::Unsupported)
        ));
        assert!(p.terminals().is_empty());
        assert!(p.pick_folder(None).is_none());
        assert!(matches!(
            p.trash(Path::new("/tmp/vento-stub-never")),
            Err(PlatformError::Unsupported)
        ));
        assert!(!p.screens_changed());
        assert_eq!(p.scale_factor_at(Point { x: 10.0, y: 10.0 }), 1.0);
    }
}
