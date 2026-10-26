//! The menu-bar panel: hangs under the menu-bar icon on a left click, its
//! arrow pointing at the icon, and closes on Esc, Done, a second click on
//! the icon or a click in another app. Losing focus alone (⌘-Tab, another
//! app coming forward) leaves it open. It holds Keep Awake, Pause, Copy
//! text, Quit and Settings…, which opens the Settings window
//! ([`crate::settings_window`]).
//!
//! The window is created on the first open and dropped once the panel has
//! been closed for [`crate::lazy_window::IDLE`], taking its native window
//! and renderer with it; a closed panel watches no clicks and runs only
//! that timer (and, on closing, one single-shot timer for the next turn of
//! the event loop).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use log::{debug, warn};
use raw_window_handle::HasWindowHandle;
use slint::winit_030::winit::event::{ElementState, WindowEvent};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{CloseRequestResponse, ComponentHandle, LogicalPosition, Model, Timer, VecModel};
use vento_core::awake::{Chip, TIMED_CHIPS, default_until};
use vento_core::geometry::{Point, Rect, screen_for};
use vento_platform::Platform;

use crate::appearance;
use crate::awake::KeepAwake;
use crate::lazy_window::LazyWindow;
use crate::settings_hub::SettingsHub;
use crate::settings_window::notice_text;
use crate::ui::{SettingsPanel, Theme};

/// Panel width in logical points (matches `settings.slint`).
const WIDTH: f64 = 340.0;
/// Gap between the bottom of the menu-bar icon and the arrow's visible tip.
const TIP_GAP: f64 = 1.25;
/// How far below the top of the window the arrow's visible tip is: the
/// outline's rounded apex lies 1.75 pt down, its hairline's outer edge half
/// a point above that (matches `settings.slint`).
const TIP_INSET: f64 = 1.25;
/// Gap between the bottom of the menu-bar icon and the top of the window.
const GAP: f64 = TIP_GAP - TIP_INSET;
/// The least distance from the panel to the left, right and bottom edges
/// of the work area.
const INSET: f64 = 8.0;
/// The panel body's corner radius and the arrow's base width (both match
/// `settings.slint`).
const CORNER_RADIUS: f64 = 16.0;
const ARROW_W: f64 = 20.0;
/// How far the base of the arrow curves into the top edge on each side
/// (matches `settings.slint`).
const ARROW_BLEND: f64 = 3.0;
/// The least distance from the arrow's centre to either side of the panel:
/// the rounded corner, half the arrow and the curve that blends it into
/// the top edge all fit beside it.
const ARROW_MARGIN: f64 = CORNER_RADIUS + ARROW_W / 2.0 + ARROW_BLEND;

/// Opens, places and closes the menu-bar panel.
pub struct PanelController {
    platform: Rc<dyn Platform>,
    hub: Rc<SettingsHub>,
    /// What the panel's Keep Awake shows and changes; it lives as long as
    /// the panel, which is as long as vento.
    keep_awake: Rc<KeepAwake>,
    /// The panel's window while it is open, and for a while after it
    /// closes. Use [`PanelController::window`] or
    /// [`PanelController::existing`].
    window: LazyWindow<SettingsPanel>,
    /// Whether vento is paused, for the Pause item of the next window.
    paused: Cell<bool>,
    /// Whether the footer offers Copy text.
    screen_text_supported: bool,
    /// Pauses or resumes vento; the panel shows the result through
    /// [`PanelController::set_paused`].
    on_pause: Box<dyn Fn(bool)>,
    /// Starts Copy from screen, for the footer's Copy text.
    on_copy_from_screen: RefCell<Option<Box<dyn Fn()>>>,
    /// Opens the Settings window, for the footer's Settings….
    on_open_settings: RefCell<Option<Box<dyn Fn()>>>,
    /// AppKit has been told to leave the window where vento puts it (needs
    /// the native window, which exists once the panel has been shown).
    frame_kept: Cell<bool>,
    open: Cell<bool>,
    this: Weak<Self>,
}

