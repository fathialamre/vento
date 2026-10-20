//! The settings panel: hangs under the menu-bar icon on a left click and
//! closes on Esc, Done, a second click on the icon or a click outside.
//!
//! The window is created on the first open and only hidden afterwards; a
//! closed panel runs no timers (closing arms one single-shot timer for the
//! next turn of the event loop).

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use log::{debug, warn};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{
    CloseRequestResponse, ComponentHandle, LogicalPosition, Model, ModelRc, SharedString, Timer,
    VecModel,
};
use vento_core::geometry::{Layout, Point, Rect, screen_for};
use vento_core::paths::PathFormat;
use vento_core::settings::{Settings, move_row};
use vento_core::trigger::Modifier;
use vento_platform::{Platform, TerminalApp};

use crate::settings_hub::SettingsHub;
use crate::ui::{ActionRow, Icons, SettingsPanel};
use crate::wheel::action_icon;

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

/// The choices of the panel's segmented controls, in the order shown.
const LAYOUTS: [Layout; 2] = [Layout::Honeycomb, Layout::Petals];
const TRIGGERS: [Modifier; 4] = [
    Modifier::Ctrl,
    Modifier::Alt,
    Modifier::Shift,
    Modifier::Meta,
];
const PATH_FORMATS: [PathFormat; 2] = [PathFormat::Plain, PathFormat::ShellQuoted];

