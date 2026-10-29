//! macOS [`Platform`] built on AppKit, with no Accessibility or Input
//! Monitoring permission needed.

use std::cell::{OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSMenu,
    NSPasteboard, NSRunningApplication, NSWorkspace,
};
use vento_core::clip::ClipSnapshot;
use vento_core::geometry::{Point, Rect};
use vento_core::share::{ShareOutcome, ShareTarget};
use vento_core::trigger::InputSample;

use crate::{
    AppInfo, AwakeHold, DropCatcher, IslandHome, MenuEntry, Platform, PlatformError, RgbaImage,
    TerminalApp,
};

mod apps;
mod awake;
mod catcher;
mod clicks;
mod clipboard;
mod icons;
mod image_io;
mod info;
mod input;
mod menu;
mod notify;
mod overlay;
mod patch;
mod pdf;
mod quarantine;
mod quit;
mod screen_text;
mod screens;
mod select;
mod share;
mod terminal;
mod thumbnail;
mod trash;

pub(crate) use icons::file_icon_at;
pub(crate) use image_io::{probe_image, write_image};
pub(crate) use info::{block_cloud_downloads_on_this_thread, is_package, volume_scope};
pub use menu::set_ns_menu_direction;
pub(crate) use pdf::pick_pdfs;
pub use pdf::{PdfRenderer, SystemPdf};
pub(crate) use quarantine::{quarantine_of, set_quarantine};
pub use screen_text::MacScreenText;
pub(crate) use screen_text::{read_screen_text, screen_text_languages};
pub(crate) use thumbnail::{picture_data_thumbnail, picture_thumbnail};

/// AppKit-backed [`Platform`].
pub struct MacPlatform {
    drag: input::DragPasteboardCache,
    catcher: OnceCell<Rc<catcher::MacDropCatcher>>,
    /// Listens for screen changes and termination while vento runs.
    _observer: Retained<notify::AppObserver>,
    /// The app that was in front when vento last brought itself forward,
    /// given focus back by [`Platform::deactivate_app`], or when a share
    /// ends. Shared with the closure that waits for that.
    previous_app: Rc<RefCell<Option<Retained<NSRunningApplication>>>>,
    /// The menu bar taken away by [`Platform::suspend_app_menu`]: `Some`
    /// while it is away, holding the menu to put back (`None` if there was
    /// none).
    suspended_menu: RefCell<Option<Option<Retained<NSMenu>>>>,
    /// The watch [`Platform::watch_outside_clicks`] runs, while it runs.
    outside_clicks: RefCell<Option<clicks::ClickMonitor>>,
    /// Keep Awake's power assertion.
    awake: awake::Assertion,
    /// The share in flight, see [`Platform::share`].
    share: share::ShareState,
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
            previous_app: Rc::new(RefCell::new(None)),
            suspended_menu: RefCell::new(None),
            outside_clicks: RefCell::new(None),
            awake: awake::Assertion::default(),
            share: share::ShareState::default(),
        }
    }
}

impl Platform for MacPlatform {
    /// An IOKit power assertion, as Amphetamine does.
    fn keep_awake(&self, hold: Option<AwakeHold>) -> Result<(), PlatformError> {
        self.awake.set(hold)
    }

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

