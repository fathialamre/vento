//! Terminal discovery and launching.

use std::path::Path;
use std::process::Command;

use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSString;

use crate::{PlatformError, TerminalApp};

const DEFAULT_ID: &str = "com.apple.Terminal";

/// Known terminals as (bundle id, display name). Terminal must stay first.
const KNOWN: [(&str, &str); 4] = [
    (DEFAULT_ID, "Terminal"),
    ("com.googlecode.iterm2", "iTerm"),
    ("com.mitchellh.ghostty", "Ghostty"),
    ("dev.warp.Warp-Stable", "Warp"),
];

pub(super) fn installed() -> Vec<TerminalApp> {
    let workspace = NSWorkspace::sharedWorkspace();
    KNOWN
        .iter()
        // Terminal.app ships with macOS, so it is listed even if the lookup fails.
        .filter(|(id, _)| {
            *id == DEFAULT_ID
                || workspace
                    .URLForApplicationWithBundleIdentifier(&NSString::from_str(id))
                    .is_some()
        })
        .map(|(id, name)| TerminalApp {
            id: (*id).into(),
            name: (*name).into(),
        })
        .collect()
}

/// The bundle id to launch: `requested` if it is installed, else Terminal.
fn resolve<'a>(installed: &'a [TerminalApp], requested: Option<&str>) -> &'a str {
    requested
        .and_then(|r| installed.iter().find(|t| t.id == r))
        .map_or(DEFAULT_ID, |t| t.id.as_str())
}

pub(super) fn open(terminal: Option<&str>, dir: &Path) -> Result<(), PlatformError> {
    let apps = installed();
    let id = resolve(&apps, terminal);
    let status = Command::new("open")
        .arg("-b")
        .arg(id)
        .arg(dir)
        .status()
        .map_err(|e| PlatformError::Failed(format!("could not run open: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::Failed(format!(
            "open -b {id} failed: {status}"
        )))
    }
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

    #[test]
    fn installed_request_is_used() {
        let a = apps(&[DEFAULT_ID, "com.googlecode.iterm2"]);
        assert_eq!(
            resolve(&a, Some("com.googlecode.iterm2")),
            "com.googlecode.iterm2"
        );
    }

    #[test]
    fn missing_or_unset_request_falls_back_to_terminal() {
        let a = apps(&[DEFAULT_ID]);
        assert_eq!(resolve(&a, Some("com.mitchellh.ghostty")), DEFAULT_ID);
        assert_eq!(resolve(&a, None), DEFAULT_ID);
    }

    #[test]
    fn terminal_is_always_listed_first() {
        assert_eq!(installed()[0].id, DEFAULT_ID);
    }
}
