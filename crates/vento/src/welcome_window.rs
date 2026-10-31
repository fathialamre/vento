//! The Welcome window: the short setup shown on the first launch, and again
//! from Settings → About. Six screens, one at a time; every choice applies
//! and saves at once through the settings hub, and the window follows the
//! changes made elsewhere (the Settings window open beside it, a hand edit).
//!
//! Like the Settings window, it is created on the first open and dropped
//! once it has been closed for [`crate::lazy_window::IDLE`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use log::{debug, warn};
use slint::winit_030::WinitWindowAccessor;
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalPosition, Model, ModelRc, SharedString,
    Timer, VecModel,
};
use vento_core::destinations::folder_exists;
use vento_core::geometry::screen_for;
use vento_core::i18n::{self, LANGUAGES, fill, system_languages, tr};
use vento_core::settings::{MAX_PINNED, Settings, ThemeName};
use vento_core::trigger::Modifier;
use vento_core::welcome::{self, Preset, apply_preset, preset_of, suggested_folders};
use vento_platform::{Platform, TerminalApp};

use crate::appearance::{self, show_menu_colors};
use crate::lazy_window::LazyWindow;
use crate::panel::set_rows;
use crate::pause::PauseSwitch;
use crate::pocket::{display_name, to_image};
use crate::settings_hub::SettingsHub;
use crate::settings_window::{
    APPEARANCES, FOLDER_ICON_PX, LAYOUTS, TRIGGER_NAMES, TRIGGERS, centred_on, choice,
    choice_index, folder_row, hands_focus_back, pin_folder, shortcut_text, show_title_bar,
    terminal_index, unpin_folder,
};
use crate::ui::{FolderChip, FolderRow, PresetCard, ShortcutLine, Theme, WelcomeWindow};

/// The window's size, in logical points (matches `welcome.slint`).
const WIDTH: f64 = 680.0;
const HEIGHT: f64 = 520.0;

/// The last screen, Done.
const LAST_STEP: i32 = 5;

/// Opens, fills and closes the Welcome window.
pub struct WelcomeController {
    platform: Rc<dyn Platform>,
    hub: Rc<SettingsHub>,
    /// Shown on the Try it screen: a paused Vento opens no wheel.
    pause: Rc<PauseSwitch>,
    /// The window while it is open, and for a while after it closes.
    window: LazyWindow<WelcomeWindow>,
    /// Whether the Done screen lists the Copy from screen shortcut.
    screen_text_supported: bool,
    /// The home folder: the suggested folders are in it, and folder places
    /// show it as `~`.
    home: Option<PathBuf>,
    /// The folders the chips suggest, as they were when the window opened.
    suggestions: RefCell<Vec<PathBuf>>,
    /// The rows behind the window's lists.
    presets: Rc<VecModel<PresetCard>>,
    chips: Rc<VecModel<FolderChip>>,
    pinned: Rc<VecModel<FolderRow>>,
    shortcuts: Rc<VecModel<ShortcutLine>>,
    /// Finder icons of the pinned folders, by path (`None`: it has none).
    folder_icons: RefCell<HashMap<PathBuf, Option<Image>>>,
    /// The installed terminals, as the Terminal menu lists them.
    terminals: RefCell<Vec<TerminalApp>>,
    /// The window has been placed since it was made (a new window opens
    /// centred; one shown again stays where the user left it).
    placed: Cell<bool>,
    open: Cell<bool>,
    /// Runs after Open Settings has closed the window; main opens the
    /// Settings window on General.
    on_open_settings: RefCell<Option<Box<dyn Fn()>>>,
    /// Whether another vento window is open; main sets it. That window
    /// keeps the focus when this one closes.
    other_window_open: RefCell<Option<Box<dyn Fn() -> bool>>>,
    this: Weak<Self>,
}