    fn drag_paths(&self) -> Vec<PathBuf> {
        input::drag_paths()
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

    fn clipboard_types(&self) -> Vec<String> {
        if MainThreadMarker::new().is_none() {
            log::warn!("clipboard_types called off the main thread; ignored");
            return Vec::new();
        }
        clipboard::types_of(&NSPasteboard::generalPasteboard())
    }

    fn clipboard_change_count(&self) -> i64 {
        NSPasteboard::generalPasteboard().changeCount() as i64
    }

    fn read_clipboard(&self) -> ClipSnapshot {
        if MainThreadMarker::new().is_none() {
            log::warn!("read_clipboard called off the main thread; ignored");
            return ClipSnapshot::default();
        }
        clipboard::read_from(&NSPasteboard::generalPasteboard())
    }

    fn write_clipboard(&self, snapshot: &ClipSnapshot) -> Result<i64, PlatformError> {
        if MainThreadMarker::new().is_none() {
            log::warn!("write_clipboard called off the main thread; ignored");
            return Err(PlatformError::Failed("not on the main thread".into()));
        }
        clipboard::write_to(&NSPasteboard::generalPasteboard(), snapshot)
    }

    fn resolve_file_reference(&self, path: &Path) -> Option<PathBuf> {
        clipboard::resolve_file_reference(path)
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

    fn keep_window_frame(
        &self,
        window: raw_window_handle::RawWindowHandle,
    ) -> Result<(), PlatformError> {
        let window = patch::window_of(window, "keep_window_frame")?;
        patch::keep_frame(&window)
    }

    fn file_icon(&self, path: &Path, px: u32) -> Option<RgbaImage> {
        icons::file_icon(path, px)
    }

    fn kind_description(&self, path: &Path) -> Option<String> {
        info::kind_description(path)
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

    fn on_quit_request(&self, f: Box<dyn Fn()>) {
        quit::on_quit_request(f);
    }

    fn activate_app(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("activate_app called off the main thread; ignored");
            return;
        };
        // Remember who had focus. When that is vento itself (already in
        // front), whatever was remembered stays; `deactivate_app` always
        // takes it, so nothing is handed focus twice.
        let me = NSRunningApplication::currentApplication().processIdentifier();
        if let Some(front) = NSWorkspace::sharedWorkspace().frontmostApplication()
            && front.processIdentifier() != me
        {
            *self.previous_app.borrow_mut() = Some(front);
        }
        screens::activate_app(mtm);
    }

    fn deactivate_app(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("deactivate_app called off the main thread; ignored");
            return;
        };
        hand_focus_back(mtm, &self.previous_app);
    }

    fn suspend_app_menu(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("suspend_app_menu called off the main thread; ignored");
            return;
        };
        let mut suspended = self.suspended_menu.borrow_mut();
        if suspended.is_some() {
            return;
        }
        let app = NSApplication::sharedApplication(mtm);
        *suspended = Some(app.mainMenu());
        app.setMainMenu(None);
        log::debug!("app menu suspended");
    }

