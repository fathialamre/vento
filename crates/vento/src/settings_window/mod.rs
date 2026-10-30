//! The Settings window: an ordinary titled window with a sidebar of pages,
//! one per feature, opened from the panel's and the menu-bar menu's
//! Settings… (on Linux also by a second launch). Every control applies and
//! saves its change at once.
//!
//! The window is created on the first open and dropped once it has been
//! closed for [`crate::lazy_window::IDLE`], taking its native window and
//! renderer with it. A second open while it is open brings it forward.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::{Rc, Weak};
use std::time::Duration;

use log::{debug, warn};
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
use vento_core::geometry::{Layout, Rect, screen_for};
use vento_core::i18n::errors::io_reason;
use vento_core::i18n::{LANGUAGES, fill, lookup, tr, trc};
use vento_core::paths::PathFormat;
use vento_core::settings::{
    Appearance, MAX_MOVE_HOLD_MS, MAX_PINNED, MIN_MOVE_HOLD_MS, MOVE_HOLD_STEP_MS, PocketView,
    Settings, SettingsError, ThemeName, is_reserved_shortcut, move_row, parse_shortcut,
};
use vento_core::trigger::{Modifier, Modifiers};
use vento_platform::{Platform, TerminalApp};

use crate::appearance::{self, show_menu_colors};
use crate::lazy_window::LazyWindow;
use crate::panel::set_rows;
use crate::pocket::{display_name, to_image};
use crate::settings_hub::SettingsHub;
use crate::ui::{ActionRow, FolderRow, Icons, PageItem, PanelIcons, SettingsWindow, Theme};
use crate::wheel::action_icon;

/// The window's size as it first opens, in logical points (matches
/// `settings_window.slint`).
const WIDTH: f64 = 680.0;
const HEIGHT: f64 = 520.0;

/// The choices of the window's segmented controls, in the order shown.
const APPEARANCES: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];
const LAYOUTS: [Layout; 2] = [Layout::Honeycomb, Layout::Petals];
const TRIGGERS: [Modifier; 4] = [
    Modifier::Ctrl,
    Modifier::Alt,
    Modifier::Shift,
    Modifier::Meta,
];
/// What the Trigger choices are called: the Mac's names on a Mac, GNOME's
/// elsewhere.
#[cfg(target_os = "macos")]
const TRIGGER_NAMES: [&str; 4] = ["Control", "Option", "Shift", "Command"];
#[cfg(not(target_os = "macos"))]
const TRIGGER_NAMES: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];
const PATH_FORMATS: [PathFormat; 2] = [PathFormat::Plain, PathFormat::ShellQuoted];

/// The licences of Vento and of what it bundles: the README's License
/// section, which links each licence file.
const LICENSES_URL: &str = "https://github.com/fathialamre/vento#license";

/// Size of the Finder icons in the Move page, in points.
const FOLDER_ICON_PX: u32 = 16;

/// Why a recorded shortcut was refused, as the English text that
/// [`refusal_text`] translates.
const NOT_A_SHORTCUT: &str = "Use a modifier with a letter or digit";
const RESERVED_SHORTCUT: &str = "macOS uses this shortcut";
const UNUSABLE_SHORTCUT: &str = "This shortcut can't be used";

/// A page of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    General,
    Wheel,
    Move,
    Pocket,
    Clipboard,
    CopyFromScreen,
    KeepAwake,
    About,
}

impl Page {
    /// The page's number in the order above. The window picks the page's
    /// body by it (matches `settings_window.slint`), so a page's name can
    /// change with the language.
    fn id(self) -> i32 {
        self as i32
    }

    /// The page's name in `closed_how_to` in the settings file.
    fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Wheel => "wheel",
            Self::Move => "move",
            Self::Pocket => "pocket",
            Self::Clipboard => "clipboard",
            Self::CopyFromScreen => "copy_from_screen",
            Self::KeepAwake => "keep_awake",
            Self::About => "about",
        }
    }

    /// The page's name, in the sidebar and over the page, in the interface
    /// language. Shown only: the page is told apart by [`Page::id`].
    fn label(self) -> String {
        match self {
            Self::General => tr("General"),
            Self::Wheel => tr("Wheel"),
            // The wheel's action "Move" is a verb: "نقل"; the page is "النقل".
            Self::Move => trc("page", "Move"),
            Self::Pocket => tr("Pocket"),
            Self::Clipboard => tr("Clipboard"),
            Self::CopyFromScreen => tr("Copy from screen"),
            Self::KeepAwake => tr("Keep Awake"),
            Self::About => tr("About"),
        }
    }

    /// The page's icon in the sidebar.
    fn icon(self, window: &SettingsWindow) -> Image {
        let icons = window.global::<Icons>();
        let panel = window.global::<PanelIcons>();
        match self {
            Self::General => panel.get_general(),
            Self::Wheel => panel.get_wheel(),
            Self::Move => icons.get_move(),
            Self::Pocket => icons.get_pocket(),
            Self::Clipboard => icons.get_clipboard(),
            Self::CopyFromScreen => panel.get_scan_text(),
            Self::KeepAwake => panel.get_awake(),
            Self::About => icons.get_info(),
        }
    }
}

/// The window's pages in sidebar order; Copy from screen only where this
/// computer can read text from the screen.
pub fn pages(screen_text_supported: bool) -> Vec<Page> {
    use Page::*;
    [
        General,
        Wheel,
        Move,
        Pocket,
        Clipboard,
        CopyFromScreen,
        KeepAwake,
        About,
    ]
    .into_iter()
    .filter(|&page| page != CopyFromScreen || screen_text_supported)
    .collect()
}

/// Opens or closes `page`'s How to card in `settings`.
fn set_how_to_open(settings: &mut Settings, page: Page, open: bool) {
    settings.closed_how_to.retain(|k| k != page.key());
    if !open {
        settings.closed_how_to.push(page.key().to_owned());
    }
}

/// Which of vento's shortcuts is meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutFor {
    /// Opens the island on the Pocket.
    Pocket,
    /// Opens the island on the clipboard history.
    Clipboard,
    /// Starts Copy from screen.
    ScreenText,
}

impl ShortcutFor {
    /// The two other shortcuts.
    fn others(self) -> [Self; 2] {
        match self {
            Self::Pocket => [Self::Clipboard, Self::ScreenText],
            Self::Clipboard => [Self::Pocket, Self::ScreenText],
            Self::ScreenText => [Self::Pocket, Self::Clipboard],
        }
    }

    /// This shortcut as `settings` saves it.
    fn saved(self, settings: &Settings) -> &str {
        match self {
            Self::Pocket => &settings.pocket_shortcut,
            Self::Clipboard => &settings.clipboard_shortcut,
            Self::ScreenText => &settings.screen_text_shortcut,
        }
    }

    /// Saves `spec` as this shortcut in `settings`.
    fn save_to(self, settings: &mut Settings, spec: &str) {
        match self {
            Self::Pocket => settings.pocket_shortcut = spec.to_owned(),
            Self::Clipboard => settings.clipboard_shortcut = spec.to_owned(),
            Self::ScreenText => settings.screen_text_shortcut = spec.to_owned(),
        }
    }

    /// Where the shortcut's refusal is kept in
    /// [`SettingsWindowController::refusals`].
    fn index(self) -> usize {
        match self {
            Self::Pocket => 0,
            Self::Clipboard => 1,
            Self::ScreenText => 2,
        }
    }

    /// Why a recorded shortcut equal to this one is refused: the English
    /// text, which [`refusal_text`] translates.
    fn in_use(self) -> &'static str {
        match self {
            Self::Pocket => "The Pocket uses this shortcut",
            Self::Clipboard => "The Clipboard uses this shortcut",
            Self::ScreenText => "Copy from screen uses this shortcut",
        }
    }
}

