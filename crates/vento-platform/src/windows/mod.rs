//! Windows [`Platform`], through Win32.
//!
//! The port is under way (see W0–W7 in the port plan): done here are the
//! pointer and keys, drags in other apps, the monitors, one Vento per
//! session and its end, vento's own windows kept off the taskbar and out
//! of focus, and the Shell's services (pickers, terminals, opening files,
//! icons, the Recycle Bin, menus, Keep Awake). The clipboard and the
//! Pocket's drop catcher go to the [`StubPlatform`] for now.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::clip::ClipSnapshot;
use vento_core::geometry::{Point, Rect};
use vento_core::trigger::{InputSample, Modifiers};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LBUTTON, VK_LWIN, VK_MENU, VK_RBUTTON,
    VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId, IsWindow,
    SM_SWAPBUTTON, SetForegroundWindow,
};

use crate::{
    AppInfo, AwakeHold, DropCatcher, IslandHome, MenuEntry, Platform, PlatformError, RgbaImage,
    StubPlatform, TerminalApp,
};

mod clicks;
mod drag;
mod heif;
/// Linux's Convert needs only the `image` crate and libwebp, with
/// `heif` standing in for libheif.
#[path = "../linux/image_io.rs"]
pub(crate) mod image_io;
mod instance;
mod menu;
mod overlay;
mod screens;
mod shell;
mod terminal;
/// Linux's thumbnails need only the `image` crate.
#[path = "../linux/thumbs.rs"]
mod thumbs;

pub use instance::claim_instance;
pub(crate) use thumbs::{picture_data_thumbnail, picture_thumbnail};

/// [`Platform::file_icon`] at `scale`, for a worker thread.
pub(crate) fn file_icon_at(path: &Path, px: u32, scale: f64) -> Option<RgbaImage> {
    shell::file_icon(path, (f64::from(px) * scale).round() as u32)
}

/// Where Windows keeps a file's Mark of the Web: its `Zone.Identifier`
/// stream, which says the file came from the internet.
fn zone_stream(path: &Path) -> PathBuf {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    stream.into()
}

/// The Mark of the Web of `path`, as its `Zone.Identifier` stream holds it;
/// `None` without one.
pub(crate) fn quarantine_of(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(zone_stream(path)).ok()
}

/// Gives `path` the Mark of the Web `value`, as [`quarantine_of`] read it.
pub(crate) fn set_quarantine(path: &Path, value: &[u8]) -> std::io::Result<()> {
    std::fs::write(zone_stream(path), value)
}

/// The system's folder picker, starting in `start`.
fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}

/// The system's file picker for PDFs, starting in `start`.
pub(crate) fn pick_pdfs(start: Option<&Path>) -> Vec<PathBuf> {
    let mut dialog = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_files().unwrap_or_default()
}

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
    /// Runs before vento goes at the session's end.
    on_terminate: RefCell<Option<Handler>>,
    /// The window of another app that had the focus when vento took it
    /// ([`Platform::activate_app`]), as an address; 0 for none.
    focus_before: Cell<usize>,
    /// The monitors [`Platform::screens_changed`] last saw.
    screens_seen: RefCell<Option<Vec<screens::Seen>>>,
    /// Counts the drags seen in other apps.
    drags: RefCell<drag::DragCounter>,
}

impl WindowsPlatform {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The window class of `window`.
fn class_of(window: windows_sys::Win32::Foundation::HWND) -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;

    let mut name = [0u16; 64];
    // SAFETY: the buffer's length is passed with it.
    let len = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..len.max(0) as usize])
}

