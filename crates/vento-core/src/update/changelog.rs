//! The changelog: parsing `CHANGELOG.md`, and the digest of what changed
//! between two versions that the What's new window shows.

use super::features::{self, Feature};
use super::version::Version;

/// What an entry did, in the order the window counts them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    /// Under `### Added`.
    New,
    /// Under `### Changed`.
    Improved,
    /// Under `### Fixed`.
    Fixed,
}

/// One changelog entry.
#[derive(Clone, Debug)]
pub struct Change {
    /// The feature it belongs to; `None` for an entry that applies everywhere.
    pub feature: Option<&'static Feature>,
    /// The entry's text, without the feature key and the settings path.
    pub text: String,
    /// Where to find it in Settings, such as `Settings → Pocket → View`.
    pub settings_path: Option<String>,
    /// Whether it is new, improved or fixed.
    pub kind: ChangeKind,
    /// The version that made the change.
    pub version: Version,
}

/// The notes of one released version.
#[derive(Clone, Debug)]
pub struct VersionNotes {
    /// The version.
    pub version: Version,
    /// Its release date as written (`2026-11-10`), if the header has one.
    pub date: Option<String>,
    /// The one-paragraph summary under `### Summary`, if any.
    pub summary: Option<String>,
    /// Its entries, in file order.
    pub changes: Vec<Change>,
}

/// Reads a changelog into one [`VersionNotes`] per released version, in file
/// order (newest first).
pub fn parse(text: &str) -> Vec<VersionNotes> {
    let mut out = Vec::new();
    // The header and body lines of the version being read; `None` while in a
    // skipped section (`[Unreleased]`, an unreadable version) or before the
    // first one.
    let mut current: Option<(Version, Option<String>, Vec<&str>)> = None;
    for line in text.lines() {
        if line.starts_with("## ") {
            if let Some((version, date, body)) = current.take() {
                out.push(read_body(version, date, &body));
            }
            current = read_header(line).map(|(version, date)| (version, date, Vec::new()));
        } else if let Some((_, _, body)) = &mut current {
            body.push(line);
        }
    }
    if let Some((version, date, body)) = current {
        out.push(read_body(version, date, &body));
    }
    out
}

/// `## [0.3.0] - 2026-11-10` gives the version and the date; the date may be
/// missing. Any other header gives `None`.
fn read_header(line: &str) -> Option<(Version, Option<String>)> {
    let rest = line.strip_prefix("## [")?;
    let (version, after) = rest.split_once(']')?;
    let version = Version::parse(version)?;
    let date = after.trim().trim_start_matches('-').trim();
    Some((version, (!date.is_empty()).then(|| date.to_string())))
}

/// Reads the body of a GitHub release, which has the `###` sections but no
/// `## [..]` header, as the notes of `version`.
pub fn parse_release_body(version: Version, body: &str) -> VersionNotes {
    let lines: Vec<&str> = body.lines().collect();
    read_body(version, None, &lines)
}

/// The `###` sections under one version header.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    /// Before the first `###`, or one that is not read.
    Ignored,
    Summary,
    Entries(ChangeKind),
}

/// Reads the lines under a version header.
fn read_body(version: Version, date: Option<String>, lines: &[&str]) -> VersionNotes {
    let mut notes = VersionNotes {
        version,
        date,
        summary: None,
        changes: Vec::new(),
    };
    let mut section = Section::Ignored;
    // The summary paragraph being read, and the entry being read (the kind it
    // was opened under, and its text so far).
    let mut summary: Vec<&str> = Vec::new();
    let mut entry: Option<(ChangeKind, String)> = None;
    for line in lines {
        if let Some(name) = line.strip_prefix("### ") {
            finish(&mut entry, &mut notes);
            section = match name.trim().to_ascii_lowercase().as_str() {
                "summary" => Section::Summary,
                "added" => Section::Entries(ChangeKind::New),
                "changed" => Section::Entries(ChangeKind::Improved),
                "fixed" => Section::Entries(ChangeKind::Fixed),
                _ => Section::Ignored,
            };
            continue;
        }
        match section {
            Section::Ignored => {}
            Section::Summary => {
                let t = line.trim();
                if t.is_empty() {
                    // Only the first paragraph is the summary.
                    if !summary.is_empty() {
                        section = Section::Ignored;
                    }
                } else {
                    summary.push(t);
                }
            }
            Section::Entries(kind) => {
                if let Some(text) = line.strip_prefix("- ") {
                    finish(&mut entry, &mut notes);
                    entry = Some((kind, text.trim().to_string()));
                } else if let Some((_, text)) = &mut entry
                    && line.starts_with([' ', '\t'])
                    && !line.trim().is_empty()
                {
                    text.push(' ');
                    text.push_str(line.trim());
                }
            }
        }
    }
    finish(&mut entry, &mut notes);
    if !summary.is_empty() {
        notes.summary = Some(summary.join(" "));
    }
    notes
}

