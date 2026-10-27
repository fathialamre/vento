//! Turns a size count into the strings the Info window shows.
//!
//! Nothing here touches the file system except [`subject_of`]'s directory
//! check; sizes, dates and layout of the details are plain functions of their
//! inputs so they can be tested without a window.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::destinations::display_path;
use crate::i18n::dates::date_time;
use crate::i18n::{fill, tr, trn, trn_shown};
use crate::sizes::Progress;

/// `bytes` in decimal units: `"Zero bytes"`, `"512 bytes"`, `"839 KB"`, `"4.82 GB"`.
pub fn human_size(bytes: u64) -> String {
    match bytes {
        0 => return tr("Zero bytes"),
        1..=999 => return trn("{n} byte", "{n} bytes", bytes),
        _ => {}
    }
    let units = [tr("KB"), tr("MB"), tr("GB"), tr("TB")];
    let mut scale = 1_000f64;
    for (i, unit) in units.iter().enumerate() {
        let value = bytes as f64 / scale;
        let text = round_trimmed(value);
        let rounded_up = text.parse::<f64>().is_ok_and(|v| v >= 1_000.0);
        if i + 1 == units.len() || !rounded_up {
            return format!("{text} {unit}");
        }
        scale *= 1_000.0;
    }
    unreachable!("the last unit always returns")
}

/// `value` with 2 decimals below 10, 1 below 100, none above; no trailing zeros.
fn round_trimmed(value: f64) -> String {
    let decimals = if value < 10.0 {
        2
    } else if value < 100.0 {
        1
    } else {
        0
    };
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

/// `n` with comma thousands separators.
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `t` as local time in `tz`, in the interface language, e.g.
/// `"20 Oct 2026, 2:35 PM"`.
///
/// Empty when `t` is outside the range dates can express.
pub fn format_time(t: SystemTime, tz: &jiff::tz::TimeZone) -> String {
    match jiff::Timestamp::try_from(t) {
        Ok(ts) => date_time(ts.to_zoned(tz.clone()).datetime()),
        Err(_) => String::new(),
    }
}

/// What the window is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// One file.
    File,
    /// One package, shown like a file.
    Package,
    /// One folder.
    Folder,
    /// Several items at once.
    Several,
}

/// Classifies `paths`; a symlink to a folder counts as a file.
pub fn subject_of(paths: &[PathBuf], is_package: &dyn Fn(&Path) -> bool) -> Subject {
    let [path] = paths else {
        return Subject::Several;
    };
    let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    match (is_dir, is_dir && is_package(path)) {
        (false, _) => Subject::File,
        (true, true) => Subject::Package,
        (true, false) => Subject::Folder,
    }
}

/// What is known about a single item without counting it.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Kind as the system names it, e.g. `"PNG image"`.
    pub kind: Option<String>,
    /// When the item was created.
    pub created: Option<SystemTime>,
    /// When the item was last modified.
    pub modified: Option<SystemTime>,
}

/// Where and when the view is shown.
#[derive(Debug, Clone)]
pub struct ViewContext {
    /// The home folder, shown as `~` in paths.
    pub home: Option<PathBuf>,
    /// The time zone dates are shown in.
    pub tz: jiff::tz::TimeZone,
}

impl ViewContext {
    /// The running user's: `$HOME` and the system time zone (UTC when it
    /// cannot be found).
    pub fn local() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            tz: jiff::tz::TimeZone::system(),
        }
    }
}

/// One bar of the "biggest parts" chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    /// File name of the part.
    pub name: String,
    /// Its size, e.g. `"4.82 GB"`.
    pub size: String,
    /// Fraction of the total, from 0 to 1.
    pub share: f32,
}

/// Everything the Info window displays.
#[derive(Debug, Clone, PartialEq)]
pub struct InfoView {
    /// File name, or `"N items"`.
    pub title: String,
    /// Header size, prefixed `"≥ "` when something could not be read.
    pub size: String,
    /// `"Calculating…"` until the count is done, then empty.
    pub status: String,
    /// Label and value rows, in display order.
    pub details: Vec<(String, String)>,
    /// A warning line, or empty.
    pub note: String,
    /// The largest parts, largest first; never any with zero bytes.
    pub biggest: Vec<Bar>,
    /// The folder was counted completely and holds nothing.
    pub empty_folder: bool,
}