/// Whether `window` is one of vento's own.
fn is_vento(window: windows_sys::Win32::Foundation::HWND) -> bool {
    let mut pid = 0;
    // SAFETY: plain calls; a stale handle just yields no process.
    unsafe {
        GetWindowThreadProcessId(window, &mut pid);
        pid == GetCurrentProcessId()
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
        if instance::asked_to_end() {
            if let Some(handler) = self.on_terminate.borrow().as_ref() {
                log::info!("tidying up before the session ends");
                (handler.0)();
            }
            instance::ended();
            std::process::exit(0);
        }
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
        let dragging = drag::active();
        InputSample {
            left_down: key_down(primary_button()),
            modifiers: Modifiers {
                ctrl: key_down(VK_CONTROL),
                alt: key_down(VK_MENU),
                shift: key_down(VK_SHIFT),
                meta: key_down(VK_LWIN) || key_down(VK_RWIN),
            },
            escape_down: key_down(VK_ESCAPE),
            drag_change_count: self.drags.borrow_mut().feed(dragging),
            // What an OLE drag carries cannot be read from outside it: any
            // drag may be of files until the wheel, under the pointer, is
            // told (see `drag`).
            drag_has_files: dragging,
            clip_change_count: 0,
            cursor,
        }
    }

    /// Always empty: the drag's files reach the wheel window itself, as
    /// winit's `HoveredFile` (see `drag`).
    fn drag_paths(&self) -> Vec<PathBuf> {
        Vec::new()
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
        std::fs::symlink_metadata(path)
            .ok()
            .map(|_| path.to_path_buf())
    }

    fn terminals(&self) -> Vec<TerminalApp> {
        terminal::installed()
    }

    fn open_terminal(
        &self,
        terminal: Option<&str>,
        dir: &Path,
        on_error: Box<dyn Fn(PlatformError) + Send>,
    ) -> Result<(), PlatformError> {
        terminal::open(terminal, dir, on_error)
    }

    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf> {
        pick_folder(start)
    }

    /// The top middle of the primary monitor. A taskbar along the top
    /// counts as its menu bar, so the island sits below it.
    fn pocket_home(&self) -> Option<IslandHome> {
        let primary = *screens::monitors().first()?;
        let (frame, work) = (primary.logical_frame(), primary.logical_work());
        Some(IslandHome {
            screen: frame,
            notch: None,
            menu_bar_h: (work.y - frame.y).max(0.0),
            slot: None,
        })
    }

    /// Off the taskbar and out of Alt+Tab; above the others it already is,
    /// through Slint's `always-on-top`.
    fn make_overlay(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        overlay::keep(window, overlay::OFF_TASKBAR)
    }

    /// Never activated, so the app in front keeps the keyboard; off the
    /// taskbar too, as every such window is a passing one.
    fn make_non_key(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        overlay::keep(window, overlay::OFF_TASKBAR | overlay::NO_ACTIVATE)
    }

    /// Windows leaves a window where it is put; the Quick panel only needs
    /// keeping off the taskbar.
    fn keep_window_frame(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        overlay::keep(window, overlay::OFF_TASKBAR)
    }

    /// At the primary monitor's scale, as on the other two.
    fn file_icon(&self, path: &Path, px: u32) -> Option<RgbaImage> {
        let scale = screens::monitors().first().map_or(1.0, |m| m.scale);
        file_icon_at(path, px, scale)
    }

    fn kind_description(&self, path: &Path) -> Option<String> {
        shell::kind_description(path)
    }

    fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>> {
        self.stub.drop_catcher()
    }

    fn trash(&self, path: &Path) -> Result<(), PlatformError> {
        shell::trash(path)
    }

    fn screens_changed(&self) -> bool {
        let now: Vec<_> = screens::monitors().iter().map(screens::Seen::of).collect();
        match self.screens_seen.replace(Some(now.clone())) {
            Some(before) => before != now,
            None => false,
        }
    }

    fn on_terminate(&self, f: Box<dyn Fn()>) {
        *self.on_terminate.borrow_mut() = Some(Handler(f));
    }

    /// Held on the main thread, which asks for it; the hold ends with the
    /// process in any case.
    fn keep_awake(&self, hold: Option<AwakeHold>) -> Result<(), PlatformError> {
        use windows_sys::Win32::System::Power::{
            ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
        };

        let state = match hold {
            None => ES_CONTINUOUS,
            Some(AwakeHold { screen: false }) => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
            Some(AwakeHold { screen: true }) => {
                ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
            }
        };
        // SAFETY: plain call. It returns the earlier state, or 0 on failure.
        if unsafe { SetThreadExecutionState(state) } == 0 {
            return Err(PlatformError::Failed(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        Ok(())
    }

    fn on_reopen(&self, f: Box<dyn Fn()>) {
        *self.on_reopen.borrow_mut() = Some(Handler(f));
    }

    /// Remembers the other app's window in front, for
    /// [`Platform::deactivate_app`]; the window that shows next takes the
    /// focus itself (winit's `focus_window`).
    fn activate_app(&self) {
        // SAFETY: plain call.
        let front = unsafe { GetForegroundWindow() };
        if !front.is_null() && !is_vento(front) {
            self.focus_before.set(front as usize);
        }
    }

    fn deactivate_app(&self) {
        let before = self.focus_before.replace(0) as windows_sys::Win32::Foundation::HWND;
        // SAFETY: plain calls; `before` is checked to still be a window.
        unsafe {
            let front = GetForegroundWindow();
            let ours = front.is_null() || is_vento(front);
            if ours && !before.is_null() && IsWindow(before) != 0 {
                SetForegroundWindow(before);
            }
        }
    }

    /// There is no app menu bar on Windows.
    fn suspend_app_menu(&self) {}

    fn restore_app_menu(&self) {}

    fn watch_outside_clicks(&self, f: Box<dyn Fn()>) {
        clicks::watch(f);
    }

    fn stop_watching_outside_clicks(&self) {
        clicks::stop();
    }

    fn scale_factor_at(&self, p: Point) -> f64 {
        screens::monitor_at_logical(&screens::monitors(), p).map_or(1.0, |m| m.scale)
    }

    fn apps_for(&self, path: &Path) -> Vec<AppInfo> {
        shell::apps_for(path)
    }

    fn open_paths(&self, paths: &[PathBuf]) -> Result<(), PlatformError> {
        shell::open_paths(paths)
    }

    fn open_with(
        &self,
        paths: &[PathBuf],
        app: &Path,
        _on_error: Box<dyn Fn(PlatformError) + Send>,
    ) -> Result<(), PlatformError> {
        // The Shell starts the app before returning, so on_error is never
        // called.
        shell::open_with(paths, app)
    }

    fn reveal_in_finder(&self, paths: &[PathBuf]) {
        shell::reveal(paths);
    }

    fn context_menu(
        &self,
        window: raw_window_handle::RawWindowHandle,
        at: Point,
        entries: &[MenuEntry],
    ) -> Option<u32> {
        menu::show(window, at, entries)
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
    fn the_mark_of_the_web_goes_with_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("download.zip");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(quarantine_of(&file), None);
        let mark = b"[ZoneTransfer]\r\nZoneId=3\r\n";
        set_quarantine(&file, mark).unwrap();
        assert_eq!(quarantine_of(&file).as_deref(), Some(&mark[..]));
        // The file's own content is untouched.
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
    }

    #[test]
    fn keep_awake_holds_and_lets_go() {
        let p = WindowsPlatform::new();
        p.keep_awake(Some(AwakeHold { screen: true })).unwrap();
        p.keep_awake(Some(AwakeHold { screen: false })).unwrap();
        p.keep_awake(None).unwrap();
    }

    #[test]
    fn the_island_lives_on_the_primary_monitor() {
        let p = WindowsPlatform::new();
        let home = p.pocket_home().expect("a primary monitor");
        let work = p.screens()[0];
        assert!(home.screen.w >= work.w && home.screen.h >= work.h);
        assert!(home.menu_bar_h >= 0.0 && home.notch.is_none());
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