    fn restore_app_menu(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("restore_app_menu called off the main thread; ignored");
            return;
        };
        let Some(menu) = self.suspended_menu.borrow_mut().take() else {
            return;
        };
        NSApplication::sharedApplication(mtm).setMainMenu(menu.as_deref());
        log::debug!("app menu restored");
    }

    fn watch_outside_clicks(&self, f: Box<dyn Fn()>) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("watch_outside_clicks called off the main thread; ignored");
            return;
        };
        // The old monitor goes first, so two never run at once.
        self.stop_watching_outside_clicks();
        let monitor = clicks::ClickMonitor::install(mtm, f);
        *self.outside_clicks.borrow_mut() = monitor;
    }

    fn stop_watching_outside_clicks(&self) {
        if MainThreadMarker::new().is_none() {
            log::warn!("stop_watching_outside_clicks called off the main thread; ignored");
            return;
        }
        // Taken out before it is dropped (which removes it), so nothing is
        // borrowed while AppKit lets go of the handler.
        let monitor = self.outside_clicks.borrow_mut().take();
        drop(monitor);
    }

    fn scale_factor_at(&self, p: Point) -> f64 {
        screens::scale_factor_at(p)
    }

    fn apps_for(&self, path: &Path) -> Vec<AppInfo> {
        apps::apps_for(path)
    }

    fn open_paths(&self, paths: &[PathBuf]) -> Result<(), PlatformError> {
        if MainThreadMarker::new().is_none() {
            log::warn!("open_paths called off the main thread; ignored");
            return Err(PlatformError::Failed("not on the main thread".into()));
        }
        apps::open_paths(paths)
    }

    fn open_with(
        &self,
        paths: &[PathBuf],
        app: &Path,
        on_error: Box<dyn Fn(PlatformError) + Send>,
    ) -> Result<(), PlatformError> {
        if MainThreadMarker::new().is_none() {
            log::warn!("open_with called off the main thread; ignored");
            return Err(PlatformError::Failed("not on the main thread".into()));
        }
        apps::open_with(paths, app, on_error)
    }

    fn reveal_in_finder(&self, paths: &[PathBuf]) {
        if MainThreadMarker::new().is_none() {
            log::warn!("reveal_in_finder called off the main thread; ignored");
            return;
        }
        apps::reveal(paths);
    }

    fn context_menu(
        &self,
        window: raw_window_handle::RawWindowHandle,
        at: Point,
        entries: &[MenuEntry],
    ) -> Option<u32> {
        menu::context_menu(window, at, entries)
    }

    fn share_targets(&self) -> Vec<ShareTarget> {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("share_targets called off the main thread; ignored");
            return Vec::new();
        };
        share::targets(mtm)
    }

    fn can_share(&self, target: ShareTarget, paths: &[PathBuf]) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("can_share called off the main thread; ignored");
            return false;
        };
        share::can_share(target, paths, mtm)
    }

    /// AirDrop's, Messages' and Notes' windows bring vento to the front by
    /// themselves and leave it there, with no window, once they close; Mail
    /// takes the front itself. So vento activates first, which remembers
    /// who had focus, and hands it back when the outcome arrives.
    fn share(
        &self,
        target: ShareTarget,
        paths: &[PathBuf],
        on_done: Box<dyn Fn(ShareOutcome)>,
    ) -> Result<(), PlatformError> {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("share called off the main thread; ignored");
            return Err(PlatformError::Failed("not on the main thread".into()));
        };
        let previous_app = Rc::clone(&self.previous_app);
        let on_done: Box<dyn Fn(ShareOutcome)> = Box::new(move |outcome| {
            hand_focus_back(mtm, &previous_app);
            on_done(outcome);
        });
        share::share(
            &self.share,
            target,
            paths,
            on_done,
            || self.activate_app(),
            mtm,
        )
    }

    fn share_picker(&self, paths: &[PathBuf], at: Point) -> Result<(), PlatformError> {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("share_picker called off the main thread; ignored");
            return Err(PlatformError::Failed("not on the main thread".into()));
        };
        share::picker(&self.share, paths, at, mtm)
    }
}

/// Hands focus back to the app in `previous_app`, taking it, as
/// [`Platform::deactivate_app`] does: nothing happens when another app
/// already took focus.
fn hand_focus_back(
    mtm: MainThreadMarker,
    previous_app: &RefCell<Option<Retained<NSRunningApplication>>>,
) {
    let app = NSApplication::sharedApplication(mtm);
    let previous = previous_app.borrow_mut().take();
    // The user already went to another app (clicked outside): leave
    // focus where it is.
    if !app.isActive() {
        return;
    }
    // Not `hide`: that would also hide the Pocket island and keep the
    // wheel from showing until vento is next activated.
    match previous {
        Some(previous) if !previous.isTerminated() => {
            if !previous.activateWithOptions(NSApplicationActivationOptions::empty()) {
                app.deactivate();
            }
        }
        _ => app.deactivate(),
    }
}

/// Helpers shared by the tests of several modules.
#[cfg(test)]
pub(super) mod test_support {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2_app_kit::NSPasteboard;

    /// A private pasteboard, released when dropped.
    pub struct TempPasteboard(pub Retained<NSPasteboard>);

    impl TempPasteboard {
        pub fn new() -> Self {
            let pb = NSPasteboard::pasteboardWithUniqueName();
            pb.clearContents();
            Self(pb)
        }
    }

    impl Drop for TempPasteboard {
        fn drop(&mut self) {
            // SAFETY: `releaseGlobally` takes no arguments and returns void;
            // objc2 does not expose it.
            let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
        }
    }
}
