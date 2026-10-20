//! User settings stored as TOML.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use log::warn;
use serde::{Deserialize, Serialize};

use crate::actions::ActionId;
use crate::geometry::{Layout, MAX_ACTIONS};
use crate::paths::PathFormat;
use crate::trigger::{Modifier, Modifiers};

/// User-configurable behaviour of the wheel.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// How the action cells are arranged.
    pub layout: Layout,
    /// Modifier that opens the wheel during a drag.
    pub trigger: Modifier,
    /// Actions shown on the wheel, in order (unique, at most `MAX_ACTIONS`).
    pub actions: Vec<ActionId>,
    /// Format used by the Copy Path action.
    pub copy_path_format: PathFormat,
    /// Bundle id of the terminal app to open; `None` (or an app that is not
    /// installed) means Terminal.app.
    pub terminal: Option<String>,
    /// Whether dropping a Pocket item onto a target removes it from the Pocket.
    pub pocket_remove_after_drop: bool,
    /// Whether the Pocket keeps its own copy of each dropped-in file.
    pub pocket_keep_copies: bool,
    /// Global shortcut that shows the Pocket, e.g. `"ctrl+alt+p"`; see
    /// [`parse_shortcut`].
    pub pocket_shortcut: String,
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
}

/// The most folders that can be pinned to the move ring.
pub const MAX_PINNED: usize = 7;

/// Default and bounds of [`Settings::move_hold_ms`].
const DEFAULT_MOVE_HOLD_MS: u32 = 400;
const MIN_MOVE_HOLD_MS: u32 = 200;
const MAX_MOVE_HOLD_MS: u32 = 1000;

fn default_pocket_shortcut() -> String {
    "ctrl+alt+p".to_owned()
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
            actions: default_actions(),
            copy_path_format: PathFormat::Plain,
            terminal: None,
            pocket_remove_after_drop: false,
            pocket_keep_copies: false,
            pocket_shortcut: default_pocket_shortcut(),
            pinned_folders: Vec::new(),
            move_hold_ms: DEFAULT_MOVE_HOLD_MS,
            hidden_actions: Vec::new(),
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
    actions: Vec<String>,
    copy_path_format: PathFormat,
    terminal: Option<String>,
    pocket_remove_after_drop: bool,
    pocket_keep_copies: bool,
    pocket_shortcut: String,
    pinned_folders: Vec<PathBuf>,
    move_hold_ms: u32,
    hidden_actions: Vec<String>,
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
            actions: s.actions.iter().map(|a| a.key().to_owned()).collect(),
            copy_path_format: s.copy_path_format,
            terminal: s.terminal.clone(),
            pocket_remove_after_drop: s.pocket_remove_after_drop,
            pocket_keep_copies: s.pocket_keep_copies,
            pocket_shortcut: s.pocket_shortcut.clone(),
            pinned_folders: s.pinned_folders.clone(),
            move_hold_ms: s.move_hold_ms,
            hidden_actions: s
                .hidden_actions
                .iter()
                .map(|a| a.key().to_owned())
                .collect(),
        }
    }
}

impl From<RawSettings> for Settings {
    fn from(raw: RawSettings) -> Self {
        let mut actions: Vec<ActionId> = Vec::new();
        for id in raw.actions.iter().filter_map(|k| ActionId::from_key(k)) {
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
        {
            if !actions.contains(&id) && !hidden_actions.contains(&id) {
                hidden_actions.push(id);
            }
        }
        let mut pinned_folders: Vec<PathBuf> = Vec::new();
        for folder in raw.pinned_folders {
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
            actions,
            copy_path_format: raw.copy_path_format,
            terminal: raw.terminal,
            pocket_remove_after_drop: raw.pocket_remove_after_drop,
            pocket_keep_copies: raw.pocket_keep_copies,
            pocket_shortcut: raw.pocket_shortcut,
            pinned_folders,
            move_hold_ms,
            hidden_actions,
        }
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
    /// text goes to `settings.toml.tmp` beside the file first and is renamed
    /// into place, so a crash never leaves a half-written file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(&RawSettings::from(self))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// Every action with whether it is shown on the wheel: the shown ones in
    /// wheel order, then the hidden ones, then any action in neither list
    /// (for example one added by a newer version), each exactly once.
    pub fn action_rows(&self) -> Vec<(ActionId, bool)> {
        let mut rows: Vec<(ActionId, bool)> = self.actions.iter().map(|a| (*a, true)).collect();
        for id in self.hidden_actions.iter().chain(&ActionId::ALL) {
            if !rows.iter().any(|(a, _)| a == id) {
                rows.push((*id, false));
            }
        }
        rows
    }

    /// Stores `rows` (as returned by [`Settings::action_rows`], possibly
    /// reordered or toggled): the shown rows become `actions`, the others
    /// `hidden_actions`. Returns `false` and changes nothing when no row is
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
        true
    }
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
        assert_eq!(s.action_rows().last(), Some(&(ActionId::CopyTo, false)));
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
        assert_eq!(
            Settings::load(&p).action_rows(),
            [
                (ActionId::Move, true),
                (ActionId::Copy, true),
                (ActionId::Terminal, false),
                (ActionId::Pocket, false),
                (ActionId::CopyPath, false),
                (ActionId::CopyTo, false),
            ]
        );
    }

    #[test]
    fn set_action_rows_round_trips_and_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        let mut s = Settings::default();
        let rows = [
            (ActionId::Terminal, true),
            (ActionId::Move, true),
            (ActionId::Copy, true),
            (ActionId::CopyTo, false),
            (ActionId::Pocket, false),
            (ActionId::CopyPath, false),
        ];
        assert!(s.set_action_rows(&rows));
        assert_eq!(s.action_rows(), rows);
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).action_rows(), rows);
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
    fn save_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        Settings::default().save(&p).unwrap();
        Settings::default().save(&p).unwrap();
        assert!(!dir.path().join("settings.toml.tmp").exists());
        assert_eq!(Settings::try_load(&p).unwrap(), Settings::default());
    }
}
