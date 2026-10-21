//! The Info action's window: dropping files on the wheel's Info cell shows
//! their size, counted live on a worker thread, with the details of a single
//! item and the biggest things inside a folder.
//!
//! The window is created on the first Info drop and only hidden afterwards;
//! a closed window runs no count and watches no clicks. It takes the
//! keyboard while open (for Esc) and closes on Done, Esc, Show in Finder or
//! a click in another app.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use log::{debug, warn};
use slint::winit_030::winit::event::{ElementState, WindowEvent};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{CloseRequestResponse, ComponentHandle, LogicalPosition, ModelRc, Timer, VecModel};
use vento_core::geometry::{Point, Rect};
use vento_core::info::{Facts, Subject, ViewContext, subject_of, view};
use vento_core::sizes::{self, CountOptions, Progress, Tally, count};
use vento_platform::Platform;

use crate::panel::set_rows;
use crate::pocket::to_image;
use crate::toast::{Toaster, position_below};
use crate::ui::{InfoBar, InfoDetail, InfoWindow};

/// The least time between two updates of the window while counting.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Window width in logical points (matches `info.slint`).
const WIDTH: f64 = 300.0;
/// Size of the item's Finder icon, in points (the platform multiplies by the
/// backing scale; it is shown at 32 pt).
const ICON_PT: u32 = 32;

thread_local! {
    /// The controller, for progress sent from the counting thread. Set by
    /// [`InfoController::new`] on the main thread.
    static MAIN_INFO: RefCell<Weak<InfoController>> = const { RefCell::new(Weak::new()) };
}

/// What the open window is about, fixed when it opens.
struct Shown {
    paths: Vec<PathBuf>,
    subject: Subject,
    facts: Facts,
    ctx: ViewContext,
    at: Point,
}

/// Opens, fills and closes the Info window.
pub struct InfoController {
    platform: Rc<dyn Platform>,
    toaster: Rc<Toaster>,
    /// Created on the first open.
    window: RefCell<Option<InfoWindow>>,
    details: Rc<VecModel<InfoDetail>>,
    biggest: Rc<VecModel<InfoBar>>,
    /// Counts opens; progress from an older count is dropped.
    generation: Cell<u64>,
    /// Stops the running count.
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    shown: RefCell<Option<Shown>>,
    /// The window height it was last placed with.
    placed_height: Cell<f64>,
    open: Cell<bool>,
    before_open: RefCell<Option<Box<dyn Fn()>>>,
    this: Weak<Self>,
}

