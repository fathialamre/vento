//! The screenshots the island lists: which files in the screenshot folder
//! count as shots, when a file is done being written, the last-week window,
//! the day sections of the grid, and the record that the user let vento
//! read a protected folder.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::durable;
use crate::i18n::dates::{time_hm, weekday_short};
use crate::i18n::{fill, tr, trc};
use crate::welcome;

/// How many days back a screenshot is still listed.
pub const SHOT_DAYS: u64 = 7;
/// The most screenshots listed.
pub const MAX_SHOTS: usize = 60;
/// How long a file's size must stay the same before the file counts as
/// saved.
pub const SETTLE: Duration = Duration::from_millis(300);

/// Newest on-disk format of the grant file this build understands.
const FORMAT_VERSION: u32 = 1;

/// Extensions of a screenshot where the system marks its own captures
/// (macOS), lowercase.
const MARKED_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "heic", "tiff", "pdf"];
/// Extensions of a screenshot where no mark tells captures apart, lowercase.
const PLAIN_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];

/// What a scan of the screenshot folder learned about one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFacts {
    /// Full path of the entry.
    pub path: PathBuf,
    /// Size in bytes.
    pub len: u64,
    /// Last modification, in seconds since the Unix epoch.
    pub modified_unix: u64,
    /// Whether the entry is a regular file (not a folder).
    pub is_file: bool,
    /// Whether the system marked the file as one of its screen captures;
    /// `true` where there is no such mark.
    pub screen_capture: bool,
}

/// Whether `path` is hidden: its name starts with a dot.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// Whether the entry is a screenshot (possibly still being written): a
/// visible regular file with a picture extension. With `needs_mark` the
/// system's capture mark is required and `heic`, `tiff` and `pdf` count too;
/// without it only `png`, `jpg` and `jpeg` do.
pub fn is_shot_candidate(f: &FileFacts, needs_mark: bool) -> bool {
    if !f.is_file || is_hidden(&f.path) || (needs_mark && !f.screen_capture) {
        return false;
    }
    let Some(ext) = f.path.extension() else {
        return false;
    };
    let ext = ext.to_string_lossy().to_lowercase();
    let allowed: &[&str] = if needs_mark {
        &MARKED_EXTENSIONS
    } else {
        &PLAIN_EXTENSIONS
    };
    allowed.contains(&ext.as_str())
}

/// One screenshot in the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    /// Full path of the file.
    pub path: PathBuf,
    /// Last modification, in seconds since the Unix epoch.
    pub modified_unix: u64,
    /// Size in bytes.
    pub len: u64,
    /// Width and height in pixels, when they could be read.
    pub dims: Option<(u32, u32)>,
}

/// Tells a finished file from one still being written: a file counts once
/// its size has stayed the same for [`SETTLE`].
#[derive(Debug, Default)]
pub struct Settler {
    /// Each file's size and when that size was first seen.
    seen: HashMap<PathBuf, (u64, Instant)>,
}

impl Settler {
    /// A settler that has seen nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks at one scan, taken at `now`. Returns the files whose size is
    /// the one seen at least [`SETTLE`] earlier, and whether some file is
    /// still unsettled (the caller scans again after [`SETTLE`]). Hidden
    /// entries and folders are not looked at: a hidden file is a temporary
    /// one that will be renamed, and the rename is a change of its own.
    pub fn settled(&mut self, facts: &[FileFacts], now: Instant) -> (Vec<PathBuf>, bool) {
        let mut done = Vec::new();
        let mut waiting = false;
        let mut seen = HashMap::new();
        for f in facts.iter().filter(|f| f.is_file && !is_hidden(&f.path)) {
            let since = match self.seen.get(&f.path) {
                Some(&(len, since)) if len == f.len => since,
                _ => now,
            };
            if now.saturating_duration_since(since) >= SETTLE {
                done.push(f.path.clone());
            } else {
                waiting = true;
            }
            seen.insert(f.path.clone(), (f.len, since));
        }
        // Files that are gone are forgotten, so a new file of the same name
        // settles afresh.
        self.seen = seen;
        (done, waiting)
    }
}

/// The shots to list: modified within [`SHOT_DAYS`] days of `now_unix`,
/// newest first, at most [`MAX_SHOTS`].
pub fn recent(mut shots: Vec<Shot>, now_unix: u64) -> Vec<Shot> {
    let oldest = now_unix.saturating_sub(SHOT_DAYS * 86_400);
    shots.retain(|s| s.modified_unix >= oldest);
    shots.sort_by_key(|s| std::cmp::Reverse(s.modified_unix));
    shots.truncate(MAX_SHOTS);
    shots
}

