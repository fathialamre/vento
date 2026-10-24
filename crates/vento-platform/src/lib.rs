//! Operating-system access for vento: global input sampling, screens,
//! clipboard, terminals and the folder picker, behind one [`Platform`] trait.
//!
//! macOS has a real implementation; every other OS gets a [`StubPlatform`] so
//! the workspace still compiles there.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::clip::ClipSnapshot;
use vento_core::convert::{ConvertError, ImageFormat};
use vento_core::geometry::{Point, Rect};
use vento_core::screen_text::{Selection, TextBox};
use vento_core::trigger::InputSample;

mod stub;
pub use stub::{StubPlatform, StubScreenText};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{MacPlatform, MacScreenText};

/// A terminal application that can be launched in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalApp {
    /// Stable identifier (macOS: bundle id).
    pub id: String,
    /// Human-readable name for menus.
    pub name: String,
}

/// Where the Pocket island lives: the notched screen, or the primary one.
///
/// All values are logical, top-left-origin global coordinates, consistent
/// with [`Platform::screens`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IslandHome {
    /// Full frame of the island's screen (menu bar included).
    pub screen: Rect,
    /// The camera housing between the two menu-bar halves, if the screen
    /// has one.
    pub notch: Option<Rect>,
    /// Height of the menu bar on that screen.
    pub menu_bar_h: f64,
}

/// An RGBA8 image, not premultiplied, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// Content dropped on the [`DropCatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// One or more files or folders.
    Files(Vec<PathBuf>),
    /// Plain text.
    Text(String),
    /// A web link (never a `file:` URL).
    Url(String),
    /// Image data as dragged; `ext` is `"png"` or `"tiff"`.
    Image { bytes: Vec<u8>, ext: &'static str },
}

/// What the [`DropCatcher`] reports while a drag passes over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatcherEvent {
    /// A drag entered the catcher (sent once per entry).
    Entered,
    /// The drag left the catcher without dropping.
    Exited,
    /// The drag was dropped on the catcher.
    Dropped(Dropped),
}

/// A native drop target placed over the Pocket island during drags, so the
/// Pocket can accept files, text, links and images (winit only reports file
/// paths). Main thread only.
pub trait DropCatcher {
    /// Show the catcher over `rect` (logical, top-left-origin global
    /// coordinates) without activating the app or taking key focus.
    fn show(&self, rect: Rect);
    /// Hide the catcher so it no longer receives drags or clicks.
    fn hide(&self);
    /// Replace the event handler; it is only ever called on the main thread.
    fn on_event(&self, f: Box<dyn Fn(CatcherEvent)>);
}

/// An app that can open a file, for an "Open With" menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// The app bundle.
    pub path: PathBuf,
    /// The name Finder shows for it, without ".app".
    pub name: String,
    /// Whether it is the file's default app.
    pub is_default: bool,
}

/// One entry of a native context menu ([`Platform::context_menu`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A leaf the user can choose; the menu reports its `id`.
    Item {
        id: u32,
        title: String,
        enabled: bool,
        /// Shows this file's Finder icon (16 pt) beside the title.
        icon: Option<PathBuf>,
    },
    /// An entry that opens `children` beside it.
    Submenu {
        title: String,
        enabled: bool,
        children: Vec<MenuEntry>,
    },
    /// A line between groups.
    Separator,
}

/// Why a platform operation failed.
#[derive(thiserror::Error, Debug)]
pub enum PlatformError {
    #[error("{0}")]
    Failed(String),
    #[error("unsupported on this platform")]
    Unsupported,
}

