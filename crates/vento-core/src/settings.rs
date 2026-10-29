//! User settings stored as TOML.

use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use log::warn;
use serde::{Deserialize, Serialize};

use crate::actions::ActionId;
use crate::durable;
use crate::geometry::{Layout, MAX_ACTIONS};
use crate::paths::PathFormat;
use crate::trigger::{Modifier, Modifiers};

/// Whether the panels and windows use a light or dark look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow the light or dark look of the system.
    #[default]
    System,
    Light,
    Dark,
}

/// The colours of the Quick panel and the Settings window, each in a light
/// and a dark version. The wheel, the Pocket and toasts keep their own look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeName {
    /// White or black with neutral greys and Vento's orange.
    #[default]
    Vento,
    Github,
    Catppuccin,
    RosePine,
    TokyoNight,
    Gruvbox,
    Nord,
}

impl ThemeName {
    /// Every theme, in the order the Settings window lists them.
    pub const ALL: [Self; 7] = [
        Self::Vento,
        Self::Github,
        Self::Catppuccin,
        Self::RosePine,
        Self::TokyoNight,
        Self::Gruvbox,
        Self::Nord,
    ];

    /// The name the Settings window shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Vento => "Vento",
            Self::Github => "GitHub",
            Self::Catppuccin => "Catppuccin",
            Self::RosePine => "Rosé Pine",
            Self::TokyoNight => "Tokyo Night",
            Self::Gruvbox => "Gruvbox",
            Self::Nord => "Nord",
        }
    }
}

/// How a full Pocket (more than eight items) lays its items out. With eight
/// or fewer it looks the same in every layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PocketLayout {
    /// The first eight items, with a button for the rest.
    #[default]
    List,
    /// Every item, in a strip of cells that scrolls sideways.
    Scroll,
    /// The items grouped by kind, each group a stack to open.
    Stacks,
}

impl PocketLayout {
    /// Every layout, in the order the Settings window lists them.
    pub const ALL: [Self; 3] = [Self::List, Self::Scroll, Self::Stacks];
}

/// User-configurable behaviour of the wheel.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// How the action cells are arranged.
    pub layout: Layout,
    /// Modifier that opens the wheel during a drag.
    pub trigger: Modifier,
    /// Whether a quick back-and-forth shake during a file drag opens the
    /// wheel too.
    pub shake_to_open: bool,
    /// Actions shown on the wheel, in order (unique, at most `MAX_ACTIONS`).
    pub actions: Vec<ActionId>,
    /// Format used by the Copy path action.
    pub copy_path_format: PathFormat,
    /// Light or dark look of the Quick panel and the Settings window. The
    /// wheel, the Pocket and toasts stay dark either way.
    pub appearance: Appearance,
    /// Colours of the Quick panel and the Settings window.
    pub theme: ThemeName,
    /// Bundle id of the terminal app to open; `None` (or an app that is not
    /// installed) means Terminal.app.
    pub terminal: Option<String>,
    /// Whether dropping a Pocket item onto a target removes it from the Pocket.
    pub pocket_remove_after_drop: bool,
    /// Whether the Pocket keeps its own copy of each dropped-in file.
    pub pocket_keep_copies: bool,
    /// How the Pocket lays its items out once it holds more than eight.
    pub pocket_layout: PocketLayout,
    /// The language of the interface: the code of one of the languages
    /// Vento has (`"en"`, the default, or `"ar"`), or `"auto"` to follow the
    /// system's. Any other value means English; see `i18n::resolve`.
    pub language: String,
    /// Global shortcut that shows the Pocket, e.g. `"ctrl+alt+p"`; see
    /// [`parse_shortcut`].
    pub pocket_shortcut: String,
    /// Whether copies are recorded into the Clipboard history.
    pub clipboard_history: bool,
    /// Global shortcut that shows the island on the Clipboard history, in the
    /// same form as [`Settings::pocket_shortcut`].
    pub clipboard_shortcut: String,
    /// Global shortcut that starts Copy from screen, in the same form as
    /// [`Settings::pocket_shortcut`].
    pub screen_text_shortcut: String,
    /// Whether Keep Awake lets the screen dim, blank and lock as usual,
    /// keeping only the system awake.
    pub keep_awake_screen_off: bool,
    /// Folders pinned to the move ring, in order (unique, at most
    /// [`MAX_PINNED`]).
    pub pinned_folders: Vec<PathBuf>,
    /// How long the pointer must rest on Move or Copy to before its ring
    /// opens, in milliseconds (200 to 1000, in steps of 100).
    pub move_hold_ms: u32,
    /// Actions the user turned off. They keep their place in the settings
    /// list but are not on the wheel; an action is never in both this and
    /// `actions`.
    pub hidden_actions: Vec<ActionId>,
    /// Every action in the settings list's order, shown or not, so turning
    /// one on or off never moves it. Ignored when its shown actions are not
    /// in `actions`' order (a hand-edited file); empty in older files.
    pub action_order: Vec<ActionId>,
    /// The PDF window's To images format: `"png"` or `"jpeg"`. Any other
    /// value reads as `"png"`.
    pub pdf_image_format: String,
    /// The PDF window's To images resolution in DPI: 300 (Print) or 150
    /// (Screen). Any other value reads as 300.
    pub pdf_image_dpi: u32,
}

/// The most folders that can be pinned to the move ring.
pub const MAX_PINNED: usize = 7;

/// Default and bounds of [`Settings::move_hold_ms`], and the step it moves
/// in.
const DEFAULT_MOVE_HOLD_MS: u32 = 400;
pub const MIN_MOVE_HOLD_MS: u32 = 200;
pub const MAX_MOVE_HOLD_MS: u32 = 1000;
pub const MOVE_HOLD_STEP_MS: u32 = 100;