/// Registers a shortcut in place of its current one, or says why it cannot
/// be registered.
pub type TryShortcut = Box<dyn Fn(ShortcutFor, &str) -> Result<(), String>>;

/// Opens, fills and closes the Settings window.
pub struct SettingsWindowController {
    platform: Rc<dyn Platform>,
    hub: Rc<SettingsHub>,
    /// The window while it is open, and for a while after it closes. Use
    /// [`SettingsWindowController::window`] or
    /// [`SettingsWindowController::existing`].
    window: LazyWindow<SettingsWindow>,
    /// The page the window was on when it last closed, which it opens on.
    page: Cell<Page>,
    /// Whether the window has a Copy from screen page.
    screen_text_supported: bool,
    /// Folders recently moved or copied to, shared with the wheel and the
    /// executor.
    recent: Rc<RefCell<RecentDestinations>>,
    /// Registers a new Pocket, Clipboard or Copy from screen shortcut
    /// (freeing its old one first).
    try_shortcut: TryShortcut,
    /// Empties the clipboard history at once, with no Undo.
    on_clear_history: Box<dyn Fn()>,
    /// The shortcut the recording field is for.
    recording: Cell<ShortcutFor>,
    /// Why each shortcut was last refused (indexed by [`ShortcutFor::index`]),
    /// as the English text, so the line under the field can be written again
    /// in another language.
    refusals: [Cell<Option<&'static str>>; 3],
    /// The home folder, shown as `~` in folder places.
    home: Option<PathBuf>,
    /// The Wheel page's action rows.
    actions: Rc<VecModel<ActionRow>>,
    /// The Move page's pinned and recent folder rows, and the folders the
    /// recent rows stand for.
    pinned: Rc<VecModel<FolderRow>>,
    recent_rows: Rc<VecModel<FolderRow>>,
    recent_paths: RefCell<Vec<PathBuf>>,
    /// Finder icons of the listed folders, by path (`None`: it has none).
    folder_icons: RefCell<HashMap<PathBuf, Option<Image>>>,
    /// The installed terminals, as the Terminal menu lists them.
    terminals: RefCell<Vec<TerminalApp>>,
    /// The window has been placed since it was made (a new window opens
    /// centred; one shown again stays where the user left it).
    placed: Cell<bool>,
    open: Cell<bool>,
    /// The modifier keys held now, for the shortcut recorder.
    modifiers: Cell<ModifiersState>,
    /// Runs when the user turns to the window; main sets it to close the
    /// Quick panel and the Info window.
    on_activated: RefCell<Option<Box<dyn Fn()>>>,
    /// Whether another vento window (the Quick panel, the Info window) is
    /// open; main sets it. That window keeps the focus when this one closes.
    other_window_open: RefCell<Option<Box<dyn Fn() -> bool>>>,
    this: Weak<Self>,
}

impl SettingsWindowController {
    /// Creates the (closed) window controller; the window comes with the
    /// first open. `recent` is the recent-folders list the wheel and the
    /// executor share; `screen_text_supported` says whether the window has
    /// a Copy from screen page; `try_shortcut` registers a recorded Pocket,
    /// Clipboard or Copy from screen shortcut; `on_clear_history` empties
    /// the clipboard history.
    pub fn new(
        platform: Rc<dyn Platform>,
        hub: Rc<SettingsHub>,
        recent: Rc<RefCell<RecentDestinations>>,
        screen_text_supported: bool,
        try_shortcut: TryShortcut,
        on_clear_history: Box<dyn Fn()>,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            hub,
            window: LazyWindow::new(),
            page: Cell::new(Page::General),
            screen_text_supported,
            recent,
            try_shortcut,
            on_clear_history,
            recording: Cell::new(ShortcutFor::Pocket),
            refusals: [Cell::new(None), Cell::new(None), Cell::new(None)],
            home: std::env::var_os("HOME").map(PathBuf::from),
            actions: Rc::new(VecModel::default()),
            pinned: Rc::new(VecModel::default()),
            recent_rows: Rc::new(VecModel::default()),
            recent_paths: RefCell::new(Vec::new()),
            folder_icons: RefCell::new(HashMap::new()),
            terminals: RefCell::new(Vec::new()),
            placed: Cell::new(false),
            open: Cell::new(false),
            modifiers: Cell::new(ModifiersState::empty()),
            on_activated: RefCell::new(None),
            other_window_open: RefCell::new(None),
            this: this.clone(),
        });
        // A saved change clears the load error: drop the notice with it.
        // A hand edit picked up on opening, or a change made elsewhere,
        // changes what the controls show. With no window there are no
        // controls to update.
        let this = Rc::downgrade(&controller);
        controller.hub.on_change(Box::new(move |_, _| {
            if let Some(controller) = this.upgrade() {
                controller.show_notice();
                controller.show_settings();
            }
        }));
        controller
    }

    /// Has the window call `f` when the user turns to it: a click in it, or
    /// it becoming the key window. The Quick panel and the Info window stay
    /// above every other window, and their watch for clicks outside them
    /// only sees clicks in other apps, so main closes them from here.
    pub fn set_on_activated(&self, f: Box<dyn Fn()>) {
        *self.on_activated.borrow_mut() = Some(f);
    }

    /// Has the window ask `f` whether another vento window is open as it
    /// closes: focus is then left with that window instead of handed back
    /// to the app that had it.
    pub fn set_other_window_open(&self, f: Box<dyn Fn() -> bool>) {
        *self.other_window_open.borrow_mut() = Some(f);
    }

    /// The window, created and connected if there is none.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    fn window(&self) -> SettingsWindow {
        self.window.get_or_create(|| {
            let window = SettingsWindow::new().expect("cannot create the settings window");
            // A Mac-window page rather than the panel's tint.
            window.global::<Theme>().set_clean(true);
            appearance::follow(&window, appearance::Look::Full);
            window.set_actions(ModelRc::from(self.actions.clone()));
            window.set_pinned(ModelRc::from(self.pinned.clone()));
            window.set_recent(ModelRc::from(self.recent_rows.clone()));
            window.set_screen_text_supported(self.screen_text_supported);
            let trigger_names: Vec<SharedString> = TRIGGER_NAMES.map(Into::into).into();
            window.set_trigger_names(ModelRc::from(trigger_names.as_slice()));
            window.set_version(env!("CARGO_PKG_VERSION").into());
            let theme_names: Vec<SharedString> =
                ThemeName::ALL.iter().map(|t| t.label().into()).collect();
            window.set_theme_names(ModelRc::from(theme_names.as_slice()));
            let pages = pages(self.screen_text_supported);
            let items: Vec<PageItem> = pages
                .iter()
                .map(|&page| PageItem {
                    icon: page.icon(&window),
                    label: page.label().into(),
                    id: page.id(),
                })
                .collect();
            window.set_pages(ModelRc::new(VecModel::from(items)));
            window.set_page(choice_index(&pages, self.page.get()));
            // A new window opens centred.
            self.placed.set(false);
            self.connect(&window);
            window
        })
    }

    /// The window, if it has one now.
    fn existing(&self) -> Option<SettingsWindow> {
        self.window.get()
    }

    fn connect(&self, window: &SettingsWindow) {
        let this = self.this.clone();
        window.window().on_close_requested(move || {
            if let Some(controller) = this.upgrade() {
                controller.close();
            }
            CloseRequestResponse::KeepWindowShown
        });
        let this = self.this.clone();
        window.window().on_winit_window_event(move |_, event| {
            let Some(controller) = this.upgrade() else {
                return EventResult::Propagate;
            };
            controller.turned_to(turn(event));
            match event {
                WindowEvent::ModifiersChanged(modifiers) => {
                    controller.modifiers.set(modifiers.state());
                }
                WindowEvent::KeyboardInput {
                    event,
                    is_synthetic: false,
                    ..
                } if controller.is_recording() => return controller.shortcut_key(event),
                _ => {}
            }
            EventResult::Propagate
        });
        let this = self.this.clone();
        window.on_close_requested(move || {
            if let Some(controller) = this.upgrade() {
                controller.close();
            }
        });
        let this = self.this.clone();
        window.on_open_settings_file(move || {
            if let Some(controller) = this.upgrade() {
                crate::tray::open_settings_file(&*controller.platform);
            }
        });
        window.on_open_licenses(|| open_link(LICENSES_URL));
        self.connect_controls(window);
    }

    /// Every control applies its change at once.
    fn connect_controls(&self, window: &SettingsWindow) {
        let this = self.this.clone();
        window.on_appearance_chosen(move |i| {
            if let (Some(controller), Some(appearance)) = (this.upgrade(), choice(&APPEARANCES, i))
            {
                controller.apply(|s| s.appearance = appearance);
            }
        });
        let this = self.this.clone();
        window.on_language_chosen(move |i| {
            if let Some(controller) = this.upgrade() {
                let setting = language_setting(i);
                controller.apply(|s| s.language = setting.clone());
            }
        });
        let this = self.this.clone();
        window.on_theme_chosen(move |i| {
            if let (Some(controller), Some(theme)) = (this.upgrade(), choice(&ThemeName::ALL, i)) {
                controller.apply(|s| s.theme = theme);
            }
        });
        let this = self.this.clone();
        window.on_layout_chosen(move |i| {
            if let (Some(controller), Some(layout)) = (this.upgrade(), choice(&LAYOUTS, i)) {
                controller.apply(|s| s.layout = layout);
            }
        });
        let this = self.this.clone();
        window.on_action_toggled(move |i, on| {
            if let (Some(controller), Ok(i)) = (this.upgrade(), usize::try_from(i)) {
                controller.apply(|s| show_action(s, i, on));
            }
        });
        let this = self.this.clone();
        window.on_action_moved(move |from, to| {
            if let (Some(controller), Ok(from), Ok(to)) =
                (this.upgrade(), usize::try_from(from), usize::try_from(to))
            {
                controller.apply(|s| move_action(s, from, to));
            }
        });
        let this = self.this.clone();
        window.on_trigger_chosen(move |i| {
            if let (Some(controller), Some(trigger)) = (this.upgrade(), choice(&TRIGGERS, i)) {
                controller.apply(|s| s.trigger = trigger);
            }
        });
        let this = self.this.clone();
        window.on_terminal_chosen(move |i| {
            let Some(controller) = this.upgrade() else {
                return;
            };
            let id = usize::try_from(i)
                .ok()
                .and_then(|i| controller.terminals.borrow().get(i).map(|t| t.id.clone()));
            if let Some(id) = id {
                controller.apply(|s| s.terminal = Some(id.clone()));
            }
        });
        let this = self.this.clone();
        window.on_path_format_chosen(move |i| {
            if let (Some(controller), Some(format)) = (this.upgrade(), choice(&PATH_FORMATS, i)) {
                controller.apply(|s| s.copy_path_format = format);
            }
        });
        self.connect_move_page(window);
        let this = self.this.clone();
        window.on_remove_after_drop_toggled(move |on| {
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| s.pocket_remove_after_drop = on);
            }
        });
        let this = self.this.clone();
        window.on_keep_copies_toggled(move |on| {
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| s.pocket_keep_copies = on);
            }
        });
        let this = self.this.clone();
        window.on_pocket_view_chosen(move |i| {
            if let (Some(controller), Some(look)) = (this.upgrade(), choice(&PocketView::ALL, i)) {
                controller.apply(|s| s.pocket_view = look);
            }
        });
        let this = self.this.clone();
        window.on_shake_to_open_toggled(move |on| {
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| s.shake_to_open = on);
            }
        });
        self.connect_clipboard_page(window);
        let this = self.this.clone();
        window.on_awake_screen_off_toggled(move |on| {
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| s.keep_awake_screen_off = on);
            }
        });
        let this = self.this.clone();
        window.on_how_to_toggled(move |id, open| {
            let Some(page) = pages(true).into_iter().find(|p| p.id() == id) else {
                return;
            };
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| set_how_to_open(s, page, open));
            }
        });
        // While recording, the menu bar is away, so ⌘Q and ⌘H reach the
        // recorder instead of quitting or hiding vento.
        let this = self.this.clone();
        window.on_shortcut_recording_started(move || {
            if let Some(controller) = this.upgrade() {
                controller.recording.set(ShortcutFor::Pocket);
                controller.platform.suspend_app_menu();
            }
        });
        let this = self.this.clone();
        window.on_clip_shortcut_recording_started(move || {
            if let Some(controller) = this.upgrade() {
                controller.recording.set(ShortcutFor::Clipboard);
                controller.platform.suspend_app_menu();
            }
        });
        let this = self.this.clone();
        window.on_screen_shortcut_recording_started(move || {
            if let Some(controller) = this.upgrade() {
                controller.recording.set(ShortcutFor::ScreenText);
                controller.platform.suspend_app_menu();
            }
        });
        let this = self.this.clone();
        window.on_shortcut_recording_stopped(move || {
            if let Some(controller) = this.upgrade() {
                controller.platform.restore_app_menu();
            }
        });
    }

    fn connect_clipboard_page(&self, window: &SettingsWindow) {
        let this = self.this.clone();
        window.on_clip_history_toggled(move |on| {
            let Some(controller) = this.upgrade() else {
                return;
            };
            // Turning the history off disables its shortcut field.
            if !on
                && let Some(window) = controller.existing()
                && window.get_clip_shortcut_recording()
            {
                window.invoke_stop_recording();
            }
            controller.apply(|s| s.clipboard_history = on);
        });
        let this = self.this.clone();
        window.on_clip_history_cleared(move || {
            if let Some(controller) = this.upgrade() {
                (controller.on_clear_history)();
            }
        });
    }

    fn connect_move_page(&self, window: &SettingsWindow) {
        let this = self.this.clone();
        window.on_add_folder(move || {
            if let Some(controller) = this.upgrade() {
                controller.add_folder();
            }
        });
        let this = self.this.clone();
        window.on_pinned_moved(move |from, to| {
            if let (Some(controller), Ok(from), Ok(to)) =
                (this.upgrade(), usize::try_from(from), usize::try_from(to))
            {
                controller.apply(|s| move_row(&mut s.pinned_folders, from, to));
            }
        });
        let this = self.this.clone();
        window.on_unpinned(move |i| {
            if let (Some(controller), Ok(i)) = (this.upgrade(), usize::try_from(i)) {
                controller.apply(|s| unpin_folder(s, i));
            }
        });
        let this = self.this.clone();
        window.on_recent_pinned(move |i| {
            let Some(controller) = this.upgrade() else {
                return;
            };
            let folder = usize::try_from(i)
                .ok()
                .and_then(|i| controller.recent_paths.borrow().get(i).cloned());
            if let Some(folder) = folder {
                controller.apply(|s| pin_folder(s, &folder));
            }
        });
        let this = self.this.clone();
        window.on_recent_cleared(move || {
            if let Some(controller) = this.upgrade() {
                controller.clear_recent();
            }
        });
        let this = self.this.clone();
        window.on_hold_stepped(move |steps| {
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| step_hold(s, steps));
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
        // be on their way when it returns, so the window takes the focus
        // back on the next turn of the event loop.
        let picked = self.platform.pick_folder(None);
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(controller) = this.upgrade() {
                controller.regain_focus();
            }
        });
        if let Some(folder) = picked {
            self.apply(|s| pin_folder(s, &folder));
        }
    }

    /// Tells main the user turned to the window, at once for `Turn::Now`,
    /// and for `Turn::IfStillKey` on the next turn of the event loop if the
    /// window is still key then.
    fn turned_to(&self, turn: Turn) {
        match turn {
            Turn::No => {}
            Turn::Now => self.activated(),
            Turn::IfStillKey => {
                let this = self.this.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(controller) = this.upgrade()
                        && controller.is_open()
                        && controller.has_focus()
                    {
                        controller.activated();
                    }
                });
            }
        }
    }

    fn activated(&self) {
        if let Some(f) = self.on_activated.borrow().as_ref() {
            f();
        }
    }

    /// Whether the window is the key window (it takes the keyboard).
    fn has_focus(&self) -> bool {
        self.existing()
            .and_then(|window| window.window().with_winit_window(|w| w.has_focus()))
            .unwrap_or(false)
    }

    /// Whether another vento window is open, as main says.
    fn other_window_open(&self) -> bool {
        self.other_window_open
            .borrow()
            .as_ref()
            .is_some_and(|f| f())
    }

    /// Makes the open window the key window again.
    fn regain_focus(&self) {
        let Some(window) = self.existing().filter(|_| self.is_open()) else {
            return;
        };
        self.platform.activate_app();
        window.window().with_winit_window(|w| w.focus_window());
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
        if let Some(window) = self.existing() {
            self.show_folders(&window, &self.hub.current());
        }
    }

    /// A key press while the shortcut field records: a combination ends the
    /// recording, and the window's own key handling never sees it. Esc (the
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

    /// Whether a shortcut field is waiting for a key combination.
    fn is_recording(&self) -> bool {
        self.existing().is_some_and(|window| {
            window.get_shortcut_recording()
                || window.get_clip_shortcut_recording()
                || window.get_screen_shortcut_recording()
        })
    }

    /// Ends the recording with `spec` and applies it to the shortcut being
    /// recorded the way [`apply_shortcut`] says: the reason a refused one
    /// was refused shows under its field, a failed save in the notice.
    fn record_shortcut(&self, spec: &str) {
        let Some(window) = self.existing() else {
            return;
        };
        let target = self.recording.get();
        window.invoke_stop_recording();
        self.hub.refresh_from_disk();
        let outcome = apply_shortcut(
            target,
            spec,
            &self.hub.current(),
            &*self.try_shortcut,
            |change| self.hub.update(change),
        );
        let error = match outcome {
            ShortcutOutcome::Applied => {
                self.refusals[target.index()].set(None);
                SharedString::new()
            }
            ShortcutOutcome::Refused(reason) => {
                self.refusals[target.index()].set(Some(reason));
                refusal_text(reason).into()
            }
            ShortcutOutcome::NotSaved(e) => {
                self.refusals[target.index()].set(None);
                window.set_notice(save_error_text(&e).into());
                SharedString::new()
            }
        };
        match target {
            ShortcutFor::Pocket => window.set_shortcut_error(error),
            ShortcutFor::Clipboard => window.set_clip_shortcut_error(error),
            ShortcutFor::ScreenText => window.set_screen_shortcut_error(error),
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
            if let Some(window) = self.existing() {
                window.set_notice(save_error_text(&e).into());
            }
        }
        self.show_settings();
    }

    /// Sets every control from the settings in effect, and the window's
    /// look, title bar included, from the Appearance setting: the window
    /// stays open while it changes, so it restyles at once.
    /// (`appearance::current` is already the new one: main's hub listener,
    /// which sets it, comes first.)
    fn show_settings(&self) {
        let Some(window) = self.existing() else {
            return;
        };
        let settings = self.hub.current();
        appearance::apply(&window.global::<Theme>());
        show_title_bar(&window);
        self.show_page_names(&window);
        self.show_refusals(&window);
        let names: Vec<SharedString> = language_names().into_iter().map(Into::into).collect();
        window.set_language_names(ModelRc::from(names.as_slice()));
        window.set_language(language_index(&settings.language));
        window.set_appearance(choice_index(&APPEARANCES, settings.appearance));
        window.set_theme(choice_index(&ThemeName::ALL, settings.theme));
        let icons = window.global::<Icons>();
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
        window.set_shown_actions(i32::try_from(shown).unwrap_or(0));
        window.set_wheel_layout(choice_index(&LAYOUTS, settings.layout));
        window.set_trigger(choice_index(&TRIGGERS, settings.trigger));
        window.set_shake_to_open(settings.shake_to_open);
        window.set_path_format(choice_index(&PATH_FORMATS, settings.copy_path_format));
        let terminal = terminal_index(&self.terminals.borrow(), settings.terminal.as_deref());
        window.set_terminal(i32::try_from(terminal).unwrap_or(0));
        self.show_folders(&window, &settings);
        let hold = settings.move_hold_ms;
        window.set_hold(hold_text(hold).into());
        window.set_hold_can_decrease(hold > MIN_MOVE_HOLD_MS);
        window.set_hold_can_increase(hold < MAX_MOVE_HOLD_MS);
        let symbols = |spec: &str| shortcut_text(spec).unwrap_or_else(|| spec.to_owned());
        window.set_shortcut(symbols(&settings.pocket_shortcut).into());
        window.set_remove_after_drop(settings.pocket_remove_after_drop);
        window.set_keep_copies(settings.pocket_keep_copies);
        window.set_pocket_view(choice_index(&PocketView::ALL, settings.pocket_view));
        window.set_clip_history(settings.clipboard_history);
        window.set_awake_screen_off(settings.keep_awake_screen_off);
        let closed: Vec<bool> = pages(true)
            .into_iter()
            .map(|page| settings.closed_how_to.iter().any(|k| k == page.key()))
            .collect();
        window.set_how_to_closed(ModelRc::from(closed.as_slice()));
        window.set_clip_shortcut(symbols(&settings.clipboard_shortcut).into());
        window.set_screen_shortcut(symbols(&settings.screen_text_shortcut).into());
    }

    /// Writes the sidebar's page names again in the language now in use.
    fn show_page_names(&self, window: &SettingsWindow) {
        let model = window.get_pages();
        for (i, page) in pages(self.screen_text_supported).into_iter().enumerate() {
            let label = page.label();
            if let Some(mut item) = model.row_data(i)
                && item.label != label.as_str()
            {
                item.label = label.into();
                model.set_row_data(i, item);
            }
        }
    }

    /// Writes the reasons shown under the shortcut fields again in the
    /// language now in use. A field the window has cleared (it does, as a
    /// recording starts) stays clear.
    fn show_refusals(&self, window: &SettingsWindow) {
        let texts =
            |shown: SharedString, target: ShortcutFor| match self.refusals[target.index()].get() {
                Some(reason) if !shown.is_empty() => SharedString::from(refusal_text(reason)),
                _ => shown,
            };
        window.set_shortcut_error(texts(window.get_shortcut_error(), ShortcutFor::Pocket));
        window.set_clip_shortcut_error(texts(
            window.get_clip_shortcut_error(),
            ShortcutFor::Clipboard,
        ));
        window.set_screen_shortcut_error(texts(
            window.get_screen_shortcut_error(),
            ShortcutFor::ScreenText,
        ));
    }

    /// Lists the pinned folders (a missing one too, dimmed, so it can be
    /// unpinned) and the recent ones the ring would show now.
    fn show_folders(&self, window: &SettingsWindow, settings: &Settings) {
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
        window.set_recent_history(!self.recent.borrow().items().is_empty());
        window.set_pinned_full(pinned.len() >= MAX_PINNED);
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

    /// Lists the terminals installed now in the Terminal menu. The menu
    /// gets a new model only when the list changed, which it rarely does.
    fn load_terminals(&self, window: &SettingsWindow) {
        let terminals = self.platform.terminals();
        let names: Vec<SharedString> = terminals.iter().map(|t| t.name.as_str().into()).collect();
        if !window.get_terminals().iter().eq(names.iter().cloned()) {
            window.set_terminals(ModelRc::new(VecModel::from(names)));
        }
        *self.terminals.borrow_mut() = terminals;
    }

    fn show_notice(&self) {
        let Some(window) = self.existing() else {
            return;
        };
        let notice = self.hub.load_error().map(|e| notice_text(&e));
        window.set_notice(notice.unwrap_or_default().into());
    }

    /// Opens the window on the page it was last on (General at first), or
    /// brings it forward when it is open already, and gives it the keyboard.
    /// A new window opens centred on the screen under the pointer.
    pub fn open(&self) {
        let window = self.window();
        let was_open = self.open.replace(true);
        // Pick up hand edits of the settings file.
        self.hub.refresh_from_disk();
        self.show_notice();
        if !was_open {
            for refusal in &self.refusals {
                refusal.set(None);
            }
            window.set_shortcut_error(SharedString::new());
            window.set_clip_shortcut_error(SharedString::new());
            window.set_screen_shortcut_error(SharedString::new());
        }
        self.load_terminals(&window);
        // Also lists the recent folders as they are now.
        self.show_settings();
        show_menu_colors(&*self.platform, &window.global::<Theme>());
        if !self.placed.replace(true) {
            let screens = self.platform.screens();
            if let Some(screen) = screen_for(self.platform.sample_input().cursor, &screens) {
                let (x, y) = centred_on(screen, WIDTH, HEIGHT);
                window
                    .window()
                    .set_position(LogicalPosition::new(x as f32, y as f32));
            }
        }
        // vento runs as an accessory app: bring it forward so the window
        // comes to the front and takes the keyboard.
        self.platform.activate_app();
        if let Err(e) = window.show() {
            warn!("cannot show the settings window: {e}");
            self.open.set(false);
            self.platform.deactivate_app();
            self.window.release_later();
            return;
        }
        // A minimised window comes back too.
        window.window().with_winit_window(|w| {
            w.set_minimized(false);
            w.focus_window();
        });
        // A new window gets its native window, and so its title bar, at the
        // end of this turn of the event loop.
        let this = self.this.clone();
        self.window.when_native(move || {
            if let Some(window) = this.upgrade().and_then(|c| c.existing()) {
                show_title_bar(&window);
            }
        });
        debug!("settings window open");
    }

    /// Hides the window (a recording in progress ends without a change),
    /// remembers its page for the next open, and hands focus back to the
    /// app that had it on the next turn of the event loop, unless the window
    /// is open again by then or another vento window is open (it has the
    /// focus, or is about to).
    pub fn close(&self) {
        if !self.open.replace(false) {
            return;
        }
        let window = self.existing();
        close_with(
            self.is_recording(),
            || {
                if let Some(window) = &window {
                    window.invoke_stop_recording();
                }
            },
            || self.platform.restore_app_menu(),
            || {
                let Some(window) = &window else { return };
                let pages = pages(self.screen_text_supported);
                if let Some(page) = choice(&pages, window.get_page()) {
                    self.page.set(page);
                }
                if let Err(e) = window.hide() {
                    warn!("cannot hide the settings window: {e}");
                }
            },
        );
        self.window.release_later();
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(controller) = this.upgrade()
                && hands_focus_back(controller.is_open(), controller.other_window_open())
            {
                controller.platform.deactivate_app();
            }
        });
        debug!("settings window closed");
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }
}