impl PanelController {
    /// Creates the (closed) panel; its window comes with the first open.
    /// `screen_text_supported` says whether the footer offers Copy text;
    /// `on_pause` is called with the state the Pause item asks for.
    pub fn new(
        platform: Rc<dyn Platform>,
        hub: Rc<SettingsHub>,
        screen_text_supported: bool,
        on_pause: Box<dyn Fn(bool)>,
    ) -> Rc<Self> {
        let keep_awake = KeepAwake::new(platform.clone(), hub.clone());
        let panel = Rc::new_cyclic(|this| Self {
            platform,
            hub,
            keep_awake,
            window: LazyWindow::new(),
            paused: Cell::new(false),
            screen_text_supported,
            on_pause,
            on_copy_from_screen: RefCell::new(None),
            on_open_settings: RefCell::new(None),
            open: Cell::new(false),
            frame_kept: Cell::new(false),
            this: this.clone(),
        });
        // A saved change clears the load error: drop the notice with it.
        // With no window there is no notice to update.
        let this = Rc::downgrade(&panel);
        panel.hub.on_change(Box::new(move |_, _| {
            if let Some(panel) = this.upgrade() {
                panel.show_notice();
            }
        }));
        // The status line counts down while the panel is open.
        let this = Rc::downgrade(&panel);
        panel.keep_awake.on_change(Box::new(move || {
            if let Some(panel) = this.upgrade() {
                panel.show_awake();
            }
        }));
        panel
    }