fn default_pocket_shortcut() -> String {
    "ctrl+alt+p".to_owned()
}

fn default_clipboard_shortcut() -> String {
    "ctrl+alt+v".to_owned()
}

/// Copy from screen's shortcut: ⌃⌥T on a Mac; elsewhere Ctrl+Alt+O (for
/// OCR), as GNOME opens a terminal on Ctrl+Alt+T.
fn default_screen_text_shortcut() -> String {
    if cfg!(target_os = "macos") {
        "ctrl+alt+t".to_owned()
    } else {
        "ctrl+alt+o".to_owned()
    }
}

fn default_actions() -> Vec<ActionId> {
    vec![
        ActionId::Pocket,
        ActionId::Copy,
        ActionId::Move,
        ActionId::CopyPath,
        ActionId::Terminal,
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            layout: Layout::Honeycomb,
            trigger: Modifier::Ctrl,
            shake_to_open: true,
            actions: default_actions(),
            copy_path_format: PathFormat::Plain,
            appearance: Appearance::System,
            theme: ThemeName::Vento,
            terminal: None,
            pocket_remove_after_drop: false,
            pocket_keep_copies: false,
            pocket_layout: PocketLayout::List,
            language: "en".to_owned(),
            pocket_shortcut: default_pocket_shortcut(),
            clipboard_history: true,
            clipboard_shortcut: default_clipboard_shortcut(),
            screen_text_shortcut: default_screen_text_shortcut(),
            keep_awake_screen_off: false,
            pinned_folders: Vec::new(),
            move_hold_ms: DEFAULT_MOVE_HOLD_MS,
            hidden_actions: Vec::new(),
            action_order: Vec::new(),
            pdf_image_format: "png".to_owned(),
            pdf_image_dpi: 300,
        }
    }
}

/// On-disk shape: actions are plain strings so unknown future keys don't
/// fail the whole file. Missing fields keep their defaults.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct RawSettings {
    layout: Layout,
    trigger: Modifier,
    shake_to_open: bool,
    actions: Vec<String>,
    copy_path_format: PathFormat,
    appearance: Appearance,
    theme: ThemeName,
    terminal: Option<String>,
    pocket_remove_after_drop: bool,
    pocket_keep_copies: bool,
    pocket_layout: PocketLayout,
    language: String,
    pocket_shortcut: String,
    clipboard_history: bool,
    clipboard_shortcut: String,
    screen_text_shortcut: String,
    keep_awake_screen_off: bool,
    pinned_folders: Vec<PathBuf>,
    move_hold_ms: u32,
    hidden_actions: Vec<String>,
    action_order: Vec<String>,
    pdf_image_format: String,
    pdf_image_dpi: u32,
}

impl Default for RawSettings {
    fn default() -> Self {
        RawSettings::from(&Settings::default())
    }
}

impl From<&Settings> for RawSettings {
    fn from(s: &Settings) -> Self {
        RawSettings {
            layout: s.layout,
            trigger: s.trigger,
            shake_to_open: s.shake_to_open,
            actions: s.actions.iter().map(|a| a.key().to_owned()).collect(),
            copy_path_format: s.copy_path_format,
            appearance: s.appearance,
            theme: s.theme,
            terminal: s.terminal.clone(),
            pocket_remove_after_drop: s.pocket_remove_after_drop,
            pocket_keep_copies: s.pocket_keep_copies,
            pocket_layout: s.pocket_layout,
            language: s.language.clone(),
            pocket_shortcut: s.pocket_shortcut.clone(),
            clipboard_history: s.clipboard_history,
            clipboard_shortcut: s.clipboard_shortcut.clone(),
            screen_text_shortcut: s.screen_text_shortcut.clone(),
            keep_awake_screen_off: s.keep_awake_screen_off,
            pinned_folders: s.pinned_folders.clone(),
            move_hold_ms: s.move_hold_ms,
            hidden_actions: s
                .hidden_actions
                .iter()
                .map(|a| a.key().to_owned())
                .collect(),
            action_order: s.action_order.iter().map(|a| a.key().to_owned()).collect(),
            pdf_image_format: s.pdf_image_format.clone(),
            pdf_image_dpi: s.pdf_image_dpi,
        }
    }
}