impl WelcomeController {
    /// Creates the (closed) window controller; the window comes with the
    /// first open. `pause` is Vento's paused state, which the Try it screen
    /// shows and can end; `screen_text_supported` says whether the Done
    /// screen lists the Copy from screen shortcut.
    pub fn new(
        platform: Rc<dyn Platform>,
        hub: Rc<SettingsHub>,
        pause: Rc<PauseSwitch>,
        screen_text_supported: bool,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            hub,
            pause,
            window: LazyWindow::new(),
            screen_text_supported,
            home: std::env::var_os("HOME").map(PathBuf::from),
            suggestions: RefCell::new(Vec::new()),
            presets: Rc::new(VecModel::default()),
            chips: Rc::new(VecModel::default()),
            pinned: Rc::new(VecModel::default()),
            shortcuts: Rc::new(VecModel::default()),
            folder_icons: RefCell::new(HashMap::new()),
            terminals: RefCell::new(Vec::new()),
            placed: Cell::new(false),
            open: Cell::new(false),
            on_open_settings: RefCell::new(None),
            other_window_open: RefCell::new(None),
            this: this.clone(),
        });
        // A change made in the Settings window or by hand shows here too.
        let this = Rc::downgrade(&controller);
        controller.hub.on_change(Box::new(move |_, _| {
            if let Some(controller) = this.upgrade() {
                controller.show_settings();
            }
        }));
        let this = Rc::downgrade(&controller);
        controller.pause.on_change(Box::new(move |paused| {
            if let Some(window) = this.upgrade().and_then(|c| c.existing()) {
                window.set_paused(paused);
            }
        }));
        // The text Rust writes (the preset cards, the Done screen's lines)
        // in the new language.
        appearance::follow_language(Rc::downgrade(&controller), Self::show_settings);
        controller
    }

    /// Has Open Settings call `f` once it has closed the window.
    pub fn set_on_open_settings(&self, f: Box<dyn Fn()>) {
        *self.on_open_settings.borrow_mut() = Some(f);
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
    fn window(&self) -> WelcomeWindow {
        self.window.get_or_create(|| {
            let window = WelcomeWindow::new().expect("cannot create the welcome window");
            // A Mac-window page, as the Settings window has.
            window.global::<Theme>().set_clean(true);
            appearance::follow(&window, appearance::Look::Full);
            window.set_presets(ModelRc::from(self.presets.clone()));
            window.set_suggestions(ModelRc::from(self.chips.clone()));
            window.set_pinned(ModelRc::from(self.pinned.clone()));
            window.set_shortcuts(ModelRc::from(self.shortcuts.clone()));
            let trigger_names: Vec<SharedString> = TRIGGER_NAMES.map(Into::into).into();
            window.set_trigger_names(ModelRc::from(trigger_names.as_slice()));
            let theme_names: Vec<SharedString> =
                ThemeName::ALL.iter().map(|t| t.label().into()).collect();
            window.set_theme_names(ModelRc::from(theme_names.as_slice()));
            // A new window opens centred.
            self.placed.set(false);
            self.connect(&window);
            window
        })
    }

    /// The window, if it has one now.
    fn existing(&self) -> Option<WelcomeWindow> {
        self.window.get()
    }

    fn connect(&self, window: &WelcomeWindow) {
        let this = self.this.clone();
        window.window().on_close_requested(move || {
            if let Some(controller) = this.upgrade() {
                controller.close();
            }
            CloseRequestResponse::KeepWindowShown
        });
        // Esc, Skip setup and the close button all end the setup.
        let this = self.this.clone();
        window.on_close_requested(move || {
            if let Some(controller) = this.upgrade() {
                controller.close();
            }
        });
        let this = self.this.clone();
        window.on_skip(move || {
            if let Some(controller) = this.upgrade() {
                controller.close();
            }
        });
        let this = self.this.clone();
        window.on_next(move || {
            if let Some(controller) = this.upgrade() {
                controller.next();
            }
        });
        let this = self.this.clone();
        window.on_back(move || {
            if let Some(window) = this.upgrade().and_then(|c| c.existing()) {
                window.set_step((window.get_step() - 1).max(0));
            }
        });
        let this = self.this.clone();
        window.on_open_settings(move || {
            let Some(controller) = this.upgrade() else {
                return;
            };
            controller.close();
            if let Some(f) = controller.on_open_settings.borrow().as_ref() {
                f();
            }
        });
        let this = self.this.clone();
        window.on_resume(move || {
            if let Some(controller) = this.upgrade() {
                controller.pause.set(false);
            }
        });
        self.connect_controls(window);
    }

    /// Every control applies its change at once.
    fn connect_controls(&self, window: &WelcomeWindow) {
        let this = self.this.clone();
        window.on_language_chosen(move |i| {
            if let (Some(controller), Some(code)) = (this.upgrade(), language_code_at(i)) {
                controller.apply(|s| s.language = code.to_owned());
            }
        });
        let this = self.this.clone();
        window.on_appearance_chosen(move |i| {
            if let (Some(controller), Some(appearance)) = (this.upgrade(), choice(&APPEARANCES, i))
            {
                controller.apply(|s| s.appearance = appearance);
            }
        });
        let this = self.this.clone();
        window.on_theme_chosen(move |i| {
            if let (Some(controller), Some(theme)) = (this.upgrade(), choice(&ThemeName::ALL, i)) {
                controller.apply(|s| s.theme = theme);
            }
        });
        let this = self.this.clone();
        window.on_trigger_chosen(move |i| {
            if let (Some(controller), Some(trigger)) = (this.upgrade(), choice(&TRIGGERS, i)) {
                controller.apply(|s| s.trigger = trigger);
            }
        });
        let this = self.this.clone();
        window.on_shake_toggled(move |on| {
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| s.shake_to_open = on);
            }
        });
        let this = self.this.clone();
        window.on_preset_chosen(move |i| {
            if let (Some(controller), Some(preset)) = (this.upgrade(), choice(&Preset::ALL, i)) {
                controller.apply(|s| apply_preset(s, preset));
            }
        });
        let this = self.this.clone();
        window.on_layout_chosen(move |i| {
            if let (Some(controller), Some(layout)) = (this.upgrade(), choice(&LAYOUTS, i)) {
                controller.apply(|s| s.layout = layout);
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
        window.on_suggestion_toggled(move |i| {
            let Some(controller) = this.upgrade() else {
                return;
            };
            let folder = usize::try_from(i)
                .ok()
                .and_then(|i| controller.suggestions.borrow().get(i).cloned());
            if let Some(folder) = folder {
                controller.apply(|s| toggle_pin(s, &folder));
            }
        });
        let this = self.this.clone();
        window.on_add_folder(move || {
            if let Some(controller) = this.upgrade() {
                controller.add_folder();
            }
        });
        let this = self.this.clone();
        window.on_unpin(move |i| {
            if let (Some(controller), Ok(i)) = (this.upgrade(), usize::try_from(i)) {
                controller.apply(|s| unpin_folder(s, i));
            }
        });
        let this = self.this.clone();
        window.on_clipboard_toggled(move |on| {
            if let Some(controller) = this.upgrade() {
                controller.apply(|s| s.clipboard_history = on);
            }
        });
    }

    /// Next: the next screen, or the end of the setup after Done.
    fn next(&self) {
        let Some(window) = self.existing() else {
            return;
        };
        match next_step(window.get_step()) {
            Some(step) => window.set_step(step),
            None => self.close(),
        }
    }

    /// Shows the folder picker and pins the folder chosen, as the Settings
    /// window's Add Folder does. Does nothing when the pinned list is full.
    fn add_folder(&self) {
        if self.hub.current().pinned_folders.len() >= MAX_PINNED {
            return;
        }
        // The modal picker takes the key focus, and its focus changes may
        // still be on their way when it returns: the window takes the focus
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

    /// Makes the open window the key window again.
    fn regain_focus(&self) {
        let Some(window) = self.existing().filter(|_| self.is_open()) else {
            return;
        };
        self.platform.activate_app();
        window.window().with_winit_window(|w| w.focus_window());
    }

    /// Whether another vento window is open, as main says.
    fn other_window_open(&self) -> bool {
        self.other_window_open
            .borrow()
            .as_ref()
            .is_some_and(|f| f())
    }

    /// Applies `change` to the settings in effect and saves it, unless it
    /// changes nothing, so an unchanged click never replaces the file. A
    /// save error is logged. Either way the controls then show the settings
    /// in effect.
    fn apply(&self, change: impl Fn(&mut Settings)) {
        self.hub.refresh_from_disk();
        let mut changed = self.hub.current();
        change(&mut changed);
        if changed != self.hub.current()
            && let Err(e) = self.hub.update(&change)
        {
            warn!("cannot save settings: {e}");
        }
        self.show_settings();
    }

    /// Sets every property from the settings in effect, and the window's
    /// look, title bar included: the window is its own preview of the
    /// Appearance and Theme settings.
    fn show_settings(&self) {
        let Some(window) = self.existing() else {
            return;
        };
        let s = self.hub.current();
        appearance::apply(&window.global::<Theme>());
        show_title_bar(&window);
        let names: Vec<SharedString> = language_choices().into_iter().map(Into::into).collect();
        window.set_language_names(ModelRc::from(names.as_slice()));
        window.set_language(language_choice(&s.language, &system_languages()));
        window.set_appearance(choice_index(&APPEARANCES, s.appearance));
        window.set_theme(choice_index(&ThemeName::ALL, s.theme));
        window.set_trigger(choice_index(&TRIGGERS, s.trigger));
        window.set_shake(s.shake_to_open);
        window.set_paused(self.pause.is_paused());
        let cards = Preset::ALL
            .iter()
            .map(|p| PresetCard {
                title: p.label().into(),
                summary: p.summary().into(),
            })
            .collect();
        set_rows(&self.presets, cards);
        window.set_preset(preset_of(&s).map_or(-1, |p| choice_index(&Preset::ALL, p)));
        window.set_layout(choice_index(&LAYOUTS, s.layout));
        let terminal = terminal_index(&self.terminals.borrow(), s.terminal.as_deref());
        window.set_terminal(i32::try_from(terminal).unwrap_or(0));
        self.show_folders(&window, &s);
        window.set_clipboard_history(s.clipboard_history);
        let clip = &s.clipboard_shortcut;
        window.set_clip_shortcut(shortcut_text(clip).unwrap_or_else(|| clip.clone()).into());
        let lines = shortcut_lines(&s, self.screen_text_supported)
            .into_iter()
            .map(|(keys, what)| ShortcutLine {
                keys: keys.into(),
                what: what.into(),
            })
            .collect();
        set_rows(&self.shortcuts, lines);
    }

    /// The suggested folders' chips, pinned or not, and the pinned folders
    /// (a missing one too, dimmed, so it can be unpinned).
    fn show_folders(&self, window: &WelcomeWindow, s: &Settings) {
        let pinned = &s.pinned_folders;
        let chips = self
            .suggestions
            .borrow()
            .iter()
            .map(|p| FolderChip {
                name: display_name(p).into(),
                pinned: pinned.contains(p),
            })
            .collect();
        set_rows(&self.chips, chips);
        self.folder_icons
            .borrow_mut()
            .retain(|p, _| pinned.contains(p));
        let rows = pinned
            .iter()
            .map(|p| {
                let exists = folder_exists(p);
                let icon = if exists { self.folder_icon(p) } else { None };
                folder_row(p, exists, icon, self.home.as_deref())
            })
            .collect();
        set_rows(&self.pinned, rows);
        window.set_can_pin(pinned.len() < MAX_PINNED);
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

    /// Lists the terminals installed now in the Terminal menu, with a new
    /// model only when the list changed.
    fn load_terminals(&self, window: &WelcomeWindow) {
        let terminals = self.platform.terminals();
        let names: Vec<SharedString> = terminals.iter().map(|t| t.name.as_str().into()).collect();
        if !window.get_terminals().iter().eq(names.iter().cloned()) {
            window.set_terminals(ModelRc::new(VecModel::from(names)));
        }
        *self.terminals.borrow_mut() = terminals;
    }

    /// Opens the window on its first screen with the settings in effect,
    /// or brings it forward (back to the first screen) when it is open
    /// already, and gives it the keyboard. A new window opens centred on
    /// the screen under the pointer. Writes no setting.
    pub fn open(&self) {
        let window = self.window();
        self.open.set(true);
        // Pick up hand edits of the settings file.
        self.hub.refresh_from_disk();
        window.set_step(0);
        *self.suggestions.borrow_mut() = self
            .home
            .as_deref()
            .map(suggested_folders)
            .unwrap_or_default();
        self.load_terminals(&window);
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
            warn!("cannot show the welcome window: {e}");
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
        debug!("welcome window open");
    }

    /// Ends the setup: saves `welcome_done = true` (closing is a decision;
    /// only quitting midway brings the setup back), hides the window and
    /// hands focus back to the app that had it on the next turn of the
    /// event loop, unless the window is open again by then or another vento
    /// window is open. Closing a closed window does nothing (Esc repeats
    /// while held).
    pub fn close(&self) {
        if !self.open.replace(false) {
            return;
        }
        self.apply(|s| s.welcome_done = true);
        if let Some(window) = self.existing()
            && let Err(e) = window.hide()
        {
            warn!("cannot hide the welcome window: {e}");
        }
        self.window.release_later();
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(controller) = this.upgrade()
                && hands_focus_back(controller.is_open(), controller.other_window_open())
            {
                controller.platform.deactivate_app();
            }
        });
        debug!("welcome window closed");
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }
}

/// On a fresh install (see [`welcome::is_fresh_install`]), saves the
/// settings it starts with to `settings_file`, the setup still to do, and
/// returns `Ok(true)`. Otherwise writes nothing and returns `Ok(false)`.
///
/// # Errors
/// If the settings file cannot be written.
pub fn first_launch(
    settings_file: &Path,
    data_dir: Option<&Path>,
    preferred: &[String],
) -> io::Result<bool> {
    if !welcome::is_fresh_install(settings_file, data_dir) {
        return Ok(false);
    }
    welcome::first_settings(preferred).save(settings_file)?;
    Ok(true)
}

/// The screen after `step`; `None` after the last, Done, which ends the
/// setup.
fn next_step(step: i32) -> Option<i32> {
    (step < LAST_STEP).then(|| (step + 1).max(0))
}

/// The Language control's choices: each language by its own name. Unlike
/// the Settings menu it has no Automatic: the setup names the language.
fn language_choices() -> Vec<&'static str> {
    LANGUAGES.iter().map(|l| l.name).collect()
}