/// Turns the entry being read, if any, into a [`Change`] of `notes`.
fn finish(entry: &mut Option<(ChangeKind, String)>, notes: &mut VersionNotes) {
    if let Some((kind, text)) = entry.take() {
        notes.changes.push(make_change(&text, kind, notes.version));
    }
}

/// Builds an entry from its text: the leading `Key:` of a known feature, and
/// the `Settings → …` path at the end.
fn make_change(text: &str, kind: ChangeKind, version: Version) -> Change {
    let (feature, rest) = match text.split_once(':') {
        Some((key, rest)) => match features::find(key.trim()) {
            Some(f) => (Some(f), rest.trim()),
            None => (None, text),
        },
        None => (None, text),
    };
    let (text, settings_path) = split_settings_path(rest);
    Change {
        feature,
        text,
        settings_path,
        kind,
        version,
    }
}

/// Splits the last `Settings → …` (or `الإعدادات ← …`) off the end of `text`
/// when it follows a sentence that ends in a full stop. The path loses its
/// trailing full stop.
fn split_settings_path(text: &str) -> (String, Option<String>) {
    let start = ["Settings →", "الإعدادات ←"]
        .iter()
        .filter_map(|marker| text.rfind(marker))
        .filter(|&i| text[..i].trim_end().ends_with('.'))
        .max();
    match start {
        Some(i) => {
            let path = text[i..].trim().trim_end_matches('.').trim_end();
            (text[..i].trim_end().to_string(), Some(path.to_string()))
        }
        None => (text.to_string(), None),
    }
}

/// What changed from one version to another, merged into one list.
#[derive(Clone, Debug)]
pub struct Digest {
    /// The newest version covered.
    pub version: Version,
    /// The version the user came from, if known.
    pub previous: Option<Version>,
    /// The summary of the newest version covered.
    pub summary: Option<String>,
    /// Every entry of the versions covered, newest version first.
    pub changes: Vec<Change>,
}

/// The versions `v` of `notes` with `after < v <= upto`, merged; or only
/// `upto` when `after` is `None`. `None` when no version is covered.
pub fn digest(notes: &[VersionNotes], after: Option<Version>, upto: Version) -> Option<Digest> {
    let mut covered: Vec<&VersionNotes> = notes
        .iter()
        .filter(|n| match after {
            Some(a) => a < n.version && n.version <= upto,
            None => n.version == upto,
        })
        .collect();
    covered.sort_by_key(|n| std::cmp::Reverse(n.version));
    let newest = covered.first()?;
    Some(Digest {
        version: upto,
        previous: after,
        summary: newest.summary.clone(),
        changes: covered.iter().flat_map(|n| n.changes.clone()).collect(),
    })
}

/// The entries of one feature in a [`Digest`].
#[derive(Clone, Debug)]
pub struct FeatureGroup {
    /// The feature; `None` for the group of entries that apply everywhere.
    pub feature: Option<&'static Feature>,
    /// Whether every entry in the group is new.
    pub is_new: bool,
    /// The group's entries.
    pub changes: Vec<Change>,
}

impl Digest {
    /// The entries grouped by feature: groups of new features first, the rest
    /// in feature-table order, and the group for everywhere last.
    pub fn groups(&self) -> Vec<FeatureGroup> {
        let group = |feature: Option<&'static Feature>| {
            let mut changes: Vec<Change> = self
                .changes
                .iter()
                .filter(|c| c.feature.map(|f| f.key) == feature.map(|f| f.key))
                .cloned()
                .collect();
            // Stable: new entries first, then improved, then fixed.
            changes.sort_by_key(|c| c.kind);
            let is_new = changes.iter().all(|c| c.kind == ChangeKind::New);
            FeatureGroup {
                feature,
                is_new,
                changes,
            }
        };
        let (new, rest): (Vec<_>, Vec<_>) = features::FEATURES
            .iter()
            .map(|f| group(Some(f)))
            .filter(|g| !g.changes.is_empty())
            .partition(|g| g.is_new);
        let everywhere = group(None);
        let mut out = new;
        out.extend(rest);
        if !everywhere.changes.is_empty() {
            out.push(everywhere);
        }
        out
    }

    /// How many entries are new, improved and fixed.
    pub fn counts(&self) -> [usize; 3] {
        let mut counts = [0; 3];
        for c in &self.changes {
            counts[c.kind as usize] += 1;
        }
        counts
    }
}

