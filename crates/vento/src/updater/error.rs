//! Why the running copy cannot be replaced, on the Mac and on Windows.

use std::path::PathBuf;

use vento_core::i18n::{fill, tr};

/// Why the app cannot be replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapError {
    /// macOS runs the app from a read-only copy (App Translocation) because
    /// it was opened straight from the Downloads folder or a disk image.
    #[cfg(target_os = "macos")]
    Translocated,
    /// The running copy is not inside an `.app` bundle (on Windows: not a
    /// copy the setup installed).
    NotABundle(PathBuf),
    /// The folder holding the bundle cannot be written to.
    NotWritable(PathBuf),
    /// The downloaded archive could not be unpacked, holds no app, or is
    /// gone.
    Unpack(String),
    /// A rename or copy failed.
    Io(String),
}

impl SwapError {
    /// The text the banner shows for this error, in the interface language.
    pub fn user_text(&self) -> String {
        match self {
            #[cfg(target_os = "macos")]
            SwapError::Translocated | SwapError::NotABundle(_) => {
                tr("Move Vento to Applications to update it")
            }
            #[cfg(windows)]
            SwapError::NotABundle(_) => tr("Install Vento with its setup to update it"),
            SwapError::NotWritable(folder) => fill(
                &tr("Can't replace Vento in {folder}"),
                &[("folder", &folder.display().to_string())],
            ),
            SwapError::Unpack(_) => tr("The download couldn't be opened"),
            SwapError::Io(_) => tr("Couldn't replace Vento"),
        }
    }
}

impl std::fmt::Display for SwapError {
    /// The error in English, for log lines.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(target_os = "macos")]
            SwapError::Translocated => write!(f, "running from App Translocation"),
            #[cfg(target_os = "macos")]
            SwapError::NotABundle(p) => write!(f, "not an app bundle: {}", p.display()),
            #[cfg(windows)]
            SwapError::NotABundle(p) => write!(f, "not installed by the setup: {}", p.display()),
            SwapError::NotWritable(p) => write!(f, "folder not writable: {}", p.display()),
            SwapError::Unpack(r) => write!(f, "cannot unpack the download: {r}"),
            SwapError::Io(r) => write!(f, "cannot replace the app: {r}"),
        }
    }
}
