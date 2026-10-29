//! Operating-system access for vento: global input sampling, screens,
//! clipboard, terminals and the folder picker, behind one [`Platform`] trait.
//!
//! macOS has a real implementation and Linux (GNOME) a partial one; every
//! other OS gets a [`StubPlatform`] so the workspace still compiles there.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use vento_core::clip::ClipSnapshot;
use vento_core::convert::{ConvertError, ImageFormat};
use vento_core::geometry::{Point, Rect};
use vento_core::screen_text::{Selection, TextBox};
use vento_core::share::{ShareOutcome, ShareTarget};
use vento_core::trigger::InputSample;

mod stub;
pub use stub::{StubPlatform, StubScreenText};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod webp;
#[cfg(target_os = "macos")]
pub use macos::{MacPlatform, MacScreenText, set_ns_menu_direction};
#[cfg(target_os = "macos")]
pub use macos::{PdfRenderer, SystemPdf};
#[cfg(not(target_os = "macos"))]
pub use stub::{PdfRenderer, SystemPdf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    APP_ID, DragEnd, GlobalShortcut, LinuxPlatform, bind_global_shortcut, claim_instance,
    end_startup, start_file_drag, tray_icon_frame,
};

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
    /// Room kept free in the menu bar for the collapsed island (GNOME's top
    /// bar keeps some for it); the island then sits there, as beside a
    /// notch, rather than in the middle of the screen.
    pub slot: Option<Rect>,
}

/// How [`Platform::keep_awake`] holds the computer awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwakeHold {
    /// The screen stays on too: it does not dim, blank or lock.
    pub screen: bool,
}

/// The colours the desktop's own menus use (GNOME Shell's theme), each
/// RGBA8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuColors {
    pub background: [u8; 4],
    /// The menus' edge.
    pub border: [u8; 4],
    pub text: [u8; 4],
}

impl MenuColors {
    /// Whether the menus are dark: their background is darker than mid
    /// grey (relative luminance, as WCAG weighs the channels).
    pub fn is_dark(&self) -> bool {
        let [r, g, b, _] = self.background.map(f64::from);
        0.2126 * r + 0.7152 * g + 0.0722 * b < 127.5
    }
}

/// An RGBA8 image, not premultiplied, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

impl AsMut<[u8]> for RgbaImage {
    /// The pixels, for drawing into.
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }
}

/// Content dropped on the [`DropCatcher`].
#[derive(Clone, PartialEq, Eq)]
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

/// Image data shows as its size: logs would otherwise print every byte.
impl std::fmt::Debug for Dropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Files(paths) => f.debug_tuple("Files").field(paths).finish(),
            Self::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Self::Url(url) => f.debug_tuple("Url").field(url).finish(),
            Self::Image { bytes, ext } => f
                .debug_struct("Image")
                .field("bytes", &bytes.len())
                .field("ext", ext)
                .finish(),
        }
    }
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
    /// The app with this id or name is not installed. Worded when shown, in
    /// the interface language.
    #[error("{}", not_installed(.0))]
    NotInstalled(String),
    /// No terminal app is installed. Worded when shown, in the interface
    /// language.
    #[error("{}", no_terminal())]
    NoTerminal,
    /// The items give no folder to open (the Terminal action). Worded when
    /// shown, in the interface language.
    #[error("{}", no_folder())]
    NoFolder,
    /// No app opened these items (their names, joined). Worded when shown,
    /// in the interface language.
    #[error("{}", no_app_opened(.0))]
    NoAppOpened(Vec<String>),
    /// This app is no longer where it was (its name). Worded when shown, in
    /// the interface language.
    #[error("{}", app_gone(.0))]
    AppGone(String),
    /// The clipboard took no text. Worded when shown, in the interface
    /// language.
    #[error("{}", vento_core::i18n::tr("clipboard rejected the text"))]
    ClipboardRejectedText,
    /// The clipboard took no files. Worded when shown, in the interface
    /// language.
    #[error("{}", vento_core::i18n::tr("clipboard rejected the files"))]
    ClipboardRejectedFiles,
    /// A program could not be started. Worded when shown, in the interface
    /// language.
    #[error("{}", could_not_run(.program, .error))]
    CouldNotRun {
        program: String,
        error: std::io::Error,
    },
    /// A program that was started ended with a failure: `code` is its exit
    /// code, `None` when a signal stopped it. Worded when shown, in the
    /// interface language.
    #[error("{}", program_failed(.program, *.code))]
    ProgramFailed { program: String, code: Option<i32> },
    /// Waiting for a program that was started failed. Worded when shown, in
    /// the interface language.
    #[error("{}", program_unwaited(.program, .error))]
    ProgramUnwaited {
        program: String,
        error: std::io::Error,
    },
    #[error("unsupported on this platform")]
    Unsupported,
}

