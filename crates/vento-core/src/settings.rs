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
}

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
        Settings {
            layout: raw.layout,
            trigger: raw.trigger,
            actions,
            copy_path_format: raw.copy_path_format,
            terminal: raw.terminal,
            pocket_remove_after_drop: raw.pocket_remove_after_drop,
            pocket_keep_copies: raw.pocket_keep_copies,
            pocket_shortcut: raw.pocket_shortcut,
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

    /// Writes settings to `path`, creating the parent folder if needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(&RawSettings::from(self))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)
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
}
