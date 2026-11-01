//! File names: stem/extension split, comparison keys and natural ordering.

use std::cmp::Ordering;

use unicode_normalization::UnicodeNormalization;

/// The operating system whose naming rules apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// macOS.
    Mac,
    /// Windows.
    Windows,
    /// Linux and every other Unix.
    Linux,
}

impl Os {
    /// The system this build runs on.
    pub const HOST: Os = if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Linux
    };

    /// Whether two names that differ only in case are the same name.
    pub fn case_insensitive(self) -> bool {
        matches!(self, Os::Mac | Os::Windows)
    }
}

/// A file name split into stem and extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameParts {
    /// Everything before the last dot (the whole name when there is no extension).
    pub stem: String,
    /// The text after the last dot: `None` when there is no dot, `Some("")` for a trailing dot.
    pub ext: Option<String>,
}

/// Splits `name` at its last dot. Folders and names whose only dot is the first
/// character (such as `.env`) have no extension.
pub fn split_name(name: &str, is_dir: bool) -> NameParts {
    if !is_dir
        && let Some(dot) = name.rfind('.')
        && dot > 0
    {
        return NameParts {
            stem: name[..dot].to_string(),
            ext: Some(name[dot + 1..].to_string()),
        };
    }
    NameParts {
        stem: name.to_string(),
        ext: None,
    }
}

/// Joins stem and extension back into a name; the inverse of [`split_name`].
pub fn join_name(parts: &NameParts) -> String {
    match &parts.ext {
        Some(ext) => format!("{}.{ext}", parts.stem),
        None => parts.stem.clone(),
    }
}

/// Normalises `s` to Unicode NFC.
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// The key under which names collide on `os`: NFC, lower-cased where the system ignores case.
pub fn name_key(name: &str, os: Os) -> String {
    let n = nfc(name);
    if os.case_insensitive() {
        n.to_lowercase()
    } else {
        n
    }
}

/// Splits `s` into alternating runs of ASCII digits and other characters.
fn runs(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let digit = c.is_ascii_digit();
        if prev.is_some_and(|p| p != digit) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(digit);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Compares two digit runs by numeric value, then by length (`2` before `02`).
fn cmp_digits(a: &str, b: &str) -> Ordering {
    let (ta, tb) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    ta.len()
        .cmp(&tb.len())
        .then_with(|| ta.cmp(tb))
        .then_with(|| a.len().cmp(&b.len()))
}

/// Natural order: digit runs by value, other text ignoring case, whole string as tie-break.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ra, rb) = (runs(a), runs(b));
    for (x, y) in ra.iter().zip(&rb) {
        let digits = |r: &str| r.starts_with(|c: char| c.is_ascii_digit());
        let ord = if digits(x) && digits(y) {
            cmp_digits(x, y)
        } else {
            x.to_lowercase().cmp(&y.to_lowercase())
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ra.len().cmp(&rb.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_at_last_dot() {
        assert_eq!(
            split_name("report.final.pdf", false),
            NameParts {
                stem: "report.final".into(),
                ext: Some("pdf".into())
            }
        );
        assert_eq!(
            split_name("archive.tar.gz", false).ext.as_deref(),
            Some("gz")
        );
        assert_eq!(
            split_name(".env", false),
            NameParts {
                stem: ".env".into(),
                ext: None
            }
        );
        assert_eq!(
            split_name("name.", false),
            NameParts {
                stem: "name".into(),
                ext: Some(String::new())
            }
        );
        assert_eq!(
            split_name("Photos.2026", true),
            NameParts {
                stem: "Photos.2026".into(),
                ext: None
            }
        );
        for n in ["a.b.c", ".env", "name.", "plain"] {
            assert_eq!(join_name(&split_name(n, false)), n);
        }
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["IMG_10", "IMG_2", "img_1", "IMG_02", "b", "A"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["A", "b", "img_1", "IMG_2", "IMG_02", "IMG_10"]);
    }

    #[test]
    fn keys_fold_nfd_and_case() {
        let nfd = "\u{0627}\u{0653}"; // alef + madda above (NFD)
        let nfc = "\u{0622}"; // alef with madda (NFC)
        assert_eq!(name_key(nfd, Os::Linux), name_key(nfc, Os::Linux));
        assert_eq!(name_key("A.JPG", Os::Mac), name_key("a.jpg", Os::Mac));
        assert_eq!(
            name_key("A.JPG", Os::Windows),
            name_key("a.jpg", Os::Windows)
        );
        assert_ne!(name_key("A.JPG", Os::Linux), name_key("a.jpg", Os::Linux));
    }
}