impl From<RawSettings> for Settings {
    fn from(raw: RawSettings) -> Self {
        let mut actions: Vec<ActionId> = Vec::new();
        for id in raw
            .actions
            .iter()
            .filter_map(|k| ActionId::from_key(k))
            .filter(|a| a.available())
        {
            if !actions.contains(&id) {
                actions.push(id);
            }
        }
        actions.truncate(MAX_ACTIONS);
        if actions.is_empty() {
            actions = default_actions();
        }
        // An action listed as both shown and hidden counts as shown.
        let mut hidden_actions: Vec<ActionId> = Vec::new();
        for id in raw
            .hidden_actions
            .iter()
            .filter_map(|k| ActionId::from_key(k))
            .filter(|a| a.available())
        {
            if !actions.contains(&id) && !hidden_actions.contains(&id) {
                hidden_actions.push(id);
            }
        }
        let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
        let mut action_order: Vec<ActionId> = Vec::new();
        for id in raw
            .action_order
            .iter()
            .filter_map(|k| ActionId::from_key(k))
            .filter(|a| a.available())
        {
            if !action_order.contains(&id) {
                action_order.push(id);
            }
        }
        let mut pinned_folders: Vec<PathBuf> = Vec::new();
        for folder in raw.pinned_folders {
            let folder = expand_home(folder, home.as_deref());
            if !pinned_folders.contains(&folder) {
                pinned_folders.push(folder);
            }
        }
        pinned_folders.truncate(MAX_PINNED);
        // Round to the nearest 100 (halves up), then keep within bounds.
        let move_hold_ms = (raw.move_hold_ms.saturating_add(50) / 100 * 100)
            .clamp(MIN_MOVE_HOLD_MS, MAX_MOVE_HOLD_MS);
        Settings {
            layout: raw.layout,
            trigger: raw.trigger,
            shake_to_open: raw.shake_to_open,
            actions,
            copy_path_format: raw.copy_path_format,
            appearance: raw.appearance,
            theme: raw.theme,
            terminal: raw.terminal,
            pocket_remove_after_drop: raw.pocket_remove_after_drop,
            pocket_keep_copies: raw.pocket_keep_copies,
            pocket_layout: raw.pocket_layout,
            language: raw.language,
            pocket_shortcut: raw.pocket_shortcut,
            clipboard_history: raw.clipboard_history,
            clipboard_shortcut: raw.clipboard_shortcut,
            screen_text_shortcut: raw.screen_text_shortcut,
            keep_awake_screen_off: raw.keep_awake_screen_off,
            pinned_folders,
            move_hold_ms,
            hidden_actions,
            action_order,
            pdf_image_format: if raw.pdf_image_format == "jpeg" {
                raw.pdf_image_format
            } else {
                "png".to_owned()
            },
            pdf_image_dpi: if raw.pdf_image_dpi == 150 { 150 } else { 300 },
        }
    }
}

/// `path` with a leading `~` (alone or followed by `/`) replaced by `home`,
/// as in a hand-written settings file. Other paths, and every path when
/// `home` is unknown, come back as they are.
fn expand_home(path: PathBuf, home: Option<&Path>) -> PathBuf {
    match (home, path.strip_prefix("~")) {
        (Some(home), Ok(rest)) => home.join(rest),
        _ => path,
    }
}

/// Why a settings file could not be loaded.
#[derive(thiserror::Error, Debug)]
pub enum SettingsError {
    /// The file exists but could not be read.
    #[error("cannot read the file: {0}")]
    Read(#[from] std::io::Error),
    /// The file is not valid settings TOML. `message` is one line.
    #[error("{}{message}", line.map_or_else(String::new, |l| format!("line {l}: ")))]
    Parse {
        /// 1-based line of the error, when known.
        line: Option<usize>,
        /// What is wrong, without the source excerpt.
        message: String,
    },
}

impl Settings {
    /// Loads settings from `path`: a missing file yields the defaults, an
    /// unreadable or invalid one an error.
    pub fn try_load(path: &Path) -> Result<Settings, SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(e) => return Err(SettingsError::Read(e)),
        };
        toml::from_str::<RawSettings>(&text)
            .map(Settings::from)
            .map_err(|e| SettingsError::Parse {
                line: e
                    .span()
                    .and_then(|span| text.get(..span.start))
                    .map(|before| before.matches('\n').count() + 1),
                message: e.message().trim().replace('\n', " "),
            })
    }

    /// Loads settings from `path`. Never fails: a missing or invalid file
    /// yields the defaults (invalid files are logged).
    pub fn load(path: &Path) -> Settings {
        Settings::try_load(path).unwrap_or_else(|e| {
            warn!("invalid settings {}: {e}", path.display());
            Settings::default()
        })
    }

    /// Writes settings to `path`, creating the parent folder if needed. The
    /// text goes to `<name>.tmp` beside the file first, is flushed to disk
    /// and renamed into place, so a crash never leaves a half-written file.
    /// When `path` is a symlink, the file it points to is the one replaced,
    /// so the link stays.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(&RawSettings::from(self))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Missing (or a dangling link): write `path` itself.
        let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = target.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            durable::sync(&file)?;
            std::fs::rename(&tmp, &target)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }

    /// Every action with whether it is shown on the wheel, each exactly once:
    /// in `action_order` when that agrees with the wheel order, otherwise
    /// the shown ones in wheel order then the hidden ones; any action in
    /// neither list (for example one added by a newer version) comes last.
    pub fn action_rows(&self) -> Vec<(ActionId, bool)> {
        let shown_in_order = self
            .action_order
            .iter()
            .filter(|a| self.actions.contains(a))
            .eq(&self.actions);
        let mut rows: Vec<(ActionId, bool)> = if shown_in_order {
            self.action_order
                .iter()
                .map(|a| (*a, self.actions.contains(a)))
                .collect()
        } else {
            self.actions.iter().map(|a| (*a, true)).collect()
        };
        let offered = ActionId::ALL.iter().filter(|a| a.available());
        for id in self.hidden_actions.iter().chain(offered) {
            if !rows.iter().any(|(a, _)| a == id) {
                rows.push((*id, false));
            }
        }
        rows
    }

    /// Stores `rows` (as returned by [`Settings::action_rows`], possibly
    /// reordered or toggled): the shown rows become `actions`, the others
    /// `hidden_actions`, and the rows' order is kept as `action_order`.
    /// Returns `false` and changes nothing when no row is
    /// shown or more than [`MAX_ACTIONS`] are.
    pub fn set_action_rows(&mut self, rows: &[(ActionId, bool)]) -> bool {
        let shown: Vec<ActionId> = rows.iter().filter(|(_, on)| *on).map(|(a, _)| *a).collect();
        if shown.is_empty() || shown.len() > MAX_ACTIONS {
            return false;
        }
        self.hidden_actions = rows
            .iter()
            .filter(|(_, on)| !*on)
            .map(|(a, _)| *a)
            .collect();
        self.actions = shown;
        self.action_order = rows.iter().map(|(a, _)| *a).collect();
        true
    }
}