impl InfoController {
    /// Creates the controller; the window itself waits for the first open.
    pub fn new(platform: Rc<dyn Platform>, toaster: Rc<Toaster>) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            toaster,
            window: RefCell::new(None),
            details: Rc::new(VecModel::default()),
            biggest: Rc::new(VecModel::default()),
            generation: Cell::new(0),
            cancel: RefCell::new(None),
            shown: RefCell::new(None),
            placed_height: Cell::new(0.0),
            open: Cell::new(false),
            before_open: RefCell::new(None),
            this: this.clone(),
        });
        MAIN_INFO.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        controller
    }

    /// Runs before each open; main sets it to close the settings panel.
    pub fn set_before_open(&self, f: Box<dyn Fn()>) {
        *self.before_open.borrow_mut() = Some(f);
    }

    /// Shows the window for `paths` under `at` (the wheel's centre, global
    /// logical points) and starts counting them. Paths gone since the drop
    /// are left out; when none is left, a toast says so instead. An open
    /// window takes the new items and its old count stops.
    pub fn open(&self, paths: Vec<PathBuf>, at: Point) {
        let several = paths.len() > 1;
        let paths = existing_paths(sizes::outermost(&paths));
        if paths.is_empty() {
            self.close();
            let text = if several {
                "Info: items not found"
            } else {
                "Info: item not found"
            };
            self.toaster.show(text, None, at);
            return;
        }
        if let Some(before_open) = self.before_open.borrow().as_ref() {
            before_open();
        }
        self.stop_count();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);

        let subject = subject_of(&paths, &vento_platform::is_package);
        let window = self.window();
        match paths.as_slice() {
            [only] => {
                let icon = self
                    .platform
                    .file_icon(only, ICON_PT)
                    .and_then(|icon| to_image(&icon));
                window.set_has_icon(icon.is_some());
                if let Some(icon) = icon {
                    window.set_item_icon(icon);
                }
            }
            _ => window.set_has_icon(false),
        }
        let facts = match paths.as_slice() {
            [only] => self.facts_of(only),
            _ => Facts::default(),
        };
        *self.shown.borrow_mut() = Some(Shown {
            paths: paths.clone(),
            subject,
            facts,
            ctx: ViewContext::local(),
            at,
        });
        self.placed_height.set(0.0);
        self.show_progress(&first_progress(subject, &paths));

        let was_open = self.open.replace(true);
        // vento runs as an accessory app: bring it forward so the window
        // becomes key and takes Esc.
        self.platform.activate_app();
        if let Err(e) = window.show() {
            warn!("cannot show the info window: {e}");
            self.open.set(false);
            self.shown.borrow_mut().take();
            if was_open {
                self.platform.stop_watching_outside_clicks();
            }
            self.platform.deactivate_app();
            return;
        }
        if !was_open {
            self.watch_outside_clicks();
        }
        self.start_count(paths, generation);
        debug!("info open for {subject:?}");
    }

    /// Hides the window, stops the count and hands focus back to the app
    /// that had it (on the next turn of the event loop, as the settings
    /// panel does). For Done, Esc and a click outside.
    pub fn close(&self) {
        self.close_with(true);
    }

    /// Hides the window and stops the count without handing focus back:
    /// another vento window takes it.
    pub fn dismiss(&self) {
        self.close_with(false);
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    fn close_with(&self, hand_back: bool) {
        if !self.open.replace(false) {
            return;
        }
        self.stop_count();
        self.platform.stop_watching_outside_clicks();
        self.shown.borrow_mut().take();
        if let Some(window) = self.window.borrow().as_ref()
            && let Err(e) = window.hide()
        {
            warn!("cannot hide the info window: {e}");
        }
        if hand_back {
            let this = self.this.clone();
            Timer::single_shot(Duration::ZERO, move || {
                // Reopened in the meantime: vento keeps the focus.
                if let Some(info) = this.upgrade()
                    && !info.is_open()
                {
                    info.platform.deactivate_app();
                }
            });
        }
        debug!("info closed");
    }

    /// The window, created and connected on first use.
    fn window(&self) -> InfoWindow {
        if let Some(window) = self.window.borrow().as_ref() {
            return window.clone_strong();
        }
        let window = InfoWindow::new().expect("cannot create the info window");
        window.set_details(ModelRc::from(self.details.clone()));
        window.set_biggest(ModelRc::from(self.biggest.clone()));
        self.connect(&window);
        *self.window.borrow_mut() = Some(window.clone_strong());
        window
    }

    fn connect(&self, window: &InfoWindow) {
        let this = self.this.clone();
        window.on_close_requested(move || {
            if let Some(info) = this.upgrade() {
                info.close();
            }
        });
        let this = self.this.clone();
        window.on_reveal_clicked(move || {
            if let Some(info) = this.upgrade() {
                info.reveal();
            }
        });
        // Closing the window any other way (⌘W) is a Done too.
        let this = self.this.clone();
        window.window().on_close_requested(move || {
            if let Some(info) = this.upgrade() {
                info.close();
            }
            CloseRequestResponse::KeepWindowShown
        });
        let this = self.this.clone();
        window.window().on_winit_window_event(move |_, event| {
            let Some(info) = this.upgrade() else {
                return EventResult::Propagate;
            };
            // Back from another app: the click goes on to the button under
            // it, and the window is key again for Esc.
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } = event
                && !info.has_focus()
            {
                info.regain_focus();
            }
            EventResult::Propagate
        });
    }

    /// Shows the items in Finder and closes. Finder comes forward, so the
    /// focus is not handed back to the app that had it before.
    fn reveal(&self) {
        let paths = self.shown.borrow().as_ref().map(|s| s.paths.clone());
        if let Some(paths) = paths {
            self.platform.reveal_in_finder(&paths);
        }
        self.dismiss();
    }

    /// Kind and dates of a single item, read without counting it.
    fn facts_of(&self, path: &Path) -> Facts {
        let metadata = std::fs::symlink_metadata(path).ok();
        Facts {
            kind: self.platform.kind_description(path),
            created: metadata.as_ref().and_then(|m| m.created().ok()),
            modified: metadata.as_ref().and_then(|m| m.modified().ok()),
        }
    }

    /// Counts `paths` on a worker thread; its progress comes back through
    /// [`deliver`].
    fn start_count(&self, paths: Vec<PathBuf>, generation: u64) {
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.borrow_mut() = Some(cancel.clone());
        let spawned = std::thread::Builder::new()
            .name("vento-info".into())
            .spawn(move || {
                // Before anything on this thread touches the items: reading
                // them must never download a cloud file.
                if let Err(e) = vento_platform::block_cloud_downloads_on_this_thread() {
                    warn!("cannot keep the info count from downloading cloud files: {e}");
                }
                let Some(first) = paths.first() else { return };
                let opts = CountOptions {
                    is_package: Arc::new(vento_platform::is_package),
                    scope: vento_platform::volume_scope(first),
                    cancel,
                    interval: PROGRESS_INTERVAL,
                };
                let result = count(&paths, &opts, &mut |progress| {
                    deliver(generation, progress.clone());
                });
                if result.is_none() {
                    debug!("info count {generation} cancelled");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start the info count: {e}");
            let at = self.shown.borrow().as_ref().map(|s| s.at);
            self.close();
            if let Some(at) = at {
                self.toaster.show("Info: cannot count", None, at);
            }
        }
    }

    /// Stops the running count, if any.
    fn stop_count(&self) {
        if let Some(cancel) = self.cancel.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Progress of count `generation`, on the main thread.
    fn on_progress(&self, generation: u64, progress: &Progress) {
        if is_current(generation, self.generation.get(), self.is_open()) {
            self.show_progress(progress);
        }
    }

    /// Fills the window from the count so far and places it under the drop
    /// point again when its height changed.
    fn show_progress(&self, progress: &Progress) {
        let shown = self.shown.borrow();
        let Some(shown) = shown.as_ref() else { return };
        let Some(window) = self.window.borrow().as_ref().map(|w| w.clone_strong()) else {
            return;
        };
        let content = view(
            &shown.paths,
            shown.subject,
            &shown.facts,
            progress,
            &shown.ctx,
        );
        window.set_item_name(content.title.into());
        window.set_size(content.size.into());
        window.set_status(content.status.into());
        window.set_note(content.note.into());
        window.set_empty_folder(content.empty_folder);
        let details = content
            .details
            .into_iter()
            .map(|(label, value)| InfoDetail {
                label: label.into(),
                value: value.into(),
            })
            .collect();
        set_rows(&self.details, details);
        let bars = content
            .biggest
            .into_iter()
            .map(|bar| InfoBar {
                name: bar.name.into(),
                size: bar.size.into(),
                share: bar.share,
            })
            .collect();
        set_rows(&self.biggest, bars);

        let height = f64::from(window.get_window_height());
        if height != self.placed_height.get() {
            self.placed_height.set(height);
            let (x, y) = position_below(shown.at, (WIDTH, height), &self.platform.screens());
            window
                .window()
                .set_position(LogicalPosition::new(x as f32, y as f32));
        }
    }

    /// Closes the window on a click in another app or on the desktop, until
    /// it closes. The close happens on the next turn of the event loop,
    /// outside AppKit's event monitor that reports the click.
    fn watch_outside_clicks(&self) {
        let this = self.this.clone();
        self.platform.watch_outside_clicks(Box::new(move || {
            let this = this.clone();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(info) = this.upgrade() {
                    // AppKit can report a click on the window itself (while
                    // vento is not the active app): only close for a click
                    // that is really outside it.
                    if info.cursor_on_window() {
                        debug!("click on the info window reported as outside: ignored");
                        return;
                    }
                    debug!("click outside the info window");
                    info.close();
                }
            });
            // A timer started from AppKit's callback does not wake the
            // event loop by itself; this does, so the window closes at once.
            if let Err(e) = slint::invoke_from_event_loop(|| {}) {
                warn!("cannot wake the event loop: {e}");
            }
        }));
    }

    /// Makes the open window the key window again.
    fn regain_focus(&self) {
        if !self.is_open() {
            return;
        }
        self.platform.activate_app();
        if let Some(window) = self.window.borrow().as_ref() {
            window.window().with_winit_window(|w| w.focus_window());
        }
    }

    /// Whether the window is the key window (it takes the keyboard).
    fn has_focus(&self) -> bool {
        self.window
            .borrow()
            .as_ref()
            .and_then(|w| w.window().with_winit_window(|w| w.has_focus()))
            .unwrap_or(true)
    }

    /// Whether the mouse is over the window now, in global logical points.
    fn cursor_on_window(&self) -> bool {
        let window = self.window.borrow();
        let Some(window) = window.as_ref() else {
            return false;
        };
        let window = window.window();
        let scale = window.scale_factor();
        let at = window.position().to_logical(scale);
        let size = window.size().to_logical(scale);
        let frame = Rect {
            x: f64::from(at.x),
            y: f64::from(at.y),
            w: f64::from(size.width),
            h: f64::from(size.height),
        };
        let cursor = self.platform.sample_input().cursor;
        cursor.x >= frame.x
            && cursor.x <= frame.x + frame.w
            && cursor.y >= frame.y
            && cursor.y <= frame.y + frame.h
    }
}