/// The paths in `after` that are not in `before`, in `after`'s order
/// (newest first).
pub fn new_shots(before: &HashSet<PathBuf>, after: &[Shot]) -> Vec<PathBuf> {
    after
        .iter()
        .filter(|s| !before.contains(&s.path))
        .map(|s| s.path.clone())
        .collect()
}

/// The calendar day a shot belongs to, counted from now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Day {
    /// The same calendar day as now.
    Today,
    /// The calendar day before now.
    Yesterday,
    /// Any day before that.
    Earlier,
}

/// The calendar day of `unix` in `tz`; `None` for a time out of range.
fn date_of(unix: u64, tz: &TimeZone) -> Option<jiff::civil::DateTime> {
    let at = jiff::Timestamp::from_second(i64::try_from(unix).ok()?).ok()?;
    Some(at.to_zoned(tz.clone()).datetime())
}

/// Which day a shot saved at `modified_unix` falls on at `now_unix`, by the
/// calendar of `tz`. A shot dated after now counts as today.
pub fn day_of(modified_unix: u64, now_unix: u64, tz: &TimeZone) -> Day {
    let (Some(shot), Some(now)) = (date_of(modified_unix, tz), date_of(now_unix, tz)) else {
        return Day::Earlier;
    };
    let (shot, now) = (shot.date(), now.date());
    if shot >= now {
        Day::Today
    } else if now.yesterday().is_ok_and(|y| y == shot) {
        Day::Yesterday
    } else {
        Day::Earlier
    }
}

/// The heading of a day's section in the grid.
pub fn day_heading(d: Day) -> String {
    match d {
        Day::Today => tr("Today"),
        Day::Yesterday => tr("Yesterday"),
        Day::Earlier => tr("Earlier this week"),
    }
}

/// The short name of a day on its filter chip.
pub fn day_chip(d: Day) -> String {
    match d {
        Day::Today => tr("Today"),
        Day::Yesterday => tr("Yesterday"),
        Day::Earlier => tr("Earlier"),
    }
}

/// When a shot was saved, as the grid labels it: the time for today and
/// yesterday (`"14:30"`), the weekday and time before that (`"Mon 17:20"`).
pub fn time_label(modified_unix: u64, now_unix: u64, tz: &TimeZone) -> String {
    let Some(at) = date_of(modified_unix, tz) else {
        return String::new();
    };
    match day_of(modified_unix, now_unix, tz) {
        Day::Today | Day::Yesterday => time_hm(at.time()),
        Day::Earlier => fill(
            &trc("shot", "{day} {time}"),
            &[
                ("day", &weekday_short(at.date())),
                ("time", &time_hm(at.time())),
            ],
        ),
    }
}

/// Which shots the grid shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShotFilter {
    /// Every day.
    All,
    /// One day only.
    Day(Day),
}

/// The grid's sections for `filter`: Today, Yesterday, Earlier in that
/// order, each with the indices into `shots` that fall on it. Sections with
/// no shot are left out.
pub fn grouped(
    shots: &[Shot],
    filter: ShotFilter,
    now_unix: u64,
    tz: &TimeZone,
) -> Vec<(Day, Vec<usize>)> {
    [Day::Today, Day::Yesterday, Day::Earlier]
        .into_iter()
        .filter(|d| matches!(filter, ShotFilter::All) || filter == ShotFilter::Day(*d))
        .filter_map(|d| {
            let at: Vec<usize> = shots
                .iter()
                .enumerate()
                .filter(|(_, s)| day_of(s.modified_unix, now_unix, tz) == d)
                .map(|(i, _)| i)
                .collect();
            (!at.is_empty()).then_some((d, at))
        })
        .collect()
}

/// How many shots each filter chip holds: All, Today, Yesterday, Earlier.
pub fn counts(shots: &[Shot], now_unix: u64, tz: &TimeZone) -> [usize; 4] {
    let mut counts = [shots.len(), 0, 0, 0];
    for s in shots {
        match day_of(s.modified_unix, now_unix, tz) {
            Day::Today => counts[1] += 1,
            Day::Yesterday => counts[2] += 1,
            Day::Earlier => counts[3] += 1,
        }
    }
    counts
}

/// The grant file as stored on disk.
#[derive(Serialize, Deserialize)]
struct RawGrant {
    version: u32,
    build: String,
}

/// Location of the grant file, in vento's data folder.
pub fn grant_path() -> Option<PathBuf> {
    Some(welcome::data_dir()?.join("screenshots.json"))
}