/// Opens, places and closes the settings panel.
pub struct PanelController {
    platform: Rc<dyn Platform>,
    hub: Rc<SettingsHub>,
    window: SettingsPanel,
    /// Pauses or resumes vento; the panel shows the result through
    /// [`PanelController::set_paused`].
    on_pause: Box<dyn Fn(bool)>,
    /// The Wheel tab's action rows.
    actions: Rc<VecModel<ActionRow>>,
    /// The installed terminals, as the Terminal menu lists them.
    terminals: RefCell<Vec<TerminalApp>>,
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
        let actions = Rc::new(VecModel::default());
        window.set_actions(ModelRc::from(actions.clone()));
        let panel = Rc::new_cyclic(|this| Self {
            platform,
            hub,
            window,
            on_pause,
            actions,
            terminals: RefCell::new(Vec::new()),
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
        // A hand edit picked up on opening changes what the controls show.
        let this = self.this.clone();
        self.hub.on_change(Box::new(move |_, _| {
            if let Some(panel) = this.upgrade() {
                panel.show_notice();
                panel.show_settings();
            }
        }));
        self.connect_controls();
    }

    /// Every control applies its change at once.
    fn connect_controls(&self) {
        let this = self.this.clone();
        self.window.on_layout_chosen(move |i| {
            if let (Some(panel), Some(layout)) = (this.upgrade(), choice(&LAYOUTS, i)) {
                panel.apply(|s| s.layout = layout);
            }
        });
        let this = self.this.clone();
        self.window.on_action_toggled(move |i, on| {
            if let (Some(panel), Ok(i)) = (this.upgrade(), usize::try_from(i)) {
                panel.apply(|s| show_action(s, i, on));
            }
        });
        let this = self.this.clone();
        self.window.on_action_moved(move |from, to| {
            if let (Some(panel), Ok(from), Ok(to)) =
                (this.upgrade(), usize::try_from(from), usize::try_from(to))
            {
                panel.apply(|s| move_action(s, from, to));
            }
        });
        let this = self.this.clone();
        self.window.on_trigger_chosen(move |i| {
            if let (Some(panel), Some(trigger)) = (this.upgrade(), choice(&TRIGGERS, i)) {
                panel.apply(|s| s.trigger = trigger);
            }
        });
        let this = self.this.clone();
        self.window.on_terminal_chosen(move |i| {
            let Some(panel) = this.upgrade() else { return };
            let id = usize::try_from(i)
                .ok()
                .and_then(|i| panel.terminals.borrow().get(i).map(|t| t.id.clone()));
            if let Some(id) = id {
                panel.apply(|s| s.terminal = Some(id.clone()));
            }
        });
        let this = self.this.clone();
        self.window.on_path_format_chosen(move |i| {
            if let (Some(panel), Some(format)) = (this.upgrade(), choice(&PATH_FORMATS, i)) {
                panel.apply(|s| s.copy_path_format = format);
            }
        });
    }

    /// Applies `change` to the settings in effect and saves it, unless it
    /// changes nothing (a refused change included), so an unchanged click
    /// never replaces the file. A save error shows in the notice. Either way
    /// the controls then show the settings in effect.
    fn apply(&self, change: impl Fn(&mut Settings)) {
        self.hub.refresh_from_disk();
        let mut changed = self.hub.current();
        change(&mut changed);
        if changed != self.hub.current()
            && let Err(e) = self.hub.update(&change)
        {
            warn!("cannot save settings: {e}");
            self.window.set_notice(save_error_text(&e).into());
        }
        self.show_settings();
    }

    /// Sets every control from the settings in effect.
    fn show_settings(&self) {
        let settings = self.hub.current();
        let icons = self.window.global::<Icons>();
        let rows: Vec<ActionRow> = settings
            .action_rows()
            .into_iter()
            .map(|(action, shown)| ActionRow {
                icon: action_icon(&icons, action),
                label: action.label().into(),
                shown,
            })
            .collect();
        let shown = rows.iter().filter(|r| r.shown).count();
        set_rows(&self.actions, rows);
        self.window
            .set_shown_actions(i32::try_from(shown).unwrap_or(0));
        self.window
            .set_wheel_layout(choice_index(&LAYOUTS, settings.layout));
        self.window
            .set_trigger(choice_index(&TRIGGERS, settings.trigger));
        self.window
            .set_path_format(choice_index(&PATH_FORMATS, settings.copy_path_format));
        let terminal = terminal_index(&self.terminals.borrow(), settings.terminal.as_deref());
        self.window
            .set_terminal(i32::try_from(terminal).unwrap_or(0));
    }

    /// Lists the terminals installed now in the Terminal menu.
    fn load_terminals(&self) {
        let terminals = self.platform.terminals();
        let names: Vec<SharedString> = terminals.iter().map(|t| t.name.as_str().into()).collect();
        self.window
            .set_terminals(ModelRc::new(VecModel::from(names)));
        *self.terminals.borrow_mut() = terminals;
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
        self.load_terminals();
        self.show_settings();
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

/// The notice for a change that could not be saved.
fn save_error_text(error: &io::Error) -> String {
    format!("Couldn't save settings: {error}")
}

/// The choice at index `i` of a segmented control; `None` out of range.
fn choice<T: Copy>(choices: &[T], i: i32) -> Option<T> {
    usize::try_from(i)
        .ok()
        .and_then(|i| choices.get(i).copied())
}

/// Where `value` is among a segmented control's choices (0 if it is not).
fn choice_index<T: PartialEq>(choices: &[T], value: T) -> i32 {
    choices
        .iter()
        .position(|c| *c == value)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

/// The Terminal menu item for the terminal setting `id`: the first item
/// when it is unset or not installed.
fn terminal_index(terminals: &[TerminalApp], id: Option<&str>) -> usize {
    id.and_then(|id| terminals.iter().position(|t| t.id == id))
        .unwrap_or(0)
}

/// Shows or hides the action in row `index` of the Wheel tab. Hiding the
/// last action shown is refused and changes nothing.
fn show_action(settings: &mut Settings, index: usize, on: bool) {
    let mut rows = settings.action_rows();
    if let Some(row) = rows.get_mut(index) {
        row.1 = on;
        settings.set_action_rows(&rows);
    }
}

/// Moves the Wheel tab's row `from` to place `to` (clamped to the last).
fn move_action(settings: &mut Settings, from: usize, to: usize) {
    let mut rows = settings.action_rows();
    move_row(&mut rows, from, to);
    settings.set_action_rows(&rows);
}

/// Makes `model` hold `rows`, touching only the rows that changed, so a
/// switch that flips animates instead of its row being rebuilt.
fn set_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
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
    fn segmented_choices_map_both_ways() {
        assert_eq!(choice(&TRIGGERS, 3), Some(Modifier::Meta));
        assert_eq!(choice(&TRIGGERS, 4), None);
        assert_eq!(choice(&TRIGGERS, -1), None);
        assert_eq!(choice_index(&TRIGGERS, Modifier::Alt), 1);
        assert_eq!(choice_index(&LAYOUTS, Layout::Petals), 1);
        assert_eq!(choice_index(&PATH_FORMATS, PathFormat::ShellQuoted), 1);
    }

    #[test]
    fn terminal_menu_falls_back_to_the_first_item() {
        let terminals: Vec<TerminalApp> = ["com.apple.Terminal", "com.mitchellh.ghostty"]
            .into_iter()
            .map(|id| TerminalApp {
                id: id.into(),
                name: id.into(),
            })
            .collect();
        assert_eq!(terminal_index(&terminals, Some("com.mitchellh.ghostty")), 1);
        assert_eq!(terminal_index(&terminals, None), 0);
        assert_eq!(terminal_index(&terminals, Some("dev.warp.Warp-Stable")), 0);
        assert_eq!(terminal_index(&[], None), 0);
    }

    #[test]
    fn hiding_the_last_shown_action_changes_nothing() {
        use vento_core::actions::ActionId;
        let mut s = Settings {
            actions: vec![ActionId::Move],
            ..Settings::default()
        };
        let before = s.clone();
        show_action(&mut s, 0, false);
        assert_eq!(s, before);
        // Out of range: nothing either.
        show_action(&mut s, 9, true);
        assert_eq!(s, before);
        // Showing another one keeps its row's place.
        let copy_to = s
            .action_rows()
            .iter()
            .position(|(a, _)| *a == ActionId::CopyTo)
            .unwrap();
        show_action(&mut s, copy_to, true);
        assert_eq!(s.actions, [ActionId::Move, ActionId::CopyTo]);
        assert!(!s.hidden_actions.contains(&ActionId::CopyTo));
        show_action(&mut s, 0, false);
        assert_eq!(s.actions, [ActionId::CopyTo]);
    }

    #[test]
    fn moving_an_action_reorders_the_wheel() {
        use vento_core::actions::ActionId;
        let mut s = Settings::default();
        // Terminal (row 4) to the top.
        move_action(&mut s, 4, 0);
        assert_eq!(
            s.actions,
            [
                ActionId::Terminal,
                ActionId::Pocket,
                ActionId::Copy,
                ActionId::Move,
                ActionId::CopyPath,
            ]
        );
        // A hidden row dragged among the shown ones stays hidden and the
        // list keeps the shown rows first: it lands first of the hidden.
        show_action(&mut s, 1, false);
        assert_eq!(s.hidden_actions, [ActionId::Pocket, ActionId::CopyTo]);
        move_action(&mut s, 5, 1);
        assert_eq!(s.hidden_actions, [ActionId::CopyTo, ActionId::Pocket]);
        assert_eq!(s.actions.len(), 4);
        let before = s.clone();
        move_action(&mut s, 9, 0);
        assert_eq!(s, before);
    }

    #[test]
    fn save_error_is_worded_for_the_notice() {
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(save_error_text(&e), format!("Couldn't save settings: {e}"));
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
