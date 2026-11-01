//! The live preview: every item's new name and the problem, if any, that blocks it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::items::Item;
use super::name::{Os, join_name, name_key, nfc, split_name};
use super::rules::{Ctx, Rule, apply_rules};

/// Longest name a file system takes, in bytes (UTF-16 units on Windows).
const MAX_NAME_LEN: usize = 255;

/// Why a new name cannot be used; one per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Another item in the same folder ends up with the same name.
    Clash,
    /// A file not in the list already has this name.
    Taken,
    /// The name is empty, or only spaces and dots.
    Empty,
    /// The name holds a character the system forbids.
    BadChar(char),
    /// Windows reserves the name (`CON`, `NUL`, `COM1`, …).
    Reserved,
    /// Windows names cannot end in a dot or space.
    TrailingDotSpace,
    /// Over 255 bytes (UTF-8; UTF-16 units on Windows).
    TooLong,
    /// The item lies inside a folder that is also being renamed.
    Nested,
}

/// Checks one name against `os`'s naming rules, without looking at the disk.
pub fn check_name(name: &str, os: Os) -> Option<Problem> {
    if name.trim_matches([' ', '.']).is_empty() {
        return Some(Problem::Empty);
    }
    let bad = |c: char| match os {
        Os::Mac => c == '/' || c == ':',
        Os::Linux => c == '/',
        Os::Windows => c < ' ' || "\\/:*?\"<>|".contains(c),
    };
    if let Some(c) = name.chars().find(|&c| bad(c)) {
        return Some(Problem::BadChar(c));
    }
    if os == Os::Windows {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let numbered = |prefix: &str| {
            stem.strip_prefix(prefix)
                .is_some_and(|d| d.len() == 1 && matches!(d.as_bytes()[0], b'1'..=b'9'))
        };
        if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || numbered("COM")
            || numbered("LPT")
        {
            return Some(Problem::Reserved);
        }
        if name.ends_with(['.', ' ']) {
            return Some(Problem::TrailingDotSpace);
        }
    }
    let len = match os {
        Os::Windows => name.encode_utf16().count(),
        Os::Mac | Os::Linux => name.len(),
    };
    (len > MAX_NAME_LEN).then_some(Problem::TooLong)
}

/// The items to rename and what is already on disk around them.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    /// The listed items, in list order.
    pub items: Vec<Item>,
    /// Names on disk per parent folder, read once when the list is built.
    pub taken: HashMap<PathBuf, Vec<String>>,
}

/// One item's preview, at the same index as the item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The new name; `None` when the name does not change.
    pub new_name: Option<String>,
    /// What blocks this row, if anything.
    pub problem: Option<Problem>,
}

/// The whole list's preview and the footer's counts.
#[derive(Debug, Clone)]
pub struct Preview {
    /// One row per item.
    pub rows: Vec<Row>,
    /// Rows that will change and have no problem.
    pub changing: usize,
    /// Rows with a problem.
    pub problems: usize,
    /// Rows whose name stays as it is.
    pub unchanged: usize,
}

impl Preview {
    /// Whether Rename may run: something changes and nothing blocks it.
    pub fn can_rename(&self) -> bool {
        self.problems == 0 && self.changing > 0
    }
}