/// Hands the progress of count `generation` to the controller on the main
/// thread. Called on the counting thread.
fn deliver(generation: u64, progress: Progress) {
    let result = slint::invoke_from_event_loop(move || {
        if let Some(info) = MAIN_INFO.with(|slot| slot.borrow().upgrade()) {
            info.on_progress(generation, &progress);
        }
    });
    if let Err(e) = result {
        warn!("cannot send the info count's progress: {e}");
    }
}

/// The paths that still exist (a dangling symlink does: it is the link).
fn existing_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|p| p.symlink_metadata().is_ok())
        .collect()
}

/// What the window shows before the first progress arrives. A single plain
/// file has nothing to walk, so its size (already known from its metadata)
/// is shown at once instead of a flash of "Zero bytes"; the worker's final
/// progress replaces it. Anything else starts empty and says "Calculating…".
fn first_progress(subject: Subject, paths: &[PathBuf]) -> Progress {
    if let (Subject::File, [only]) = (subject, paths)
        && let Ok(metadata) = only.symlink_metadata()
    {
        return Progress {
            tally: Tally {
                bytes: metadata.len(),
                files: 1,
                ..Tally::default()
            },
            biggest: Vec::new(),
            done: true,
        };
    }
    Progress::default()
}

/// Whether progress of count `generation` belongs in the window: it is the
/// latest count (`current`) and the window is `open`.
fn is_current(generation: u64, current: u64, open: bool) -> bool {
    open && generation == current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_paths_drops_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"x").unwrap();
        let gone = dir.path().join("gone");
        assert_eq!(existing_paths(vec![a.clone(), gone.clone()]), vec![a]);
        assert!(existing_paths(vec![gone.clone()]).is_empty());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&gone, &link).unwrap();
        assert_eq!(existing_paths(vec![link.clone()]), vec![link]);
    }

    #[test]
    fn a_plain_file_is_sized_before_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"12345").unwrap();
        let p = first_progress(Subject::File, std::slice::from_ref(&a));
        assert_eq!((p.tally.bytes, p.tally.files, p.done), (5, 1, true));
        let p = first_progress(Subject::Folder, &[dir.path().to_path_buf()]);
        assert_eq!(p, Progress::default());
        let p = first_progress(Subject::Several, &[a, dir.path().join("b")]);
        assert_eq!(p, Progress::default());
    }

    #[test]
    fn stale_generation_is_ignored() {
        assert!(is_current(3, 3, true));
        assert!(!is_current(2, 3, true));
        assert!(!is_current(3, 3, false));
    }
}
