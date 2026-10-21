//! The settings panel: hangs under the menu-bar icon on a left click, its
//! arrow pointing at the icon, and closes on Esc, Done, a second click on
//! the icon or a click in another app. Losing focus alone (⌘-Tab, another
//! app coming forward, the folder picker) leaves it open.
//!
//! The window is created on the first open and only hidden afterwards; a
//! closed panel runs no timers and watches no clicks (closing arms one
//! single-shot timer for the next turn of the event loop).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use log::{debug, warn};
use raw_window_handle::HasWindowHandle;
use slint::winit_030::winit::event::{ElementState, KeyEvent, WindowEvent};
use slint::winit_030::winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalPosition, Model, ModelRc, SharedString,
    Timer, VecModel,
};
use vento_core::destinations::{
    RecentDestinations, display_path, folder_exists, recent_path, ring_destinations,
};
use vento_core::geometry::{Layout, Point, Rect, screen_for};
use vento_core::paths::PathFormat;
use vento_core::settings::{
    MAX_MOVE_HOLD_MS, MAX_PINNED, MIN_MOVE_HOLD_MS, MOVE_HOLD_STEP_MS, Settings,
    is_reserved_shortcut, move_row, parse_shortcut, shortcut_symbols,
};
use vento_core::trigger::{Modifier, Modifiers};
use vento_platform::{Platform, TerminalApp};

use crate::pocket::{display_name, to_image};
use crate::settings_hub::SettingsHub;
use crate::ui::{ActionRow, FolderRow, Icons, SettingsPanel};
use crate::wheel::action_icon;

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

/// The choices of the panel's segmented controls, in the order shown.
const LAYOUTS: [Layout; 2] = [Layout::Honeycomb, Layout::Petals];
const TRIGGERS: [Modifier; 4] = [
    Modifier::Ctrl,
    Modifier::Alt,
    Modifier::Shift,
    Modifier::Meta,
];
const PATH_FORMATS: [PathFormat; 2] = [PathFormat::Plain, PathFormat::ShellQuoted];

/// Size of the Finder icons in the Move tab, in points.
const FOLDER_ICON_PX: u32 = 16;

/// Why a recorded shortcut was refused.
const NOT_A_SHORTCUT: &str = "Use a modifier with a letter or digit";
const RESERVED_SHORTCUT: &str = "macOS uses this shortcut";
const UNUSABLE_SHORTCUT: &str = "This shortcut can't be used";

/// Registers a Pocket shortcut in place of the current one, or says why it
/// cannot be registered.
pub type TryShortcut = Box<dyn Fn(&str) -> Result<(), String>>;

/// Opens, places and closes the settings panel.
pub struct PanelController {
    platform: Rc<dyn Platform>,
    hub: Rc<SettingsHub>,
    window: SettingsPanel,
    /// Pauses or resumes vento; the panel shows the result through
    /// [`PanelController::set_paused`].
    on_pause: Box<dyn Fn(bool)>,
    /// Folders recently moved or copied to, shared with the wheel and the
    /// executor.
    recent: Rc<RefCell<RecentDestinations>>,
    /// Registers a new Pocket shortcut (freeing the old one first).
    try_shortcut: TryShortcut,
    /// The home folder, shown as `~` in folder places.
    home: Option<PathBuf>,
    /// The Wheel tab's action rows.
    actions: Rc<VecModel<ActionRow>>,
    /// The Move tab's pinned and recent folder rows, and the folders the
    /// recent rows stand for.
    pinned: Rc<VecModel<FolderRow>>,
    recent_rows: Rc<VecModel<FolderRow>>,
    recent_paths: RefCell<Vec<PathBuf>>,
    /// Finder icons of the listed folders, by path (`None`: it has none).
    folder_icons: RefCell<HashMap<PathBuf, Option<Image>>>,
    /// The installed terminals, as the Terminal menu lists them.
    terminals: RefCell<Vec<TerminalApp>>,
    /// AppKit has been told to leave the window where vento puts it (needs
    /// the native window, which exists once the panel has been shown).
    frame_kept: Cell<bool>,
    open: Cell<bool>,
    /// The modifier keys held now, for the shortcut recorder.
    modifiers: Cell<ModifiersState>,
    this: Weak<Self>,
}