/// Everything vento needs from the operating system.
///
/// Implementations are single-threaded: methods that touch UI frameworks must
/// be called on the main thread and degrade to a safe default when they are
/// not.
pub trait Platform {
    /// One-time process setup (macOS: Accessory activation policy, so there
    /// is no Dock icon and activation is never stolen).
    fn prepare_app(&self);
    /// Snapshot of global input state. Main thread only.
    fn sample_input(&self) -> InputSample;
    /// The local files in the drag in progress (empty when none, or when
    /// the drag carries no files). Main thread only.
    fn drag_paths(&self) -> Vec<PathBuf>;
    /// Screen work areas (each screen without the menu bar and Dock) in
    /// logical, top-left-origin global coordinates; the primary screen comes
    /// first.
    fn screens(&self) -> Vec<Rect>;
    /// Replace the clipboard with these files.
    fn copy_files(&self, paths: &[PathBuf]) -> Result<(), PlatformError>;
    /// Replace the clipboard with this text.
    fn copy_text(&self, text: &str) -> Result<(), PlatformError>;
    /// Every pasteboard type name of every clipboard item, in order,
    /// duplicates kept. Cheap: reads names only, never data.
    fn clipboard_types(&self) -> Vec<String>;
    /// The clipboard's change counter now (the one input samples carry as
    /// `clip_change_count`). Cheap: reads no content. Main thread only.
    fn clipboard_change_count(&self) -> i64;
    /// The whole clipboard: per item, every representation that has data.
    /// Items left with none are dropped. A copy too big to keep is cut as
    /// `vento_core::clip::capture` cuts it, and not read any further.
    fn read_clipboard(&self) -> ClipSnapshot;
    /// Replace the clipboard with this snapshot, one pasteboard item per
    /// snapshot item. Returns the clipboard's change counter after writing,
    /// so the caller can tell its own write from a copy.
    fn write_clipboard(&self, snapshot: &ClipSnapshot) -> Result<i64, PlatformError>;
    /// The real path of a file reference (`/.file/id=…`: Finder copies
    /// files as reference URLs), or `None` if it leads to no file now.
    fn resolve_file_reference(&self, path: &Path) -> Option<PathBuf>;
    /// Installed terminal apps; on macOS Terminal is always first.
    fn terminals(&self) -> Vec<TerminalApp>;
    /// Open a terminal in `dir`. `terminal` is a [`TerminalApp::id`]; an
    /// unknown or uninstalled id falls back to the default terminal.
    fn open_terminal(&self, terminal: Option<&str>, dir: &Path) -> Result<(), PlatformError>;
    /// Show a modal folder picker, optionally starting in `start`.
    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf>;
    /// Where the Pocket island sits: the first notched screen, otherwise the
    /// primary one. Main thread only.
    fn pocket_home(&self) -> Option<IslandHome>;
    /// Float this window above the menu bar on every Space (including
    /// full-screen ones), out of the window cycle and without a shadow.
    /// Main thread only.
    fn make_overlay(&self, window: raw_window_handle::RawWindowHandle)
    -> Result<(), PlatformError>;
    /// Keep this window from ever becoming the key or main window, and
    /// clicks on it from activating vento, so showing or clicking it never
    /// takes keystrokes from the frontmost app. Main thread only.
    fn make_non_key(&self, window: raw_window_handle::RawWindowHandle)
    -> Result<(), PlatformError>;
    /// Keep this window exactly where vento places it: AppKit would push a
    /// window whose top meets the menu bar further down. Main thread only.
    fn keep_window_frame(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError>;
    /// The Finder icon of `path`, rendered at `px`×`px` points times the
    /// main screen's backing scale. `None` if `path` doesn't exist or the
    /// icon can't be rendered. Main thread only.
    fn file_icon(&self, path: &Path, px: u32) -> Option<RgbaImage>;
    /// The localised kind Finder shows for `path` ("PNG image", "Folder",
    /// "Application"); `None` if unknown. Main thread only.
    fn kind_description(&self, path: &Path) -> Option<String>;
    /// The native drop catcher, created on first use and shared afterwards.
    /// `None` where unsupported or off the main thread.
    fn drop_catcher(&self) -> Option<Rc<dyn DropCatcher>>;
    /// Move `path` (a file, folder or link, never what a link points to) to
    /// the Trash. `Unsupported` where there is no Trash: callers then leave
    /// the file where it is.
    fn trash(&self, path: &Path) -> Result<(), PlatformError>;
    /// Whether the screens changed (added, removed, resized or rearranged)
    /// since the last call. Cheap enough to ask on every tick.
    fn screens_changed(&self) -> bool;
    /// Run `f` on the main thread when the app is about to terminate, also
    /// when the system ends it (log out, shut down). Replaces any earlier
    /// `f`.
    fn on_terminate(&self, f: Box<dyn Fn()>);
    /// Bring vento to the front so a window of its own can take keystrokes.
    /// Main thread only.
    fn activate_app(&self);
    /// Hand focus back to the app that was in front before
    /// [`Platform::activate_app`], without hiding vento's other windows;
    /// nothing happens when another app already took focus. Main thread
    /// only.
    fn deactivate_app(&self);
    /// Take the app's menu bar away, so its key equivalents (⌘Q, ⌘H, …)
    /// reach vento's key window as ordinary key presses. Kept until
    /// [`Platform::restore_app_menu`]; calling it again meanwhile does
    /// nothing. Main thread only; nothing happens where unsupported.
    fn suspend_app_menu(&self);
    /// Put back the exact menu bar [`Platform::suspend_app_menu`] took
    /// away; nothing happens when none was taken. Main thread only.
    fn restore_app_menu(&self);
    /// Call `f` on the main thread for each mouse-button press (left, right
    /// or other) in another app or on the desktop, until
    /// [`Platform::stop_watching_outside_clicks`]. Presses in vento's own
    /// windows and on its menu-bar icon are not reported. Replaces any
    /// earlier watch. Needs no permission. Main thread only; nothing
    /// happens where unsupported.
    fn watch_outside_clicks(&self, f: Box<dyn Fn()>);
    /// Stop the watch [`Platform::watch_outside_clicks`] started, and drop
    /// its `f`; nothing happens when none runs. Main thread only.
    fn stop_watching_outside_clicks(&self);
    /// The backing scale factor (physical pixels per logical point) of the
    /// screen that contains `p`, a global logical point; the primary screen's
    /// when none does. 1.0 where unsupported.
    fn scale_factor_at(&self, p: Point) -> f64;
    /// The apps that can open `path`, its default app first (marked
    /// [`AppInfo::is_default`]), each app once. Empty where unsupported,
    /// or when `path` doesn't exist.
    fn apps_for(&self, path: &Path) -> Vec<AppInfo>;
    /// Open each of `paths` with its default app. Main thread only.
    fn open_paths(&self, paths: &[PathBuf]) -> Result<(), PlatformError>;
    /// Open all of `paths` with the app bundle `app`. Errors found at once
    /// are returned; a launch that fails later calls `on_error` with the
    /// reason, once, on any thread. Main thread only.
    fn open_with(
        &self,
        paths: &[PathBuf],
        app: &Path,
        on_error: Box<dyn Fn(String) + Send>,
    ) -> Result<(), PlatformError>;
    /// Show `paths` in Finder, selected. Nothing happens where
    /// unsupported. Main thread only.
    fn reveal_in_finder(&self, paths: &[PathBuf]);
    /// Show a context menu of `entries` with its top-left corner at `at`
    /// (window-local logical point, top-left origin) over `window`, and
    /// wait until it closes. Returns the id of the [`MenuEntry::Item`]
    /// chosen, or `None` if the menu was dismissed (or is unsupported).
    /// Works from a window that is never key, in an app that is not
    /// active. Main thread only.
    fn context_menu(
        &self,
        window: raw_window_handle::RawWindowHandle,
        at: Point,
        entries: &[MenuEntry],
    ) -> Option<u32>;
}

/// What Copy from screen needs from the operating system, apart from
/// [`read_screen_text`]: the Screen Recording permission and the area
/// selection. A trait of its own, so [`Platform`] fakes need not change.
pub trait ScreenText {
    /// Whether this Mac can run Copy from screen (macOS 14 or later).
    fn supported(&self) -> bool;
    /// Screen Recording granted (`CGPreflightScreenCaptureAccess`). Shows nothing.
    fn has_access(&self) -> bool;
    /// `CGRequestScreenCaptureAccess`: macOS's own prompt the first time, then nothing.
    fn request_access(&self);
    /// Opens Privacy & Security → Screen Recording.
    fn open_access_settings(&self);
    /// Dims every screen and lets the user drag a rectangle; `done` gets the
    /// selection, or `None` when cancelled. Main thread only.
    fn select_area(&self, done: Box<dyn FnOnce(Option<Selection>)>);
}

/// The [`ScreenText`] implementation for the running OS.
pub fn screen_text() -> Rc<dyn ScreenText> {
    #[cfg(target_os = "macos")]
    {
        Rc::new(MacScreenText)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Rc::new(StubScreenText)
    }
}

/// Captures `sel` and reads the text in it. Blocks; call off the main thread.
/// The picture is never written to disk and is dropped before this returns.
/// `Unsupported` where [`ScreenText::supported`] is false.
pub fn read_screen_text(sel: &Selection) -> Result<Vec<TextBox>, PlatformError> {
    #[cfg(target_os = "macos")]
    {
        macos::read_screen_text(sel)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::read_screen_text(sel)
    }
}

/// The languages the text reader recognises, as language codes (`en-US`,
/// `ar-SA`, ...), for the log. Empty where [`ScreenText::supported`] is
/// false or the list cannot be had. Loads Vision; call off the main thread.
pub fn screen_text_languages() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        macos::screen_text_languages()
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::screen_text_languages()
    }
}