/// What an event of the window says about the user turning to it.
#[derive(Debug, PartialEq)]
enum Turn {
    /// A click in the window: the user turned to it.
    Now,
    /// The window became key. Whether the user turned to it shows on the
    /// next turn of the event loop: an activation that brings another vento
    /// window forward can make this one key on the way.
    IfStillKey,
    /// Neither.
    No,
}

/// What `event` says about the user turning to the window.
fn turn(event: &WindowEvent) -> Turn {
    match event {
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } => Turn::Now,
        WindowEvent::Focused(true) => Turn::IfStillKey,
        _ => Turn::No,
    }
}

/// Whether the closed window hands focus back to the app that had it:
/// not when it is open again (`reopened`), nor while another vento window
/// is open (`other_window_open`), which has the focus or is taking it.
fn hands_focus_back(reopened: bool, other_window_open: bool) -> bool {
    !reopened && !other_window_open
}

/// Gives the native title bar of `window` the Appearance setting, as
/// [`appearance::title_bar_theme`] says: the platform draws it, so the
/// window's `Theme` alone leaves it with the system's look. Does nothing
/// before the window has its native window.
fn show_title_bar(window: &SettingsWindow) {
    let theme =
        appearance::title_bar_theme(appearance::current(), window.global::<Theme>().get_dark());
    window.window().with_winit_window(|w| w.set_theme(theme));
}