    /// The panel's window, created and connected if there is none.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    fn window(&self) -> SettingsPanel {
        self.window.get_or_create(|| {
            let window = SettingsPanel::new().expect("cannot create the settings panel");
            appearance::apply(&window.global::<Theme>());
            window.set_screen_text_supported(self.screen_text_supported);
            window.set_paused(self.paused.get());
            // A new native window: AppKit has yet to be told to keep it.
            self.frame_kept.set(false);
            self.connect(&window);
            window
        })
    }

    /// The panel's window, if it has one now.
    fn existing(&self) -> Option<SettingsPanel> {
        self.window.get()
    }

    fn connect(&self, window: &SettingsPanel) {
        let this = self.this.clone();
        window.on_close_requested(move || {
            if let Some(panel) = this.upgrade() {
                panel.close();
            }
        });
        let this = self.this.clone();
        window.on_pause_clicked(move || {
            if let Some(panel) = this.upgrade() {
                (panel.on_pause)(!panel.paused.get());
            }
        });
        let this = self.this.clone();
        window.on_settings_clicked(move || {
            let Some(panel) = this.upgrade() else { return };
            // The Settings window takes the focus from here.
            panel.close_for_other_window();
            if let Some(open) = panel.on_open_settings.borrow().as_ref() {
                open();
            }
        });
        let this = self.this.clone();
        window.on_copy_from_screen_clicked(move || {
            let Some(panel) = this.upgrade() else { return };
            // Out of the way of the area the user is about to choose.
            panel.close();
            if let Some(start) = panel.on_copy_from_screen.borrow().as_ref() {
                start();
            }
        });
        window.on_quit_clicked(|| {
            if let Err(e) = slint::quit_event_loop() {
                warn!("cannot quit: {e}");
            }
        });
        let this = self.this.clone();
        window.window().on_close_requested(move || {
            if let Some(panel) = this.upgrade() {
                panel.close();
            }
            CloseRequestResponse::KeepWindowShown
        });
        let this = self.this.clone();
        window.window().on_winit_window_event(move |_, event| {
            let Some(panel) = this.upgrade() else {
                return EventResult::Propagate;
            };
            // Back from another app: the click goes on to the control under
            // it, and the panel is key again for Esc.
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } = event
                && !panel.has_focus()
            {
                panel.regain_focus();
            }
            EventResult::Propagate
        });
        self.connect_awake(window);
    }

    fn connect_awake(&self, window: &SettingsPanel) {
        let this = self.this.clone();
        window.on_awake_switched(move |on| {
            let Some(panel) = this.upgrade() else { return };
            if on {
                panel.keep_awake.start(Chip::Forever);
            } else {
                panel.keep_awake.stop();
            }
        });
        let this = self.this.clone();
        window.on_awake_chip_clicked(move |i| {
            let Some(panel) = this.upgrade() else { return };
            match chip_at(i) {
                Some(chip) => panel.keep_awake.toggle(chip),
                // Until… while it runs: the panel opens its popup otherwise.
                None => panel.keep_awake.stop(),
            }
        });
        let this = self.this.clone();
        window.on_awake_until_started(move |hour, minute| {
            let Some(panel) = this.upgrade() else { return };
            let (Ok(hour), Ok(minute)) = (i8::try_from(hour), i8::try_from(minute)) else {
                return;
            };
            panel.keep_awake.start(Chip::Until { hour, minute });
        });
    }

    /// Shows what Keep Awake holds now.
    fn show_awake(&self) {
        let Some(window) = self.existing() else {
            return;
        };
        let session = self.keep_awake.session();
        window.set_awake_status(self.keep_awake.status().into());
        window.set_awake_on(session.is_some());
        window.set_awake_chip(session.map_or(-1, |s| chip_index(s.chip)));
    }

    /// Makes the open panel the key window again.
    fn regain_focus(&self) {
        let Some(window) = self.existing().filter(|_| self.is_open()) else {
            return;
        };
        self.platform.activate_app();
        window.window().with_winit_window(|w| w.focus_window());
    }

    /// Whether the panel is the key window (it takes the keyboard).
    fn has_focus(&self) -> bool {
        self.existing()
            .and_then(|window| window.window().with_winit_window(|w| w.has_focus()))
            .unwrap_or(true)
    }

    /// Has the footer's Copy text call `start`.
    pub fn set_on_copy_from_screen(&self, start: Box<dyn Fn()>) {
        *self.on_copy_from_screen.borrow_mut() = Some(start);
    }

    /// Has the footer's Settings… call `open`, once the panel has closed.
    pub fn set_on_open_settings(&self, open: Box<dyn Fn()>) {
        *self.on_open_settings.borrow_mut() = Some(open);
    }

    /// What the panel's Keep Awake shows and changes.
    pub fn keep_awake(&self) -> Rc<KeepAwake> {
        self.keep_awake.clone()
    }

    /// Opens the panel under `icon` (the menu-bar icon's frame, global
    /// logical points), or closes it when it is open.
    pub fn toggle_at(&self, icon: Rect) {
        if self.is_open() {
            self.close();
        } else {
            self.open_at(icon);
        }
    }

    /// Hides the panel, stops watching for clicks outside it and hands
    /// focus back to the app that had it.
    ///
    /// Focus is handed back on the next turn of the event loop: when the
    /// panel closes because the user clicked into another app, AppKit takes
    /// the key window away before vento stops being active, and only once
    /// that is over can `deactivate_app` see that focus is already
    /// elsewhere and leave it there.
    pub fn close(&self) {
        self.close_with(true);
    }

    /// Hides the panel like [`PanelController::close`], but keeps the focus:
    /// another vento window is about to take it.
    pub fn close_for_other_window(&self) {
        self.close_with(false);
    }

    fn close_with(&self, hand_back: bool) {
        if !self.open.replace(false) {
            return;
        }
        self.platform.stop_watching_outside_clicks();
        if let Some(window) = self.existing()
            && let Err(e) = window.hide()
        {
            warn!("cannot hide the settings panel: {e}");
        }
        self.window.release_later();
        if hand_back {
            let this = self.this.clone();
            Timer::single_shot(Duration::ZERO, move || {
                // Reopened in the meantime: vento keeps the focus.
                if let Some(panel) = this.upgrade()
                    && !panel.is_open()
                {
                    panel.platform.deactivate_app();
                }
            });
        }
        debug!("panel closed");
    }

    /// Whether the panel is open.
    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Shows whether vento is paused.
    pub fn set_paused(&self, paused: bool) {
        self.paused.set(paused);
        if let Some(window) = self.existing() {
            window.set_paused(paused);
        }
    }

    /// Opens the panel under `icon`. A new window opens once it has its
    /// native window, so that AppKit can be told to keep its frame as it
    /// shows.
    /// Opens the panel under `icon` (global logical points), if it is
    /// closed.
    pub fn open_at(&self, icon: Rect) {
        self.window();
        let this = self.this.clone();
        self.window.when_native(move || {
            if let Some(panel) = this.upgrade()
                && !panel.is_open()
            {
                panel.show_at(icon);
            }
        });
    }

    fn show_at(&self, icon: Rect) {
        let started = Instant::now();
        let window = self.window();
        // Pick up hand edits of the settings file.
        self.hub.refresh_from_disk();
        self.show_notice();
        show_menu_colors(&*self.platform, &window.global::<Theme>());
        self.show_awake();
        let (hour, minute) = default_until(jiff::Timestamp::now(), &jiff::tz::TimeZone::system());
        window.set_awake_until_hour(hour.into());
        window.set_awake_until_minute(minute.into());
        let height = f64::from(window.get_window_height());
        let Placement { x, y, arrow_x } = panel_position(icon, height, &self.platform.screens());
        window.set_arrow_x(arrow_x as f32);
        window
            .window()
            .set_position(LogicalPosition::new(x as f32, y as f32));
        self.open.set(true);
        // Shown faded out, then revealed on the next turn (see `revealed`).
        window.set_revealed(false);
        // vento runs as an accessory app: bring it forward so the panel
        // becomes key and takes Esc.
        self.platform.activate_app();
        if let Err(e) = window.show() {
            warn!("cannot show the settings panel: {e}");
            self.open.set(false);
            self.platform.deactivate_app();
            self.window.release_later();
            return;
        }
        if !self.frame_kept.get() {
            self.keep_frame(&window);
            // AppKit may already have pushed the window down while showing it.
            window
                .window()
                .set_position(LogicalPosition::new(x as f32, y as f32));
        }
        self.watch_outside_clicks();
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(panel) = this.upgrade()
                && let Some(window) = panel.existing()
            {
                window.set_revealed(true);
                let w = window.window();
                let scale = w.scale_factor();
                debug!(
                    "panel shown at {:?}, size {:?}, scale {scale}",
                    w.position().to_logical(scale),
                    w.size().to_logical(scale)
                );
            }
        });
        debug!("panel open at ({x}, {y}) in {:?}", started.elapsed());
    }

    /// Closes the panel on a click in another app or on the desktop, until
    /// it closes. The close happens on the next turn of the event loop,
    /// outside AppKit's event monitor that reports the click.
    fn watch_outside_clicks(&self) {
        let this = self.this.clone();
        self.platform.watch_outside_clicks(Box::new(move || {
            let this = this.clone();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(panel) = this.upgrade() {
                    // AppKit can report a click on the panel itself (while
                    // vento is not the active app): only close for a click
                    // that is really outside it.
                    if panel.cursor_on_panel() {
                        debug!("click on the panel reported as outside: ignored");
                        return;
                    }
                    debug!("click outside the panel");
                    panel.close();
                }
            });
            // A timer started from AppKit's callback does not wake the
            // event loop by itself; this does, so the panel closes at once.
            if let Err(e) = slint::invoke_from_event_loop(|| {}) {
                warn!("cannot wake the event loop: {e}");
            }
        }));
    }

    /// Tells AppKit to leave the panel where it is placed: on its own it
    /// pushes a window whose top meets the menu bar a menu bar further down.
    fn keep_frame(&self, window: &SettingsPanel) {
        let result = match window.window().window_handle().window_handle() {
            Ok(handle) => self
                .platform
                .keep_window_frame(handle.as_raw())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => self.frame_kept.set(true),
            Err(e) => warn!("cannot keep the settings panel under the menu bar: {e}"),
        }
    }

    /// Whether the mouse is over the panel window now, in global logical
    /// points (the window's frame, arrow band included).
    fn cursor_on_panel(&self) -> bool {
        let Some(panel) = self.existing() else {
            return false;
        };
        let window = panel.window();
        let scale = window.scale_factor();
        let at = window.position().to_logical(scale);
        let size = window.size().to_logical(scale);
        let frame = Rect {
            x: f64::from(at.x),
            y: f64::from(at.y),
            w: f64::from(size.width),
            h: f64::from(size.height),
        };
        contains(&frame, self.platform.sample_input().cursor)
    }

    fn show_notice(&self) {
        let Some(window) = self.existing() else {
            return;
        };
        let notice = self.hub.load_error().map(|e| notice_text(&e));
        window.set_notice(notice.unwrap_or_default().into());
    }
}

