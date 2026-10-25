//! Linux [`Platform`] for GNOME.
//!
//! The desktop's own services (Trash, apps, terminals, the folder picker)
//! go through gio. The pointer, drags, screens and focus come from Vento's
//! GNOME Shell extension (see `shell`); without it Vento sees no input.
//! The clipboard is read through the extension and written as the owner
//! of the X11 `CLIPBOARD`; vento's windows are X11 ones under XWayland.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use vento_core::clip::{ClipRep, ClipSnapshot, MAX_SNAPSHOT_BYTES, TYPE_FILE_URL, TYPE_TEXT};
use vento_core::geometry::{Point, Rect};
use vento_core::trigger::InputSample;

use crate::{
    AppInfo, DropCatcher, IslandHome, MenuColors, MenuEntry, Platform, PlatformError, RgbaImage,
    StubPlatform, TerminalApp,
};

mod apps;
mod catcher;
mod clip_owner;
mod clipboard;
mod drag_out;
mod icons;
mod instance;
mod portal;
mod shell;
mod shortcuts;
mod terminal;
mod terminate;
mod thumbs;
mod trash;
mod x11;

pub use drag_out::{DragEnd, start_file_drag};
pub use instance::claim_instance;

/// Where the top bar shows the tray icon whose StatusNotifierItem id is
/// `id`, in logical global coordinates; `None` when it shows none. Blocks
/// for one short call.
pub fn tray_icon_frame(id: &str) -> Option<Rect> {
    shell::tray_icon_frame(id)
}

/// Ends the startup of activation `token`, given with a tray click: vento's
/// X11 windows cannot use it, and GNOME shows a busy pointer until it ends.
/// Blocks for one short call; any thread.
pub fn end_startup(token: &str) {
    shell::end_startup(token);
}
pub use shortcuts::{APP_ID, GlobalShortcut, bind_global_shortcut};
pub(crate) use thumbs::{picture_data_thumbnail, picture_thumbnail};

/// [`Platform::file_icon`] at `scale`, for a worker thread.
pub(crate) fn file_icon_at(path: &Path, px: u32, scale: f64) -> Option<RgbaImage> {
    icons::file_icon(path, (f64::from(px) * scale).round() as u32)
}

/// [`Platform`] for GNOME, through gio and Vento's GNOME Shell extension.
#[derive(Debug)]
pub struct LinuxPlatform {
    /// Answers what is not done on Linux yet.
    stub: StubPlatform,
    shell: shell::Shell,
    /// The monitors serial [`Platform::screens_changed`] last saw.
    screens_seen: Cell<Option<u32>>,
    /// Owns the clipboard for vento's copies; made on the first one.
    clip_owner: OnceCell<Option<clip_owner::ClipOwner>>,
    /// The Pocket's drop target; made when first asked for.
    catcher: OnceCell<Option<Rc<catcher::LinuxCatcher>>>,
    /// Runs before vento quits on a signal.
    on_terminate: RefCell<Option<terminate::Handler>>,
    /// Runs when another launch asks this Vento to open.
    on_reopen: RefCell<Option<terminate::Handler>>,
}

impl Default for LinuxPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxPlatform {
    pub fn new() -> Self {
        // First, before any other portal call on the session bus.
        shortcuts::register_app();
        Self {
            stub: StubPlatform,
            shell: shell::Shell::connect(),
            screens_seen: Cell::new(None),
            clip_owner: OnceCell::new(),
            catcher: OnceCell::new(),
            on_terminate: RefCell::new(None),
            on_reopen: RefCell::new(None),
        }
    }

    fn clip_owner(&self) -> Result<&clip_owner::ClipOwner, PlatformError> {
        self.clip_owner
            .get_or_init(|| {
                clip_owner::ClipOwner::start()
                    .inspect_err(|e| log::warn!("cannot own the clipboard: {e}"))
                    .ok()
            })
            .as_ref()
            .ok_or(PlatformError::Unsupported)
    }
}

/// The session bus, one connection for the whole run. GLib keeps its
/// shared connection only while something holds it, and a new one is a new
/// client to the portal: the app id registered on the first would not
/// cover calls made on a later one.
fn session_bus() -> Result<gio::DBusConnection, gio::glib::Error> {
    static BUS: std::sync::OnceLock<gio::DBusConnection> = std::sync::OnceLock::new();
    if let Some(bus) = BUS.get() {
        return Ok(bus.clone());
    }
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    Ok(BUS.get_or_init(|| bus).clone())
}

/// How long [`Platform::write_clipboard`] waits for GNOME to see the copy.
const CLIP_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

impl Platform for LinuxPlatform {
    fn prepare_app(&self) {}

    fn sample_input(&self) -> InputSample {
        if terminate::asked() {
            if let Some(handler) = self.on_terminate.borrow().as_ref() {
                log::info!("tidying up before quitting");
                (handler.0)();
            }
            std::process::exit(0);
        }
        if let Some(Some(catcher)) = self.catcher.get() {
            catcher.pump();
        }
        if instance::asked_to_open()
            && let Some(handler) = self.on_reopen.borrow().as_ref()
        {
            (handler.0)();
        }
        match self.shell.state() {
            Some(state) => shell::to_sample(&state),
            None => self.stub.sample_input(),
        }
    }

    fn drag_paths(&self) -> Vec<PathBuf> {
        shell::local_paths(&self.shell.drag_uris())
    }

    fn screens(&self) -> Vec<Rect> {
        let monitors = self.shell.monitors();
        if monitors.is_empty() {
            return self.stub.screens();
        }
        monitors.iter().map(|m| m.work).collect()
    }