/// Closes the window: `stop_recording` ends a recording in progress (when
/// `recording`), `restore_app_menu` brings the menu bar back whatever
/// happened to the recording, and `hide` hides the window, in that order.
fn close_with(
    recording: bool,
    stop_recording: impl FnOnce(),
    restore_app_menu: impl FnOnce(),
    hide: impl FnOnce(),
) {
    if recording {
        stop_recording();
    }
    restore_app_menu();
    hide();
}

/// Opens `url` in the default browser. The opener is waited for on a
/// thread of its own: unwaited it would stay a zombie, and waiting here
/// would stall the window.
fn open_link(url: &'static str) {
    #[cfg(target_os = "macos")]
    const OPENER: &str = "open";
    #[cfg(not(target_os = "macos"))]
    const OPENER: &str = "xdg-open";
    let opener = std::thread::Builder::new()
        .name("open-link".into())
        .spawn(move || match Command::new(OPENER).arg(url).status() {
            Ok(status) if status.success() => {}
            Ok(status) => warn!("{OPENER} {url} failed: {status}"),
            Err(e) => warn!("cannot open {url}: {e}"),
        });
    if let Err(e) = opener {
        warn!("cannot start opening {url}: {e}");
    }
}

/// The top-left corner that centres a `w` × `h` window on `screen`; a
/// window larger than the screen starts at its top-left corner.
pub(crate) fn centred_on(screen: &Rect, w: f64, h: f64) -> (f64, f64) {
    (
        screen.x + ((screen.w - w) / 2.0).max(0.0),
        screen.y + ((screen.h - h) / 2.0).max(0.0),
    )
}

