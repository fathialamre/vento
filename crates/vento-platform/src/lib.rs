//! Operating-system access for vento: global input sampling, screens,
//! clipboard, terminals and the folder picker, behind one [`Platform`] trait.
//!
//! macOS has a real implementation; every other OS gets a [`StubPlatform`] so
//! the workspace still compiles there.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::geometry::{Point, Rect};
use vento_core::trigger::InputSample;

mod stub;
pub use stub::StubPlatform;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacPlatform;

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
    /// Screen work areas (each screen without the menu bar and Dock) in
    /// logical, top-left-origin global coordinates; the primary screen comes
    /// first.
    fn screens(&self) -> Vec<Rect>;
    /// Replace the clipboard with these files.
    fn copy_files(&self, paths: &[PathBuf]) -> Result<(), PlatformError>;
    /// Replace the clipboard with this text.
    fn copy_text(&self, text: &str) -> Result<(), PlatformError>;
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
    /// The Finder icon of `path`, rendered at `px`×`px` points times the
    /// main screen's backing scale. `None` if `path` doesn't exist or the
    /// icon can't be rendered. Main thread only.
    fn file_icon(&self, path: &Path, px: u32) -> Option<RgbaImage>;
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
    /// [`Platform::activate_app`]. Main thread only.
    fn deactivate_app(&self);
    /// The backing scale factor (physical pixels per logical point) of the
    /// screen that contains `p`, a global logical point; the primary screen's
    /// when none does. 1.0 where unsupported.
    fn scale_factor_at(&self, p: Point) -> f64;
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