    fn copy_files(&self, paths: &[PathBuf]) -> Result<(), PlatformError> {
        let items = paths
            .iter()
            .map(|p| {
                vec![ClipRep {
                    ty: TYPE_FILE_URL.to_owned(),
                    data: gio::File::for_path(p).uri().as_bytes().to_vec(),
                }]
            })
            .collect();
        self.clip_owner()?
            .own(clipboard::offers(&ClipSnapshot { items }))
    }

    fn copy_text(&self, text: &str) -> Result<(), PlatformError> {
        let item = vec![ClipRep {
            ty: TYPE_TEXT.to_owned(),
            data: text.as_bytes().to_vec(),
        }];
        self.clip_owner()?
            .own(clipboard::offers(&ClipSnapshot { items: vec![item] }))
    }

    fn clipboard_types(&self) -> Vec<String> {
        clipboard::core_types(&self.shell.clipboard_types())
    }

    fn clipboard_change_count(&self) -> i64 {
        self.shell.state().map_or(0, |s| i64::from(s.clip_serial))
    }

    fn read_clipboard(&self) -> ClipSnapshot {
        let types = self.shell.clipboard_types();
        clipboard::snapshot(&types, MAX_SNAPSHOT_BYTES, |mime| {
            self.shell.read_clipboard(mime)
        })
    }

    /// Returns the serial GNOME gives the copy, waiting up to
    /// [`CLIP_SETTLE`] for it; if it has not moved by then, the one it will
    /// move to.
    fn write_clipboard(&self, snapshot: &ClipSnapshot) -> Result<i64, PlatformError> {
        let before = self.shell.clip_serial_now().unwrap_or(0);
        self.clip_owner()?.own(clipboard::offers(snapshot))?;
        let deadline = std::time::Instant::now() + CLIP_SETTLE;
        while std::time::Instant::now() < deadline {
            match self.shell.clip_serial_now() {
                Some(now) if now != before => return Ok(i64::from(now)),
                _ => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        Ok(i64::from(before) + 1)
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

    fn open_terminal(
        &self,
        terminal: Option<&str>,
        dir: &Path,
        on_error: Box<dyn Fn(String) + Send>,
    ) -> Result<(), PlatformError> {
        terminal::open(terminal, dir, on_error)
    }

    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf> {
        portal::pick_folder(start)
    }

    /// The primary screen: GNOME screens have no notch, and the top bar
    /// stands in for the menu bar. The island sits in the room the
    /// extension keeps for it in the top bar, when that is on this screen.
    fn pocket_home(&self) -> Option<IslandHome> {
        let primary = *self.shell.monitors().first()?;
        let slot = self.shell.island_slot().filter(|s| {
            let f = primary.frame;
            s.x >= f.x && s.x + s.w <= f.x + f.w && s.y >= f.y && s.y < f.y + f.h
        });
        Some(IslandHome {
            screen: primary.frame,
            notch: None,
            menu_bar_h: (primary.work.y - primary.frame.y).max(0.0),
            slot,
        })
    }

    /// Override-redirect, which on X11 does all three: above everything,
    /// never focused, kept where vento puts it.
    fn make_overlay(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        x11::make_override_redirect(window)
    }

    /// See [`Platform::make_overlay`].
    fn make_non_key(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        x11::make_override_redirect(window)
    }

    /// See [`Platform::make_overlay`].
    fn keep_window_frame(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        x11::make_override_redirect(window)
    }

    fn set_input_rect(
        &self,
        window: raw_window_handle::RawWindowHandle,
        rect: Option<Rect>,
    ) -> Result<(), PlatformError> {
        x11::set_input_rect(window, rect)
    }

    fn file_icon(&self, path: &Path, px: u32) -> Option<RgbaImage> {
        let scale = self.scale_factor_at(Point { x: 0.0, y: 0.0 });
        icons::file_icon(path, (f64::from(px) * scale).round() as u32)
    }

    fn kind_description(&self, path: &Path) -> Option<String> {
        apps::kind_description(path)
    }

    fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>> {
        let catcher = self.catcher.get_or_init(|| {
            catcher::LinuxCatcher::start()
                .map(Rc::new)
                .inspect_err(|e| log::warn!("no drop catcher: {e}"))
                .ok()
        });
        catcher.clone().map(|c| c as Rc<dyn DropCatcher>)
    }

    fn trash(&self, path: &Path) -> Result<(), PlatformError> {
        trash::trash(path)
    }

    fn screens_changed(&self) -> bool {
        let now = self.shell.monitors_serial();
        let before = self.screens_seen.replace(now);
        before.is_some() && before != now
    }

    /// On SIGTERM (GNOME ending the session), SIGINT or SIGHUP, run from
    /// the next input sample, which comes at least every 100 ms.
    fn on_terminate(&self, f: Box<dyn Fn()>) {
        *self.on_terminate.borrow_mut() = Some(terminate::Handler(f));
        terminate::listen();
    }

    /// GNOME Shell's theme, as it styles the top bar's menus.
    fn menu_colors(&self) -> Option<MenuColors> {
        shell::menu_colors()
    }

    /// When another launch asks this Vento to open (see `claim_instance`),
    /// from the next input sample.
    fn on_reopen(&self, f: Box<dyn Fn()>) {
        *self.on_reopen.borrow_mut() = Some(terminate::Handler(f));
    }

    fn activate_app(&self) {
        if !self.shell.focus("Activate") {
            log::debug!("activate_app: no window of vento's to focus");
        }
    }

    fn deactivate_app(&self) {
        self.shell.focus("Deactivate");
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
        let monitors = self.shell.monitors();
        shell::monitor_at(&monitors, p)
            .or(monitors.first())
            .map_or(1.0, |m| m.scale)
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
        // GNOME opens it at the pointer, where the click was.
        let _ = (window, at);
        shell::show_menu(entries)
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