/// Moves the item at `from` so it ends up at `to` (clamped to the last
/// index), shifting the ones in between. Does nothing when `from` is out of
/// range.
pub fn move_row<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from >= v.len() {
        return;
    }
    let to = to.min(v.len() - 1);
    let item = v.remove(from);
    v.insert(to, item);
}

/// Parses a shortcut such as `"ctrl+alt+p"` into its modifiers and key.
///
/// Lower-case, `+`-separated: one or more of `ctrl|alt|shift|meta` (`cmd` is
/// an alias of `meta`) in any order, then exactly one ASCII letter or digit.
/// At least one modifier is required, since a bare key would hijack typing.
/// The key is returned lower-case. `None` for anything else.
pub fn parse_shortcut(s: &str) -> Option<(Modifiers, char)> {
    let mut parts: Vec<&str> = s.split('+').collect();
    let key = parts.pop()?;
    let mut chars = key.chars();
    let (Some(key), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if !key.is_ascii_alphanumeric() {
        return None;
    }
    let mut modifiers = Modifiers::default();
    for part in &parts {
        match *part {
            "ctrl" => modifiers.ctrl = true,
            "alt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "meta" | "cmd" => modifiers.meta = true,
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some((modifiers, key.to_ascii_lowercase()))
}

/// Why a shortcut could not be registered.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ShortcutError {
    /// The spec is not a shortcut. Worded when shown, in the interface
    /// language.
    #[error("{}", invalid_shortcut(.0))]
    Invalid(String),
    /// The system refused the key, in its own (English) words.
    #[error("{0}")]
    Refused(String),
}

/// `invalid shortcut "spec"`, in the interface language.
fn invalid_shortcut(spec: &str) -> String {
    crate::i18n::fill(
        &crate::i18n::tr("invalid shortcut \"{spec}\""),
        &[("spec", spec)],
    )
}

/// Whether macOS keeps `spec` for itself: Cmd alone (quit, hide, copy and
/// the like), Cmd+Shift+3/4/5 (screenshots), or Shift alone (typing a capital
/// letter). A spec that does not parse is not reserved; it is simply invalid.
pub fn is_reserved_shortcut(spec: &str) -> bool {
    let Some((m, key)) = parse_shortcut(spec) else {
        return false;
    };
    let only = |ctrl, alt, shift, meta| {
        m == Modifiers {
            ctrl,
            alt,
            shift,
            meta,
        }
    };
    only(false, false, false, true)
        || only(false, false, true, false)
        || (only(false, false, true, true) && matches!(key, '3' | '4' | '5'))
}

/// How macOS writes the shortcut `spec`: its modifiers as symbols in the
/// order ⌃⌥⇧⌘, then the key upper-cased (`"ctrl+alt+p"` → `"⌃⌥P"`). `None`
/// when it does not parse.
pub fn shortcut_symbols(spec: &str) -> Option<String> {
    let (m, key) = parse_shortcut(spec)?;
    let mut text = String::new();
    for (held, symbol) in [(m.ctrl, '⌃'), (m.alt, '⌥'), (m.shift, '⇧'), (m.meta, '⌘')] {
        if held {
            text.push(symbol);
        }
    }
    text.push(key.to_ascii_uppercase());
    Some(text)
}

/// How GNOME writes the shortcut `spec`: its modifiers as words in GTK's
/// order Shift, Ctrl, Alt, Super, then the key upper-cased, all joined by
/// `+` (`"ctrl+alt+p"` → `"Ctrl+Alt+P"`). `None` when it does not parse.
pub fn shortcut_label(spec: &str) -> Option<String> {
    let (m, key) = parse_shortcut(spec)?;
    let mut text = String::new();
    for (held, word) in [
        (m.shift, "Shift+"),
        (m.ctrl, "Ctrl+"),
        (m.alt, "Alt+"),
        (m.meta, "Super+"),
    ] {
        if held {
            text.push_str(word);
        }
    }
    text.push(key.to_ascii_uppercase());
    Some(text)
}

/// Location of the settings file.
pub fn settings_path() -> PathBuf {
    match ProjectDirs::from("", "", "vento") {
        Some(dirs) => dirs.config_dir().join("settings.toml"),
        None => PathBuf::from("settings.toml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_shortcut_is_worded_when_it_is_shown() {
        use crate::i18n::with_language;
        let error = ShortcutError::Invalid("p".into());
        assert_eq!(error.to_string(), "invalid shortcut \"p\"");
        with_language("ar", || {
            assert_eq!(error.to_string(), "اختصار غير صالح \"p\"");
        });
        // Made while the language was Arabic, read in English for a log line.
        assert_eq!(
            with_language("en", || error.to_string()),
            "invalid shortcut \"p\""
        );
        // The system's own words are shown as they are.
        let refused = ShortcutError::Refused("in use".into());
        assert_eq!(with_language("ar", || refused.to_string()), "in use");
    }

    /// Settings read from `text`, as a settings file would be.
    fn parse(text: &str) -> Settings {
        Settings::from(toml::from_str::<RawSettings>(text).unwrap())
    }

    #[test]
    fn image_settings_default_and_clamp() {
        let s = parse("pdf_image_dpi = 600");
        assert_eq!(s.pdf_image_dpi, 300);
        assert_eq!(Settings::default().pdf_image_format, "png");
    }

    #[test]
    fn image_settings_keep_the_two_choices_and_nothing_else() {
        let s = parse("pdf_image_format = \"jpeg\"\npdf_image_dpi = 150");
        assert_eq!(s.pdf_image_format, "jpeg");
        assert_eq!(s.pdf_image_dpi, 150);
        let s = parse("pdf_image_format = \"tiff\"\npdf_image_dpi = 0");
        assert_eq!(s.pdf_image_format, "png");
        assert_eq!(s.pdf_image_dpi, 300);
        let s = parse("");
        assert_eq!((s.pdf_image_format.as_str(), s.pdf_image_dpi), ("png", 300));
    }

    #[test]
    fn image_settings_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            pdf_image_format: "jpeg".into(),
            pdf_image_dpi: 150,
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::try_load(&p).unwrap(), s);
    }

    #[test]
    fn default_settings() {
        let s = Settings::default();
        assert_eq!(s.layout, Layout::Honeycomb);
        assert_eq!(s.trigger, Modifier::Ctrl);
        assert_eq!(
            s.actions,
            [
                ActionId::Pocket,
                ActionId::Copy,
                ActionId::Move,
                ActionId::CopyPath,
                ActionId::Terminal
            ]
        );
        assert_eq!(s.copy_path_format, PathFormat::Plain);
        assert_eq!(s.terminal, None);
    }

    #[test]
    fn default_actions_start_with_pocket() {
        assert_eq!(default_actions()[0], ActionId::Pocket);
        assert_eq!(Settings::default().actions, default_actions());
    }

    #[test]
    fn old_settings_file_without_pocket_keys_loads_with_defaults() {
        // The 1a default file, verbatim.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(
            &p,
            "layout = \"honeycomb\"\ntrigger = \"ctrl\"\n\
             actions = [\"copy\", \"move\", \"copy_path\", \"terminal\"]\n\
             copy_path_format = \"plain\"\n",
        )
        .unwrap();
        let s = Settings::try_load(&p).unwrap();
        assert_eq!(
            s.actions,
            [
                ActionId::Copy,
                ActionId::Move,
                ActionId::CopyPath,
                ActionId::Terminal
            ]
        );
        assert!(!s.pocket_remove_after_drop);
        assert!(!s.pocket_keep_copies);
        assert_eq!(s.pocket_shortcut, "ctrl+alt+p");
    }

    #[test]
    fn pocket_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            pocket_remove_after_drop: true,
            pocket_keep_copies: true,
            pocket_shortcut: "cmd+shift+9".into(),
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn clipboard_defaults() {
        let s = Settings::default();
        assert!(s.clipboard_history);
        assert_eq!(s.clipboard_shortcut, "ctrl+alt+v");
    }

    #[test]
    fn old_file_without_clipboard_keys_loads() {
        // A file from before the Clipboard, with the Pocket keys already set.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(
            &p,
            "layout = \"honeycomb\"\ntrigger = \"ctrl\"\n\
             pocket_shortcut = \"cmd+shift+9\"\n",
        )
        .unwrap();
        let s = Settings::try_load(&p).unwrap();
        assert!(s.clipboard_history);
        assert_eq!(s.clipboard_shortcut, "ctrl+alt+v");
        assert_eq!(s.pocket_shortcut, "cmd+shift+9");
    }

    #[test]
    fn clipboard_keys_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        // Non-default values, so a key lost on save would load back as its
        // default and fail the comparison.
        let s = Settings {
            clipboard_history: false,
            clipboard_shortcut: "cmd+shift+c".into(),
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn keep_awake_keeps_the_screen_on_unless_saved_otherwise() {
        assert!(!Settings::default().keep_awake_screen_off);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            keep_awake_screen_off: true,
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn appearance_follows_the_system_unless_saved_otherwise() {
        assert_eq!(Settings::default().appearance, Appearance::System);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            appearance: Appearance::Dark,
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("appearance = \"dark\"")
        );
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn the_theme_is_vento_unless_saved_otherwise() {
        assert_eq!(Settings::default().theme, ThemeName::Vento);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            theme: ThemeName::RosePine,
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("theme = \"rose_pine\"")
        );
        assert_eq!(Settings::load(&p), s);
        let (_dir, old) = write_temp("trigger = \"ctrl\"\n");
        assert_eq!(Settings::try_load(&old).unwrap().theme, ThemeName::Vento);
    }

    #[test]
    fn the_pocket_lists_its_items_unless_saved_otherwise() {
        assert_eq!(Settings::default().pocket_layout, PocketLayout::List);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            pocket_layout: PocketLayout::Stacks,
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("pocket_layout = \"stacks\"")
        );
        assert_eq!(Settings::load(&p), s);
        let (_dir, old) = write_temp("trigger = \"ctrl\"\n");
        assert_eq!(
            Settings::try_load(&old).unwrap().pocket_layout,
            PocketLayout::List
        );
    }

    #[test]
    fn the_language_is_english_unless_saved_otherwise() {
        assert_eq!(Settings::default().language, "en");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            language: "ar".to_owned(),
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("language = \"ar\"")
        );
        assert_eq!(Settings::load(&p), s);
        let (_dir, old) = write_temp("trigger = \"ctrl\"\n");
        assert_eq!(Settings::try_load(&old).unwrap().language, "en");
        // A value Vento does not know loads as it is; `i18n::resolve`
        // treats it as English.
        let (_dir, odd) = write_temp("language = \"xx\"\n");
        assert_eq!(Settings::try_load(&odd).unwrap().language, "xx");
    }

    #[test]
    fn old_file_without_appearance_follows_the_system() {
        let (_dir, p) = write_temp("trigger = \"ctrl\"\n");
        assert_eq!(
            Settings::try_load(&p).unwrap().appearance,
            Appearance::System
        );
    }

    #[test]
    fn a_misspelt_appearance_rejects_the_file_like_any_other_choice() {
        let (_dir, p) = write_temp("appearance = \"sepia\"\n");
        assert!(matches!(
            Settings::try_load(&p),
            Err(SettingsError::Parse { line: Some(1), .. })
        ));
    }

    #[test]
    fn screen_text_defaults() {
        let expected = if cfg!(target_os = "macos") {
            "ctrl+alt+t"
        } else {
            "ctrl+alt+o"
        };
        assert_eq!(Settings::default().screen_text_shortcut, expected);
    }

    #[test]
    fn old_file_without_screen_text_key_loads() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, "trigger = \"ctrl\"\n").unwrap();
        let s = Settings::try_load(&p).unwrap();
        assert_eq!(s.screen_text_shortcut, default_screen_text_shortcut());
    }

    #[test]
    fn screen_text_key_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            screen_text_shortcut: "cmd+shift+e".into(),
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn shake_to_open_defaults_on_and_round_trips() {
        assert!(Settings::default().shake_to_open);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        // A file from before the key: on.
        std::fs::write(&p, "trigger = \"ctrl\"\n").unwrap();
        assert!(Settings::try_load(&p).unwrap().shake_to_open);
        let s = Settings {
            shake_to_open: false,
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn parse_shortcut_accepts_ctrl_alt_p_and_cmd_alias() {
        let (m, k) = parse_shortcut("ctrl+alt+p").unwrap();
        assert_eq!(
            m,
            Modifiers {
                ctrl: true,
                alt: true,
                ..Modifiers::default()
            }
        );
        assert_eq!(k, 'p');
        let (m, k) = parse_shortcut("cmd+shift+7").unwrap();
        assert_eq!(
            m,
            Modifiers {
                meta: true,
                shift: true,
                ..Modifiers::default()
            }
        );
        assert_eq!(k, '7');
        assert_eq!(parse_shortcut("meta+x"), parse_shortcut("cmd+x"));
        // Order is free; the key is lower-cased.
        assert_eq!(parse_shortcut("alt+ctrl+P"), parse_shortcut("ctrl+alt+p"));
    }

    #[test]
    fn parse_shortcut_rejects_bad() {
        for bad in ["", "ctrl+", "ctrl+pp", "hyper+p", "ctrl+é", "+p", "ctrl++p"] {
            assert_eq!(parse_shortcut(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn reserved_shortcuts() {
        for reserved in ["cmd+q", "cmd+shift+4", "shift+a", "meta+h", "cmd+shift+3"] {
            assert!(is_reserved_shortcut(reserved), "{reserved:?}");
        }
        for free in ["ctrl+alt+p", "cmd+alt+p", "cmd+shift+a", "ctrl+p", "bogus"] {
            assert!(!is_reserved_shortcut(free), "{free:?}");
        }
    }

    #[test]
    fn parse_shortcut_requires_a_modifier() {
        assert_eq!(parse_shortcut("p"), None);
    }

    #[test]
    fn shortcut_symbols() {
        assert_eq!(
            super::shortcut_symbols("ctrl+alt+p").as_deref(),
            Some("⌃⌥P")
        );
        assert_eq!(
            super::shortcut_symbols("cmd+shift+7").as_deref(),
            Some("⇧⌘7")
        );
        // Always in the macOS order ⌃⌥⇧⌘, whatever order the spec uses.
        assert_eq!(
            super::shortcut_symbols("meta+shift+alt+ctrl+k").as_deref(),
            Some("⌃⌥⇧⌘K")
        );
        assert_eq!(super::shortcut_symbols("ctrl+p+q"), None);
        assert_eq!(super::shortcut_symbols("p"), None);
    }

    #[test]
    fn shortcut_label() {
        assert_eq!(
            super::shortcut_label("ctrl+alt+p").as_deref(),
            Some("Ctrl+Alt+P")
        );
        // Always in GTK's order Shift, Ctrl, Alt, Super.
        assert_eq!(
            super::shortcut_label("meta+alt+ctrl+shift+k").as_deref(),
            Some("Shift+Ctrl+Alt+Super+K")
        );
        assert_eq!(super::shortcut_label("cmd+7").as_deref(), Some("Super+7"));
        assert_eq!(super::shortcut_label("ctrl+p+q"), None);
        assert_eq!(super::shortcut_label("p"), None);
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::load(&dir.path().join("nope.toml")),
            Settings::default()
        );
    }

    #[test]
    fn load_invalid_toml_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, "layout = [").unwrap();
        assert_eq!(Settings::load(&p), Settings::default());
    }

    #[test]
    fn try_load_missing_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::try_load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn try_load_reads_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, "layout = \"petals\"\n").unwrap();
        assert_eq!(Settings::try_load(&p).unwrap().layout, Layout::Petals);
    }

    #[test]
    fn try_load_reports_a_parse_error_with_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, "trigger = \"alt\"\nlayout = \"hexagons\"\n").unwrap();
        let err = Settings::try_load(&p).unwrap_err();
        assert!(
            matches!(err, SettingsError::Parse { line: Some(2), .. }),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(text.starts_with("line 2: "), "{text}");
        assert!(!text.contains('\n'), "short reason is one line: {text}");
    }

    #[test]
    fn try_load_reports_a_read_error() {
        let dir = tempfile::tempdir().unwrap();
        // A folder where the file should be cannot be read as text.
        let err = Settings::try_load(dir.path()).unwrap_err();
        assert!(matches!(err, SettingsError::Read(_)), "{err:?}");
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, "trigger = \"alt\"\n").unwrap();
        let s = Settings::load(&p);
        assert_eq!(s.trigger, Modifier::Alt);
        assert_eq!(
            Settings {
                trigger: Modifier::Ctrl,
                ..s
            },
            Settings::default()
        );
    }

    #[test]
    fn unknown_and_duplicate_actions() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, "actions = [\"copy\",\"teleport\",\"move\",\"copy\"]\n").unwrap();
        assert_eq!(Settings::load(&p).actions, [ActionId::Copy, ActionId::Move]);
    }

    #[test]
    fn no_valid_actions_falls_back_to_default_list() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, "actions = [\"teleport\"]\n").unwrap();
        assert_eq!(Settings::load(&p).actions, default_actions());
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("settings.toml");
        let s = Settings {
            layout: Layout::Petals,
            trigger: Modifier::Alt,
            actions: vec![ActionId::Terminal, ActionId::Copy],
            copy_path_format: PathFormat::ShellQuoted,
            terminal: Some("com.googlecode.iterm2".into()),
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    fn write_temp(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        std::fs::write(&p, text).unwrap();
        (dir, p)
    }

    #[test]
    fn old_file_without_new_keys_loads_defaults() {
        let (_dir, p) = write_temp("layout = \"petals\"\n");
        let s = Settings::try_load(&p).unwrap();
        assert!(s.pinned_folders.is_empty());
        assert_eq!(s.move_hold_ms, 400);
        assert!(s.hidden_actions.is_empty());
        assert_eq!(s.action_rows().last(), Some(&last_action_row()));
    }

    /// The last row the Wheel page offers on this platform: Share where it is
    /// available, Convert elsewhere.
    fn last_action_row() -> (ActionId, bool) {
        let last = ActionId::ALL.iter().rev().find(|a| a.available());
        (*last.unwrap(), false)
    }

    #[test]
    fn macos_only_actions_are_dropped_elsewhere() {
        let (_dir, p) = write_temp(
            "actions = [\"copy\", \"pdf\", \"share\"]\nhidden_actions = [\"pdf\"]\naction_order = [\"pdf\", \"copy\"]\n",
        );
        let s = Settings::load(&p);
        if cfg!(target_os = "macos") {
            assert_eq!(s.actions, [ActionId::Copy, ActionId::Pdf, ActionId::Share]);
        } else {
            assert_eq!(s.actions, [ActionId::Copy]);
            assert!(s.hidden_actions.is_empty());
            assert!(!s.action_order.contains(&ActionId::Pdf));
        }
    }

    #[test]
    fn not_yet_placed_actions_are_only_available_ones() {
        let rows = Settings::default().action_rows();
        let has_pdf = rows.iter().any(|(a, _)| *a == ActionId::Pdf);
        assert_eq!(has_pdf, cfg!(target_os = "macos"));
        assert!(rows.iter().all(|(a, _)| a.available()));
    }

    #[test]
    fn archive_is_off_by_default() {
        let rows = Settings::default().action_rows();
        assert!(rows.contains(&(ActionId::Archive, false)));
        assert!(!Settings::default().actions.contains(&ActionId::Archive));
    }

    #[test]
    fn convert_is_off_by_default() {
        let rows = Settings::default().action_rows();
        assert!(rows.contains(&(ActionId::Convert, false)));
        assert!(!Settings::default().actions.contains(&ActionId::Convert));
    }

    #[test]
    fn info_is_not_on_the_default_wheel() {
        assert!(!Settings::default().actions.contains(&ActionId::Info));
        let (_dir, p) = write_temp("actions = [\"copy\", \"info\"]\n");
        assert_eq!(Settings::load(&p).actions, [ActionId::Copy, ActionId::Info]);
    }

    #[test]
    fn hold_is_clamped_and_rounded() {
        for (raw, want) in [
            (50, 200),
            (1234, 1000),
            (449, 400),
            (450, 500),
            (u32::MAX, 1000),
        ] {
            let (_dir, p) = write_temp(&format!("move_hold_ms = {raw}\n"));
            assert_eq!(Settings::load(&p).move_hold_ms, want, "{raw}");
        }
    }

    #[test]
    fn pinned_folders_dedupe_and_cap() {
        let (_dir, p) = write_temp(
            "pinned_folders = [\"/a\", \"/b\", \"/a\", \"/c\", \"/d\", \"/e\", \"/f\", \"/g\", \"/h\", \"/i\"]\n",
        );
        let want: Vec<PathBuf> = ["/a", "/b", "/c", "/d", "/e", "/f", "/g"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(Settings::load(&p).pinned_folders, want);
    }

    #[test]
    fn action_in_both_lists_counts_as_shown() {
        let (_dir, p) = write_temp(
            "actions = [\"move\"]\nhidden_actions = [\"move\", \"copy\", \"copy\", \"teleport\"]\n",
        );
        let s = Settings::load(&p);
        assert_eq!(s.actions, [ActionId::Move]);
        assert_eq!(s.hidden_actions, [ActionId::Copy]);
    }

    #[test]
    fn action_rows_shown_then_hidden_then_new() {
        let (_dir, p) =
            write_temp("actions = [\"move\", \"copy\"]\nhidden_actions = [\"terminal\"]\n");
        let mut expected = vec![
            (ActionId::Move, true),
            (ActionId::Copy, true),
            (ActionId::Terminal, false),
            (ActionId::Pocket, false),
            (ActionId::CopyPath, false),
            (ActionId::CopyTo, false),
            (ActionId::Archive, false),
            (ActionId::Info, false),
            (ActionId::Convert, false),
        ];
        if ActionId::Pdf.available() {
            expected.push((ActionId::Pdf, false));
        }
        if ActionId::Share.available() {
            expected.push((ActionId::Share, false));
        }
        assert_eq!(Settings::load(&p).action_rows(), expected);
    }

    #[test]
    fn set_action_rows_round_trips_and_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let mut s = Settings::default();
        let mut rows = vec![
            (ActionId::Terminal, true),
            (ActionId::Move, true),
            (ActionId::Copy, true),
            (ActionId::CopyTo, false),
            (ActionId::Pocket, false),
            (ActionId::CopyPath, false),
            (ActionId::Archive, false),
            (ActionId::Info, false),
            (ActionId::Convert, false),
        ];
        if ActionId::Pdf.available() {
            rows.push((ActionId::Pdf, false));
        }
        if ActionId::Share.available() {
            rows.push((ActionId::Share, false));
        }
        assert!(s.set_action_rows(&rows));
        assert_eq!(s.action_rows(), rows);
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).action_rows(), rows);
    }

    /// `rows` with the action `id` switched to `on`.
    fn toggled(rows: &[(ActionId, bool)], id: ActionId, on: bool) -> Vec<(ActionId, bool)> {
        rows.iter()
            .map(|&(a, shown)| (a, if a == id { on } else { shown }))
            .collect()
    }

    #[test]
    fn turning_an_action_off_keeps_its_place() {
        let mut s = Settings::default();
        let rows = toggled(&s.action_rows(), ActionId::Copy, false);
        assert!(s.set_action_rows(&rows));
        assert_eq!(s.action_rows(), rows);
        assert!(!s.actions.contains(&ActionId::Copy));
    }

    #[test]
    fn turning_an_action_on_keeps_its_place_and_wheel_order() {
        let mut s = Settings::default();
        let off = toggled(&s.action_rows(), ActionId::Copy, false);
        assert!(s.set_action_rows(&off));
        let on = toggled(&s.action_rows(), ActionId::Copy, true);
        assert!(s.set_action_rows(&on));
        assert_eq!(s.action_rows(), on);
        assert_eq!(s.actions, default_actions());
    }

    #[test]
    fn action_order_survives_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let mut s = Settings::default();
        let rows = toggled(&s.action_rows(), ActionId::Move, false);
        assert!(s.set_action_rows(&rows));
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).action_rows(), rows);
    }

    #[test]
    fn hand_edited_actions_win_over_a_stale_order() {
        let (_dir, p) = write_temp(
            "actions = [\"move\", \"copy\"]\nhidden_actions = [\"terminal\"]\n\
             action_order = [\"copy\", \"terminal\", \"move\"]\n",
        );
        let rows = Settings::load(&p).action_rows();
        assert_eq!(
            rows[..3],
            [
                (ActionId::Move, true),
                (ActionId::Copy, true),
                (ActionId::Terminal, false),
            ]
        );
    }

    #[test]
    fn set_action_rows_refuses_all_hidden() {
        let mut s = Settings::default();
        let rows: Vec<_> = s
            .action_rows()
            .into_iter()
            .map(|(a, _)| (a, false))
            .collect();
        assert!(!s.set_action_rows(&rows));
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn new_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let s = Settings {
            pinned_folders: vec![PathBuf::from("/Users/x/Projects"), PathBuf::from("/tmp")],
            move_hold_ms: 700,
            hidden_actions: vec![ActionId::Terminal],
            actions: vec![ActionId::Pocket, ActionId::CopyTo],
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn move_row_moves_and_clamps() {
        let rows = || vec!['a', 'b', 'c', 'd'];
        let mut v = rows();
        move_row(&mut v, 0, 2);
        assert_eq!(v, ['b', 'c', 'a', 'd']);
        let mut v = rows();
        move_row(&mut v, 3, 0);
        assert_eq!(v, ['d', 'a', 'b', 'c']);
        let mut v = rows();
        move_row(&mut v, 1, 9);
        assert_eq!(v, ['a', 'c', 'd', 'b']);
    }

    #[test]
    fn pinned_folders_expand_a_leading_tilde() {
        let home = directories::BaseDirs::new()
            .expect("a home folder")
            .home_dir()
            .to_path_buf();
        let (_dir, p) = write_temp(&format!(
            // A literal string: a Windows path's `\` is not an escape there.
            "pinned_folders = [\"~/Work\", \"~\", '{}', \"~other/x\", \"/a/~/b\"]\n",
            home.join("Work").display()
        ));
        let want = [
            home.join("Work"),
            home.clone(),
            PathBuf::from("~other/x"),
            PathBuf::from("/a/~/b"),
        ];
        assert_eq!(Settings::load(&p).pinned_folders, want);
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_a_symlinked_file_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("dotfiles");
        std::fs::create_dir(&real_dir).unwrap();
        let real = real_dir.join("vento.toml");
        std::fs::write(&real, "layout = \"petals\"\n").unwrap();
        let link = dir.path().join("settings.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let s = Settings {
            move_hold_ms: 700,
            ..Settings::default()
        };
        s.save(&link).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(Settings::try_load(&real).unwrap(), s);
        assert_eq!(Settings::try_load(&link).unwrap(), s);
        assert!(!real_dir.join("vento.toml.tmp").exists());
        assert!(!dir.path().join("settings.toml.tmp").exists());
    }

    #[test]
    fn save_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        Settings::default().save(&p).unwrap();
        Settings::default().save(&p).unwrap();
        assert!(!dir.path().join("settings.toml.tmp").exists());
        assert_eq!(Settings::try_load(&p).unwrap(), Settings::default());
    }
}