/// "no app opened `names`", in the interface language.
fn no_app_opened(names: &[String]) -> String {
    use vento_core::i18n::{fill, tr};
    fill(
        &tr("no app opened {names}"),
        &[("names", &names.join(&tr(", ")))],
    )
}

/// "`app` is no longer there", in the interface language.
fn app_gone(app: &str) -> String {
    use vento_core::i18n::{fill, tr};
    fill(&tr("{app} is no longer there"), &[("app", app)])
}

/// "could not run `program`: why", in the interface language.
fn could_not_run(program: &str, error: &std::io::Error) -> String {
    use vento_core::i18n::{errors::io_reason, fill, tr};
    fill(
        &tr("could not run {program}: {reason}"),
        &[("program", program), ("reason", &io_reason(error))],
    )
}

/// How a program that failed ended, in the interface language.
fn program_failed(program: &str, code: Option<i32>) -> String {
    use vento_core::i18n::{fill, tr};
    match code {
        Some(code) => fill(
            &tr("{program} exited with code {code}"),
            &[("program", program), ("code", &code.to_string())],
        ),
        None => fill(
            &tr("{program} was stopped by a signal"),
            &[("program", program)],
        ),
    }
}

/// "`program`: why", for a program that could not be waited for, in the
/// interface language.
fn program_unwaited(program: &str, error: &std::io::Error) -> String {
    use vento_core::i18n::{errors::io_reason, fill, tr};
    fill(
        &tr("{program}: {reason}"),
        &[("program", program), ("reason", &io_reason(error))],
    )
}

/// "`app` is not installed", in the interface language.
fn not_installed(app: &str) -> String {
    vento_core::i18n::fill(
        &vento_core::i18n::tr("{app} is not installed"),
        &[("app", app)],
    )
}

/// "no terminal is installed", in the interface language.
fn no_terminal() -> String {
    vento_core::i18n::tr("no terminal is installed")
}

