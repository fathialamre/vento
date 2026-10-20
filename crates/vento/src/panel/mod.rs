//! The settings panel: hangs under the menu-bar icon on a left click and
//! closes on Esc, Done, a second click on the icon or a click outside.
//!
//! The window is created on the first open and only hidden afterwards; a
//! closed panel runs no timers (closing arms one single-shot timer for the
//! next turn of the event loop).

use std::cell::Cell;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use log::{debug, warn};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{CloseRequestResponse, ComponentHandle, LogicalPosition, Timer};
use vento_core::geometry::{Point, Rect, screen_for};
use vento_platform::Platform;

use crate::settings_hub::SettingsHub;
use crate::ui::SettingsPanel;

/// Panel width in logical points (matches `settings.slint`).
const WIDTH: f64 = 340.0;
/// Gap between the bottom of the menu-bar icon and the top of the panel.
const GAP: f64 = 6.0;
/// The least distance from the panel to the left, right and bottom edges
/// of the work area.
const INSET: f64 = 8.0;
/// A click on the icon this soon after the panel closed for losing focus
/// is the click that took the focus: it closed the panel and must not open
/// it again.
const REOPEN_GUARD: Duration = Duration::from_millis(500);

/// Opens, places and closes the settings panel.
pub struct PanelController {
    platform: Rc<dyn Platform>,
    hub: Rc<SettingsHub>,
    window: SettingsPanel,
    /// Pauses or resumes vento; the panel shows the result through
    /// [`PanelController::set_paused`].
    on_pause: Box<dyn Fn(bool)>,
    open: Cell<bool>,
    /// The window became key since it opened, so losing focus now means
    /// the user went elsewhere.
    had_focus: Cell<bool>,
    /// When the panel last closed for losing focus.
    blurred_at: Cell<Option<Instant>>,
    this: Weak<Self>,
}

impl PanelController {
    /// Creates the (hidden) panel. `on_pause` is called with the state the
    /// Pause item asks for.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    pub fn new(
        platform: Rc<dyn Platform>,
        hub: Rc<SettingsHub>,
        on_pause: Box<dyn Fn(bool)>,
    ) -> Rc<Self> {
        let window = SettingsPanel::new().expect("cannot create the settings panel");
        let panel = Rc::new_cyclic(|this| Self {
            platform,
            hub,
            window,
            on_pause,
            open: Cell::new(false),
            had_focus: Cell::new(false),
            blurred_at: Cell::new(None),
            this: this.clone(),
        });
        panel.connect();
        panel
    }

    fn connect(&self) {
        let this = self.this.clone();
        self.window.on_close_requested(move || {
            if let Some(panel) = this.upgrade() {
                panel.close();
            }
        });
        let this = self.this.clone();
        self.window.on_pause_clicked(move || {
            if let Some(panel) = this.upgrade() {
                (panel.on_pause)(!panel.window.get_paused());
            }
        });
        self.window.on_quit_clicked(|| {
            if let Err(e) = slint::quit_event_loop() {
                warn!("cannot quit: {e}");
            }
        });
        let this = self.this.clone();
        self.window.window().on_close_requested(move || {
            if let Some(panel) = this.upgrade() {
                panel.close();
            }
            CloseRequestResponse::KeepWindowShown
        });
        let this = self.this.clone();
        self.window.window().on_winit_window_event(move |_, event| {
            if let WindowEvent::Focused(focused) = event
                && let Some(panel) = this.upgrade()
            {
                panel.focus_changed(*focused);
            }
            EventResult::Propagate
        });
        // A saved change clears the load error: drop the notice with it.
        let this = self.this.clone();
        self.hub.on_change(Box::new(move |_, _| {
            if let Some(panel) = this.upgrade() {
                panel.show_notice();
            }
        }));
    }

    /// Opens the panel under `icon` (the menu-bar icon's frame, global
    /// logical points), or closes it when it is open.
    pub fn toggle_at(&self, icon: Rect) {
        if self.is_open() {
            self.close();
        } else if !reopen_blocked(self.blurred_at.take(), Instant::now()) {
            self.open_at(icon);
        }
    }

    /// Hides the panel and hands focus back to the app that had it.
    ///
    /// Focus is handed back on the next turn of the event loop: when the
    /// panel closes because the user clicked into another app, AppKit takes
    /// the key window away before vento stops being active, and only once
    /// that is over can `deactivate_app` see that focus is already
    /// elsewhere and leave it there.
    pub fn close(&self) {
        if !self.open.replace(false) {
            return;
        }
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the settings panel: {e}");
        }
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            // Reopened in the meantime: vento keeps the focus.
            if let Some(panel) = this.upgrade()
                && !panel.is_open()
            {
                panel.platform.deactivate_app();
            }
        });
        debug!("panel closed");
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Shows whether vento is paused.
    pub fn set_paused(&self, paused: bool) {
        self.window.set_paused(paused);
    }

    fn open_at(&self, icon: Rect) {
        let started = Instant::now();
        // Pick up hand edits of the settings file.
        self.hub.refresh_from_disk();
        self.show_notice();
        let height = f64::from(self.window.get_panel_height());
        let (x, y) = panel_position(icon, height, &self.platform.screens());
        self.window
            .window()
            .set_position(LogicalPosition::new(x as f32, y as f32));
        self.had_focus.set(false);
        self.open.set(true);
        // vento runs as an accessory app: bring it forward so the panel
        // becomes key and takes Esc.
        self.platform.activate_app();
        if let Err(e) = self.window.show() {
            warn!("cannot show the settings panel: {e}");
            self.open.set(false);
            self.platform.deactivate_app();
            return;
        }
        debug!("panel open at ({x}, {y}) in {:?}", started.elapsed());
    }

    fn focus_changed(&self, focused: bool) {
        if focused {
            self.had_focus.set(true);
        } else if self.is_open() && self.had_focus.get() {
            self.blurred_at.set(Some(Instant::now()));
            self.close();
        }
    }

    fn show_notice(&self) {
        let notice = self.hub.load_error().map(|e| notice_text(&e));
        self.window.set_notice(notice.unwrap_or_default().into());
    }
}