/// Runs `rules` over the batch in list order and checks every new name for `os`.
pub fn preview(batch: &Batch, rules: &[Rule], include_dirs: bool, os: Os) -> Preview {
    let items = &batch.items;

    // Each item's folder as a small index, so paths are hashed once per item.
    let mut folders: Vec<&Path> = Vec::new();
    let mut folder_ix: HashMap<&Path, usize> = HashMap::new();
    let dirs: Vec<usize> = items
        .iter()
        .map(|item| {
            let dir = parent(&item.path);
            *folder_ix.entry(dir).or_insert_with(|| {
                folders.push(dir);
                folders.len() - 1
            })
        })
        .collect();

    // New names. The counter only advances for items the rules apply to.
    let mut counter = 0;
    let new_names: Vec<Option<String>> = items
        .iter()
        .zip(&dirs)
        .map(|(item, &dir)| {
            let old = item.name.as_deref()?;
            if item.is_dir && !include_dirs {
                return None;
            }
            let folder = folders[dir]
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            let ctx = Ctx {
                counter,
                folder: &folder,
                modified_day: &item.modified_day,
            };
            counter += 1;
            let parts = apply_rules(rules, &split_name(old, item.is_dir), item.is_dir, &ctx);
            let built = join_name(&parts);
            let new = if os == Os::Mac { built } else { nfc(&built) };
            (new != old).then_some(new)
        })
        .collect();

    // Keys of every item's current and final name; how many rows share each final
    // name per folder; which current names in each folder belong to listed items.
    let old_keys: Vec<Option<String>> = items
        .iter()
        .map(|i| i.name.as_deref().map(|n| name_key(n, os)))
        .collect();
    let new_keys: Vec<Option<String>> = new_names
        .iter()
        .map(|n| n.as_deref().map(|n| name_key(n, os)))
        .collect();
    let mut finals: HashMap<(usize, &str), usize> = HashMap::new();
    let mut listed: HashSet<(usize, &str)> = HashSet::new();
    for (i, &dir) in dirs.iter().enumerate() {
        let Some(old) = old_keys[i].as_deref() else {
            continue;
        };
        let fin = new_keys[i].as_deref().unwrap_or(old);
        *finals.entry((dir, fin)).or_default() += 1;
        listed.insert((dir, old));
    }
    let changing_dirs: HashSet<&Path> = items
        .iter()
        .zip(&new_names)
        .filter(|(item, new)| item.is_dir && new.is_some())
        .map(|(item, _)| item.path.as_path())
        .collect();

    // Keys of names on disk that no listed item holds, per folder, built on demand.
    let mut others: HashMap<usize, HashSet<String>> = HashMap::new();

    let mut rows = Vec::with_capacity(items.len());
    for (i, new_name) in new_names.into_iter().enumerate() {
        let (Some(new), Some(key)) = (&new_name, new_keys[i].as_deref()) else {
            rows.push(Row {
                new_name: None,
                problem: None,
            });
            continue;
        };
        let dir = dirs[i];
        let problem = check_name(new, os)
            .or_else(|| {
                let mut up = items[i].path.ancestors().skip(1);
                (!changing_dirs.is_empty() && up.any(|a| changing_dirs.contains(a)))
                    .then_some(Problem::Nested)
            })
            .or_else(|| {
                let names = others.entry(dir).or_insert_with(|| {
                    let on_disk = batch.taken.get(folders[dir]).into_iter().flatten();
                    on_disk
                        .map(|n| name_key(n, os))
                        .filter(|k| !listed.contains(&(dir, k.as_str())))
                        .collect()
                });
                names.contains(key).then_some(Problem::Taken)
            })
            .or_else(|| (finals[&(dir, key)] > 1).then_some(Problem::Clash));
        rows.push(Row { new_name, problem });
    }

    let problems = rows.iter().filter(|r| r.problem.is_some()).count();
    let unchanged = rows.iter().filter(|r| r.new_name.is_none()).count();
    Preview {
        changing: rows.len() - problems - unchanged,
        rows,
        problems,
        unchanged,
    }
}

/// The folder holding `path`; empty for a root.
fn parent(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new(""))
}

#[cfg(test)]
mod tests {
    use super::super::rules::{CaseTo, ExtTo};
    use super::*;
    use std::time::SystemTime;

    fn item(path: &str, is_dir: bool) -> Item {
        let path = PathBuf::from(path);
        Item {
            name: path.file_name().map(|n| n.to_str().unwrap().to_string()),
            path,
            is_dir,
            len: 0,
            modified: SystemTime::UNIX_EPOCH,
            modified_day: "2026-11-01".into(),
        }
    }

    fn files(paths: &[&str]) -> Vec<Item> {
        paths.iter().map(|p| item(p, false)).collect()
    }

    /// `taken` for one folder: every name listed.
    fn on_disk(dir: &str, names: &[&str]) -> HashMap<PathBuf, Vec<String>> {
        HashMap::from([(
            PathBuf::from(dir),
            names.iter().map(|n| n.to_string()).collect(),
        )])
    }

    fn replace(find: &str, with: &str) -> Rule {
        Rule::Replace {
            find: find.into(),
            with: with.into(),
            match_case: false,
        }
    }

    fn add(text: &str) -> Rule {
        Rule::Add {
            text: text.into(),
            after: true,
        }
    }

