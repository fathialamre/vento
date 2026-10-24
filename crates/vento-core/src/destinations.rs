//! Where the Move ring sends files: recent destinations, the ring's
//! contents, and the labels drawn on its cells.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use log::warn;
use serde::{Deserialize, Serialize};

use crate::durable;

/// How many recent destinations the ring shows.
pub const RING_RECENT: usize = 5;
/// How many recent destinations are remembered.
pub const RECENT_KEEP: usize = 15;
/// Width of one line of a honeycomb ring cell label, in characters.
pub const LABEL_CHARS: usize = 10;
/// Width of one line of a petal ring cell label, in characters (petals are
/// narrower and drawn in smaller type).
pub const PETAL_LABEL_CHARS: usize = 6;

/// Newest on-disk format this build understands.
const FORMAT_VERSION: u32 = 1;

/// Folders files were recently moved or copied to, newest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecentDestinations {
    items: Vec<PathBuf>,
}

/// On-disk shape of the recent destinations file.
#[derive(Serialize, Deserialize)]
struct RawRecent {
    version: u32,
    recent: Vec<PathBuf>,
}

impl RecentDestinations {
    /// The remembered folders, newest first.
    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    /// Puts `dir` first, dropping an equal entry and the oldest ones beyond
    /// [`RECENT_KEEP`].
    pub fn push(&mut self, dir: PathBuf) {
        self.items.retain(|d| *d != dir);
        self.items.insert(0, dir);
        self.items.truncate(RECENT_KEEP);
    }

    /// Forgets every folder.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Loads the list from `path`. Never fails: a missing file gives an
    /// empty list; an unreadable one, or one written by a newer version,
    /// gives an empty list with a warning and is left in place.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                warn!("cannot read recent destinations {}: {e}", path.display());
                return Self::default();
            }
        };
        match Self::parse(&text) {
            Ok(mut recent) => {
                recent.items.truncate(RECENT_KEEP);
                recent
            }
            Err(why) => {
                warn!("ignoring recent destinations {}: {why}", path.display());
                Self::default()
            }
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v > u64::from(FORMAT_VERSION) => {
                return Err(format!("written by a newer version (format {v})"));
            }
            Some(_) => {}
            None => return Err("no format version".to_owned()),
        }
        let raw: RawRecent = serde_json::from_value(value).map_err(|e| e.to_string())?;
        Ok(Self { items: raw.recent })
    }

    /// Writes the list to `path`, creating the parent folder if needed. The
    /// content goes to `<name>.tmp` first and is renamed into place, so a
    /// crash never leaves a truncated file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let raw = RawRecent {
            version: FORMAT_VERSION,
            recent: self.items.clone(),
        };
        let text = serde_json::to_string_pretty(&raw).map_err(io::Error::other)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".tmp");
        let tmp = path.with_file_name(name);
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            durable::sync(&file)?;
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }
}

/// Location of the recent destinations file.
pub fn recent_path() -> PathBuf {
    match ProjectDirs::from("", "", "vento") {
        Some(dirs) => dirs.data_dir().join("recent.json"),
        None => PathBuf::from("recent.json"),
    }
}

/// One folder on the ring.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    /// The folder files are sent to.
    pub path: PathBuf,
    /// Whether the user pinned it (as opposed to it being a recent one).
    pub pinned: bool,
}

