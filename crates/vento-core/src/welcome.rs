//! Rules of the first-launch Welcome setup: spotting a fresh install, the
//! settings it starts with, and the wheel presets it offers. No window code
//! lives here.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::actions::ActionId;
use crate::i18n::{self, tr};
use crate::paths::PathFormat;
use crate::settings::Settings;

/// Vento's data folder (`ProjectDirs::data_dir()`); `None` when the system
/// gives no home folder.
pub fn data_dir() -> Option<PathBuf> {
    Some(ProjectDirs::from("", "", "vento")?.data_dir().to_path_buf())
}

/// Whether this is the first launch: the settings file does not exist and
/// the data folder is unknown or does not exist either. Either one alone
/// means Vento ran here before.
pub fn is_fresh_install(settings_file: &Path, data_dir: Option<&Path>) -> bool {
    !settings_file.exists() && data_dir.is_none_or(|d| !d.exists())
}

/// The settings a fresh install starts with: the defaults, the Welcome setup
/// still to do, and the interface language set to the one that `"auto"`
/// picks from the system's `preferred` languages.
pub fn first_settings(preferred: &[String]) -> Settings {
    Settings {
        welcome_done: false,
        language: i18n::resolve("auto", preferred).code.to_owned(),
        ..Settings::default()
    }
}

/// A set of wheel actions offered on the Wheel screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// The default wheel: Pocket, Copy, Move, Copy path, Terminal.
    Essentials,
    /// Essentials plus Archive and Info, with paths copied shell-quoted.
    Developer,
    /// Pocket, Copy, Move, Convert, PDF, Share and Info.
    Designer,
    /// Every action Vento has on this platform.
    Everything,
}

impl Preset {
    /// The presets in the order the cards are shown.
    pub const ALL: [Preset; 4] = [
        Preset::Essentials,
        Preset::Developer,
        Preset::Designer,
        Preset::Everything,
    ];

    /// The wheel's actions, in order; the ones this platform cannot run are
    /// left out.
    pub fn actions(self) -> Vec<ActionId> {
        use ActionId::*;
        let list: &[ActionId] = match self {
            Preset::Essentials => &[Pocket, Copy, Move, CopyPath, Terminal],
            Preset::Developer => &[Pocket, Copy, Move, CopyPath, Terminal, Archive, Info],
            Preset::Designer => &[Pocket, Copy, Move, Convert, Pdf, Share, Info],
            Preset::Everything => &ActionId::ALL,
        };
        list.iter().copied().filter(|a| a.available()).collect()
    }

    /// The path format the preset copies paths in.
    fn path_format(self) -> PathFormat {
        match self {
            Preset::Developer => PathFormat::ShellQuoted,
            _ => PathFormat::Plain,
        }
    }

    /// The card's title, in the interface language.
    pub fn label(self) -> String {
        match self {
            Preset::Essentials => tr("Essentials"),
            Preset::Developer => tr("Developer"),
            Preset::Designer => tr("Designer"),
            Preset::Everything => tr("Everything"),
        }
    }

    /// The card's second line: the names of the shown actions joined with
    /// the language's list separator, or a plain "Every action".
    pub fn summary(self) -> String {
        if self == Preset::Everything {
            return tr("Every action");
        }
        let separator = if i18n::current().rtl { "، " } else { ", " };
        self.actions()
            .iter()
            .map(|a| a.label())
            .collect::<Vec<_>>()
            .join(separator)
    }
}

/// Puts `p`'s actions on the wheel, the rest of the available actions hidden
/// after them in [`ActionId::ALL`] order, and sets the path format `p` uses.
pub fn apply_preset(s: &mut Settings, p: Preset) {
    let shown = p.actions();
    let mut rows: Vec<(ActionId, bool)> = shown.iter().map(|a| (*a, true)).collect();
    rows.extend(
        ActionId::ALL
            .iter()
            .filter(|a| a.available() && !shown.contains(a))
            .map(|a| (*a, false)),
    );
    let applied = s.set_action_rows(&rows);
    debug_assert!(applied, "a preset fits the wheel");
    s.copy_path_format = p.path_format();
}

/// The preset the settings match: same shown actions in the same order and
/// the same path format; `None` for a custom wheel.
pub fn preset_of(s: &Settings) -> Option<Preset> {
    Preset::ALL
        .into_iter()
        .find(|p| p.actions() == s.actions && p.path_format() == s.copy_path_format)
}