/// "no folder to open", in the interface language.
fn no_folder() -> String {
    vento_core::i18n::tr("no folder to open")
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
    /// unknown or uninstalled id falls back to the default terminal. Errors
    /// found at once are returned; a launch that fails later calls
    /// `on_error` with the reason, once, on any thread.
    fn open_terminal(
        &self,
        terminal: Option<&str>,
        dir: &Path,
        on_error: Box<dyn Fn(PlatformError) + Send>,
    ) -> Result<(), PlatformError>;
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
    /// Let this window take the pointer only inside `rect`, in physical
    /// pixels from its top-left corner, or nowhere when `None`; clicks
    /// elsewhere fall through to what is below. `Unsupported` where vento
    /// turns hit-testing on and off as the pointer moves instead. Main
    /// thread only.
    fn set_input_rect(
        &self,
        window: raw_window_handle::RawWindowHandle,
        rect: Option<Rect>,
    ) -> Result<(), PlatformError> {
        let _ = (window, rect);
        Err(PlatformError::Unsupported)
    }
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
    /// Run `f` instead of quitting when the user quits from the app menu
    /// (⌘Q on macOS), so vento's own quit can ask about unsaved edits. The
    /// system ending vento (log out, shut down) is not held back. Replaces
    /// any earlier `f`. Main thread, once the event loop exists.
    fn on_quit_request(&self, f: Box<dyn Fn()>) {
        let _ = f;
    }
    /// Run `f` when vento is launched again while it runs (the app grid,
    /// a second start at login). Main thread only.
    fn on_reopen(&self, f: Box<dyn Fn()>) {
        let _ = f;
    }
    /// Keeps the computer from sleeping while idle, and the screen on too
    /// when `hold.screen`, until called again; `None` lets it sleep as
    /// usual again. Replaces any earlier hold, which ends with the process
    /// in any case. Main thread only.
    fn keep_awake(&self, hold: Option<AwakeHold>) -> Result<(), PlatformError> {
        let _ = hold;
        Err(PlatformError::Unsupported)
    }
    /// Shows or hides a Keep Awake mark that the desktop draws in its top
    /// bar (GNOME's, through the Vento extension); a click on it runs the
    /// [`Platform::on_awake_indicator`] handler. Where the app draws its own
    /// (a menu-bar icon on the Mac), nothing. Main thread only.
    fn show_awake_indicator(&self, shown: bool) {
        let _ = shown;
    }
    /// Run `f` with the mark's frame, in global logical points, when it is
    /// clicked. Replaces any earlier `f`. Main thread only.
    fn on_awake_indicator(&self, f: Box<dyn Fn(Rect)>) {
        let _ = f;
    }
    /// The colours the desktop's own menus use, for vento's panel to match;
    /// `None` where it keeps its own. Asked each time the panel opens.
    fn menu_colors(&self) -> Option<MenuColors> {
        None
    }
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
        on_error: Box<dyn Fn(PlatformError) + Send>,
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
    /// The services [`Platform::share`] can hand items to on this machine:
    /// those of AirDrop, Messages, Mail and Notes that exist, in that order.
    /// Never [`ShareTarget::More`], which [`Platform::share_picker`] shows.
    /// Empty where unsupported. Main thread only.
    fn share_targets(&self) -> Vec<ShareTarget> {
        Vec::new()
    }
    /// Whether `target`'s service takes all of `paths` (Notes takes no
    /// folder together with files). Always true for [`ShareTarget::More`];
    /// false where unsupported or when the service is missing. Main thread
    /// only.
    fn can_share(&self, target: ShareTarget, paths: &[PathBuf]) -> bool {
        let _ = (target, paths);
        false
    }
    /// Hand `paths` to `target`'s service, which shows its own window, and
    /// return. `on_done` is called once, later, on the main thread, with how
    /// the share ended; focus has gone back to the app that had it by then.
    /// [`ShareTarget::More`] is an error (it is [`Platform::share_picker`]),
    /// and so is a call while another share or the share popover is open.
    /// Main thread only.
    fn share(
        &self,
        target: ShareTarget,
        paths: &[PathBuf],
        on_done: Box<dyn Fn(ShareOutcome)>,
    ) -> Result<(), PlatformError> {
        let _ = (target, paths, on_done);
        Err(PlatformError::Unsupported)
    }
    /// Show the system's share popover for `paths`, pointing at `at` (global
    /// logical point, top-left origin), and return. The popover closes
    /// itself; a service picked from it runs on its own and reports nothing.
    /// A call while a share or another popover is open is an error. Main
    /// thread only.
    fn share_picker(&self, paths: &[PathBuf], at: Point) -> Result<(), PlatformError> {
        let _ = (paths, at);
        Err(PlatformError::Unsupported)
    }
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
    #[cfg(target_os = "linux")]
    {
        Rc::new(linux::screen_text::LinuxScreenText)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    #[cfg(target_os = "linux")]
    {
        linux::screen_text::read_screen_text(sel)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    #[cfg(target_os = "linux")]
    {
        linux::screen_text::screen_text_languages()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

/// The Finder icon of `path`, as [`Platform::file_icon`] renders it but at
/// `scale` physical pixels per point. A free function so a worker thread
/// can call it. Always `None` where unsupported.
pub fn file_icon_at(path: &Path, px: u32, scale: f64) -> Option<RgbaImage> {
    #[cfg(target_os = "macos")]
    {
        macos::file_icon_at(path, px, scale)
    }
    #[cfg(target_os = "linux")]
    {
        linux::file_icon_at(path, px, scale)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (path, px, scale);
        None
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
    #[cfg(target_os = "linux")]
    {
        linux::picture_thumbnail(path, short_px, max_ratio)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (path, short_px, max_ratio);
        None
    }
}

/// [`picture_thumbnail`] of encoded picture `bytes` (PNG, TIFF, JPEG, HEIC:
/// any format macOS reads). Only about as many pixels as the thumbnail
/// needs are decoded. Always `None` where unsupported.
pub fn picture_data_thumbnail(bytes: &[u8], short_px: u32, max_ratio: u32) -> Option<RgbaImage> {
    #[cfg(target_os = "macos")]
    {
        macos::picture_data_thumbnail(bytes, short_px, max_ratio)
    }
    #[cfg(target_os = "linux")]
    {
        linux::picture_data_thumbnail(bytes, short_px, max_ratio)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (bytes, short_px, max_ratio);
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
    #[cfg(target_os = "linux")]
    {
        linux::image_io::probe_image(path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        stub::probe_image(path)
    }
}

/// The formats Convert can write here, in the ring's order: on Linux, HEIC
/// only with libheif's encoder.
pub fn convert_targets() -> Vec<ImageFormat> {
    #[cfg(target_os = "linux")]
    {
        linux::image_io::targets()
    }
    #[cfg(not(target_os = "linux"))]
    {
        ImageFormat::TARGETS.to_vec()
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
    #[cfg(target_os = "linux")]
    {
        linux::image_io::write_image(src, dst, format)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        stub::write_image(src, dst, format)
    }
}

/// Asks the user for PDF files with the system's open panel, starting in
/// `start`; several can be chosen. Empty when cancelled. Main thread only.
/// Always empty where unsupported.
pub fn pick_pdfs(start: Option<&Path>) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos::pick_pdfs(start)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::pick_pdfs(start)
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
    #[cfg(target_os = "linux")]
    {
        Rc::new(LinuxPlatform::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Rc::new(StubPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_are_dark_when_their_background_is() {
        let menus = |background| MenuColors {
            background,
            border: [0; 4],
            text: [0; 4],
        };
        // Yaru's menus, light and dark.
        assert!(!menus([0xff, 0xff, 0xff, 0xff]).is_dark());
        assert!(menus([0x36, 0x36, 0x3a, 0xff]).is_dark());
        // Green weighs most: a bright green is light, a bright blue dark.
        assert!(!menus([0, 0xc0, 0, 0xff]).is_dark());
        assert!(menus([0, 0, 0xff, 0xff]).is_dark());
    }

    #[test]
    fn a_missing_terminal_is_worded_when_it_is_shown() {
        use vento_core::i18n::with_language;
        let app = PlatformError::NotInstalled("org.gnome.Terminal".into());
        let none = PlatformError::NoTerminal;
        assert_eq!(app.to_string(), "org.gnome.Terminal is not installed");
        assert_eq!(none.to_string(), "no terminal is installed");
        with_language("ar", || {
            // The right-to-left mark first: the name starts the line.
            assert_eq!(app.to_string(), "\u{200f}org.gnome.Terminal غير مثبّت");
            assert_eq!(none.to_string(), "لا توجد طرفية مثبّتة");
        });
        // The same error read again in English: a log line stays English.
        let toast = with_language("ar", || app.to_string());
        assert_ne!(toast, with_language("en", || app.to_string()));
    }

    #[test]
    fn no_folder_to_open_is_worded_when_it_is_shown() {
        use vento_core::i18n::with_language;
        let error = PlatformError::NoFolder;
        assert_eq!(error.to_string(), "no folder to open");
        // Made in Arabic, read again in English for the log line.
        with_language("ar", || assert_eq!(error.to_string(), "لا يوجد مجلد لفتحه"));
        assert_eq!(
            with_language("en", || error.to_string()),
            "no folder to open"
        );
    }

    #[test]
    fn the_sentences_of_a_failed_open_are_worded_when_shown() {
        use vento_core::i18n::with_language;
        let names = PlatformError::NoAppOpened(vec!["a.xyz".into(), "b.xyz".into()]);
        let gone = PlatformError::AppGone("Notes".into());
        assert_eq!(names.to_string(), "no app opened a.xyz, b.xyz");
        assert_eq!(gone.to_string(), "Notes is no longer there");
        with_language("ar", || {
            assert_eq!(names.to_string(), "لا يوجد تطبيق يفتح a.xyz، b.xyz");
            // The right-to-left mark first: the name starts the line.
            assert_eq!(gone.to_string(), "\u{200f}Notes لم يعد موجوداً");
        });
        // Made in Arabic, read again in English for the log line.
        assert_eq!(
            with_language("ar", || with_language("en", || gone.to_string())),
            "Notes is no longer there"
        );
    }

    #[test]
    fn a_clipboard_that_rejects_is_worded_when_shown() {
        use vento_core::i18n::with_language;
        let text = PlatformError::ClipboardRejectedText;
        let files = PlatformError::ClipboardRejectedFiles;
        assert_eq!(text.to_string(), "clipboard rejected the text");
        assert_eq!(files.to_string(), "clipboard rejected the files");
        with_language("ar", || {
            assert_eq!(text.to_string(), "رفضت الحافظة النص");
            assert_eq!(files.to_string(), "رفضت الحافظة الملفات");
        });
    }

    #[test]
    fn a_terminal_that_fails_is_worded_when_shown() {
        use std::io::{Error, ErrorKind};
        use vento_core::i18n::with_language;
        let run = PlatformError::CouldNotRun {
            program: "/usr/bin/term".into(),
            error: Error::from(ErrorKind::NotFound),
        };
        let exited = PlatformError::ProgramFailed {
            program: "/usr/bin/term".into(),
            code: Some(2),
        };
        let signalled = PlatformError::ProgramFailed {
            program: "/usr/bin/term".into(),
            code: None,
        };
        let waited = PlatformError::ProgramUnwaited {
            program: "/usr/bin/term".into(),
            error: Error::from(ErrorKind::PermissionDenied),
        };
        assert_eq!(run.to_string(), "could not run /usr/bin/term: not found");
        assert_eq!(exited.to_string(), "/usr/bin/term exited with code 2");
        assert_eq!(
            signalled.to_string(),
            "/usr/bin/term was stopped by a signal"
        );
        assert_eq!(waited.to_string(), "/usr/bin/term: permission denied");
        with_language("ar", || {
            assert_eq!(run.to_string(), "تعذّر تشغيل /usr/bin/term: غير موجود");
            assert_eq!(
                exited.to_string(),
                "\u{200f}/usr/bin/term انتهى برمز الخروج 2"
            );
            assert_eq!(signalled.to_string(), "\u{200f}/usr/bin/term أوقفته إشارة");
            assert_eq!(waited.to_string(), "\u{200f}/usr/bin/term: لا توجد صلاحية");
        });
        assert_eq!(
            with_language("ar", || with_language("en", || run.to_string())),
            "could not run /usr/bin/term: not found"
        );
    }
}
