//! Windows [`Platform`], through Win32.
//!
//! The port is under way (see W0–W7 in the port plan): what is done here
//! is the pointer and keys, the monitors and one Vento per session; every
//! other call goes to the [`StubPlatform`] for now.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::clip::ClipSnapshot;
use vento_core::geometry::{Point, Rect};
use vento_core::trigger::{InputSample, Modifiers};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LBUTTON, VK_LWIN, VK_MENU, VK_RBUTTON,
    VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_SWAPBUTTON};

use crate::{
    AppInfo, DropCatcher, IslandHome, MenuEntry, Platform, PlatformError, RgbaImage, StubPlatform,
    TerminalApp,
};

mod instance;
mod screens;
/// Linux's thumbnails need only the `image` crate.
#[path = "../linux/thumbs.rs"]
mod thumbs;

pub use instance::claim_instance;
pub(crate) use thumbs::{picture_data_thumbnail, picture_thumbnail};

/// Whether the taskbar is light, so the tray icon is drawn dark on it.
/// Windows 10 1903 and later keep this apart from the apps' own theme;
/// older ones have no such value and a dark taskbar.
pub fn taskbar_is_light() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows_sys::w;

    let mut light: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the out-pointers are live locals of the sizes given.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut light).cast(),
            &mut size,
        )
    };
    read == 0 && light != 0
}

/// Runs a handler the app registers.
struct Handler(Box<dyn Fn()>);

impl std::fmt::Debug for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handler")
    }
}

/// [`Platform`] for Windows.
#[derive(Debug, Default)]
pub struct WindowsPlatform {
    /// Answers what is not done on Windows yet.
    stub: StubPlatform,
    /// Runs when another launch asks this Vento to open.
    on_reopen: RefCell<Option<Handler>>,
}

impl WindowsPlatform {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Whether the key `vk` is down now.
fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain call. The high bit is set while the key is down.
    (unsafe { GetAsyncKeyState(i32::from(vk)) } as u16) & 0x8000 != 0
}

/// The key of the primary mouse button, which the user may have swapped
/// to the right one.
fn primary_button() -> VIRTUAL_KEY {
    // SAFETY: plain call.
    if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 {
        VK_RBUTTON
    } else {
        VK_LBUTTON
    }
}

impl Platform for WindowsPlatform {
    fn prepare_app(&self) {}

    fn sample_input(&self) -> InputSample {
        if instance::asked_to_open()
            && let Some(handler) = self.on_reopen.borrow().as_ref()
        {
            (handler.0)();
        }
        let mut cursor = POINT::default();
        // SAFETY: `cursor` is a live local.
        let cursor = if unsafe { GetCursorPos(&mut cursor) } != 0 {
            screens::to_logical(&screens::monitors(), cursor)
        } else {
            Point { x: 0.0, y: 0.0 }
        };
        InputSample {
            left_down: key_down(primary_button()),
            modifiers: Modifiers {
                ctrl: key_down(VK_CONTROL),
                alt: key_down(VK_MENU),
                shift: key_down(VK_SHIFT),
                meta: key_down(VK_LWIN) || key_down(VK_RWIN),
            },
            escape_down: key_down(VK_ESCAPE),
            // Drags come with the wheel (W3).
            drag_change_count: 0,
            drag_has_files: false,
            clip_change_count: 0,
            cursor,
        }
    }

    fn drag_paths(&self) -> Vec<PathBuf> {
        self.stub.drag_paths()
    }

    fn screens(&self) -> Vec<Rect> {
        let monitors = screens::monitors();
        if monitors.is_empty() {
            return self.stub.screens();
        }
        monitors
            .iter()
            .map(screens::Monitor::logical_work)
            .collect()
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

    fn resolve_file_reference(&self, path: &Path) -> Option<PathBuf> {
        self.stub.resolve_file_reference(path)
    }

    fn terminals(&self) -> Vec<TerminalApp> {
        self.stub.terminals()
    }

    fn open_terminal(
        &self,
        terminal: Option<&str>,
        dir: &Path,
        on_error: Box<dyn Fn(PlatformError) + Send>,
    ) -> Result<(), PlatformError> {
        self.stub.open_terminal(terminal, dir, on_error)
    }

    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf> {
        self.stub.pick_folder(start)
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
        self.stub.file_icon(path, px)
    }

    fn kind_description(&self, path: &Path) -> Option<String> {
        self.stub.kind_description(path)
    }

    fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>> {
        self.stub.drop_catcher()
    }

    fn trash(&self, path: &Path) -> Result<(), PlatformError> {
        self.stub.trash(path)
    }

    fn screens_changed(&self) -> bool {
        self.stub.screens_changed()
    }

    fn on_terminate(&self, f: Box<dyn Fn()>) {
        self.stub.on_terminate(f);
    }

    fn on_reopen(&self, f: Box<dyn Fn()>) {
        *self.on_reopen.borrow_mut() = Some(Handler(f));
    }

    fn activate_app(&self) {}

    fn deactivate_app(&self) {}

    /// There is no app menu bar on Windows.
    fn suspend_app_menu(&self) {}

    fn restore_app_menu(&self) {}

    fn watch_outside_clicks(&self, f: Box<dyn Fn()>) {
        self.stub.watch_outside_clicks(f);
    }

    fn stop_watching_outside_clicks(&self) {
        self.stub.stop_watching_outside_clicks();
    }

    fn scale_factor_at(&self, p: Point) -> f64 {
        screens::monitor_at_logical(&screens::monitors(), p).map_or(1.0, |m| m.scale)
    }

    fn apps_for(&self, path: &Path) -> Vec<AppInfo> {
        self.stub.apps_for(path)
    }

    fn open_paths(&self, paths: &[PathBuf]) -> Result<(), PlatformError> {
        self.stub.open_paths(paths)
    }

    fn open_with(
        &self,
        paths: &[PathBuf],
        app: &Path,
        on_error: Box<dyn Fn(PlatformError) + Send>,
    ) -> Result<(), PlatformError> {
        self.stub.open_with(paths, app, on_error)
    }

    fn reveal_in_finder(&self, paths: &[PathBuf]) {
        self.stub.reveal_in_finder(paths);
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
    fn the_screens_are_the_monitors_work_areas() {
        let p = WindowsPlatform::new();
        let screens = p.screens();
        assert!(!screens.is_empty());
        assert!(screens.iter().all(|s| s.w > 0.0 && s.h > 0.0));
        let centre = Point {
            x: screens[0].x + screens[0].w / 2.0,
            y: screens[0].y + screens[0].h / 2.0,
        };
        assert!(p.scale_factor_at(centre) >= 1.0);
    }

    #[test]
    fn a_reopen_waits_for_the_next_sample() {
        let p = WindowsPlatform::new();
        let hits = Rc::new(std::cell::Cell::new(0));
        let seen = hits.clone();
        p.on_reopen(Box::new(move || seen.set(seen.get() + 1)));
        p.sample_input();
        assert_eq!(hits.get(), 0);
    }
}