/// Builds the window content for `paths` from the count so far.
pub fn view(
    paths: &[PathBuf],
    subject: Subject,
    facts: &Facts,
    progress: &Progress,
    ctx: &ViewContext,
) -> InfoView {
    let tally = &progress.tally;
    let home = ctx.home.as_deref();
    let mut details: Vec<(String, String)> = Vec::new();
    let mut push = |label: String, value: String| details.push((label, value));
    let time_row =
        |t: Option<SystemTime>| t.map(|t| format_time(t, &ctx.tz)).filter(|s| !s.is_empty());
    let contains = fill(
        &tr("{files}, {folders}"),
        &[
            ("files", &counted_files(tally.files)),
            ("folders", &counted_folders(tally.folders)),
        ],
    );
    let mut with_bars = true;

    match subject {
        Subject::File | Subject::Package => {
            with_bars = false;
            if let Some(kind) = &facts.kind {
                push(tr("Kind"), kind.clone());
            }
            let exact = exact_size(tally.bytes);
            push(
                tr("Size"),
                if tally.skipped {
                    format!("≥ {exact}")
                } else {
                    exact
                },
            );
            if let Some(parent) = paths.first().and_then(|p| p.parent()) {
                push(tr("Where"), display_path(parent, home));
            }
            if let Some(created) = time_row(facts.created) {
                push(tr("Created"), created);
            }
            if let Some(modified) = time_row(facts.modified) {
                push(tr("Modified"), modified);
            }
        }
        Subject::Folder => {
            push(tr("Contains"), contains);
            if let Some(modified) = time_row(facts.modified) {
                push(tr("Modified"), modified);
            }
        }
        Subject::Several => {
            push(tr("Contains"), contains);
            let mut parents = paths.iter().map(|p| p.parent());
            let first = parents.next().flatten();
            if parents.all(|p| p == first) {
                if let Some(parent) = first {
                    push(tr("Where"), display_path(parent, home));
                }
            } else {
                push(tr("Where"), tr("Several folders"));
            }
        }
    }

    let title = match subject {
        Subject::Several => trn("{n} item", "{n} items", paths.len() as u64),
        _ => paths.first().map(|p| name_of(p)).unwrap_or_default(),
    };
    let size = if tally.skipped {
        format!("≥ {}", human_size(tally.bytes))
    } else {
        human_size(tally.bytes)
    };
    let biggest = if with_bars {
        progress
            .biggest
            .iter()
            .filter(|part| part.bytes > 0)
            .map(|part| Bar {
                name: name_of(&part.path),
                size: human_size(part.bytes),
                share: if tally.bytes == 0 {
                    0.0
                } else {
                    (part.bytes as f64 / tally.bytes as f64).min(1.0) as f32
                },
            })
            .collect()
    } else {
        Vec::new()
    };

    InfoView {
        title,
        size,
        status: if progress.done {
            String::new()
        } else {
            tr("Calculating…")
        },
        details,
        note: if tally.skipped {
            tr("Some items could not be read")
        } else {
            String::new()
        },
        biggest,
        empty_folder: subject == Subject::Folder
            && progress.done
            && !tally.skipped
            && tally.bytes == 0
            && tally.files == 0
            && tally.folders == 0,
    }
}

/// The file name of `path`, or the whole path when it has none (`/`).
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// `"1 file"`, `"2 files"`, with thousands separators.
fn counted_files(n: u64) -> String {
    trn_shown("{n} file", "{n} files", n, &group_thousands(n))
}

/// `"1 folder"`, `"2 folders"`, with thousands separators.
fn counted_folders(n: u64) -> String {
    trn_shown("{n} folder", "{n} folders", n, &group_thousands(n))
}