/// The notice for a settings file that could not be loaded, given why: a
/// parse error names its line, a read error says why in Vento's own words
/// for the common kinds.
pub(crate) fn notice_text(error: &SettingsError) -> String {
    match error {
        SettingsError::Parse {
            line: Some(line),
            message,
        } => fill(
            &tr(
                "Your settings file has an error on line {line}: {message}; changing a setting here will replace it",
            ),
            &[("line", &line.to_string()), ("message", message)],
        ),
        SettingsError::Parse {
            line: None,
            message,
        } => unreadable_notice(message),
        SettingsError::Read(e) => unreadable_notice(&io_reason(e)),
    }
}

/// The notice for a settings file that can't be read, for `why`.
fn unreadable_notice(why: &str) -> String {
    fill(
        &tr("Your settings file can't be read ({error}); changing a setting here will replace it"),
        &[("error", why)],
    )
}

/// The notice for a change that could not be saved.
fn save_error_text(error: &io::Error) -> String {
    fill(
        &tr("Couldn't save settings: {error}"),
        &[("error", &io_reason(error))],
    )
}

/// The Move page's hold delay, in the interface language: `"400 ms"`.
fn hold_text(ms: u32) -> String {
    fill(&tr("{ms} ms"), &[("ms", &ms.to_string())])
}