/// The ring's folders: up to [`RING_RECENT`] recent ones that exist and are
/// not pinned, in order, then the pinned ones that exist, in order.
pub fn ring_destinations(
    recent: &[PathBuf],
    pinned: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Vec<Destination> {
    let recents = recent
        .iter()
        .filter(|d| !pinned.contains(d) && exists(d))
        .take(RING_RECENT)
        .map(|d| Destination {
            path: d.clone(),
            pinned: false,
        });
    let pins = pinned.iter().filter(|d| exists(d)).map(|d| Destination {
        path: d.clone(),
        pinned: true,
    });
    recents.chain(pins).collect()
}

/// Whether `path` is a folder that can be reached. A path on a volume that
/// is not mounted (`/Volumes/<name>/...` with no `/Volumes/<name>`) is
/// reported missing without touching it, so a dead network share cannot
/// stall the caller.
pub fn folder_exists(path: &Path) -> bool {
    if path.starts_with("/Volumes") {
        let mount: PathBuf = path.components().take(3).collect();
        if mount != path && std::fs::metadata(&mount).is_err() {
            return false;
        }
    }
    std::fs::metadata(path).is_ok_and(|m| m.is_dir())
}

/// The text on a ring cell: one or two lines.
#[derive(Debug, Clone, PartialEq)]
pub struct CellLabel {
    /// First line (the whole label when `line2` is `None`).
    pub line1: String,
    /// Second line: the rest of a long name, or the parent folder of a
    /// duplicate name.
    pub line2: Option<String>,
    /// Whether `line2` is drawn dimmed (the parent folder of a duplicate).
    pub line2_dim: bool,
}

/// The folder name drawn for `path`.
fn folder_name(path: &Path) -> String {
    match path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Labels for the cells, in the order of `dests`, each line at most `chars`
/// characters. Folders whose names are equal ignoring case show their
/// parent folder as well.
pub fn cell_labels(dests: &[Destination], chars: usize) -> Vec<CellLabel> {
    let names: Vec<String> = dests.iter().map(|d| folder_name(&d.path)).collect();
    let lowered: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    dests
        .iter()
        .zip(&names)
        .enumerate()
        .map(|(i, (d, name))| {
            let duplicated = lowered
                .iter()
                .enumerate()
                .any(|(j, other)| j != i && *other == lowered[i]);
            if duplicated {
                let parent = d.path.parent().map_or_else(|| "/".to_owned(), folder_name);
                CellLabel {
                    line1: middle_ellipsis(name, chars),
                    line2: Some(middle_ellipsis(&parent, chars)),
                    line2_dim: true,
                }
            } else {
                split_name(name, chars)
            }
        })
        .collect()
}

/// Lays a unique name out on one or two lines of at most `chars` characters.
fn split_name(name: &str, chars: usize) -> CellLabel {
    let one_line = |text: String| CellLabel {
        line1: text,
        line2: None,
        line2_dim: false,
    };
    if name.chars().count() <= chars {
        return one_line(name.to_owned());
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    let mut line1 = String::new();
    let mut taken = 0;
    for word in &words {
        let joined = if line1.is_empty() {
            word.chars().count()
        } else {
            line1.chars().count() + 1 + word.chars().count()
        };
        if joined > chars {
            break;
        }
        if !line1.is_empty() {
            line1.push(' ');
        }
        line1.push_str(word);
        taken += 1;
    }
    if taken == 0 {
        // No space, or the first word alone is too long.
        return one_line(middle_ellipsis(name, chars));
    }
    let rest = words[taken..].join(" ");
    CellLabel {
        line1,
        line2: Some(middle_ellipsis(&rest, chars)),
        line2_dim: false,
    }
}

/// Shortens `s` to `max` characters (the ellipsis included) by cutting out
/// its middle. Whitespace left at either side of the cut is dropped.
/// Strings of `max` characters or fewer come back unchanged.
pub fn middle_ellipsis(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_owned();
    }
    let lead = max.saturating_sub(1) / 2;
    let tail = max.saturating_sub(1) - lead;
    let head: String = s.chars().take(lead).collect();
    let end: String = s.chars().skip(count - tail).collect();
    format!("{}…{}", head.trim_end(), end.trim_start())
}

/// Shortens the displayed path `display` (as made by [`display_path`]) to
/// at most `max_chars` characters, keeping its end: whole components after
/// `~` or the root give way to `…`.
pub fn fit_path(display: &str, max_chars: usize) -> String {
    let len = |s: &str| s.chars().count();
    if len(display) <= max_chars {
        return display.to_owned();
    }
    // `~/a/b` splits into `~`, `a`, `b`; `/a/b` into ``, `a`, `b`.
    let Some((head, rest)) = display.split_once('/') else {
        return middle_ellipsis(display, max_chars);
    };
    let parts: Vec<&str> = rest.split('/').collect();
    let last = parts[parts.len() - 1];
    if parts.len() == 1 {
        let room = max_chars.saturating_sub(len(head) + 1);
        return format!("{head}/{}", middle_ellipsis(last, room));
    }
    let prefix = format!("{head}/…/");
    let room = max_chars.saturating_sub(len(&prefix));
    if len(last) > room {
        return format!("{prefix}{}", middle_ellipsis(last, room));
    }
    // Keep as many whole trailing components as fit after the prefix.
    let mut kept = len(last);
    let mut first = parts.len() - 1;
    while first > 1 && kept + 1 + len(parts[first - 1]) <= room {
        first -= 1;
        kept += 1 + len(parts[first]);
    }
    format!("{prefix}{}", parts[first..].join("/"))
}

/// `path` for display: the `home` prefix becomes `~`.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(rest) = home.and_then(|h| path.strip_prefix(h).ok()) {
        if rest.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn dest(path: &str) -> Destination {
        Destination {
            path: p(path),
            pinned: false,
        }
    }

    #[test]
    fn push_moves_to_front_and_caps() {
        let mut r = RecentDestinations::default();
        r.push(p("/a"));
        r.push(p("/b"));
        r.push(p("/a"));
        assert_eq!(r.items(), [p("/a"), p("/b")]);

        r.clear();
        assert!(r.items().is_empty());
        for n in 0..20 {
            r.push(p(&format!("/d{n}")));
        }
        assert_eq!(r.items().len(), RECENT_KEEP);
        assert_eq!(r.items()[0], p("/d19"));
        assert_eq!(r.items()[RECENT_KEEP - 1], p("/d5"));
    }

    #[test]
    fn ring_skips_pinned_and_missing_then_adds_pinned() {
        let recent: Vec<PathBuf> = ["/a", "/b", "/c", "/d", "/e", "/f", "/g"]
            .into_iter()
            .map(p)
            .collect();
        let pinned = [p("/c"), p("/x")];
        let ring = ring_destinations(&recent, &pinned, |d| d != Path::new("/b"));
        let got: Vec<(&str, bool)> = ring
            .iter()
            .map(|d| (d.path.to_str().unwrap(), d.pinned))
            .collect();
        assert_eq!(
            got,
            [
                ("/a", false),
                ("/d", false),
                ("/e", false),
                ("/f", false),
                ("/g", false),
                ("/c", true),
                ("/x", true),
            ]
        );
    }

    #[test]
    fn ring_drops_missing_pinned_and_caps_recent() {
        let recent: Vec<PathBuf> = (0..9).map(|n| p(&format!("/r{n}"))).collect();
        let pinned = [p("/gone"), p("/kept")];
        let ring = ring_destinations(&recent, &pinned, |d| d != Path::new("/gone"));
        assert_eq!(ring.len(), RING_RECENT + 1);
        assert_eq!(ring[RING_RECENT].path, p("/kept"));
        assert!(ring[RING_RECENT].pinned);
    }

    #[test]
    fn labels_short_long_and_duplicates() {
        let labels = cell_labels(
            &[
                dest("/x/Downloads"),
                dest("/x/Client Invoices 2026"),
                dest("/x/ScreenRecordings2026"),
            ],
            LABEL_CHARS,
        );
        assert_eq!(labels[0].line1, "Downloads");
        assert_eq!(labels[0].line2, None);
        assert_eq!(labels[1].line1, "Client");
        assert_eq!(labels[1].line2.as_deref(), Some("Invo…2026"));
        assert!(!labels[1].line2_dim);
        assert_eq!(labels[2].line1, "Scre…s2026");
        assert_eq!(labels[2].line2, None);

        let dup = cell_labels(
            &[dest("/w/site-v2/Assets"), dest("/w/app-ios/assets")],
            LABEL_CHARS,
        );
        assert_eq!(dup[0].line1, "Assets");
        assert_eq!(dup[0].line2.as_deref(), Some("site-v2"));
        assert!(dup[0].line2_dim);
        assert_eq!(dup[1].line1, "assets");
        assert_eq!(dup[1].line2.as_deref(), Some("app-ios"));
        assert!(dup[1].line2_dim);
    }

    #[test]
    fn petal_labels_use_the_narrower_budget() {
        let labels = cell_labels(
            &[
                dest("/x/Downloads"),
                dest("/x/Client Invoices 2026"),
                dest("/x/Work"),
            ],
            PETAL_LABEL_CHARS,
        );
        assert_eq!(labels[0].line1, "Do…ads");
        assert_eq!(labels[0].line2, None);
        assert_eq!(labels[1].line1, "Client");
        assert_eq!(labels[1].line2.as_deref(), Some("In…026"));
        assert_eq!(labels[2].line1, "Work");

        let dup = cell_labels(
            &[dest("/w/site-v2/Assets"), dest("/w/app-ios/assets")],
            PETAL_LABEL_CHARS,
        );
        assert_eq!(dup[0].line1, "Assets");
        assert_eq!(dup[0].line2.as_deref(), Some("si…-v2"));
        assert_eq!(dup[1].line2.as_deref(), Some("ap…ios"));
        for label in labels.iter().chain(&dup) {
            assert!(label.line1.chars().count() <= PETAL_LABEL_CHARS);
            let line2 = label.line2.as_deref().unwrap_or("");
            assert!(line2.chars().count() <= PETAL_LABEL_CHARS);
        }
    }

    #[test]
    fn fit_path_short_is_unchanged() {
        assert_eq!(fit_path("~/Projects/a", 36), "~/Projects/a");
        assert_eq!(fit_path("~", 36), "~");
        assert_eq!(fit_path("/opt/a", 36), "/opt/a");
        let exact = "~/0123456789/0123456789/0123456789/";
        assert_eq!(fit_path(exact, exact.chars().count()), exact);
    }

    #[test]
    fn fit_path_keeps_the_tail_components() {
        let long = "~/Library/Mobile Documents/com~apple~CloudDocs/Work/Clients/Acme/Assets";
        assert_eq!(fit_path(long, 36), "~/…/Work/Clients/Acme/Assets");
        assert_eq!(fit_path(long, 24), "~/…/Clients/Acme/Assets");
        assert_eq!(fit_path(long, 10), "~/…/Assets");
        assert_eq!(
            fit_path("/Volumes/Backup/Archive/2026/Clients/Acme", 20),
            "/…/2026/Clients/Acme"
        );
        for max in 8..long.chars().count() {
            assert!(fit_path(long, max).chars().count() <= max, "max {max}");
        }
    }

    #[test]
    fn fit_path_shortens_one_huge_last_component() {
        let huge = format!("~/Work/{}", "x".repeat(30) + "-final-2026");
        let got = fit_path(&huge, 20);
        assert_eq!(got.chars().count(), 20);
        assert!(got.starts_with("~/…/xxxxxxx"), "{got}");
        assert!(got.ends_with("l-2026"), "{got}");
        // With nothing before it to drop, no `…/` is added.
        let only = format!("~/{}", "y".repeat(40));
        let got = fit_path(&only, 20);
        assert_eq!(got.chars().count(), 20);
        assert!(got.starts_with("~/yyy"), "{got}");
        // No separator at all: cut in the middle.
        assert_eq!(fit_path("abcdefghijklmnop", 10), "abcd…lmnop");
    }

    #[test]
    fn middle_ellipsis_exact() {
        assert_eq!(middle_ellipsis("Invoices 2026", 10), "Invo…2026");
        assert_eq!(middle_ellipsis("ScreenRecordings2026", 10), "Scre…s2026");
        assert_eq!(middle_ellipsis("Downloads", 10), "Downloads");
        assert_eq!(middle_ellipsis("0123456789", 10), "0123456789");
        assert_eq!(middle_ellipsis("Ünïcödé-Ärchive-1", 10), "Ünïc…ive-1");
    }

    #[test]
    fn display_path_uses_tilde() {
        let home = p("/Users/f");
        assert_eq!(
            display_path(Path::new("/Users/f/Projects/a"), Some(&home)),
            "~/Projects/a"
        );
        assert_eq!(display_path(Path::new("/Users/f"), Some(&home)), "~");
        assert_eq!(
            display_path(Path::new("/Users/fx/a"), Some(&home)),
            "/Users/fx/a"
        );
        assert_eq!(display_path(Path::new("/opt/a"), Some(&home)), "/opt/a");
        assert_eq!(display_path(Path::new("/Users/f/a"), None), "/Users/f/a");
    }

    #[test]
    fn recent_file_round_trip_and_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("recent.json");
        assert!(RecentDestinations::load(&file).items().is_empty());

        let mut r = RecentDestinations::default();
        r.push(p("/a"));
        r.push(p("/b"));
        r.save(&file).unwrap();
        assert!(!dir.path().join("sub").join("recent.json.tmp").exists());
        let text = std::fs::read_to_string(&file).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(RecentDestinations::load(&file), r);

        std::fs::write(&file, "{nope").unwrap();
        assert!(RecentDestinations::load(&file).items().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{nope");

        let newer = r#"{"version": 2, "recent": ["/a"]}"#;
        std::fs::write(&file, newer).unwrap();
        assert!(RecentDestinations::load(&file).items().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), newer);
    }

    #[test]
    fn unmounted_volume_is_missing() {
        assert!(!folder_exists(Path::new("/Volumes/vento-no-such-volume/x")));
    }

    #[test]
    fn folder_exists_needs_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(folder_exists(dir.path()));
        assert!(!folder_exists(&file));
        assert!(!folder_exists(&dir.path().join("nope")));
    }
}