/// The build the user last let read the screenshot folder, or `None` when
/// the file is missing, unreadable or from a newer format.
pub fn load_grant(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw: RawGrant = serde_json::from_str(&text).ok()?;
    (raw.version <= FORMAT_VERSION).then_some(raw.build)
}

/// Records that the user let `build` read the screenshot folder, creating
/// the parent folder if needed. The content goes to `<name>.tmp` first and
/// is renamed into place, so a crash never leaves a truncated file.
pub fn save_grant(path: &Path, build: &str) -> io::Result<()> {
    let raw = RawGrant {
        version: FORMAT_VERSION,
        build: build.to_owned(),
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

/// Whether the island must wait to open the folder: a protected folder
/// (macOS asks before an app reads it) that this build was not yet let into.
pub fn wait_for_open(protected: bool, granted_build: Option<&str>, this_build: &str) -> bool {
    protected && granted_build != Some(this_build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_language;

    /// 2026-10-28 00:00:00 UTC, a Wednesday.
    const DAY0: u64 = 1_793_145_600;

    fn facts(name: &str, len: u64) -> FileFacts {
        FileFacts {
            path: PathBuf::from("/shots").join(name),
            len,
            modified_unix: DAY0,
            is_file: true,
            screen_capture: false,
        }
    }

    fn marked(name: &str) -> FileFacts {
        FileFacts {
            screen_capture: true,
            ..facts(name, 1)
        }
    }

    fn shot(name: &str, modified_unix: u64) -> Shot {
        Shot {
            path: PathBuf::from("/shots").join(name),
            modified_unix,
            len: 1,
            dims: None,
        }
    }

    #[test]
    fn mac_needs_the_mark() {
        let plain = facts("Screenshot 1.png", 1);
        assert!(!is_shot_candidate(&plain, true));
        assert!(is_shot_candidate(&plain, false));
        assert!(is_shot_candidate(&marked("Screenshot 1.png"), true));
    }

    #[test]
    fn only_picture_extensions() {
        for name in ["clip.mov", "notes.txt", "noext"] {
            assert!(!is_shot_candidate(&marked(name), true), "{name}");
            assert!(!is_shot_candidate(&facts(name, 1), false), "{name}");
        }
        let folder = FileFacts {
            is_file: false,
            ..marked("Shots.png")
        };
        assert!(!is_shot_candidate(&folder, true));
        for name in ["a.PNG", "a.heic", "a.pdf", "a.tiff", "a.JPeg"] {
            assert!(is_shot_candidate(&marked(name), true), "{name}");
        }
        assert!(!is_shot_candidate(&facts("a.heic", 1), false));
        assert!(!is_shot_candidate(&facts("a.pdf", 1), false));
        assert!(is_shot_candidate(&facts("a.JPG", 1), false));
    }

    #[test]
    fn hidden_and_growing_files_are_not_shots_yet() {
        let mut settler = Settler::new();
        let t0 = Instant::now();
        let hidden = [facts(".Screenshot.png", 10)];
        for step in 0..3u32 {
            let (done, waiting) = settler.settled(&hidden, t0 + SETTLE * (step + 1));
            assert!(done.is_empty());
            assert!(!waiting);
        }

        let file = |len| [facts("Screenshot.png", len)];
        let path = PathBuf::from("/shots/Screenshot.png");
        let (done, waiting) = settler.settled(&file(10), t0);
        assert!(done.is_empty() && waiting);
        // Grew: the clock starts over.
        let (done, waiting) = settler.settled(&file(20), t0 + SETTLE);
        assert!(done.is_empty() && waiting);
        // Same size, but not for long enough yet.
        let (done, waiting) = settler.settled(&file(20), t0 + SETTLE + SETTLE / 2);
        assert!(done.is_empty() && waiting);
        let (done, waiting) = settler.settled(&file(20), t0 + SETTLE * 2);
        assert_eq!(done, std::slice::from_ref(&path));
        assert!(!waiting);
        // Gone, then back under the same name: settles afresh.
        settler.settled(&[], t0 + SETTLE * 3);
        let (done, waiting) = settler.settled(&file(20), t0 + SETTLE * 4);
        assert!(done.is_empty() && waiting);
    }

    #[test]
    fn recent_keeps_seven_days_newest_first_at_most_sixty() {
        let now = DAY0 + 10 * 86_400;
        // 90 shots, one every 2h13m going back 8 days: 76 of them are inside
        // the week, and one more is much older.
        let mut shots: Vec<Shot> = (0..90)
            .map(|i| shot(&format!("{i}.png"), now - i * 8_000))
            .collect();
        shots.push(shot("old.png", now - 9 * 86_400));
        shots.reverse();
        let kept = recent(shots, now);
        assert_eq!(kept.len(), MAX_SHOTS);
        assert!(
            kept.windows(2)
                .all(|w| w[0].modified_unix > w[1].modified_unix)
        );
        assert!(
            kept.iter()
                .all(|s| now - s.modified_unix <= SHOT_DAYS * 86_400)
        );
        assert_eq!(kept[0].modified_unix, now);

        let week = recent(
            vec![
                shot("in.png", now - SHOT_DAYS * 86_400),
                shot("out.png", now - SHOT_DAYS * 86_400 - 1),
            ],
            now,
        );
        assert_eq!(week, [shot("in.png", now - SHOT_DAYS * 86_400)]);
    }

    #[test]
    fn new_shots_are_newest_first() {
        let after = [shot("c.png", 30), shot("b.png", 20), shot("a.png", 10)];
        let before: HashSet<PathBuf> = [PathBuf::from("/shots/a.png")].into();
        assert_eq!(
            new_shots(&before, &after),
            [PathBuf::from("/shots/c.png"), PathBuf::from("/shots/b.png")]
        );
        assert!(new_shots(&HashSet::new(), &[]).is_empty());
    }

    #[test]
    fn grouping_follows_now() {
        let utc = TimeZone::UTC;
        let at = DAY0 - 60; // 23:59 the day before DAY0
        assert_eq!(day_of(at, DAY0 - 30, &utc), Day::Today);
        assert_eq!(day_of(at, DAY0 + 30, &utc), Day::Yesterday);
        assert_eq!(day_of(at, DAY0 + 86_400 + 30, &utc), Day::Earlier);
        // After now counts as today.
        assert_eq!(day_of(DAY0 + 5, DAY0, &utc), Day::Today);
    }

    #[test]
    fn grouped_skips_empty_days_and_filters() {
        let utc = TimeZone::UTC;
        let now = DAY0 + 12 * 3_600;
        let shots = [
            shot("today.png", now - 60),
            shot("yday.png", DAY0 - 3_600),
            shot("yday2.png", DAY0 - 7_200),
            shot("old.png", DAY0 - 3 * 86_400),
        ];
        let all = grouped(&shots, ShotFilter::All, now, &utc);
        assert_eq!(
            all,
            [
                (Day::Today, vec![0]),
                (Day::Yesterday, vec![1, 2]),
                (Day::Earlier, vec![3]),
            ]
        );
        assert_eq!(
            grouped(&shots, ShotFilter::Day(Day::Yesterday), now, &utc),
            [(Day::Yesterday, vec![1, 2])]
        );
        assert!(grouped(&shots[1..], ShotFilter::Day(Day::Today), now, &utc).is_empty());

        let c = counts(&shots, now, &utc);
        assert_eq!(c, [4, 1, 2, 1]);
        assert_eq!(c[1] + c[2] + c[3], c[0]);
    }

    #[test]
    fn earlier_label_has_the_weekday() {
        let utc = TimeZone::UTC;
        // DAY0 is a Wednesday; two days before it is a Monday.
        let now = DAY0 + 12 * 3_600;
        let at = DAY0 - 2 * 86_400 + 17 * 3_600 + 20 * 60;
        with_language("en", || {
            assert_eq!(time_label(at, now, &utc), "Mon 17:20");
            assert_eq!(time_label(now - 3_600, now, &utc), "11:00");
            assert_eq!(time_label(DAY0 - 3_600, now, &utc), "23:00");
            assert_eq!(day_heading(Day::Earlier), "Earlier this week");
            assert_eq!(day_chip(Day::Earlier), "Earlier");
            assert_eq!(day_heading(Day::Today), "Today");
            assert_eq!(day_chip(Day::Yesterday), "Yesterday");
        });
    }

    #[test]
    fn wait_for_open_only_for_protected_new_builds() {
        assert!(wait_for_open(true, None, "b"));
        assert!(wait_for_open(true, Some("a"), "b"));
        assert!(!wait_for_open(true, Some("b"), "b"));
        assert!(!wait_for_open(false, None, "b"));
    }

    #[test]
    fn grant_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("screenshots.json");
        assert_eq!(load_grant(&p), None);
        save_grant(&p, "0.3.0").unwrap();
        assert_eq!(load_grant(&p).as_deref(), Some("0.3.0"));
        save_grant(&p, "0.4.0").unwrap();
        assert_eq!(load_grant(&p).as_deref(), Some("0.4.0"));
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(load_grant(&p), None);
        std::fs::write(&p, r#"{"version":9,"build":"x"}"#).unwrap();
        assert_eq!(load_grant(&p), None);
    }
}