/// The text shown under a shortcut field for the refusal `reason` (one of
/// the English texts a refusal carries), in the interface language.
fn refusal_text(reason: &str) -> String {
    match reason {
        NOT_A_SHORTCUT => tr("Use a modifier with a letter or digit"),
        RESERVED_SHORTCUT => tr("macOS uses this shortcut"),
        UNUSABLE_SHORTCUT => tr("This shortcut can't be used"),
        "The Pocket uses this shortcut" => tr("The Pocket uses this shortcut"),
        "The Clipboard uses this shortcut" => tr("The Clipboard uses this shortcut"),
        "Copy from screen uses this shortcut" => tr("Copy from screen uses this shortcut"),
        other => other.to_owned(),
    }
}

/// The choice at index `i` of a segmented control; `None` out of range.
fn choice<T: Copy>(choices: &[T], i: i32) -> Option<T> {
    usize::try_from(i)
        .ok()
        .and_then(|i| choices.get(i).copied())
}

/// The Language drop-down's items: Automatic, then each language under its
/// own name, whatever the language in use.
fn language_names() -> Vec<String> {
    std::iter::once(tr("Automatic (system language)"))
        .chain(LANGUAGES.iter().map(|l| l.name.to_owned()))
        .collect()
}

/// The `language` setting for the drop-down's item `i`: "auto" for the
/// first, then the code of each language. An item out of range is "auto".
fn language_setting(i: i32) -> String {
    usize::try_from(i)
        .ok()
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| LANGUAGES.get(i))
        .map_or("auto", |l| l.code)
        .to_owned()
}