/// The `language` setting for choice `i`: that language's code; `None` for
/// a choice out of range.
fn language_code_at(i: i32) -> Option<&'static str> {
    usize::try_from(i)
        .ok()
        .and_then(|i| LANGUAGES.get(i))
        .map(|l| l.code)
}

/// The choice that shows the `language` setting `setting`: the language it
/// resolves to, so `"auto"` shows the one picked from the system's
/// `preferred` languages.
fn language_choice(setting: &str, preferred: &[String]) -> i32 {
    let language = i18n::resolve(setting, preferred);
    LANGUAGES
        .iter()
        .position(|l| l == language)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

/// Pins `folder`, or unpins it when it is pinned.
fn toggle_pin(s: &mut Settings, folder: &Path) {
    if s.pinned_folders.iter().any(|p| p == folder) {
        s.pinned_folders.retain(|p| p != folder);
    } else {
        pin_folder(s, folder);
    }
}

/// The platform's name of the trigger key `m`, as the Trigger menu shows it.
fn trigger_name(m: Modifier) -> &'static str {
    TRIGGERS
        .iter()
        .position(|t| *t == m)
        .map_or(TRIGGER_NAMES[0], |i| TRIGGER_NAMES[i])
}

/// The Done screen's lines, as (keys, what they do) in the interface
/// language: the trigger, then the Pocket, Clipboard (while the history is
/// on) and Copy from screen (where `screen_text_supported`) shortcuts.
fn shortcut_lines(s: &Settings, screen_text_supported: bool) -> Vec<(String, String)> {
    let keys = |spec: &str| shortcut_text(spec).unwrap_or_else(|| spec.to_owned());
    let mut lines = vec![
        (
            fill(
                &tr("Drag a file, press {key}"),
                &[("key", trigger_name(s.trigger))],
            ),
            tr("Wheel"),
        ),
        (keys(&s.pocket_shortcut), tr("Pocket")),
    ];
    if s.clipboard_history {
        lines.push((keys(&s.clipboard_shortcut), tr("Clipboard history")));
    }
    if screen_text_supported {
        lines.push((keys(&s.screen_text_shortcut), tr("Copy from screen")));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::i18n::{tr, with_language};
    use vento_core::settings::Settings;

    use crate::settings_window::shortcut_text;

    #[test]
    fn shortcut_lines_follow_settings() {
        let s = Settings::default();
        let lines = shortcut_lines(&s, true);
        assert_eq!(lines.len(), 4);
        #[cfg(target_os = "macos")]
        assert!(lines[0].0.contains("Control"), "{:?}", lines[0]);
        let keys: Vec<&str> = lines[1..].iter().map(|(k, _)| k.as_str()).collect();
        let want: Vec<String> = [
            &s.pocket_shortcut,
            &s.clipboard_shortcut,
            &s.screen_text_shortcut,
        ]
        .iter()
        .map(|spec| shortcut_text(spec).unwrap())
        .collect();
        assert_eq!(keys, want);
        #[cfg(target_os = "macos")]
        assert_eq!(keys, ["⌃⌥P", "⌃⌥V", "⌃⌥T"]);
        let what: Vec<&str> = lines.iter().map(|(_, w)| w.as_str()).collect();
        assert_eq!(
            what,
            [
                tr("Wheel"),
                tr("Pocket"),
                tr("Clipboard history"),
                tr("Copy from screen")
            ]
        );
    }

    #[test]
    fn the_trigger_line_names_the_trigger() {
        use vento_core::trigger::Modifier;
        let s = Settings {
            trigger: Modifier::Shift,
            ..Settings::default()
        };
        assert!(shortcut_lines(&s, true)[0].0.contains("Shift"));
    }

    #[test]
    fn no_clipboard_line_while_history_is_off() {
        let s = Settings {
            clipboard_history: false,
            ..Settings::default()
        };
        let lines = shortcut_lines(&s, true);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|(_, w)| *w != tr("Clipboard history")));
    }

    #[test]
    fn no_screen_text_line_when_unsupported() {
        let lines = shortcut_lines(&Settings::default(), false);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|(_, w)| *w != tr("Copy from screen")));
    }

    #[test]
    fn next_step_runs_to_done() {
        for step in 0..5 {
            assert_eq!(next_step(step), Some(step + 1));
        }
        assert_eq!(next_step(5), None);
    }

    #[test]
    fn a_suggested_folder_pins_and_unpins() {
        let mut s = Settings {
            pinned_folders: vec![PathBuf::from("/a")],
            ..Settings::default()
        };
        let desktop = Path::new("/Users/me/Desktop");
        toggle_pin(&mut s, desktop);
        assert_eq!(s.pinned_folders, [Path::new("/a"), desktop]);
        toggle_pin(&mut s, desktop);
        assert_eq!(s.pinned_folders, [Path::new("/a")]);
    }

    #[test]
    fn screens_text_follows_language() {
        let what: Vec<String> = with_language("ar", || {
            shortcut_lines(&Settings::default(), true)
                .into_iter()
                .map(|(_, w)| w)
                .collect()
        });
        assert_eq!(what, ["العجلة", "الجيب", "سجل الحافظة", "النسخ من الشاشة"]);
        let first = with_language("ar", || shortcut_lines(&Settings::default(), true))
            .remove(0)
            .0;
        assert!(first.starts_with("اسحب"), "{first}");
    }

    #[test]
    fn language_choices_are_the_languages() {
        assert_eq!(language_choices(), ["English", "العربية"]);
        for (i, code) in [(0, "en"), (1, "ar")] {
            assert_eq!(language_code_at(i), Some(code));
            assert_eq!(language_choice(code, &[]), i);
        }
        assert_eq!(language_code_at(2), None);
        assert_eq!(language_code_at(-1), None);
        // "auto" shows the language it resolves to.
        assert_eq!(language_choice("auto", &["ar-LY".into()]), 1);
        assert_eq!(language_choice("auto", &["fr".into()]), 0);
        assert_eq!(language_choice("auto", &[]), 0);
    }

    #[test]
    fn first_launch_writes_nothing_for_an_existing_user() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        assert!(!first_launch(&file, Some(&data), &["ar".into()]).unwrap());
        assert!(!file.exists());
    }

    #[test]
    fn first_launch_saves_pending_settings() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config").join("settings.toml");
        let data = dir.path().join("data");
        assert!(first_launch(&file, Some(&data), &["ar".into()]).unwrap());
        let saved = Settings::try_load(&file).unwrap();
        assert!(!saved.welcome_done);
        assert_eq!(saved.language, "ar");
        // The data folder is left for the features that use it.
        assert!(!data.exists());
    }
}
