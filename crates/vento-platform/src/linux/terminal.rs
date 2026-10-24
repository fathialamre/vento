//! Terminal discovery and launching.
//!
//! Terminals are the installed apps in the `TerminalEmulator` category,
//! named by their desktop file id. The default one comes from the
//! `xdg-terminals.list` files that `xdg-terminal-exec` (Ubuntu's default
//! terminal launcher) reads.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gio::prelude::*;
use gio_unix::DesktopAppInfo;

use crate::{PlatformError, TerminalApp};

/// Terminals whose working folder is set by a flag, as (desktop file id,
/// arguments before the folder). A flag ending in `=` takes the folder
/// joined to it. The rest start in the folder they are launched from.
/// Ptyxis, GNOME Terminal and Konsole are single-instance, so for them
/// only the flag works.
const WORKDIR_ARGS: [(&str, &[&str]); 11] = [
    (
        "org.gnome.Ptyxis.desktop",
        &["--new-window", "--working-directory"],
    ),
    (
        "org.gnome.Terminal.desktop",
        &["--window", "--working-directory"],
    ),
    ("org.gnome.Console.desktop", &["--working-directory"]),
    ("org.kde.konsole.desktop", &["--new-tab", "--workdir"]),
    ("com.gexperts.Tilix.desktop", &["--working-directory"]),
    ("xfce4-terminal.desktop", &["--working-directory"]),
    ("kitty.desktop", &["--directory"]),
    ("Alacritty.desktop", &["--working-directory"]),
    ("org.wezfurlong.wezterm.desktop", &["start", "--cwd"]),
    ("com.mitchellh.ghostty.desktop", &["--working-directory="]),
    ("org.codeberg.dnkl.foot.desktop", &["--working-directory="]),
];

/// The arguments that open `id` in `dir`; empty for a terminal not listed
/// in [`WORKDIR_ARGS`].
fn workdir_args(id: &str, dir: &Path) -> Vec<OsString> {
    let Some((_, args)) = WORKDIR_ARGS.iter().find(|(known, _)| *known == id) else {
        return Vec::new();
    };
    let mut out: Vec<OsString> = args.iter().map(OsString::from).collect();
    match out.pop() {
        Some(last) if last.to_string_lossy().ends_with('=') => {
            let mut joined = last;
            joined.push(dir);
            out.push(joined);
        }
        Some(last) => {
            out.push(last);
            out.push(dir.into());
        }
        None => {}
    }
    out
}

/// The desktop file ids an `xdg-terminals.list` file asks for, in order.
/// Comments, `/directives` and `-exclusions` are skipped, and `:action`
/// suffixes dropped.
fn listed_ids(list: &str) -> Vec<String> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', '/', '-']))
        .map(|l| l.trim_start_matches('+'))
        .map(|l| l.split_once(':').map_or(l, |(id, _)| id).to_owned())
        .collect()
}

/// `xdg-terminals.list` files in the order `xdg-terminal-exec` reads them:
/// config dirs, then data dirs; in each, the current desktops' own lists
/// before the generic one.
fn list_files() -> Vec<PathBuf> {
    let env_dirs = |var: &str, default: &str| -> Vec<PathBuf> {
        std::env::var(var)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_owned())
            .split(':')
            .map(PathBuf::from)
            .collect()
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let mut dirs = vec![config_home];
    dirs.extend(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
    dirs.push(data_home.join("xdg-terminal-exec"));
    dirs.extend(
        env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share")
            .into_iter()
            .map(|d| d.join("xdg-terminal-exec")),
    );
    let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut files = Vec::new();
    for dir in dirs {
        for desktop in &desktops {
            files.push(dir.join(format!("{desktop}-xdg-terminals.list")));
        }
        files.push(dir.join("xdg-terminals.list"));
    }
    files
}

/// Puts the terminals named in `preferred` first, in that order; the rest
/// keep theirs.
fn order(mut terminals: Vec<TerminalApp>, preferred: &[String]) -> Vec<TerminalApp> {
    let rank = |t: &TerminalApp| {
        preferred
            .iter()
            .position(|p| *p == t.id)
            .unwrap_or(preferred.len())
    };
    terminals.sort_by_key(rank);
    terminals
}

/// Installed terminals, the default one first.
pub(super) fn installed() -> Vec<TerminalApp> {
    let found: Vec<TerminalApp> = gio::AppInfo::all()
        .into_iter()
        .filter_map(|app| {
            let desktop = app.downcast_ref::<DesktopAppInfo>()?;
            let categories = desktop.categories()?;
            if !categories.split(';').any(|c| c == "TerminalEmulator") || !app.should_show() {
                return None;
            }
            Some(TerminalApp {
                id: app.id()?.to_string(),
                name: app.display_name().to_string(),
            })
        })
        .collect();
    let preferred: Vec<String> = list_files()
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .flat_map(|l| listed_ids(&l))
        .collect();
    order(found, &preferred)
}

/// The desktop file id to launch: `requested` if it is installed, else
/// the default (first) terminal.
fn resolve<'a>(installed: &'a [TerminalApp], requested: Option<&str>) -> Option<&'a str> {
    requested
        .and_then(|r| installed.iter().find(|t| t.id == r))
        .or_else(|| installed.first())
        .map(|t| t.id.as_str())
}