/// The drop-down's item for the `language` setting `setting`. A value that
/// names no language acts as Automatic, so it shows as that.
fn language_index(setting: &str) -> i32 {
    lookup(setting)
        .and_then(|l| LANGUAGES.iter().position(|each| each == l))
        .and_then(|i| i32::try_from(i + 1).ok())
        .unwrap_or(0)
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

/// Shows or hides the action in row `index` of the Wheel page. Hiding the
/// last action shown is refused and changes nothing.
fn show_action(settings: &mut Settings, index: usize, on: bool) {
    let mut rows = settings.action_rows();
    if let Some(row) = rows.get_mut(index) {
        row.1 = on;
        settings.set_action_rows(&rows);
    }
}

/// Moves the Wheel page's row `from` to place `to` (clamped to the last).
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

/// How the platform writes the shortcut `spec`: symbols (`⌃⌥P`) on a Mac,
/// GNOME's words (`Ctrl+Alt+P`) elsewhere. `None` when it does not parse.
pub(crate) fn shortcut_text(spec: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    return vento_core::settings::shortcut_symbols(spec);
    #[cfg(not(target_os = "macos"))]
    return vento_core::settings::shortcut_label(spec);
}

/// Why the recorded shortcut `spec` is refused before trying to register
/// it, or `None` when it may be tried. Shift alone only types a capital, so
/// it is not a modifier for this. The ones macOS keeps for itself are
/// refused on a Mac; elsewhere one the desktop keeps fails to register,
/// which says so. The same keys as either of the shortcuts
/// `target` is not, as `saved` holds them, are refused too, even while one is
/// not registered.
fn shortcut_refusal(spec: &str, target: ShortcutFor, saved: &Settings) -> Option<&'static str> {
    let shift_only = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let keys = parse_shortcut(spec);
    match keys {
        None => Some(NOT_A_SHORTCUT),
        Some((m, _)) if m == shift_only => Some(NOT_A_SHORTCUT),
        Some(_) if cfg!(target_os = "macos") && is_reserved_shortcut(spec) => {
            Some(RESERVED_SHORTCUT)
        }
        Some(_) => target
            .others()
            .into_iter()
            .find(|other| keys == parse_shortcut(other.saved(saved)))
            .map(ShortcutFor::in_use),
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

/// Applies the recorded shortcut `spec` as `target`'s, in place of the one
/// `saved` holds: refuses one that is invalid, reserved or another
/// shortcut's without trying it; otherwise registers it with `register`
/// (which frees the old key first) and saves it with `save` (given the
/// change to make) only once that worked, and only if it differs from the
/// old one. When registering or saving fails, the old one is registered
/// again. The raw reason a registration failed only goes to the log.
fn apply_shortcut(
    target: ShortcutFor,
    spec: &str,
    saved: &Settings,
    register: &dyn Fn(ShortcutFor, &str) -> Result<(), String>,
    save: impl FnOnce(&dyn Fn(&mut Settings)) -> io::Result<()>,
) -> ShortcutOutcome {
    let old = target.saved(saved);
    let restore = || {
        if let Err(e) = register(target, old) {
            warn!("{target:?} shortcut {old:?} unavailable: {e}");
        }
    };
    if let Some(reason) = shortcut_refusal(spec, target, saved) {
        debug!("shortcut {spec:?} refused: {reason}");
        return ShortcutOutcome::Refused(reason);
    }
    if let Err(reason) = register(target, spec) {
        warn!("{target:?} shortcut {spec:?} refused: {reason}");
        restore();
        return ShortcutOutcome::Refused(UNUSABLE_SHORTCUT);
    }
    if spec != old
        && let Err(e) = save(&|s: &mut Settings| target.save_to(s, spec))
    {
        warn!("cannot save settings: {e}");
        restore();
        return ShortcutOutcome::NotSaved(e);
    }
    ShortcutOutcome::Applied
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_language_picker_maps_to_the_setting_and_back() {
        // Automatic first, then each language in the order Vento lists them.
        assert_eq!(language_names().len(), LANGUAGES.len() + 1);
        assert_eq!(&language_names()[1..], ["English", "العربية"]);
        assert_eq!(language_setting(0), "auto");
        assert_eq!(language_setting(1), "en");
        assert_eq!(language_setting(2), "ar");
        assert_eq!(language_setting(3), "auto");
        assert_eq!(language_setting(-1), "auto");
        for (setting, index) in [("auto", 0), ("en", 1), ("ar", 2), ("AR", 2), ("ar-LY", 2)] {
            assert_eq!(language_index(setting), index, "{setting}");
        }
        // A value Vento has no language for acts as Automatic.
        assert_eq!(language_index("xx"), 0);
        for i in 0..=LANGUAGES.len() as i32 {
            assert_eq!(language_index(&language_setting(i)), i);
        }
    }

    use super::*;

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
        show_action(&mut s, 99, true);
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
            [
                ActionId::Pocket,
                ActionId::CopyTo,
                ActionId::Archive,
                ActionId::Info,
                ActionId::Convert,
                ActionId::Pdf,
                ActionId::Share
            ][..5
                + usize::from(ActionId::Pdf.available())
                + usize::from(ActionId::Share.available())]
        );
        move_action(&mut s, 5, 1);
        assert_eq!(
            s.hidden_actions,
            [
                ActionId::CopyTo,
                ActionId::Pocket,
                ActionId::Archive,
                ActionId::Info,
                ActionId::Convert,
                ActionId::Pdf,
                ActionId::Share
            ][..5
                + usize::from(ActionId::Pdf.available())
                + usize::from(ActionId::Share.available())]
        );
        assert_eq!(s.actions.len(), 4);
        let before = s.clone();
        move_action(&mut s, 11, 0);
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
        let defaults = Settings::default();
        let refusal = |spec| shortcut_refusal(spec, ShortcutFor::Pocket, &defaults);
        assert_eq!(refusal("ctrl+alt+k"), None);
        assert_eq!(refusal("p"), Some(NOT_A_SHORTCUT));
        assert_eq!(refusal("ctrl+alt+?"), Some(NOT_A_SHORTCUT));
        assert_eq!(refusal("shift+k"), Some(NOT_A_SHORTCUT));
        // macOS's own shortcuts are only refused on a Mac.
        let reserved = cfg!(target_os = "macos").then_some(RESERVED_SHORTCUT);
        assert_eq!(refusal("cmd+q"), reserved);
        assert_eq!(refusal("cmd+h"), reserved);
        assert_eq!(refusal("cmd+shift+4"), reserved);
        assert_eq!(
            refusal("ctrl+alt+v"),
            Some("The Clipboard uses this shortcut")
        );
    }

    #[test]
    fn screen_text_refuses_the_clipboard_and_pocket_keys() {
        let defaults = Settings::default();
        assert_eq!(
            shortcut_refusal("ctrl+alt+v", ShortcutFor::ScreenText, &defaults),
            Some("The Clipboard uses this shortcut")
        );
        assert_eq!(
            shortcut_refusal("ctrl+alt+p", ShortcutFor::ScreenText, &defaults),
            Some("The Pocket uses this shortcut")
        );
        assert_eq!(
            shortcut_refusal(
                &defaults.screen_text_shortcut,
                ShortcutFor::Pocket,
                &defaults
            ),
            Some("Copy from screen uses this shortcut")
        );
        assert_eq!(
            shortcut_refusal("ctrl+alt+e", ShortcutFor::ScreenText, &defaults),
            None
        );
    }

    /// Settings with these Pocket and Clipboard shortcuts.
    fn saved(pocket: &str, clipboard: &str) -> Settings {
        Settings {
            pocket_shortcut: pocket.to_owned(),
            clipboard_shortcut: clipboard.to_owned(),
            ..Settings::default()
        }
    }

    /// Runs [`apply_shortcut`] for `target` over the settings `saved`, with
    /// a register that fails for the specs in `unregistrable` and a save
    /// that fails when `save_fails`, and returns the outcome, every call
    /// made, in order, and the settings as the save left them.
    fn apply_with_fakes(
        target: ShortcutFor,
        spec: &str,
        saved: &Settings,
        unregistrable: &[&str],
        save_fails: bool,
    ) -> (ShortcutOutcome, Vec<String>, Settings) {
        let calls = RefCell::new(Vec::new());
        let after = RefCell::new(saved.clone());
        let register = |which: ShortcutFor, s: &str| {
            calls.borrow_mut().push(format!("register {which:?} {s}"));
            if unregistrable.contains(&s) {
                Err("already taken".to_owned())
            } else {
                Ok(())
            }
        };
        let save = |change: &dyn Fn(&mut Settings)| {
            change(&mut after.borrow_mut());
            calls.borrow_mut().push("save".to_owned());
            if save_fails {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(())
            }
        };
        let outcome = apply_shortcut(target, spec, saved, &register, save);
        (outcome, calls.into_inner(), after.into_inner())
    }

    #[test]
    fn a_shortcut_is_saved_only_after_it_registered() {
        let defaults = saved("ctrl+alt+p", "ctrl+alt+v");
        let (outcome, calls, after) =
            apply_with_fakes(ShortcutFor::Pocket, "ctrl+alt+k", &defaults, &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Applied));
        assert_eq!(calls, ["register Pocket ctrl+alt+k", "save"]);
        assert_eq!(after.pocket_shortcut, "ctrl+alt+k");
        // The one saved already: registered (a no-op for the real one), not
        // saved again.
        let (outcome, calls, _) =
            apply_with_fakes(ShortcutFor::Pocket, "ctrl+alt+p", &defaults, &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Applied));
        assert_eq!(calls, ["register Pocket ctrl+alt+p"]);
    }

    #[test]
    fn clipboard_shortcut_is_saved_to_its_key() {
        let defaults = saved("ctrl+alt+p", "ctrl+alt+v");
        let (outcome, calls, after) =
            apply_with_fakes(ShortcutFor::Clipboard, "ctrl+alt+c", &defaults, &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Applied));
        assert_eq!(calls, ["register Clipboard ctrl+alt+c", "save"]);
        assert_eq!(after.clipboard_shortcut, "ctrl+alt+c");
        assert_eq!(after.pocket_shortcut, "ctrl+alt+p");
        // One that cannot register brings the Clipboard's old one back.
        let (outcome, calls, _) = apply_with_fakes(
            ShortcutFor::Clipboard,
            "ctrl+alt+c",
            &defaults,
            &["ctrl+alt+c"],
            false,
        );
        assert!(matches!(
            outcome,
            ShortcutOutcome::Refused(UNUSABLE_SHORTCUT)
        ));
        assert_eq!(
            calls,
            [
                "register Clipboard ctrl+alt+c",
                "register Clipboard ctrl+alt+v"
            ]
        );
    }

    #[test]
    fn same_shortcut_as_the_other_is_refused() {
        let defaults = saved("ctrl+alt+p", "ctrl+alt+v");
        let (outcome, calls, _) =
            apply_with_fakes(ShortcutFor::Clipboard, "ctrl+alt+p", &defaults, &[], false);
        assert!(matches!(
            outcome,
            ShortcutOutcome::Refused("The Pocket uses this shortcut")
        ));
        assert!(calls.is_empty());
        let (outcome, calls, _) =
            apply_with_fakes(ShortcutFor::Pocket, "ctrl+alt+v", &defaults, &[], false);
        assert!(matches!(
            outcome,
            ShortcutOutcome::Refused("The Clipboard uses this shortcut")
        ));
        assert!(calls.is_empty());
        // The same keys written another way are the same shortcut.
        let (outcome, _, _) = apply_with_fakes(
            ShortcutFor::Pocket,
            "ctrl+alt+v",
            &saved("ctrl+alt+p", "alt+ctrl+v"),
            &[],
            false,
        );
        assert!(matches!(
            outcome,
            ShortcutOutcome::Refused("The Clipboard uses this shortcut")
        ));
    }

    #[test]
    fn a_refused_shortcut_is_never_tried() {
        let defaults = saved("ctrl+alt+p", "ctrl+alt+v");
        #[cfg(target_os = "macos")]
        {
            let (outcome, calls, _) =
                apply_with_fakes(ShortcutFor::Pocket, "cmd+q", &defaults, &[], false);
            assert!(matches!(
                outcome,
                ShortcutOutcome::Refused(RESERVED_SHORTCUT)
            ));
            assert!(calls.is_empty());
        }
        let (outcome, calls, _) =
            apply_with_fakes(ShortcutFor::Pocket, "shift+k", &defaults, &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Refused(NOT_A_SHORTCUT)));
        assert!(calls.is_empty());
        let (outcome, calls, _) =
            apply_with_fakes(ShortcutFor::Pocket, "ctrl+alt+?", &defaults, &[], false);
        assert!(matches!(outcome, ShortcutOutcome::Refused(NOT_A_SHORTCUT)));
        assert!(calls.is_empty());
    }

    #[test]
    fn a_shortcut_that_cannot_register_brings_the_old_one_back_unsaved() {
        let (outcome, calls, _) = apply_with_fakes(
            ShortcutFor::Pocket,
            "ctrl+alt+k",
            &saved("ctrl+alt+p", "ctrl+alt+v"),
            &["ctrl+alt+k"],
            false,
        );
        assert!(matches!(
            outcome,
            ShortcutOutcome::Refused(UNUSABLE_SHORTCUT)
        ));
        assert_eq!(
            calls,
            ["register Pocket ctrl+alt+k", "register Pocket ctrl+alt+p"]
        );
    }

    #[test]
    fn a_failed_save_brings_the_old_shortcut_back() {
        let (outcome, calls, _) = apply_with_fakes(
            ShortcutFor::Pocket,
            "ctrl+alt+k",
            &saved("ctrl+alt+p", "ctrl+alt+v"),
            &[],
            true,
        );
        assert!(matches!(outcome, ShortcutOutcome::NotSaved(_)));
        assert_eq!(
            calls,
            [
                "register Pocket ctrl+alt+k",
                "save",
                "register Pocket ctrl+alt+p"
            ]
        );
    }

    #[test]
    fn save_error_is_worded_for_the_notice() {
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            save_error_text(&e),
            "Couldn't save settings: permission denied"
        );
        // Without wording of its own, a kind shows the text of the error.
        let other = io::Error::other("wrong format");
        assert_eq!(
            save_error_text(&other),
            "Couldn't save settings: wrong format"
        );
    }

    fn parse_error(line: Option<usize>, message: &str) -> SettingsError {
        SettingsError::Parse {
            line,
            message: message.into(),
        }
    }

    fn read_error(kind: io::ErrorKind) -> SettingsError {
        SettingsError::Read(io::Error::from(kind))
    }

    #[test]
    fn notice_names_the_error() {
        assert_eq!(
            notice_text(&parse_error(Some(3), "expected =")),
            "Your settings file has an error on line 3: expected =; changing a setting here will replace it"
        );
        assert_eq!(
            notice_text(&parse_error(None, "unexpected end")),
            "Your settings file can't be read (unexpected end); changing a setting here will replace it"
        );
        assert_eq!(
            notice_text(&read_error(io::ErrorKind::PermissionDenied)),
            "Your settings file can't be read (permission denied); changing a setting here will replace it"
        );
    }

    #[test]
    fn a_notice_is_worded_when_it_is_shown() {
        // The error is kept as it happened and worded at each showing, so the
        // same error follows the language.
        let error = read_error(io::ErrorKind::PermissionDenied);
        let english = notice_text(&error);
        let arabic = vento_core::i18n::with_language("ar", || notice_text(&error));
        assert_ne!(english, arabic);
        assert_eq!(notice_text(&error), english);
    }

    #[test]
    fn notices_and_refusals_read_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            assert_eq!(
                notice_text(&parse_error(Some(3), "expected =")),
                "في ملف الإعدادات خطأ في السطر 3: expected =؛ تغيير أي إعداد هنا سيستبدله"
            );
            assert_eq!(
                notice_text(&read_error(io::ErrorKind::PermissionDenied)),
                "تعذّرت قراءة ملف الإعدادات (لا توجد صلاحية)؛ تغيير أي إعداد هنا سيستبدله"
            );
            let e = io::Error::from(io::ErrorKind::PermissionDenied);
            assert_eq!(save_error_text(&e), "تعذّر حفظ الإعدادات: لا توجد صلاحية");
            assert_eq!(hold_text(400), "400 م.ث");
            assert_eq!(
                refusal_text(NOT_A_SHORTCUT),
                "استخدم مفتاح تعديل مع حرف أو رقم"
            );
        });
        assert_eq!(hold_text(400), "400 ms");
    }

    #[test]
    fn every_refusal_has_a_translation() {
        let refusals = [
            NOT_A_SHORTCUT,
            RESERVED_SHORTCUT,
            UNUSABLE_SHORTCUT,
            ShortcutFor::Pocket.in_use(),
            ShortcutFor::Clipboard.in_use(),
            ShortcutFor::ScreenText.in_use(),
        ];
        for reason in refusals {
            assert_eq!(refusal_text(reason), reason, "English is the text itself");
            vento_core::i18n::with_language("ar", || {
                assert_ne!(refusal_text(reason), reason, "{reason} has no Arabic");
            });
        }
    }

    #[test]
    fn page_names_read_in_arabic_and_the_move_page_differs_from_the_action() {
        vento_core::i18n::with_language("ar", || {
            let names: Vec<_> = pages(true).into_iter().map(Page::label).collect();
            assert_eq!(
                names,
                [
                    "عام",
                    "العجلة",
                    "النقل",
                    "الجيب",
                    "الحافظة",
                    "النسخ من الشاشة",
                    // No tanween: a bold title's line box cuts the mark off.
                    "إبقاء الجهاز مستيقظا",
                    "حول"
                ]
            );
            assert_ne!(
                Page::Move.label(),
                vento_core::actions::ActionId::Move.label()
            );
        });
        assert_eq!(Page::Move.label(), "Move");
        assert_eq!(Page::CopyFromScreen.label(), "Copy from screen");
    }

    #[test]
    fn copy_from_screen_has_a_page_only_where_supported() {
        use Page::*;
        assert_eq!(
            pages(true),
            vec![
                General,
                Wheel,
                Move,
                Pocket,
                Clipboard,
                CopyFromScreen,
                KeepAwake,
                About
            ]
        );
        assert!(!pages(false).contains(&CopyFromScreen));
        assert_eq!(pages(false).len(), 7);
    }

    #[test]
    fn closing_a_how_to_card_saves_its_page_once_and_opening_forgets_it() {
        let mut s = Settings::default();
        set_how_to_open(&mut s, Page::Pocket, false);
        set_how_to_open(&mut s, Page::Pocket, false);
        set_how_to_open(&mut s, Page::CopyFromScreen, false);
        assert_eq!(s.closed_how_to, ["pocket", "copy_from_screen"]);
        set_how_to_open(&mut s, Page::Pocket, true);
        assert_eq!(s.closed_how_to, ["copy_from_screen"]);
    }

    #[test]
    fn pages_have_ids_in_the_order_of_the_enum() {
        use Page::*;
        let all = pages(true);
        let ids: Vec<i32> = all.iter().map(|p| p.id()).collect();
        assert_eq!(ids, (0..8).collect::<Vec<_>>());
        assert_eq!(General.id(), 0);
        assert_eq!(CopyFromScreen.id(), 5);
        assert_eq!(About.id(), 7);
        // A page left out does not shift the ids of the others.
        let ids: Vec<i32> = pages(false).iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4, 6, 7]);
    }

    #[test]
    fn appearance_choices_map_both_ways() {
        assert_eq!(choice(&APPEARANCES, 2), Some(Appearance::Dark));
        assert_eq!(choice_index(&APPEARANCES, Appearance::Light), 1);
    }

    #[test]
    fn recording_stops_when_the_window_closes() {
        let calls = RefCell::new(Vec::new());
        let close = |recording| {
            calls.borrow_mut().clear();
            close_with(
                recording,
                || calls.borrow_mut().push("stop recording"),
                || calls.borrow_mut().push("restore menu"),
                || calls.borrow_mut().push("hide"),
            );
            calls.borrow().clone()
        };
        assert_eq!(close(true), ["stop recording", "restore menu", "hide"]);
        // Not recording: nothing to stop, but the menu bar comes back all
        // the same.
        assert_eq!(close(false), ["restore menu", "hide"]);
    }

    #[test]
    fn a_click_or_the_keyboard_turns_the_user_to_the_window() {
        use slint::winit_030::winit::event::{DeviceId, MouseButton};
        let click = |state| WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        };
        assert_eq!(turn(&click(ElementState::Pressed)), Turn::Now);
        assert_eq!(turn(&click(ElementState::Released)), Turn::No);
        assert_eq!(turn(&WindowEvent::Focused(true)), Turn::IfStillKey);
        assert_eq!(turn(&WindowEvent::Focused(false)), Turn::No);
    }

    #[test]
    fn closing_leaves_the_focus_with_another_vento_window() {
        assert!(hands_focus_back(false, false));
        // Open again by then, or the Quick panel or Info is open.
        assert!(!hands_focus_back(true, false));
        assert!(!hands_focus_back(false, true));
    }

    #[test]
    fn a_new_window_opens_centred_on_the_screen() {
        let screen = Rect {
            x: 1440.0,
            y: 25.0,
            w: 1920.0,
            h: 1055.0,
        };
        assert_eq!(centred_on(&screen, 680.0, 520.0), (2060.0, 292.5));
        // Larger than the screen: from its top-left corner.
        let small = Rect {
            x: 0.0,
            y: 25.0,
            w: 600.0,
            h: 400.0,
        };
        assert_eq!(centred_on(&small, 680.0, 520.0), (0.0, 25.0));
    }
}
