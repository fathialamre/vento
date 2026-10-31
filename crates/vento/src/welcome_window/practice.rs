//! The Try it screen: the sample file, its drag, and the drop on the wheel
//! that is only practice.
//!
//! The sample is a small text file in `<data folder>/welcome/`, written when
//! the screen shows (and again when the tile is pressed, should it have gone
//! missing); the folder goes when the window closes. Dragging the tile starts
//! a real file drag, as the Pocket's tiles do, so the real wheel opens over
//! it. A drop of the sample alone, on any action or ring cell, is caught
//! before it reaches the action: nothing happens to the file.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use log::{debug, info, warn};
use slint::ComponentHandle;
use slint::platform::WindowEvent;
use slint::winit_030::WinitWindowAccessor;
use vento_core::geometry::{Point, Rect};
use vento_core::i18n::tr;
use vento_core::welcome;

use super::WelcomeController;
#[cfg(target_os = "linux")]
use crate::drag;
use crate::pocket::out::{Press, drag_end_events, drop_point, placeholder_icon, preview_png};
use crate::pocket::to_image;
use crate::ui::WelcomeWindow;
use crate::wheel::WheelDrop;

/// The Try it screen.
pub(super) const TRY_IT_STEP: i32 = 2;

/// The sample folder's name in the data folder. Only a folder of this name
/// is ever removed.
const SAMPLE_DIR: &str = "welcome";

/// The sample's icon, on the tile and as the drag's image, in points.
const ICON_PX: u32 = 40;

thread_local! {
    /// The controller on the main thread, for the end of a drag, which the
    /// drag crate reports through a `Send` callback.
    static MAIN_WELCOME: RefCell<Weak<WelcomeController>> = const { RefCell::new(Weak::new()) };
}

/// Makes `controller` the one the end of a sample drag reaches.
pub(super) fn register(controller: &Rc<WelcomeController>) {
    MAIN_WELCOME.with(|slot| *slot.borrow_mut() = Rc::downgrade(controller));
}

/// The folder the sample is written to; `None` without a data folder.
pub(super) fn sample_dir() -> Option<PathBuf> {
    welcome::data_dir().map(|d| d.join(SAMPLE_DIR))
}

/// What the sample file says, in the interface language.
fn sample_text() -> String {
    format!("{}\n", tr("Vento was here."))
}

/// Whether a wheel drop of `paths` is practice: the window is `open` and
/// the drop is of the sample alone. The action and any ring cell dropped on
/// play no part.
fn is_practice_drop(open: bool, sample: Option<&Path>, paths: &[PathBuf]) -> bool {
    open && sample.is_some_and(|s| matches!(paths, [only] if only == s))
}

/// Writes `text` to `dir/name` unless that file exists (its contents are
/// then left alone), creating `dir` as needed. Returns the file's path.
///
/// # Errors
/// If the folder or the file cannot be made.
fn ensure_sample(dir: &Path, name: &str, text: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => io::Write::write_all(&mut file, text.as_bytes())?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Ok(path)
}