/// Starts `cmd` in `dir` with none of vento's standard streams: a terminal
/// (and the helpers it starts, such as `ptyxis-agent`) outlives vento and
/// would otherwise keep its stdout and stderr open and write to them.
fn detached<'a>(cmd: &'a mut Command, dir: &Path) -> &'a mut Command {
    cmd.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
}

pub(super) fn open(terminal: Option<&str>, dir: &Path) -> Result<(), PlatformError> {
    let apps = installed();
    let id = resolve(&apps, terminal)
        .ok_or_else(|| PlatformError::Failed("no terminal is installed".into()))?;
    let app = DesktopAppInfo::new(id)
        .ok_or_else(|| PlatformError::Failed(format!("{id} is not installed")))?;
    let program = app.executable();
    let mut child = detached(Command::new(&program).args(workdir_args(id, dir)), dir)
        .spawn()
        .map_err(|e| PlatformError::Failed(format!("could not run {}: {e}", program.display())))?;
    // Reap it, so a launcher that hands off to a running instance and quits
    // leaves no zombie behind.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps(ids: &[&str]) -> Vec<TerminalApp> {
        ids.iter()
            .map(|id| TerminalApp {
                id: (*id).into(),
                name: (*id).into(),
            })
            .collect()
    }

    fn ids(terminals: &[TerminalApp]) -> Vec<&str> {
        terminals.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn lists_give_ids_without_actions_comments_or_exclusions() {
        let list = "# comment\n\n-org.kde.yakuake.desktop\n\
                    /execarg_default:kitty.desktop:--\n\
                    org.gnome.Ptyxis.desktop:new-window\n+kitty.desktop\n";
        assert_eq!(
            listed_ids(list),
            ["org.gnome.Ptyxis.desktop", "kitty.desktop"]
        );
    }

    #[test]
    fn preferred_terminals_come_first_in_list_order() {
        let found = apps(&["xterm.desktop", "kitty.desktop", "org.gnome.Ptyxis.desktop"]);
        let preferred = ["org.gnome.Ptyxis.desktop".into(), "kitty.desktop".into()];
        assert_eq!(
            ids(&order(found, &preferred)),
            ["org.gnome.Ptyxis.desktop", "kitty.desktop", "xterm.desktop"]
        );
    }

    #[test]
    fn unlisted_terminals_keep_their_order() {
        let found = apps(&["b.desktop", "a.desktop"]);
        assert_eq!(ids(&order(found, &[])), ["b.desktop", "a.desktop"]);
    }

    #[test]
    fn installed_request_is_used_else_the_default() {
        let a = apps(&["org.gnome.Ptyxis.desktop", "kitty.desktop"]);
        assert_eq!(resolve(&a, Some("kitty.desktop")), Some("kitty.desktop"));
        assert_eq!(
            resolve(&a, Some("com.mitchellh.ghostty.desktop")),
            Some("org.gnome.Ptyxis.desktop")
        );
        assert_eq!(resolve(&a, None), Some("org.gnome.Ptyxis.desktop"));
        assert_eq!(resolve(&[], None), None);
    }

    #[test]
    fn terminals_start_in_the_folder_without_vento_streams() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report");
        // `$$` is the shell itself, whose streams are the ones it was given.
        // They are read before anything is redirected: dash applies a
        // command's redirect to the shell itself while the command runs.
        let script = format!(
            "s=$(readlink /proc/$$/fd/0 /proc/$$/fd/1 /proc/$$/fd/2); \
             printf '%s\\n' \"$PWD\" \"$s\" > '{}'",
            report.display()
        );
        let status = detached(Command::new("sh").args(["-c", &script]), dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        let lines: Vec<String> = std::fs::read_to_string(&report)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let real_dir = dir.path().canonicalize().unwrap();
        assert_eq!(lines[0], real_dir.to_string_lossy());
        assert_eq!(lines[1..], ["/dev/null", "/dev/null", "/dev/null"]);
    }

    #[test]
    fn workdir_flags_take_the_folder_apart_or_joined() {
        let dir = Path::new("/home/me/My Files");
        assert_eq!(
            workdir_args("org.gnome.Ptyxis.desktop", dir),
            ["--new-window", "--working-directory", "/home/me/My Files"]
        );
        assert_eq!(
            workdir_args("com.mitchellh.ghostty.desktop", dir),
            ["--working-directory=/home/me/My Files"]
        );
        assert!(workdir_args("xterm.desktop", dir).is_empty());
    }
}
