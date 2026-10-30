//! Terminals: Windows Terminal, PowerShell 7, Windows PowerShell and the
//! Command Prompt, in that order of preference, whichever are installed.
//! Windows Terminal is told the folder (`wt -d`); a shell starts in it in
//! a console window of its own, as vento has none to share.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

use crate::{PlatformError, TerminalApp};

/// A terminal vento knows: its id (the setting's value), its name, the
/// program it runs, and where else than `PATH` that may be.
struct Known {
    id: &'static str,
    name: &'static str,
    program: &'static str,
    /// Folders to look in besides `PATH`, from environment variables.
    places: &'static [(&'static str, &'static str)],
}

const KNOWN: [Known; 4] = [
    Known {
        id: "wt",
        name: "Windows Terminal",
        program: "wt.exe",
        // An app execution alias, which `PATH` normally holds.
        places: &[("LOCALAPPDATA", "Microsoft\\WindowsApps")],
    },
    Known {
        id: "pwsh",
        name: "PowerShell",
        program: "pwsh.exe",
        places: &[("ProgramFiles", "PowerShell\\7")],
    },
    Known {
        id: "powershell",
        name: "Windows PowerShell",
        program: "powershell.exe",
        places: &[("SystemRoot", "System32\\WindowsPowerShell\\v1.0")],
    },
    Known {
        id: "cmd",
        name: "Command Prompt",
        program: "cmd.exe",
        places: &[("SystemRoot", "System32")],
    },
];

/// Where `known`'s program is, if it is installed.
fn find(known: &Known) -> Option<PathBuf> {
    let from_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
    let from_places = known
        .places
        .iter()
        .filter_map(|(var, sub)| Some(PathBuf::from(std::env::var_os(var)?).join(sub)));
    from_path
        .chain(from_places)
        .map(|dir| dir.join(known.program))
        .find(|p| p.is_file())
}

/// The terminals installed here, the preferred first.
pub(super) fn installed() -> Vec<TerminalApp> {
    KNOWN
        .iter()
        .filter(|k| find(k).is_some())
        .map(|k| TerminalApp {
            id: k.id.into(),
            name: k.name.into(),
        })
        .collect()
}

/// Opens `requested` (an id, or the preferred one for `None` or one not
/// installed) in `dir`. Windows Terminal hands the window on and exits at
/// once; if it fails, `on_error` hears why, from another thread. A shell
/// is not waited for: its exit code is the user's session's.
pub(super) fn open(
    requested: Option<&str>,
    dir: &Path,
    on_error: Box<dyn Fn(PlatformError) + Send>,
) -> Result<(), PlatformError> {
    let installed: Vec<(&Known, PathBuf)> =
        KNOWN.iter().filter_map(|k| Some((k, find(k)?))).collect();
    let (known, program) = installed
        .iter()
        .find(|(k, _)| Some(k.id) == requested)
        .or(installed.first())
        .ok_or(PlatformError::NoTerminal)?;
    let mut command = Command::new(program);
    if known.id == "wt" {
        command.arg("-d").arg(dir);
    } else {
        command.current_dir(dir).creation_flags(CREATE_NEW_CONSOLE);
    }
    let mut child = command
        .spawn()
        .map_err(|error| PlatformError::CouldNotRun {
            program: known.name.into(),
            error,
        })?;
    if known.id == "wt" {
        let name = known.name;
        std::thread::spawn(move || match child.wait() {
            Ok(status) if !status.success() => on_error(PlatformError::ProgramFailed {
                program: name.into(),
                code: status.code(),
            }),
            Ok(_) => {}
            Err(error) => on_error(PlatformError::ProgramUnwaited {
                program: name.into(),
                error,
            }),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_prompt_is_always_there() {
        let found = installed();
        assert!(found.iter().any(|t| t.id == "cmd"), "{found:?}");
        // The preferred come first.
        let order: Vec<usize> = found
            .iter()
            .map(|t| KNOWN.iter().position(|k| k.id == t.id).unwrap())
            .collect();
        assert!(order.is_sorted(), "{order:?}");
    }
}