/// The quarantine attribute (`com.apple.quarantine`) of `path` itself, never
/// of what a link points to. `None` when there is none, and also when it
/// can't be read. A free function so a worker thread can call it. Always
/// `None` where unsupported.
pub fn quarantine_of(path: &Path) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        macos::quarantine_of(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::quarantine_of(path)
    }
}

/// Mark `path` itself (never what a link points to) with the quarantine
/// attribute `value`, as read by [`quarantine_of`]. A free function so a
/// worker thread can call it. Does nothing where unsupported.
pub fn set_quarantine(path: &Path, value: &[u8]) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::set_quarantine(path, value)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::set_quarantine(path, value)
    }
}

/// Whether `path` is a package: a folder Finder shows as a single file (an
/// app or other bundle). False for files and missing paths. A free function
/// so a worker thread can call it. Always false where unsupported.
pub fn is_package(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_package(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::is_package(path)
    }
}

/// A thumbnail of the picture file at `path`: its shorter side `short_px`
/// pixels, its longer one in proportion but at most `max_ratio` times as
/// long. `None` if it is missing or not a readable picture. A free function
/// so a worker thread can call it. Always `None` where unsupported.
pub fn picture_thumbnail(path: &Path, short_px: u32, max_ratio: u32) -> Option<RgbaImage> {
    #[cfg(target_os = "macos")]
    {
        macos::picture_thumbnail(path, short_px, max_ratio)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, short_px, max_ratio);
        None
    }
}