/// Matches the desktop's own menus where the platform gives their colours,
/// as they are now (a theme change shows at the next open), and gives
/// `theme` the Appearance setting, which decides whether those colours
/// apply.
pub(crate) fn show_menu_colors(platform: &dyn Platform, theme: &Theme<'_>) {
    appearance::apply(theme);
    let Some(colors) = platform.menu_colors() else {
        theme.set_system_colors(false);
        return;
    };
    let color = |[r, g, b, a]: [u8; 4]| slint::Color::from_argb_u8(a, r, g, b);
    theme.set_system_bg(color(colors.background));
    theme.set_system_border(color(colors.border));
    theme.set_system_text(color(colors.text));
    theme.set_system_dark(colors.is_dark());
    theme.set_system_colors(true);
}

/// The Keep Awake chip at `i` of the panel (see `KeepAwakeTab`): the timed
/// ones, then ∞. `None` for Until…, which asks for its time first.
fn chip_at(i: i32) -> Option<Chip> {
    let i = usize::try_from(i).ok()?;
    match TIMED_CHIPS.get(i) {
        Some(&length) => Some(Chip::For(length)),
        None if i == TIMED_CHIPS.len() => Some(Chip::Forever),
        None => None,
    }
}

/// Where `chip` is in the panel; -1 for a timed one it does not show.
fn chip_index(chip: Chip) -> i32 {
    let index = match chip {
        Chip::For(length) => TIMED_CHIPS.iter().position(|&l| l == length),
        Chip::Forever => Some(TIMED_CHIPS.len()),
        Chip::Until { .. } => Some(TIMED_CHIPS.len() + 1),
    };
    index.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1)
}