    fn names(p: &Preview) -> Vec<Option<&str>> {
        p.rows.iter().map(|r| r.new_name.as_deref()).collect()
    }

    fn problems(p: &Preview) -> Vec<Option<Problem>> {
        p.rows.iter().map(|r| r.problem).collect()
    }

    #[test]
    fn counter_skips_items_rules_do_not_touch() {
        let batch = Batch {
            items: vec![
                item("/t/D", true),
                item("/t/a.txt", false),
                item("/t/E", true),
                item("/t/b.txt", false),
            ],
            taken: on_disk("/t", &["D", "a.txt", "E", "b.txt"]),
        };
        let p = preview(&batch, &[Rule::new_number()], false, Os::Linux);
        assert_eq!(
            names(&p),
            [None, Some("a-001.txt"), None, Some("b-002.txt")]
        );
        let p = preview(&batch, &[Rule::new_number()], true, Os::Linux);
        assert_eq!(
            names(&p),
            [
                Some("D-001"),
                Some("a-002.txt"),
                Some("E-003"),
                Some("b-004.txt")
            ]
        );
    }

    #[test]
    fn clash_between_two_items() {
        let batch = Batch {
            items: files(&["/t/a.txt", "/t/B.txt"]),
            taken: on_disk("/t", &["a.txt", "B.txt"]),
        };
        let p = preview(&batch, &[replace("a", "b")], true, Os::Mac);
        assert_eq!(names(&p), [Some("b.txt"), None]);
        assert_eq!(problems(&p), [Some(Problem::Clash), None]);
        assert!(!p.can_rename());
        // Linux tells b.txt from B.txt.
        let p = preview(&batch, &[replace("a", "b")], true, Os::Linux);
        assert_eq!(problems(&p), [None, None]);
        assert!(p.can_rename());
        // Two changing rows landing on one name are both marked.
        let batch = Batch {
            items: files(&["/t/a1.txt", "/t/a2.txt"]),
            taken: on_disk("/t", &["a1.txt", "a2.txt"]),
        };
        let p = preview(
            &batch,
            &[Rule::Template { text: "x".into() }],
            true,
            Os::Linux,
        );
        assert_eq!(problems(&p), [Some(Problem::Clash), Some(Problem::Clash)]);
        assert_eq!((p.changing, p.problems, p.unchanged), (0, 2, 0));
    }

    #[test]
    fn same_name_in_different_folders_is_fine() {
        let batch = Batch {
            items: files(&["/t/a.txt", "/u/a.txt"]),
            taken: HashMap::new(),
        };
        let p = preview(
            &batch,
            &[Rule::Template { text: "x".into() }],
            true,
            Os::Linux,
        );
        assert_eq!(problems(&p), [None, None]);
        assert_eq!(p.changing, 2);
    }

    #[test]
    fn taken_by_a_file_not_in_the_list() {
        let batch = Batch {
            items: files(&["/t/a.txt"]),
            taken: on_disk("/t", &["a.txt", "c.txt"]),
        };
        let p = preview(&batch, &[replace("a", "c")], true, Os::Linux);
        assert_eq!(names(&p), [Some("c.txt")]);
        assert_eq!(problems(&p), [Some(Problem::Taken)]);
        assert_eq!((p.changing, p.problems, p.unchanged), (0, 1, 0));
    }

    #[test]
    fn swap_and_chain_are_fine() {
        let taken = on_disk("/t", &["a.txt", "b.txt"]);
        let batch = Batch {
            items: files(&["/t/a.txt", "/t/b.txt"]),
            taken,
        };
        // a ↔ b
        let swap = [replace("a", "c"), replace("b", "a"), replace("c", "b")];
        let p = preview(&batch, &swap, true, Os::Mac);
        assert_eq!(names(&p), [Some("b.txt"), Some("a.txt")]);
        assert_eq!(problems(&p), [None, None]);
        assert!(p.can_rename());
        // a → b while b → c
        let chain = [replace("b", "c"), replace("a", "b")];
        let p = preview(&batch, &chain, true, Os::Mac);
        assert_eq!(names(&p), [Some("b.txt"), Some("c.txt")]);
        assert_eq!(problems(&p), [None, None]);
        assert_eq!(p.changing, 2);
    }