/// The format the image file at `path` decodes as, judged by its content:
/// `None` for a readable image of another format, `Err(NotAnImage)` when it
/// cannot be decoded. A free function so a worker thread can call it.
/// Always `Err(Unsupported)` where unsupported.
pub fn probe_image(path: &Path) -> Result<Option<ImageFormat>, ConvertError> {
    #[cfg(target_os = "macos")]
    {
        macos::probe_image(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::probe_image(path)
    }
}

/// Converts the image at `src` to `format`, filling the existing empty file
/// at `dst`: the first frame, upright, with the source's metadata. A free
/// function so a worker thread can call it. Always `Err(Unsupported)` where
/// unsupported.
pub fn write_image(src: &Path, dst: &Path, format: ImageFormat) -> Result<(), ConvertError> {
    #[cfg(target_os = "macos")]
    {
        macos::write_image(src, dst, format)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::write_image(src, dst, format)
    }
}

/// Make file reads on the calling thread fail on cloud-only files instead
/// of downloading them. Call it on the thread that walks, before walking.
/// Does nothing where unsupported.
pub fn block_cloud_downloads_on_this_thread() -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        macos::block_cloud_downloads_on_this_thread()
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::block_cloud_downloads_on_this_thread()
    }
}

/// Which volumes a size count starting at `start` may enter. A free
/// function so a worker thread can call it. The default scope (stay on
/// `start`'s device) where unsupported.
pub fn volume_scope(start: &Path) -> vento_core::sizes::VolumeScope {
    #[cfg(target_os = "macos")]
    {
        macos::volume_scope(start)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::volume_scope(start)
    }
}

/// The platform implementation for the running OS.
pub fn current() -> Rc<dyn Platform> {
    #[cfg(target_os = "macos")]
    {
        Rc::new(MacPlatform::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Rc::new(StubPlatform)
    }
}