/// Exact byte count: `"838,591 bytes"`, `"1 byte"`.
fn exact_size(bytes: u64) -> String {
    trn_shown("{n} byte", "{n} bytes", bytes, &group_thousands(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_language;
    use crate::sizes::{Part, Tally};

    fn at(rfc3339: &str) -> SystemTime {
        SystemTime::from(rfc3339.parse::<jiff::Timestamp>().unwrap())
    }

    fn ctx() -> ViewContext {
        ViewContext {
            home: Some(PathBuf::from("/Users/me")),
            tz: jiff::tz::TimeZone::UTC,
        }
    }

    fn progress(tally: Tally, biggest: Vec<Part>, done: bool) -> Progress {
        Progress {
            tally,
            biggest,
            done,
        }
    }

    fn part(path: &str, bytes: u64) -> Part {
        Part {
            path: PathBuf::from(path),
            bytes,
        }
    }

    fn tally(bytes: u64, files: u64, folders: u64) -> Tally {
        Tally {
            bytes,
            files,
            folders,
            skipped: false,
        }
    }

    #[test]
    fn human_sizes() {
        for (bytes, want) in [
            (0, "Zero bytes"),
            (1, "1 byte"),
            (512, "512 bytes"),
            (1_000, "1 KB"),
            (838_591, "839 KB"),
            (999_999, "1 MB"),
            (4_823_117_402, "4.82 GB"),
            (12_400_000_000, "12.4 GB"),
            (1_000_000_000, "1 GB"),
            (4_800_000_000, "4.8 GB"),
            (2_500_000_000_000, "2.5 TB"),
        ] {
            assert_eq!(human_size(bytes), want, "{bytes}");
        }
    }

    #[test]
    fn thousands() {
        for (n, want) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1,000"),
            (838_591, "838,591"),
            (4_823_117_402, "4,823,117,402"),
        ] {
            assert_eq!(group_thousands(n), want);
        }
    }

    #[test]
    fn times_are_local() {
        let t = at("2026-10-20T14:35:00Z");
        assert_eq!(
            format_time(t, &jiff::tz::TimeZone::UTC),
            "20 Oct 2026, 2:35 PM"
        );
        let plus3 = jiff::tz::TimeZone::fixed(jiff::tz::offset(3));
        assert_eq!(format_time(t, &plus3), "20 Oct 2026, 5:35 PM");
        assert_eq!(
            format_time(at("2026-03-03T09:05:00Z"), &jiff::tz::TimeZone::UTC),
            "3 Mar 2026, 9:05 AM"
        );
    }

    #[test]
    fn english_times_match_strftime_for_every_month_and_hour() {
        let tz = jiff::tz::TimeZone::UTC;
        for month in 1..=12 {
            for hour in [0, 1, 9, 11, 12, 13, 23] {
                let ts = format!("2026-{month:02}-{:02}T{hour:02}:07:00Z", month + 3);
                let t = at(&ts);
                let want = jiff::Timestamp::try_from(t)
                    .unwrap()
                    .to_zoned(tz.clone())
                    .strftime("%-d %b %Y, %-I:%M %p")
                    .to_string();
                assert_eq!(format_time(t, &tz), want, "{ts}");
            }
        }
    }

    #[test]
    fn subjects() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let app = dir.path().join("Foo.app");
        std::fs::create_dir(&app).unwrap();
        let no = |_: &Path| false;
        let yes = |_: &Path| true;

        assert_eq!(subject_of(std::slice::from_ref(&file), &no), Subject::File);
        assert_eq!(
            subject_of(&[dir.path().to_path_buf()], &no),
            Subject::Folder
        );
        assert_eq!(subject_of(&[app], &yes), Subject::Package);
        assert_eq!(subject_of(&[file.clone(), file], &no), Subject::Several);
    }

    #[test]
    fn file_view() {
        let created = at("2026-10-20T14:35:00Z");
        let modified = at("2026-03-03T09:05:00Z");
        let facts = Facts {
            kind: Some("PNG image".to_owned()),
            created: Some(created),
            modified: Some(modified),
        };
        let paths = [PathBuf::from("/Users/me/Desktop/shot.png")];
        let p = progress(tally(838_591, 1, 0), Vec::new(), true);
        let v = view(&paths, Subject::File, &facts, &p, &ctx());

        assert_eq!(v.title, "shot.png");
        assert_eq!(v.size, "839 KB");
        assert_eq!(v.status, "");
        assert_eq!(v.note, "");
        assert!(v.biggest.is_empty());
        let row = |l: &str, s: &str| (l.to_owned(), s.to_owned());
        assert_eq!(
            v.details,
            [
                row("Kind", "PNG image"),
                row("Size", "838,591 bytes"),
                row("Where", "~/Desktop"),
                row("Created", "20 Oct 2026, 2:35 PM"),
                row("Modified", "3 Mar 2026, 9:05 AM"),
            ]
        );
    }

    #[test]
    fn file_view_leaves_out_what_is_unknown() {
        let paths = [PathBuf::from("/tmp/a")];
        let p = progress(tally(1, 1, 0), Vec::new(), true);
        let v = view(&paths, Subject::File, &Facts::default(), &p, &ctx());
        assert_eq!(
            v.details,
            [
                ("Size".to_owned(), "1 byte".to_owned()),
                ("Where".to_owned(), "/tmp".to_owned()),
            ]
        );
    }

    #[test]
    fn folder_view_while_counting() {
        let paths = [PathBuf::from("/Users/me/Projects")];
        let p = progress(tally(5_000, 12_408, 1_031), Vec::new(), false);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        assert_eq!(v.status, "Calculating…");
        assert_eq!(
            v.details[0],
            (
                "Contains".to_owned(),
                "12,408 files, 1,031 folders".to_owned()
            )
        );
        assert!(!v.empty_folder);
    }

    #[test]
    fn singular_contains() {
        let paths = [PathBuf::from("/a")];
        let p = progress(tally(5, 1, 1), Vec::new(), true);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        assert_eq!(v.details[0].1, "1 file, 1 folder");
    }

    #[test]
    fn contains_reads_in_english_and_in_arabic() {
        let paths = [PathBuf::from("/a")];
        let p = progress(tally(5, 2, 1), Vec::new(), true);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        assert_eq!(v.details[0].1, "2 files, 1 folder");
        with_language("ar", || {
            let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
            assert_eq!(v.details[0].0, "يحتوي");
            assert_eq!(v.details[0].1, "ملفان، مجلد واحد");
            for (files, folders, want) in [
                (0, 0, "0 ملف، 0 مجلد"),
                (1, 3, "ملف واحد، 3 مجلدات"),
                (12_408, 1_031, "12,408 ملفات، 1,031 مجلداً"),
                (11, 100, "11 ملفاً، 100 مجلد"),
            ] {
                let p = progress(tally(5, files, folders), Vec::new(), true);
                let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
                assert_eq!(v.details[0].1, want);
            }
        });
    }

    #[test]
    fn sizes_read_in_arabic() {
        with_language("ar", || {
            for (bytes, want) in [
                (0, "صفر بايت"),
                (1, "بايت واحد"),
                (512, "512 بايت"),
                (1_000, "1 ك.ب"),
                (838_591, "839 ك.ب"),
                (4_823_117_402, "4.82 ج.ب"),
                (999_999_999, "1 ج.ب"),
                (2_500_000_000_000, "2.5 ت.ب"),
            ] {
                assert_eq!(human_size(bytes), want, "{bytes}");
            }
            assert_eq!(human_size(1_000_000), "1 م.ب");
            assert_eq!(exact_size(838_591), "838,591 بايت");
            assert_eq!(exact_size(2), "2 بايت");
        });
    }

    #[test]
    fn a_file_reads_in_arabic() {
        let facts = Facts {
            kind: Some("PNG image".to_owned()),
            created: Some(at("2026-11-05T15:20:00Z")),
            modified: Some(at("2026-03-03T09:05:00Z")),
        };
        let paths = [PathBuf::from("/Users/me/Desktop/shot.png")];
        let p = progress(tally(838_591, 1, 0), Vec::new(), true);
        with_language("ar", || {
            let v = view(&paths, Subject::File, &facts, &p, &ctx());
            let row = |l: &str, s: &str| (l.to_owned(), s.to_owned());
            assert_eq!(v.size, "839 ك.ب");
            assert_eq!(
                v.details,
                [
                    row("النوع", "PNG image"),
                    row("الحجم", "838,591 بايت"),
                    row("المكان", "~/Desktop"),
                    row("الإنشاء", "5 نوفمبر 2026، 3:20 م"),
                    row("آخر تعديل", "3 مارس 2026، 9:05 ص"),
                ]
            );
        });
    }

    #[test]
    fn several_and_unfinished_counts_read_in_arabic() {
        let p = progress(tally(10, 2, 0), Vec::new(), false);
        let paths = [PathBuf::from("/a/x"), PathBuf::from("/b/y")];
        with_language("ar", || {
            let v = view(&paths, Subject::Several, &Facts::default(), &p, &ctx());
            assert_eq!(v.title, "عنصران");
            assert_eq!(v.status, "جارٍ الحساب…");
            assert!(
                v.details
                    .contains(&("المكان".to_owned(), "عدة مجلدات".to_owned()))
            );
            let three = [paths[0].clone(), paths[1].clone(), PathBuf::from("/c/z")];
            let v = view(&three, Subject::Several, &Facts::default(), &p, &ctx());
            assert_eq!(v.title, "3 عناصر");
            let skipped = Tally {
                skipped: true,
                ..tally(1, 1, 0)
            };
            let v = view(
                &paths[..1],
                Subject::Folder,
                &Facts::default(),
                &progress(skipped, Vec::new(), true),
                &ctx(),
            );
            assert_eq!(v.note, "تعذّرت قراءة بعض العناصر");
        });
    }

    #[test]
    fn a_single_item_title_is_its_name_and_several_count_one_by_one() {
        let p = progress(tally(10, 2, 0), Vec::new(), true);
        let paths = [PathBuf::from("/a/x"), PathBuf::from("/b/y")];
        let v = view(&paths, Subject::Several, &Facts::default(), &p, &ctx());
        assert_eq!(v.title, "2 items");
        // A group of one (never made by the window) still reads right.
        let v = view(&paths[..1], Subject::Several, &Facts::default(), &p, &ctx());
        assert_eq!(v.title, "1 item");
    }

    #[test]
    fn bars_share_the_total() {
        let paths = [PathBuf::from("/a")];
        let parts = vec![part("/a/x", 50), part("/a/y", 30), part("/a/z", 20)];
        let p = progress(tally(100, 3, 0), parts, true);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        let got: Vec<_> = v
            .biggest
            .iter()
            .map(|b| (b.name.as_str(), b.size.as_str(), b.share))
            .collect();
        assert_eq!(
            got,
            [
                ("x", "50 bytes", 0.5),
                ("y", "30 bytes", 0.3),
                ("z", "20 bytes", 0.2),
            ]
        );
    }

    #[test]
    fn zero_byte_parts_are_not_bars() {
        let paths = [PathBuf::from("/a")];
        let parts = vec![part("/a/x", 10), part("/a/empty", 0)];
        let p = progress(tally(10, 1, 1), parts, true);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        assert_eq!(v.biggest.len(), 1);
        assert_eq!(v.biggest[0].name, "x");
    }

    #[test]
    fn bar_share_is_zero_for_an_empty_total_and_never_above_one() {
        let paths = [PathBuf::from("/a")];
        let p = progress(tally(0, 0, 0), vec![part("/a/x", 5)], false);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        assert_eq!(v.biggest[0].share, 0.0);
        let p = progress(tally(4, 1, 0), vec![part("/a/x", 5)], false);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        assert_eq!(v.biggest[0].share, 1.0);
    }

    #[test]
    fn empty_folder_only_when_done() {
        let paths = [PathBuf::from("/a")];
        let done = progress(tally(0, 0, 0), Vec::new(), true);
        let v = view(&paths, Subject::Folder, &Facts::default(), &done, &ctx());
        assert!(v.empty_folder);
        let counting = progress(tally(0, 0, 0), Vec::new(), false);
        let v = view(
            &paths,
            Subject::Folder,
            &Facts::default(),
            &counting,
            &ctx(),
        );
        assert!(!v.empty_folder);
    }

    #[test]
    fn skipped_count_is_marked_at_least() {
        let paths = [PathBuf::from("/a")];
        let t = Tally {
            bytes: 4_823_117_402,
            files: 3,
            folders: 1,
            skipped: true,
        };
        let v = view(
            &paths,
            Subject::Folder,
            &Facts::default(),
            &progress(t, Vec::new(), true),
            &ctx(),
        );
        assert_eq!(v.size, "≥ 4.82 GB");
        assert_eq!(v.note, "Some items could not be read");

        let empty = Tally {
            skipped: true,
            ..Tally::default()
        };
        let v = view(
            &paths,
            Subject::Folder,
            &Facts::default(),
            &progress(empty, Vec::new(), true),
            &ctx(),
        );
        assert!(!v.empty_folder);
    }

    #[test]
    fn skipped_package_marks_the_size_row_at_least() {
        let paths = [PathBuf::from("/Applications/Foo.app")];
        let t = Tally {
            bytes: 1_234_567_890,
            files: 1,
            folders: 0,
            skipped: true,
        };
        let v = view(
            &paths,
            Subject::Package,
            &Facts::default(),
            &progress(t, Vec::new(), true),
            &ctx(),
        );
        assert_eq!(v.size, "≥ 1.23 GB");
        let size_row = v.details.iter().find(|(l, _)| l == "Size").unwrap();
        assert_eq!(size_row.1, "≥ 1,234,567,890 bytes");
    }

    #[test]
    fn several_items_from_different_folders() {
        let p = progress(tally(10, 2, 0), Vec::new(), true);
        let paths = [PathBuf::from("/a/x"), PathBuf::from("/b/y")];
        let v = view(&paths, Subject::Several, &Facts::default(), &p, &ctx());
        assert_eq!(v.title, "2 items");
        assert!(
            v.details
                .contains(&("Where".to_owned(), "Several folders".to_owned()))
        );

        let paths = [PathBuf::from("/a/x"), PathBuf::from("/a/y")];
        let v = view(&paths, Subject::Several, &Facts::default(), &p, &ctx());
        assert!(v.details.contains(&("Where".to_owned(), "/a".to_owned())));
    }

    #[test]
    fn root_title_falls_back_to_the_path() {
        let paths = [PathBuf::from("/")];
        let p = progress(tally(1, 1, 0), Vec::new(), true);
        let v = view(&paths, Subject::Folder, &Facts::default(), &p, &ctx());
        assert_eq!(v.title, "/");
    }
}
