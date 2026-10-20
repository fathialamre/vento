//! Formatting dragged file paths for the clipboard.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How paths are written when copied as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathFormat {
    /// Paths as-is, one per line.
    Plain,
    /// Each path wrapped in single quotes for POSIX shells, one per line.
    ShellQuoted,
}

/// Joins `paths` with `\n` in the requested `format`.
pub fn format_paths(paths: &[PathBuf], format: PathFormat) -> String {
    paths
        .iter()
        .map(|p| {
            let s = p.to_string_lossy();
            match format {
                PathFormat::Plain => s.into_owned(),
                PathFormat::ShellQuoted => format!("'{}'", s.replace('\'', "'\\''")),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_keep_unicode() {
        let paths = [
            PathBuf::from("/Users/a/مستندات/ملف.pdf"),
            PathBuf::from("/tmp/b c"),
        ];
        assert_eq!(
            format_paths(&paths, PathFormat::Plain),
            "/Users/a/مستندات/ملف.pdf\n/tmp/b c"
        );
    }

    #[test]
    fn shell_quoted_escapes_single_quote() {
        let paths = [PathBuf::from("/tmp/it's here")];
        assert_eq!(
            format_paths(&paths, PathFormat::ShellQuoted),
            "'/tmp/it'\\''s here'"
        );
    }
}