impl PanelController {
    /// Creates the (hidden) panel. `recent` is the recent-folders list the
    /// wheel and the executor share; `try_shortcut` registers a recorded
    /// Pocket shortcut; `on_pause` is called with the state the Pause item
    /// asks for.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    pub fn new(
        platform: Rc<dyn Platform>,
        hub: Rc<SettingsHub>,
        recent: Rc<RefCell<RecentDestinations>>,
        try_shortcut: TryShortcut,
        on_pause: Box<dyn Fn(bool)>,
    ) -> Rc<Self> {
        let window = SettingsPanel::new().expect("cannot create the settings panel");
        let actions = Rc::new(VecModel::default());
        window.set_actions(ModelRc::from(actions.clone()));
        let pinned = Rc::new(VecModel::default());
        window.set_pinned(ModelRc::from(pinned.clone()));
        let recent_rows = Rc::new(VecModel::default());
        window.set_recent(ModelRc::from(recent_rows.clone()));
        let panel = Rc::new_cyclic(|this| Self {
            platform,
            hub,
            window,
            on_pause,
            recent,
            try_shortcut,
            home: std::env::var_os("HOME").map(PathBuf::from),
            actions,
            pinned,
            recent_rows,
            recent_paths: RefCell::new(Vec::new()),
            folder_icons: RefCell::new(HashMap::new()),
            terminals: RefCell::new(Vec::new()),
            open: Cell::new(false),
            frame_kept: Cell::new(false),
            modifiers: Cell::new(ModifiersState::empty()),
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
            let Some(panel) = this.upgrade() else {
                return EventResult::Propagate;
            };
            match event {
                // Back from another app: the click goes on to the control
                // under it, and the panel is key again for Esc.
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } if !panel.has_focus() => panel.regain_focus(),
                WindowEvent::ModifiersChanged(modifiers) => panel.modifiers.set(modifiers.state()),
                WindowEvent::KeyboardInput {
                    event,
                    is_synthetic: false,
                    ..
                } if panel.window.get_shortcut_recording() => return panel.shortcut_key(event),
                _ => {}
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
        self.connect_move_tab();
        let this = self.this.clone();
        self.window.on_remove_after_drop_toggled(move |on| {
            if let Some(panel) = this.upgrade() {
                panel.apply(|s| s.pocket_remove_after_drop = on);
            }
        });
        let this = self.this.clone();
        self.window.on_keep_copies_toggled(move |on| {
            if let Some(panel) = this.upgrade() {
                panel.apply(|s| s.pocket_keep_copies = on);
            }
        });
        // While recording, the menu bar is away, so ⌘Q and ⌘H reach the
        // recorder instead of quitting or hiding vento.
        let this = self.this.clone();
        self.window.on_shortcut_recording_started(move || {
            if let Some(panel) = this.upgrade() {
                panel.platform.suspend_app_menu();
            }
        });
        let this = self.this.clone();
        self.window.on_shortcut_recording_stopped(move || {
            if let Some(panel) = this.upgrade() {
                panel.platform.restore_app_menu();
            }
        });
    }

    fn connect_move_tab(&self) {
        let this = self.this.clone();
        self.window.on_add_folder(move || {
            if let Some(panel) = this.upgrade() {
                panel.add_folder();
            }
        });
        let this = self.this.clone();
        self.window.on_pinned_moved(move |from, to| {
            if let (Some(panel), Ok(from), Ok(to)) =
                (this.upgrade(), usize::try_from(from), usize::try_from(to))
            {
                panel.apply(|s| move_row(&mut s.pinned_folders, from, to));
            }
        });
        let this = self.this.clone();
        self.window.on_unpinned(move |i| {
            if let (Some(panel), Ok(i)) = (this.upgrade(), usize::try_from(i)) {
                panel.apply(|s| unpin_folder(s, i));
            }
        });
        let this = self.this.clone();
        self.window.on_recent_pinned(move |i| {
            let Some(panel) = this.upgrade() else { return };
            let folder = usize::try_from(i)
                .ok()
                .and_then(|i| panel.recent_paths.borrow().get(i).cloned());
            if let Some(folder) = folder {
                panel.apply(|s| pin_folder(s, &folder));
            }
        });
        let this = self.this.clone();
        self.window.on_recent_cleared(move || {
            if let Some(panel) = this.upgrade() {
                panel.clear_recent();
            }
        });
        let this = self.this.clone();
        self.window.on_hold_stepped(move |steps| {
            if let Some(panel) = this.upgrade() {
                panel.apply(|s| step_hold(s, steps));
            }
        });
    }

    /// Shows the folder picker and pins the folder chosen. Does nothing
    /// when the pinned list is full.
    fn add_folder(&self) {
        if self.hub.current().pinned_folders.len() >= MAX_PINNED {
            return;
        }
        // The modal picker takes the key focus. Its focus changes may still
        // be on their way when it returns, so the panel takes the focus
        // back on the next turn of the event loop, for Esc.
        let picked = self.platform.pick_folder(None);
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(panel) = this.upgrade() {
                panel.regain_focus();
            }
        });
        if let Some(folder) = picked {
            self.apply(|s| pin_folder(s, &folder));
        }
    }

    /// Makes the open panel the key window again.
    fn regain_focus(&self) {
        if !self.is_open() {
            return;
        }
        self.platform.activate_app();
        self.window.window().with_winit_window(|w| w.focus_window());
    }

    /// Whether the panel is the key window (it takes the keyboard).
    fn has_focus(&self) -> bool {
        self.window
            .window()
            .with_winit_window(|w| w.has_focus())
            .unwrap_or(true)
    }

    /// Forgets every recent folder and saves the empty list. A failed save
    /// is logged.
    fn clear_recent(&self) {
        {
            let mut recent = self.recent.borrow_mut();
            recent.clear();
            if let Err(e) = recent.save(&recent_path()) {
                warn!("cannot save the recent folders: {e}");
            }
        }
        self.show_folders(&self.hub.current());
    }

    /// A key press while the shortcut field records: a combination ends the
    /// recording, and the panel's own key handling never sees it. Esc (the
    /// field cancels) and modifier keys (it keeps listening) go on to the
    /// field.
    fn shortcut_key(&self, event: &KeyEvent) -> EventResult {
        if event.state != ElementState::Pressed {
            return EventResult::Propagate;
        }
        match key_press(self.modifiers.get(), event.physical_key) {
            KeyPress::Cancel | KeyPress::Modifier => EventResult::Propagate,
            KeyPress::Combo(spec) => {
                if !event.repeat {
                    self.record_shortcut(&spec);
                }
                EventResult::PreventDefault
            }
        }
    }

    /// Ends the recording with `spec` and applies it the way
    /// [`apply_shortcut`] says: the reason a refused one was refused shows
    /// under the field, a failed save in the notice.
    fn record_shortcut(&self, spec: &str) {
        self.window.invoke_stop_recording();
        self.hub.refresh_from_disk();
        let old = self.hub.current().pocket_shortcut;
        let outcome = apply_shortcut(spec, &old, &*self.try_shortcut, |spec| {
            self.hub.update(|s| s.pocket_shortcut = spec.to_owned())
        });
        match outcome {
            ShortcutOutcome::Applied => self.window.set_shortcut_error(SharedString::new()),
            ShortcutOutcome::Refused(reason) => self.window.set_shortcut_error(reason.into()),
            ShortcutOutcome::NotSaved(e) => {
                self.window.set_shortcut_error(SharedString::new());
                self.window.set_notice(save_error_text(&e).into());
            }
        }
        self.show_settings();
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
        self.show_folders(&settings);
        let hold = settings.move_hold_ms;
        self.window.set_hold(format!("{hold} ms").into());
        self.window.set_hold_can_decrease(hold > MIN_MOVE_HOLD_MS);
        self.window.set_hold_can_increase(hold < MAX_MOVE_HOLD_MS);
        let shortcut = shortcut_symbols(&settings.pocket_shortcut)
            .unwrap_or_else(|| settings.pocket_shortcut.clone());
        self.window.set_shortcut(shortcut.into());
        self.window
            .set_remove_after_drop(settings.pocket_remove_after_drop);
        self.window.set_keep_copies(settings.pocket_keep_copies);
    }

    /// Lists the pinned folders (a missing one too, dimmed, so it can be
    /// unpinned) and the recent ones the ring would show now.
    fn show_folders(&self, settings: &Settings) {
        let pinned = &settings.pinned_folders;
        let recent: Vec<PathBuf> =
            ring_destinations(self.recent.borrow().items(), pinned, folder_exists)
                .into_iter()
                .filter(|d| !d.pinned)
                .map(|d| d.path)
                .collect();
        let pinned_rows = pinned
            .iter()
            .map(|p| self.folder_row(p, folder_exists(p)))
            .collect();
        let recent_rows = recent.iter().map(|p| self.folder_row(p, true)).collect();
        self.folder_icons
            .borrow_mut()
            .retain(|p, _| pinned.contains(p) || recent.contains(p));
        set_rows(&self.pinned, pinned_rows);
        set_rows(&self.recent_rows, recent_rows);
        *self.recent_paths.borrow_mut() = recent;
        self.window
            .set_recent_history(!self.recent.borrow().items().is_empty());
        self.window.set_pinned_full(pinned.len() >= MAX_PINNED);
    }

    fn folder_row(&self, folder: &Path, exists: bool) -> FolderRow {
        let icon = if exists {
            self.folder_icon(folder)
        } else {
            None
        };
        FolderRow {
            has_icon: icon.is_some(),
            icon: icon.unwrap_or_default(),
            name: display_name(folder).into(),
            place: folder
                .parent()
                .map(|p| display_path(p, self.home.as_deref()))
                .unwrap_or_default()
                .into(),
            missing: !exists,
        }
    }

    /// The Finder icon of `folder`, fetched once and then kept.
    fn folder_icon(&self, folder: &Path) -> Option<Image> {
        if let Some(icon) = self.folder_icons.borrow().get(folder) {
            return icon.clone();
        }
        let icon = self
            .platform
            .file_icon(folder, FOLDER_ICON_PX)
            .as_ref()
            .and_then(to_image);
        self.folder_icons
            .borrow_mut()
            .insert(folder.to_path_buf(), icon.clone());
        icon
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
        if !self.open.replace(false) {
            return;
        }
        self.platform.stop_watching_outside_clicks();
        if self.window.get_shortcut_recording() {
            self.window.invoke_stop_recording();
        }
        // Whatever happened to the recording, the menu bar comes back.
        self.platform.restore_app_menu();
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
        self.window.set_shortcut_error(SharedString::new());
        self.load_terminals();
        // Also lists the recent folders as they are now.
        self.show_settings();
        let height = f64::from(self.window.get_window_height());
        let Placement { x, y, arrow_x } = panel_position(icon, height, &self.platform.screens());
        self.window.set_arrow_x(arrow_x as f32);
        self.window
            .window()
            .set_position(LogicalPosition::new(x as f32, y as f32));
        self.open.set(true);
        // Shown faded out, then revealed on the next turn (see `revealed`).
        self.window.set_revealed(false);
        // vento runs as an accessory app: bring it forward so the panel
        // becomes key and takes Esc.
        self.platform.activate_app();
        if let Err(e) = self.window.show() {
            warn!("cannot show the settings panel: {e}");
            self.open.set(false);
            self.platform.deactivate_app();
            return;
        }
        if !self.frame_kept.get() {
            self.keep_frame();
            // AppKit may already have pushed the window down while showing it.
            self.window
                .window()
                .set_position(LogicalPosition::new(x as f32, y as f32));
        }
        self.watch_outside_clicks();
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(panel) = this.upgrade() {
                panel.window.set_revealed(true);
                let w = panel.window.window();
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
    fn keep_frame(&self) {
        let result = match self.window.window().window_handle().window_handle() {
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
        let window = self.window.window();
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

/// Pins `folder` at the end of the pinned list, unless it is pinned
/// already or the list is full.
fn pin_folder(settings: &mut Settings, folder: &Path) {
    let pinned = &mut settings.pinned_folders;
    if pinned.len() < MAX_PINNED && !pinned.iter().any(|p| p == folder) {
        pinned.push(folder.to_path_buf());
    }
}

/// Unpins the folder in row `index` of the pinned list.
fn unpin_folder(settings: &mut Settings, index: usize) {
    if index < settings.pinned_folders.len() {
        settings.pinned_folders.remove(index);
    }
}

/// Moves the hold delay `steps` steps up (or down, when negative), within
/// its bounds.
fn step_hold(settings: &mut Settings, steps: i32) {
    let ms = i64::from(settings.move_hold_ms) + i64::from(steps) * i64::from(MOVE_HOLD_STEP_MS);
    let ms = ms.clamp(i64::from(MIN_MOVE_HOLD_MS), i64::from(MAX_MOVE_HOLD_MS));
    settings.move_hold_ms = u32::try_from(ms).unwrap_or(settings.move_hold_ms);
}

/// What a key pressed while recording a shortcut means.
#[derive(Debug, PartialEq)]
enum KeyPress {
    /// Esc: the recording ends without a change.
    Cancel,
    /// A modifier key: keep listening.
    Modifier,
    /// Any other key, with the modifiers held, as a shortcut spec (see
    /// [`parse_shortcut`]). A key that is not an ASCII letter or digit
    /// gives a spec that does not parse.
    Combo(String),
}

/// What pressing `key` with `modifiers` held means to the recorder. The key
/// is taken by its place on the keyboard, as the hot key is registered,
/// so Option, which changes the character typed, does not change it.
fn key_press(modifiers: ModifiersState, key: PhysicalKey) -> KeyPress {
    let code = match key {
        PhysicalKey::Code(KeyCode::Escape) => return KeyPress::Cancel,
        PhysicalKey::Code(
            KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::SuperLeft
            | KeyCode::SuperRight
            | KeyCode::Meta
            | KeyCode::Hyper
            | KeyCode::Fn
            | KeyCode::FnLock
            | KeyCode::CapsLock,
        ) => return KeyPress::Modifier,
        PhysicalKey::Code(code) => Some(code),
        PhysicalKey::Unidentified(_) => None,
    };
    let mut parts: Vec<String> = [
        (modifiers.control_key(), "ctrl"),
        (modifiers.alt_key(), "alt"),
        (modifiers.shift_key(), "shift"),
        (modifiers.super_key(), "cmd"),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .map(|(_, name)| name.to_owned())
    .collect();
    // "?" stands for a key a shortcut cannot use.
    parts.push(code.and_then(key_char).unwrap_or('?').to_string());
    KeyPress::Combo(parts.join("+"))
}

/// The lower-case letter or the digit of a letter or digit key.
fn key_char(code: KeyCode) -> Option<char> {
    const KEYS: [(KeyCode, char); 36] = [
        (KeyCode::KeyA, 'a'),
        (KeyCode::KeyB, 'b'),
        (KeyCode::KeyC, 'c'),
        (KeyCode::KeyD, 'd'),
        (KeyCode::KeyE, 'e'),
        (KeyCode::KeyF, 'f'),
        (KeyCode::KeyG, 'g'),
        (KeyCode::KeyH, 'h'),
        (KeyCode::KeyI, 'i'),
        (KeyCode::KeyJ, 'j'),
        (KeyCode::KeyK, 'k'),
        (KeyCode::KeyL, 'l'),
        (KeyCode::KeyM, 'm'),
        (KeyCode::KeyN, 'n'),
        (KeyCode::KeyO, 'o'),
        (KeyCode::KeyP, 'p'),
        (KeyCode::KeyQ, 'q'),
        (KeyCode::KeyR, 'r'),
        (KeyCode::KeyS, 's'),
        (KeyCode::KeyT, 't'),
        (KeyCode::KeyU, 'u'),
        (KeyCode::KeyV, 'v'),
        (KeyCode::KeyW, 'w'),
        (KeyCode::KeyX, 'x'),
        (KeyCode::KeyY, 'y'),
        (KeyCode::KeyZ, 'z'),
        (KeyCode::Digit0, '0'),
        (KeyCode::Digit1, '1'),
        (KeyCode::Digit2, '2'),
        (KeyCode::Digit3, '3'),
        (KeyCode::Digit4, '4'),
        (KeyCode::Digit5, '5'),
        (KeyCode::Digit6, '6'),
        (KeyCode::Digit7, '7'),
        (KeyCode::Digit8, '8'),
        (KeyCode::Digit9, '9'),
    ];
    KEYS.iter().find(|(k, _)| *k == code).map(|(_, c)| *c)
}

/// Why the recorded shortcut `spec` is refused before trying to register
/// it, or `None` when it may be tried. Shift alone only types a capital, so
/// it is not a modifier for this.
fn shortcut_refusal(spec: &str) -> Option<&'static str> {
    let shift_only = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    match parse_shortcut(spec) {
        None => Some(NOT_A_SHORTCUT),
        Some((m, _)) if m == shift_only => Some(NOT_A_SHORTCUT),
        Some(_) if is_reserved_shortcut(spec) => Some(RESERVED_SHORTCUT),
        Some(_) => None,
    }
}

/// How applying a recorded shortcut ended.
#[derive(Debug)]
enum ShortcutOutcome {
    /// It is registered and saved (or was the one saved already).
    Applied,
    /// It was refused, for the reason shown under the field; the old one is
    /// registered again and stays saved.
    Refused(&'static str),
    /// It could not be saved; the old one is registered again.
    NotSaved(io::Error),
}

/// Applies the recorded shortcut `spec` in place of `old`: refuses one that
/// is invalid or reserved without trying it; otherwise registers it with
/// `register` (which frees the old key first) and saves it with `save` only
/// once that worked, and only if it differs from `old`. When registering or
/// saving fails, `old` is registered again. The raw reason a registration
/// failed only goes to the log.
fn apply_shortcut(
    spec: &str,
    old: &str,
    register: &dyn Fn(&str) -> Result<(), String>,
    save: impl FnOnce(&str) -> io::Result<()>,
) -> ShortcutOutcome {
    let restore = || {
        if let Err(e) = register(old) {
            warn!("pocket shortcut {old:?} unavailable: {e}");
        }
    };
    if let Some(reason) = shortcut_refusal(spec) {
        debug!("shortcut {spec:?} refused: {reason}");
        return ShortcutOutcome::Refused(reason);
    }
    if let Err(reason) = register(spec) {
        warn!("pocket shortcut {spec:?} refused: {reason}");
        restore();
        return ShortcutOutcome::Refused(UNUSABLE_SHORTCUT);
    }
    if spec != old
        && let Err(e) = save(spec)
    {
        warn!("cannot save settings: {e}");
        restore();
        return ShortcutOutcome::NotSaved(e);
    }
    ShortcutOutcome::Applied
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
        assert_eq!(
            s.hidden_actions,
            [ActionId::Pocket, ActionId::CopyTo, ActionId::Archive]
        );
        move_action(&mut s, 5, 1);
        assert_eq!(
            s.hidden_actions,
            [ActionId::CopyTo, ActionId::Pocket, ActionId::Archive]
        );
        assert_eq!(s.actions.len(), 4);
        let before = s.clone();
        move_action(&mut s, 9, 0);
        assert_eq!(s, before);
    }

    #[test]
    fn pinning_refuses_a_duplicate_and_stops_at_seven() {
        let mut s = Settings::default();
        pin_folder(&mut s, Path::new("/a"));
        pin_folder(&mut s, Path::new("/a"));
        assert_eq!(s.pinned_folders, [PathBuf::from("/a")]);
        for i in 1..10 {
            pin_folder(&mut s, &PathBuf::from(format!("/f{i}")));
        }
        assert_eq!(s.pinned_folders.len(), MAX_PINNED);
        assert_eq!(s.pinned_folders.last(), Some(&PathBuf::from("/f6")));
        unpin_folder(&mut s, 0);
        assert_eq!(s.pinned_folders.first(), Some(&PathBuf::from("/f1")));
        let before = s.clone();
        unpin_folder(&mut s, 9);
        assert_eq!(s, before);
    }

    #[test]
    fn hold_steps_by_100_within_200_to_1000() {
        let mut s = Settings::default();
        step_hold(&mut s, 1);
        assert_eq!(s.move_hold_ms, 500);
        step_hold(&mut s, -1);
        step_hold(&mut s, -1);
        assert_eq!(s.move_hold_ms, 300);
        step_hold(&mut s, -5);
        assert_eq!(s.move_hold_ms, MIN_MOVE_HOLD_MS);
        step_hold(&mut s, 20);
        assert_eq!(s.move_hold_ms, MAX_MOVE_HOLD_MS);
    }

    #[test]
    fn recorder_reads_the_key_by_its_place_with_the_modifiers_held() {
        let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
        let key = |code| PhysicalKey::Code(code);
        assert_eq!(
            key_press(ctrl_alt, key(KeyCode::KeyK)),
            KeyPress::Combo("ctrl+alt+k".into())
        );
        let all = ctrl_alt | ModifiersState::SHIFT | ModifiersState::SUPER;
        assert_eq!(
            key_press(all, key(KeyCode::Digit7)),
            KeyPress::Combo("ctrl+alt+shift+cmd+7".into())
        );
        assert_eq!(
            key_press(ModifiersState::SUPER, key(KeyCode::KeyQ)),
            KeyPress::Combo("cmd+q".into())
        );
        assert_eq!(key_press(ctrl_alt, key(KeyCode::Escape)), KeyPress::Cancel);
        assert_eq!(
            key_press(ModifiersState::CONTROL, key(KeyCode::AltLeft)),
            KeyPress::Modifier
        );
        // Keys a shortcut cannot use give a spec that does not parse.
        let KeyPress::Combo(f1) = key_press(ctrl_alt, key(KeyCode::F1)) else {
            panic!("F1 is a combination");
        };
        assert!(parse_shortcut(&f1).is_none());
        let KeyPress::Combo(bare) = key_press(ModifiersState::empty(), key(KeyCode::KeyP)) else {
            panic!("P is a combination");
        };
        assert!(parse_shortcut(&bare).is_none());
    }

    #[test]
    fn refused_shortcuts_say_why() {
        assert_eq!(shortcut_refusal("ctrl+alt+k"), None);
        assert_eq!(shortcut_refusal("p"), Some(NOT_A_SHORTCUT));
        assert_eq!(shortcut_refusal("ctrl+alt+?"), Some(NOT_A_SHORTCUT));
        assert_eq!(shortcut_refusal("shift+k"), Some(NOT_A_SHORTCUT));
        assert_eq!(shortcut_refusal("cmd+q"), Some(RESERVED_SHORTCUT));
        assert_eq!(shortcut_refusal("cmd+h"), Some(RESERVED_SHORTCUT));
        assert_eq!(shortcut_refusal("cmd+shift+4"), Some(RESERVED_SHORTCUT));
    }

    /// Runs [`apply_shortcut`] with a register that fails for the specs in
    /// `unregistrable` and a save that fails when `save_fails`, and returns
    /// the outcome with every call made, in order.
    fn apply_with_fakes(
        spec: &str,
        old: &str,
        unregistrable: &[&str],
        save_fails: bool,
    ) -> (ShortcutOutcome, Vec<String>) {
        let calls = RefCell::new(Vec::new());
        let register = |s: &str| {
            calls.borrow_mut().push(format!("register {s}"));
            if unregistrable.contains(&s) {
                Err("already taken".to_owned())
            } else {
                Ok(())
            }
        };
        let save = |s: &str| {
            calls.borrow_mut().push(format!("save {s}"));
            if save_fails {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(())
            }
        };
        let outcome = apply_shortcut(spec, old, &register, save);
        (outcome, calls.into_inner())
    }

    #[test]
    fn a_shortcut_is_saved_only_after_it_registered() {
        let (outcome, calls) = apply_with_fakes("ctrl+alt+k", "ctrl+alt+p", &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Applied));
        assert_eq!(calls, ["register ctrl+alt+k", "save ctrl+alt+k"]);
        // The one saved already: registered (a no-op for the real one), not
        // saved again.
        let (outcome, calls) = apply_with_fakes("ctrl+alt+p", "ctrl+alt+p", &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Applied));
        assert_eq!(calls, ["register ctrl+alt+p"]);
    }

    #[test]
    fn a_refused_shortcut_is_never_tried() {
        let (outcome, calls) = apply_with_fakes("cmd+q", "ctrl+alt+p", &[], false);
        assert!(matches!(
            outcome,
            ShortcutOutcome::Refused(RESERVED_SHORTCUT)
        ));
        assert!(calls.is_empty());
        let (outcome, calls) = apply_with_fakes("ctrl+alt+?", "ctrl+alt+p", &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Refused(NOT_A_SHORTCUT)));
        assert!(calls.is_empty());
    }

    #[test]
    fn a_shortcut_that_cannot_register_brings_the_old_one_back_unsaved() {
        let (outcome, calls) = apply_with_fakes("ctrl+alt+k", "ctrl+alt+p", &["ctrl+alt+k"], false);
        assert!(matches!(
            outcome,
            ShortcutOutcome::Refused(UNUSABLE_SHORTCUT)
        ));
        assert_eq!(calls, ["register ctrl+alt+k", "register ctrl+alt+p"]);
    }

    #[test]
    fn a_failed_save_brings_the_old_shortcut_back() {
        let (outcome, calls) = apply_with_fakes("ctrl+alt+k", "ctrl+alt+p", &[], true);
        assert!(matches!(outcome, ShortcutOutcome::NotSaved(_)));
        assert_eq!(
            calls,
            [
                "register ctrl+alt+k",
                "save ctrl+alt+k",
                "register ctrl+alt+p"
            ]
        );
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