/// The folders under `home` worth offering to pin: Desktop, Documents and
/// Downloads, in that order, those that exist as folders.
pub fn suggested_folders(home: &Path) -> Vec<PathBuf> {
    ["Desktop", "Documents", "Downloads"]
        .iter()
        .map(|name| home.join(name))
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionId;
    use crate::paths::PathFormat;
    use crate::settings::Settings;

    #[test]
    fn nothing_there_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        let data = dir.path().join("data");
        assert!(is_fresh_install(&file, Some(&data)));
    }

    #[test]
    fn settings_file_alone_is_not_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        std::fs::write(&file, "").unwrap();
        assert!(!is_fresh_install(&file, Some(&dir.path().join("data"))));
    }

    #[test]
    fn data_folder_alone_is_not_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        assert!(!is_fresh_install(&file, Some(&data)));
    }

    #[test]
    fn both_there_is_not_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        let data = dir.path().join("data");
        std::fs::write(&file, "").unwrap();
        std::fs::create_dir(&data).unwrap();
        assert!(!is_fresh_install(&file, Some(&data)));
    }

    #[test]
    fn unknown_data_folder_and_no_file_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_fresh_install(&dir.path().join("settings.toml"), None));
    }

    #[test]
    fn first_settings_takes_the_system_language() {
        let s = first_settings(&["ar-LY".into()]);
        assert_eq!(s.language, "ar");
        assert!(!s.welcome_done);
        let s = first_settings(&["fr-FR".into()]);
        assert_eq!(s.language, "en");
        assert_eq!(
            s,
            Settings {
                welcome_done: false,
                ..Settings::default()
            }
        );
    }

    #[test]
    fn each_preset_sets_its_actions() {
        for p in Preset::ALL {
            let mut s = Settings::default();
            apply_preset(&mut s, p);
            assert_eq!(s.actions, p.actions());
            let rest: Vec<ActionId> = ActionId::ALL
                .into_iter()
                .filter(|a| a.available() && !s.actions.contains(a))
                .collect();
            assert_eq!(s.hidden_actions, rest);
            assert_eq!(preset_of(&s), Some(p));
        }
    }

    #[test]
    fn presets_have_their_exact_actions() {
        use ActionId::*;
        assert_eq!(
            Preset::Essentials.actions(),
            [Pocket, Copy, Move, CopyPath, Terminal]
        );
        assert_eq!(
            Preset::Developer.actions(),
            [Pocket, Copy, Move, CopyPath, Terminal, Archive, Info]
        );
        let designer: Vec<ActionId> = [Pocket, Copy, Move, Convert, Pdf, Share, Info]
            .into_iter()
            .filter(|a| a.available())
            .collect();
        assert_eq!(Preset::Designer.actions(), designer);
    }

    #[test]
    fn developer_quotes_paths() {
        let mut s = Settings::default();
        apply_preset(&mut s, Preset::Developer);
        assert_eq!(s.copy_path_format, PathFormat::ShellQuoted);
    }

    #[test]
    fn essentials_after_developer_restores_plain_paths() {
        let mut s = Settings::default();
        apply_preset(&mut s, Preset::Developer);
        apply_preset(&mut s, Preset::Essentials);
        assert_eq!(s.copy_path_format, PathFormat::Plain);
        assert_eq!(preset_of(&s), Some(Preset::Essentials));
    }

    #[test]
    fn a_preset_needs_its_path_format() {
        let mut s = Settings::default();
        apply_preset(&mut s, Preset::Developer);
        s.copy_path_format = PathFormat::Plain;
        assert_eq!(preset_of(&s), None);
    }

    #[test]
    fn essentials_is_the_default_wheel() {
        assert_eq!(preset_of(&Settings::default()), Some(Preset::Essentials));
    }

    #[test]
    fn custom_set_is_no_preset() {
        let s = Settings {
            actions: vec![ActionId::Copy, ActionId::Move],
            ..Settings::default()
        };
        assert_eq!(preset_of(&s), None);
    }

    #[test]
    fn summaries_name_the_shown_actions() {
        assert_eq!(
            Preset::Essentials.summary(),
            "Pocket, Copy, Move, Copy path, Terminal"
        );
        assert_eq!(Preset::Everything.summary(), "Every action");
        crate::i18n::with_language("ar", || {
            assert!(Preset::Essentials.summary().contains("، "));
        });
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unavailable_actions_are_left_out() {
        assert!(!Preset::Designer.actions().contains(&ActionId::Pdf));
        assert!(!Preset::Designer.actions().contains(&ActionId::Share));
        assert_eq!(Preset::Everything.actions().len(), 9);
    }

    #[test]
    fn suggests_only_existing_folders() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Desktop")).unwrap();
        std::fs::create_dir(home.path().join("Downloads")).unwrap();
        std::fs::write(home.path().join("Documents"), "a file").unwrap();
        assert_eq!(
            suggested_folders(home.path()),
            [home.path().join("Desktop"), home.path().join("Downloads")]
        );
    }
}