/// The English notes with the Arabic ones in place of those versions that
/// have an Arabic text.
pub(crate) fn merge(en: Vec<VersionNotes>, ar: Vec<VersionNotes>) -> Vec<VersionNotes> {
    en.into_iter()
        .map(|e| match ar.iter().find(|a| a.version == e.version) {
            Some(a) => a.clone(),
            None => e,
        })
        .collect()
}

/// The notes bundled with the app, in `lang` where it has a translation.
pub fn bundled_notes(lang: &str) -> Vec<VersionNotes> {
    let en = parse(include_str!("../../../../CHANGELOG.md"));
    if lang == "ar" {
        merge(en, parse(include_str!("../../../../CHANGELOG.ar.md")))
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "# Changelog\n\n## [Unreleased]\n- PDF Pages: hidden.\n\n## [0.5.0] - 2026-12-01\n### Summary\nFive.\n### Added\n- Pocket: New thing. Settings → Pocket → View.\n### Fixed\n- PDF Pages: Fix.\n\n## [0.4.0] - 2026-11-20\n### Summary\nFour.\n### Changed\n- Clipboard: Faster.\n- Something global.\n\n## [0.3.0]\n### Added\n- Updates: Self-update.\n";

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn skips_unreleased_and_reads_versions_in_order() {
        let v: Vec<_> = parse(LOG).iter().map(|n| n.version.to_string()).collect();
        assert_eq!(v, ["0.5.0", "0.4.0", "0.3.0"]);
    }

    #[test]
    fn reads_date_and_summary() {
        let n = parse(LOG);
        assert_eq!(n[0].date.as_deref(), Some("2026-12-01"));
        assert_eq!(n[0].summary.as_deref(), Some("Five."));
        assert_eq!(n[2].date, None);
        assert_eq!(n[2].summary, None);
    }

    #[test]
    fn splits_feature_key_and_settings_path() {
        let n = &parse(LOG)[0];
        let c = &n.changes[0];
        assert_eq!(c.feature.unwrap().key, "Pocket");
        assert_eq!(c.text, "New thing.");
        assert_eq!(c.settings_path.as_deref(), Some("Settings → Pocket → View"));
        assert_eq!(c.kind, ChangeKind::New);
        assert_eq!(c.version, v("0.5.0"));
        assert_eq!(n.changes[1].kind, ChangeKind::Fixed);
        assert_eq!(parse(LOG)[1].changes[0].kind, ChangeKind::Improved);
    }

    #[test]
    fn unknown_or_missing_key_goes_everywhere() {
        let n = &parse(LOG)[1];
        assert!(n.changes[1].feature.is_none());
        assert_eq!(n.changes[1].text, "Something global.");
        let n = parse("## [1.0.0]\n### Added\n- Nonsense: whole line.\n");
        assert!(n[0].changes[0].feature.is_none());
        assert_eq!(n[0].changes[0].text, "Nonsense: whole line.");
    }

    #[test]
    fn skips_headers_with_a_suffix_and_their_bodies() {
        let n = parse(
            "## [0.9.0-beta]\n### Added\n- Pocket: x.\n## [0.8.0]\n### Added\n- Pocket: y.\n",
        );
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].changes[0].text, "y.");
    }

    #[test]
    fn ignores_other_sections_and_joins_continuation_lines() {
        let n = parse(
            "## [1.0.0]\n### Removed\n- Pocket: gone.\n### Added\n- Pocket: one\n  and two.\n  Settings → Pocket.\n- Wheel: next.\n",
        );
        assert_eq!(n[0].changes.len(), 2);
        assert_eq!(n[0].changes[0].text, "one and two.");
        assert_eq!(
            n[0].changes[0].settings_path.as_deref(),
            Some("Settings → Pocket")
        );
    }

    #[test]
    fn reads_an_arabic_settings_path() {
        let n = parse("## [1.0.0]\n### Added\n- Updates: نص جديد. الإعدادات ← حول.\n");
        let c = &n[0].changes[0];
        assert_eq!(c.text, "نص جديد.");
        assert_eq!(c.settings_path.as_deref(), Some("الإعدادات ← حول"));
    }

    #[test]
    fn a_path_not_after_a_sentence_stays_in_the_text() {
        let n = parse("## [1.0.0]\n### Added\n- Pocket: Open Settings → Pocket to see it.\n");
        let c = &n[0].changes[0];
        assert_eq!(c.text, "Open Settings → Pocket to see it.");
        assert_eq!(c.settings_path, None);
    }

    #[test]
    fn digest_merges_skipped_versions() {
        let d = digest(&parse(LOG), Version::parse("0.3.0"), v("0.5.0")).unwrap();
        assert_eq!(d.changes.len(), 4);
        assert_eq!(d.summary.as_deref(), Some("Five."));
        assert_eq!(d.previous, Version::parse("0.3.0"));
        assert_eq!(d.version, v("0.5.0"));
        assert_eq!(d.changes[0].version, v("0.5.0"));
    }

    #[test]
    fn digest_without_previous_is_only_the_running_version() {
        let d = digest(&parse(LOG), None, v("0.4.0")).unwrap();
        assert_eq!(d.changes.len(), 2);
        assert_eq!(d.summary.as_deref(), Some("Four."));
    }

    #[test]
    fn digest_of_unknown_version_is_none() {
        assert!(digest(&parse(LOG), None, v("9.9.9")).is_none());
    }

    fn sample_digest() -> Digest {
        digest(&parse(LOG), Version::parse("0.3.0"), v("0.5.0")).unwrap()
    }

    #[test]
    fn groups_put_new_first_and_everywhere_last() {
        let g = sample_digest().groups();
        let keys: Vec<_> = g.iter().map(|g| g.feature.map(|f| f.key)).collect();
        assert_eq!(
            keys,
            [Some("Pocket"), Some("Clipboard"), Some("PDF Pages"), None]
        );
        let new: Vec<_> = g.iter().map(|g| g.is_new).collect();
        assert_eq!(new, [true, false, false, false]);
    }

    #[test]
    fn a_group_with_one_non_new_entry_is_not_new() {
        let n = parse(
            "## [1.0.0]\n### Added\n- Wheel: a.\n### Fixed\n- Wheel: b.\n- Move: c.\n### Added\n- Move: d.\n",
        );
        let d = digest(&n, None, v("1.0.0")).unwrap();
        let g = d.groups();
        assert_eq!(g[0].feature.unwrap().key, "Wheel");
        assert!(!g[0].is_new);
        assert_eq!(g.len(), 2);
        assert!(!g[1].is_new);
    }

    #[test]
    fn counts_are_new_improved_fixed() {
        assert_eq!(sample_digest().counts(), [1, 2, 1]);
    }

    #[test]
    fn release_body_parses_without_header() {
        let n = parse_release_body(v("0.3.0"), "### Added\n- Updates: x.");
        assert_eq!(n.changes.len(), 1);
        assert_eq!(n.version, v("0.3.0"));
        assert_eq!(n.changes[0].version, v("0.3.0"));
        assert_eq!(n.date, None);
    }

    #[test]
    fn arabic_falls_back_per_version() {
        let en = parse(LOG);
        let ar = parse(
            "## [0.4.0]\n### Summary\nأربعة.\n### Changed\n- Clipboard: أسرع.\n\n## [0.2.0]\n### Added\n- Pocket: x.\n",
        );
        let m = merge(en, ar);
        let vs: Vec<_> = m.iter().map(|n| n.version.to_string()).collect();
        assert_eq!(vs, ["0.5.0", "0.4.0", "0.3.0"]);
        assert_eq!(m[0].summary.as_deref(), Some("Five."));
        assert_eq!(m[1].summary.as_deref(), Some("أربعة."));
    }

    #[test]
    fn bundled_changelogs_parse_and_match() {
        let en = parse(include_str!("../../../../CHANGELOG.md"));
        let ar = parse(include_str!("../../../../CHANGELOG.ar.md"));
        assert!(!en.is_empty());
        let ev: Vec<_> = en.iter().map(|n| n.version).collect();
        let av: Vec<_> = ar.iter().map(|n| n.version).collect();
        assert_eq!(
            ev, av,
            "every version in CHANGELOG.md is in CHANGELOG.ar.md"
        );
        assert_eq!(
            en[0].version,
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
            "top entry is the crate version"
        );
    }

    #[test]
    fn bundled_notes_by_language() {
        let en = bundled_notes("en");
        let ar = bundled_notes("ar");
        assert_eq!(en.len(), ar.len());
        assert_ne!(en[0].summary, ar[0].summary);
        assert_eq!(bundled_notes("de")[0].summary, en[0].summary);
    }

    #[test]
    fn every_bundled_entry_has_a_known_feature_key() {
        for lang in ["en", "ar"] {
            for n in bundled_notes(lang) {
                for c in n.changes {
                    assert!(c.feature.is_some(), "{lang} {}: {}", n.version, c.text);
                    assert!(!c.text.is_empty());
                }
            }
        }
    }
}