/// Makes `model` hold `rows`, touching only the rows that changed, so a
/// switch that flips animates instead of its row being rebuilt.
pub(crate) fn set_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (i, row) in rows.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
}

/// Where the panel's window goes, in global logical points, and where its
/// arrow points.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    /// The window's top-left corner (the top is the arrow's tip).
    x: f64,
    y: f64,
    /// The arrow's centre, from the window's left edge.
    arrow_x: f64,
}

/// Placement of a panel window `height` points tall (the arrow included):
/// centred under `icon`, its arrow's tip [`TIP_GAP`] below it (the window
/// [`GAP`] below it), and clamped to
/// the icon's screen (the work area that holds, or is nearest to, the point
/// where the window's top edge meets its centre line, just below the menu
/// bar), [`INSET`] in from its left, right and bottom edges. The top edge
/// needs no inset: the panel hangs from the menu bar, already [`GAP`] below
/// the icon. The arrow points at the icon's centre wherever the body ends
/// up, but stays [`ARROW_MARGIN`] in from either side.
fn panel_position(icon: Rect, height: f64, screens: &[Rect]) -> Placement {
    let anchor = Point {
        x: icon.x + icon.w / 2.0,
        y: icon.y + icon.h + GAP,
    };
    let (mut x, mut y) = (anchor.x - WIDTH / 2.0, anchor.y);
    if let Some(screen) = screen_for(anchor, screens) {
        x = clamp(x, screen.x + INSET, screen.x + screen.w - WIDTH - INSET);
        y = clamp(y, screen.y, screen.y + screen.h - height - INSET);
    }
    let arrow_x = clamp(anchor.x - x, ARROW_MARGIN, WIDTH - ARROW_MARGIN);
    Placement { x, y, arrow_x }
}

/// `v` limited to `[lo, hi]`; an empty range (a screen smaller than the
/// panel) resolves to `lo`.
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.min(hi).max(lo)
}