/// Removes the sample folder `dir` and all it holds. A folder not named
/// [`SAMPLE_DIR`] is refused, so a wrong path never costs the user files.
/// Errors are logged.
pub(super) fn remove_samples(dir: &Path) {
    if dir.file_name().is_none_or(|n| n != SAMPLE_DIR) {
        warn!("not removing {}: not the sample folder", dir.display());
        return;
    }
    match fs::remove_dir_all(dir) {
        Ok(()) => debug!("sample folder removed"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => warn!("cannot remove the sample folder: {e}"),
    }
}

/// The window's content area, in global logical coordinates.
fn content_rect(window: &WelcomeWindow) -> Option<Rect> {
    window
        .window()
        .with_winit_window(|w| {
            let scale = w.scale_factor();
            let pos = w.inner_position().ok()?.to_logical::<f64>(scale);
            let size = w.inner_size().to_logical::<f64>(scale);
            Some(Rect {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
            })
        })
        .flatten()
}

impl WelcomeController {
    /// The sample file's path: `<data folder>/welcome/` and its name in the
    /// interface language. `None` without a data folder.
    pub fn sample_path(&self) -> Option<PathBuf> {
        sample_dir().map(|d| d.join(tr("Try me.txt")))
    }

    /// Takes a wheel drop of the sample alone while the window is open:
    /// the screen says it worked and the drop goes no further (`true`).
    /// Any other drop is left for its action (`false`).
    pub fn catch_practice_drop(&self, drop: &WheelDrop) -> bool {
        let sample = self.sample_path();
        if !is_practice_drop(self.is_open(), sample.as_deref(), &drop.paths) {
            return false;
        }
        info!("practice drop on {:?} caught", drop.action);
        if let Some(window) = self.existing() {
            window.set_practiced(true);
        }
        true
    }

    /// Writes the sample if it is missing and shows it on the tile, by name
    /// and Finder icon. Returns its path; `None` (logged) when it can't be
    /// written.
    pub(super) fn show_sample(&self) -> Option<PathBuf> {
        let window = self.existing()?;
        let dir = sample_dir()?;
        let path = match ensure_sample(&dir, &tr("Try me.txt"), &sample_text()) {
            Ok(path) => path,
            Err(e) => {
                warn!("cannot write the sample file: {e}");
                return None;
            }
        };
        window.set_sample_name(tr("Try me.txt").into());
        let icon = self
            .platform
            .file_icon(&path, ICON_PX)
            .as_ref()
            .and_then(to_image);
        // Without one, the tile shows its own plain icon.
        window.set_sample_icon(icon.unwrap_or_default());
        Some(path)
    }

    /// The tile was pressed: the sample is written again if it went
    /// missing, and the press may become a drag.
    pub(super) fn sample_pressed(&self) {
        if self.show_sample().is_some() {
            self.latch.press(Press::All);
        }
    }

    /// The press moved: starts its one drag once it has moved far enough.
    pub(super) fn sample_moved(&self, dx: f32, dy: f32) {
        if self.latch.moved(dx, dy).is_some() {
            self.start_sample_drag();
        }
    }

    /// The press ended; a click on the tile does nothing.
    pub(super) fn sample_released(&self) {
        self.latch.release();
    }

    /// Starts a drag session with the sample, its icon as the image. The
    /// press latch is already marked running.
    fn start_sample_drag(&self) {
        let (Some(window), Some(path)) = (self.existing(), self.sample_path()) else {
            self.latch.ended();
            return;
        };
        let icon = self
            .platform
            .file_icon(&path, ICON_PX)
            .unwrap_or_else(|| placeholder_icon(ICON_PX));
        let Some(preview) = preview_png(&icon, ICON_PX)
            .or_else(|| preview_png(&placeholder_icon(ICON_PX), ICON_PX))
        else {
            self.latch.ended();
            warn!("cannot make a drag preview");
            return;
        };
        debug!("sample drag start");
        let result = drag::start_drag(
            &window.window().window_handle(),
            drag::DragItem::Files(vec![path]),
            drag::Image::Raw(preview),
            move |result, pos| {
                // Called on the main thread, but the callback must be Send:
                // reach the controller through the event loop.
                let at = drop_point(&pos);
                let delivered = slint::invoke_from_event_loop(move || {
                    if let Some(c) = MAIN_WELCOME.with(|slot| slot.borrow().upgrade()) {
                        debug!("sample drag end: {result:?}");
                        c.sample_drag_ended(at);
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report the end of the sample drag: {e}");
                }
            },
            drag::Options::default(),
        );
        if let Err(e) = result {
            warn!("cannot start the sample drag: {e}");
            self.latch.ended();
        }
    }

    /// The drag session ended at `at`: it took the mouse, so Slint never saw
    /// the press's release and would keep the tile pressed. The window is
    /// told of it now, and that the pointer left unless it is over the
    /// window.
    fn sample_drag_ended(&self, at: Point) {
        self.latch.ended();
        let Some(window) = self.existing() else {
            return;
        };
        let rect = content_rect(&window).unwrap_or(Rect {
            x: at.x,
            y: at.y,
            w: 0.0,
            h: 0.0,
        });
        let over =
            at.x >= rect.x && at.x <= rect.x + rect.w && at.y >= rect.y && at.y <= rect.y + rect.h;
        for event in drag_end_events(&rect, at, over) {
            dispatch(&window, event);
        }
    }
}

fn dispatch(window: &WelcomeWindow, event: WindowEvent) {
    if let Err(e) = window.window().dispatch_event_with_result(event) {
        warn!("cannot pass a pointer event to the welcome window: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::actions::ActionId;
    use vento_core::convert::ImageFormat;
    use vento_core::geometry::Point;
    use vento_core::share::ShareTarget;

    use crate::wheel::WheelDrop;

    fn sample() -> PathBuf {
        PathBuf::from("/Users/me/Library/Application Support/vento/welcome/Try me.txt")
    }

    fn drop_of(action: ActionId, paths: Vec<PathBuf>) -> WheelDrop {
        WheelDrop {
            action,
            paths,
            at: Point { x: 0.0, y: 0.0 },
            dest: None,
            format: None,
            target: None,
        }
    }

    #[test]
    fn practice_drop_is_caught() {
        let s = sample();
        assert!(is_practice_drop(true, Some(&s), std::slice::from_ref(&s)));
    }

    #[test]
    fn practice_drop_on_ring_cell_is_caught() {
        let s = sample();
        let on_folder = WheelDrop {
            dest: Some(PathBuf::from("/Users/me/Desktop")),
            ..drop_of(ActionId::Move, vec![s.clone()])
        };
        let on_format = WheelDrop {
            format: Some(ImageFormat::Png),
            ..drop_of(ActionId::Convert, vec![s.clone()])
        };
        let on_target = WheelDrop {
            target: Some(ShareTarget::Mail),
            ..drop_of(ActionId::Share, vec![s.clone()])
        };
        for drop in [on_folder, on_format, on_target] {
            assert!(is_practice_drop(true, Some(&s), &drop.paths), "{drop:?}");
        }
    }

    #[test]
    fn drop_with_more_than_the_sample_runs_as_usual() {
        let s = sample();
        let other = PathBuf::from("/Users/me/Desktop/report.pdf");
        assert!(!is_practice_drop(
            true,
            Some(&s),
            &[s.clone(), other.clone()]
        ));
        assert!(!is_practice_drop(true, Some(&s), &[other]));
        assert!(!is_practice_drop(true, Some(&s), &[]));
        // Without a sample (no data folder), nothing is practice.
        assert!(!is_practice_drop(true, None, &[s]));
    }

    #[test]
    fn closed_window_catches_nothing() {
        let s = sample();
        assert!(!is_practice_drop(false, Some(&s), std::slice::from_ref(&s)));
    }

    #[test]
    fn ensure_sample_writes_once() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("vento").join("welcome");
        let path = ensure_sample(&dir, "Try me.txt", "Vento was here.\n").unwrap();
        assert_eq!(path, dir.join("Try me.txt"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "Vento was here.\n");
        fs::write(&path, "changed").unwrap();
        assert_eq!(
            ensure_sample(&dir, "Try me.txt", "Vento was here.\n").unwrap(),
            path
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "changed");
    }

    #[test]
    fn ensure_sample_rewrites_a_missing_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("welcome");
        let path = ensure_sample(&dir, "Try me.txt", "Vento was here.\n").unwrap();
        // Dragged to Finder and moved away, say.
        fs::remove_file(&path).unwrap();
        ensure_sample(&dir, "Try me.txt", "Vento was here.\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "Vento was here.\n");
    }

    #[test]
    fn remove_samples_removes_only_welcome() {
        let root = tempfile::tempdir().unwrap();
        let welcome = root.path().join("welcome");
        ensure_sample(&welcome, "Try me.txt", "x").unwrap();
        remove_samples(&welcome);
        assert!(!welcome.exists());
        // Removing it again (already gone) is no error.
        remove_samples(&welcome);
        // Any other folder passed by mistake stays, with what it holds.
        let pocket = root.path().join("pocket");
        let kept = ensure_sample(&pocket, "item.txt", "keep").unwrap();
        remove_samples(&pocket);
        assert_eq!(fs::read_to_string(kept).unwrap(), "keep");
    }

    #[test]
    fn the_sample_text_ends_with_a_newline() {
        assert_eq!(sample_text(), "Vento was here.\n");
    }
}