/// The notice for a settings file that could not be loaded, given why: a
/// parse error (`"line 3: expected ="`) or a read error.
fn notice_text(error: &str) -> String {
    if error.starts_with("line ") {
        format!(
            "Your settings file has an error on {error}; changing a setting here will replace it"
        )
    } else {
        format!(
            "Your settings file can't be read ({error}); changing a setting here will replace it"
        )
    }
}

/// Whether a click on the icon at `now` must not open the panel, because
/// it closed for losing focus at `blurred_at` just before.
fn reopen_blocked(blurred_at: Option<Instant>, now: Instant) -> bool {
    blurred_at.is_some_and(|t| now.saturating_duration_since(t) < REOPEN_GUARD)
}

/// Top-left corner of a panel `height` points tall: centred under `icon`,
/// [`GAP`] below it, and clamped to the icon's screen (the work area that
/// holds, or is nearest to, the point where the panel's top edge meets its
/// centre line, just below the menu bar), [`INSET`] in from its left, right
/// and bottom edges. The top edge needs no inset: the panel hangs from the
/// menu bar, already [`GAP`] below the icon.
fn panel_position(icon: Rect, height: f64, screens: &[Rect]) -> (f64, f64) {
    let anchor = Point {
        x: icon.x + icon.w / 2.0,
        y: icon.y + icon.h + GAP,
    };
    let (x, y) = (anchor.x - WIDTH / 2.0, anchor.y);
    let Some(screen) = screen_for(anchor, screens) else {
        return (x, y);
    };
    (
        clamp(x, screen.x + INSET, screen.x + screen.w - WIDTH - INSET),
        clamp(y, screen.y, screen.y + screen.h - height - INSET),
    )
}

/// `v` limited to `[lo, hi]`; an empty range (a screen smaller than the
/// panel) resolves to `lo`.
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.min(hi).max(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    /// A 1440 × 900 screen with a 25 pt menu bar.
    const PRIMARY: Rect = Rect {
        x: 0.0,
        y: 25.0,
        w: 1440.0,
        h: 875.0,
    };

    #[test]
    fn panel_hangs_centred_under_the_icon() {
        let icon = rect(1000.0, 0.0, 24.0, 25.0);
        assert_eq!(panel_position(icon, 400.0, &[PRIMARY]), (842.0, 31.0));
    }

    #[test]
    fn panel_stays_on_the_screen_near_its_edges() {
        // An icon at the right end of the menu bar.
        let icon = rect(1410.0, 0.0, 24.0, 25.0);
        assert_eq!(panel_position(icon, 400.0, &[PRIMARY]).0, 1092.0);
        // And one at the far left.
        let icon = rect(4.0, 0.0, 24.0, 25.0);
        assert_eq!(panel_position(icon, 400.0, &[PRIMARY]).0, 8.0);
        // Tall enough to reach the bottom: lifted to stay 8 pt above it.
        let icon = rect(700.0, 0.0, 24.0, 25.0);
        assert_eq!(panel_position(icon, 864.0, &[PRIMARY]).1, 28.0);
        // Too tall for the screen: the top stays below the menu bar.
        let icon = rect(700.0, 0.0, 24.0, 25.0);
        assert_eq!(panel_position(icon, 2000.0, &[PRIMARY]).1, 25.0);
    }

    #[test]
    fn panel_is_clamped_to_the_icons_screen() {
        // A notched 1x screen to the right of the primary, its icon near
        // its right edge.
        let second = rect(1440.0, 37.0, 1920.0, 1043.0);
        let icon = rect(3330.0, 0.0, 24.0, 37.0);
        let at = panel_position(icon, 400.0, &[PRIMARY, second]);
        assert_eq!(at, (3012.0, 43.0));
    }

    #[test]
    fn panel_picks_the_screen_below_the_menu_bar_not_the_one_above() {
        // A wider screen stacked above the primary: its work area ends at
        // y = 0, nearer the icon's centre than the primary's work area is.
        let above = rect(0.0, -1055.0, 1920.0, 1055.0);
        let icon = rect(1410.0, 0.0, 24.0, 25.0);
        let at = panel_position(icon, 400.0, &[above, PRIMARY]);
        assert_eq!(at, (1092.0, 31.0));
    }

    #[test]
    fn panel_without_screens_hangs_under_the_icon() {
        let icon = rect(100.0, 0.0, 20.0, 24.0);
        assert_eq!(panel_position(icon, 400.0, &[]), (-60.0, 30.0));
    }

    #[test]
    fn a_click_right_after_losing_focus_does_not_reopen() {
        let now = Instant::now();
        assert!(!reopen_blocked(None, now));
        assert!(reopen_blocked(Some(now), now + Duration::from_millis(120)));
        assert!(!reopen_blocked(Some(now), now + REOPEN_GUARD));
        // A clock that went backwards still counts as just now.
        assert!(reopen_blocked(Some(now + Duration::from_secs(1)), now));
    }

    #[test]
    fn notice_names_the_error() {
        assert_eq!(
            notice_text("line 3: expected ="),
            "Your settings file has an error on line 3: expected =; changing a setting here will replace it"
        );
        assert_eq!(
            notice_text("Permission denied (os error 13)"),
            "Your settings file can't be read (Permission denied (os error 13)); changing a setting here will replace it"
        );
    }
}