/// Whether `p` lies inside `r`, edges included.
fn contains(r: &Rect, p: Point) -> bool {
    p.x >= r.x && p.x <= r.x + r.w && p.y >= r.y && p.y <= r.y + r.h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_on_the_panel_is_inside_it() {
        let frame = Rect {
            x: 933.0,
            y: 33.75,
            w: 340.0,
            h: 500.0,
        };
        assert!(contains(&frame, Point { x: 1000.0, y: 60.0 }));
        assert!(contains(
            &frame,
            Point {
                x: 933.0,
                y: 533.75
            }
        ));
        assert!(!contains(&frame, Point { x: 900.0, y: 60.0 }));
        assert!(!contains(
            &frame,
            Point {
                x: 1000.0,
                y: 600.0
            }
        ));
    }

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

    fn at(x: f64, y: f64, arrow_x: f64) -> Placement {
        Placement { x, y, arrow_x }
    }

    #[test]
    fn panel_hangs_centred_under_the_icon_with_the_arrow_in_the_middle() {
        // The arrow's visible tip 2 pt below the icon (the window 0.75 pt),
        // the body centred under it.
        let icon = rect(1000.0, 0.0, 24.0, 25.0);
        assert_eq!(
            panel_position(icon, 400.0, &[PRIMARY]),
            at(842.0, 25.0, WIDTH / 2.0)
        );
    }

    #[test]
    fn arrow_follows_the_icon_when_the_panel_is_clamped_at_the_right_edge() {
        // Centred, the panel would end at 1482: it is pulled back to end
        // 8 pt in from the edge, and the arrow moves right to stay on the
        // icon's centre (1312).
        let icon = rect(1300.0, 0.0, 24.0, 25.0);
        assert_eq!(
            panel_position(icon, 400.0, &[PRIMARY]),
            at(1092.0, 25.0, 220.0)
        );
    }

    #[test]
    fn arrow_keeps_clear_of_the_corners_when_the_icon_is_at_the_screen_edge() {
        // An icon at the right end of the menu bar: its centre is 330 pt
        // into the clamped panel, too close to the rounded corner.
        let icon = rect(1410.0, 0.0, 24.0, 25.0);
        assert_eq!(
            panel_position(icon, 400.0, &[PRIMARY]),
            at(1092.0, 25.0, WIDTH - ARROW_MARGIN)
        );
        // And one at the far left.
        let icon = rect(4.0, 0.0, 24.0, 25.0);
        assert_eq!(
            panel_position(icon, 400.0, &[PRIMARY]),
            at(8.0, 25.0, ARROW_MARGIN)
        );
        // Never closer to a corner than its radius plus half the arrow.
        const { assert!(ARROW_MARGIN >= CORNER_RADIUS + ARROW_W / 2.0) };
    }

    #[test]
    fn panel_stays_above_the_bottom_of_the_screen() {
        // The window's top meets the menu bar, so a panel that reaches the
        // bottom inset has no room to be lifted: its top stays there.
        let icon = rect(700.0, 0.0, 24.0, 25.0);
        assert_eq!(panel_position(icon, 866.5, &[PRIMARY]).y, 25.0);
        // Too tall for the screen: the top stays below the menu bar.
        assert_eq!(panel_position(icon, 2000.0, &[PRIMARY]).y, 25.0);
    }

    #[test]
    fn panel_is_clamped_to_the_icons_screen() {
        // A notched 1x screen to the right of the primary, its icon near
        // its right edge.
        let second = rect(1440.0, 37.0, 1920.0, 1043.0);
        let icon = rect(3330.0, 0.0, 24.0, 37.0);
        let placed = panel_position(icon, 400.0, &[PRIMARY, second]);
        assert_eq!(placed, at(3012.0, 37.0, WIDTH - ARROW_MARGIN));
    }

    #[test]
    fn panel_picks_the_screen_below_the_menu_bar_not_the_one_above() {
        // A wider screen stacked above the primary: its work area ends at
        // y = 0, nearer the icon's centre than the primary's work area is.
        let above = rect(0.0, -1055.0, 1920.0, 1055.0);
        let icon = rect(1300.0, 0.0, 24.0, 25.0);
        let placed = panel_position(icon, 400.0, &[above, PRIMARY]);
        assert_eq!(placed, at(1092.0, 25.0, 220.0));
    }

    #[test]
    fn panel_without_screens_hangs_under_the_icon() {
        let icon = rect(100.0, 0.0, 20.0, 24.0);
        assert_eq!(
            panel_position(icon, 400.0, &[]),
            at(-60.0, 24.0, WIDTH / 2.0)
        );
    }

    #[test]
    fn keep_awake_chips_and_their_places_match() {
        for i in 0..7 {
            let chip = chip_at(i).unwrap();
            assert_eq!(chip_index(chip), i);
        }
        assert_eq!(chip_at(2), Some(Chip::For(Duration::from_secs(60 * 60))));
        assert_eq!(chip_at(6), Some(Chip::Forever));
        // Until… asks for its time first; nothing past it.
        assert_eq!(chip_at(7), None);
        assert_eq!(chip_at(8), None);
        assert_eq!(chip_at(-1), None);
        assert_eq!(chip_index(Chip::Until { hour: 7, minute: 0 }), 7);
        assert_eq!(chip_index(Chip::For(Duration::from_secs(1))), -1);
    }
}