    #[test]
    fn case_only_change_is_not_a_clash() {
        let batch = Batch {
            items: files(&["/t/A.JPG"]),
            taken: on_disk("/t", &["A.JPG"]),
        };
        let ext = Rule::Extension {
            to: ExtTo::Lower,
            text: String::new(),
        };
        let p = preview(&batch, &[ext], true, Os::Mac);
        assert_eq!(names(&p), [Some("A.jpg")]);
        assert_eq!((p.changing, p.problems, p.unchanged), (1, 0, 0));
        assert!(p.can_rename());
    }

    #[test]
    fn nfd_on_disk_matches_nfc_new_name() {
        let nfd = "\u{0627}\u{0653}.txt";
        let batch = Batch {
            items: files(&["/t/b.txt"]),
            taken: on_disk("/t", &["b.txt", nfd]),
        };
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            let p = preview(&batch, &[replace("b", "\u{0622}")], true, os);
            assert_eq!(names(&p), [Some("\u{0622}.txt")]);
            assert_eq!(problems(&p), [Some(Problem::Taken)], "{os:?}");
        }
    }

    #[test]
    fn slash_from_a_rule_is_a_bad_char() {
        let batch = Batch {
            items: files(&["/t/a.txt"]),
            taken: on_disk("/t", &["a.txt"]),
        };
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            let p = preview(&batch, &[add("x/")], true, os);
            assert_eq!(problems(&p), [Some(Problem::BadChar('/'))], "{os:?}");
        }
        let colon = |os| preview(&batch, &[add("x:")], true, os).rows[0].problem;
        assert_eq!(colon(Os::Mac), Some(Problem::BadChar(':')));
        assert_eq!(colon(Os::Windows), Some(Problem::BadChar(':')));
        assert_eq!(colon(Os::Linux), None);
    }

    #[test]
    fn windows_rules() {
        assert_eq!(check_name("CON.txt", Os::Windows), Some(Problem::Reserved));
        assert_eq!(check_name("con", Os::Windows), Some(Problem::Reserved));
        assert_eq!(
            check_name("Lpt9.tar.gz", Os::Windows),
            Some(Problem::Reserved)
        );
        assert_eq!(check_name("COM10", Os::Windows), None);
        assert_eq!(check_name("CON.txt", Os::Linux), None);
        assert_eq!(
            check_name("name.", Os::Windows),
            Some(Problem::TrailingDotSpace)
        );
        assert_eq!(
            check_name("name ", Os::Windows),
            Some(Problem::TrailingDotSpace)
        );
        assert_eq!(check_name("name.", Os::Linux), None);
        assert_eq!(check_name(" . ", Os::Linux), Some(Problem::Empty));
        assert_eq!(check_name("", Os::Mac), Some(Problem::Empty));
        assert_eq!(check_name("a?b", Os::Windows), Some(Problem::BadChar('?')));
        assert_eq!(
            check_name("a\u{1}b", Os::Windows),
            Some(Problem::BadChar('\u{1}'))
        );
        assert_eq!(check_name("a?b", Os::Mac), None);
        assert_eq!(
            check_name(&"ب".repeat(128), Os::Linux),
            Some(Problem::TooLong)
        ); // 256 bytes
        assert_eq!(check_name(&"ب".repeat(128), Os::Windows), None); // 128 units
        assert_eq!(check_name(&"a".repeat(255), Os::Mac), None);
        assert_eq!(
            check_name(&"a".repeat(256), Os::Windows),
            Some(Problem::TooLong)
        );
    }

    #[test]
    fn nested_item_in_changing_folder_is_a_problem() {
        let batch = Batch {
            items: vec![item("/t/A", true), item("/t/A/x.txt", false)],
            taken: HashMap::from([
                (PathBuf::from("/t"), vec!["A".to_string()]),
                (PathBuf::from("/t/A"), vec!["x.txt".to_string()]),
            ]),
        };
        let p = preview(&batch, &[add("-1")], true, Os::Linux);
        assert_eq!(names(&p), [Some("A-1"), Some("x-1.txt")]);
        assert_eq!(problems(&p), [None, Some(Problem::Nested)]);
        let p = preview(&batch, &[add("-1")], false, Os::Linux);
        assert_eq!(names(&p), [None, Some("x-1.txt")]);
        assert_eq!(problems(&p), [None, None]);
        assert!(p.can_rename());
    }

    #[test]
    fn new_names_are_nfc_except_on_mac() {
        let batch = Batch {
            items: files(&["/t/a.txt"]),
            taken: on_disk("/t", &["a.txt"]),
        };
        let nfd = add("\u{0627}\u{0653}");
        let new = |os| {
            preview(&batch, std::slice::from_ref(&nfd), true, os).rows[0]
                .new_name
                .clone()
        };
        assert_eq!(new(Os::Linux).as_deref(), Some("a\u{0622}.txt"));
        assert_eq!(new(Os::Windows).as_deref(), Some("a\u{0622}.txt"));
        assert_eq!(new(Os::Mac).as_deref(), Some("a\u{0627}\u{0653}.txt"));
    }

    #[test]
    fn normalization_only_change_is_a_change() {
        let nfd = "\u{0627}\u{0653}.txt";
        let batch = Batch {
            items: files(&[&format!("/t/{nfd}")]),
            taken: on_disk("/t", &[nfd]),
        };
        // A rule that matches nothing still writes the name NFC on Linux.
        let p = preview(&batch, &[replace("zz", "y")], true, Os::Linux);
        assert_eq!(names(&p), [Some("\u{0622}.txt")]);
        assert_eq!((p.changing, p.problems), (1, 0));
        let p = preview(&batch, &[replace("zz", "y")], true, Os::Mac);
        assert_eq!(names(&p), [None]);
        assert_eq!(p.unchanged, 1);
    }

    #[test]
    fn non_utf8_item_is_unchanged() {
        let mut odd = item("/t/odd", false);
        odd.name = None;
        let batch = Batch {
            items: vec![odd, item("/t/a.txt", false)],
            taken: on_disk("/t", &["a.txt"]),
        };
        let p = preview(&batch, &[Rule::new_number()], true, Os::Linux);
        assert_eq!(
            p.rows[0],
            Row {
                new_name: None,
                problem: None
            }
        );
        // It takes no counter slot.
        assert_eq!(names(&p)[1], Some("a-001.txt"));
        assert_eq!((p.changing, p.problems, p.unchanged), (1, 0, 1));
    }

    #[test]
    fn footer_counts() {
        let mut items = files(&[
            "/t/a1.txt",
            "/t/a2.txt",
            "/t/a3.txt",
            "/t/a4.txt",
            "/t/a5.txt",
            "/t/a6.txt",
            "/t/a7.txt",
            "/t/a8.txt",
        ]);
        items.push(item("/t/D", true));
        let batch = Batch {
            items,
            taken: on_disk("/t", &["a1.txt", "a8.txt", "D", "a8-x.txt"]),
        };
        let p = preview(&batch, &[add("-x")], false, Os::Linux);
        assert_eq!(p.rows[7].problem, Some(Problem::Taken));
        assert_eq!((p.changing, p.problems, p.unchanged), (7, 1, 1));
        assert!(!p.can_rename());
    }

    #[test]
    fn empty_rules_change_nothing() {
        let batch = Batch {
            items: files(&["/t/a.txt"]),
            taken: on_disk("/t", &["a.txt"]),
        };
        let p = preview(&batch, &[add("")], true, Os::Linux);
        assert_eq!((p.changing, p.problems, p.unchanged), (0, 0, 1));
        assert!(!p.can_rename());
    }

    #[test]
    fn five_thousand_items() {
        let items: Vec<Item> = (0..5_000)
            .map(|i| item(&format!("/t/IMG_{i}.HEIC"), false))
            .collect();
        let names_on_disk: Vec<String> = items.iter().map(|i| i.name.clone().unwrap()).collect();
        let batch = Batch {
            items,
            taken: HashMap::from([(PathBuf::from("/t"), names_on_disk)]),
        };
        let rules = [
            replace("IMG_", "Trip-"),
            add("-x"),
            Rule::new_number(),
            Rule::Case { to: CaseTo::Kebab },
            Rule::Extension {
                to: ExtTo::Lower,
                text: String::new(),
            },
            Rule::Template {
                text: "{name}".into(),
            },
        ];
        let p = preview(&batch, &rules, true, Os::Mac);
        assert_eq!((p.changing, p.problems, p.unchanged), (5_000, 0, 0));
        assert_eq!(p.rows[0].new_name.as_deref(), Some("trip-0-x-001.heic"));
    }
}
