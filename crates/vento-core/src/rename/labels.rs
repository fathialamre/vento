//! Every user-facing text of the Rename window and its toasts.

use std::path::{Path, PathBuf};

use super::items::OrderBy;
use super::name::Os;
use super::preview::{Preview, Problem};
use super::rules::{CaseTo, ExtTo, Rule, unknown_tags};
use super::run::{RenameRecord, UndoOutcome};
use crate::i18n::errors::clauses;
use crate::i18n::{fill, tr, trn};

/// A path's last component, for texts that name an item.
fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The name on a rule's chip.
pub fn chip_title(rule: &Rule) -> String {
    match rule {
        Rule::Replace { .. } => tr("Replace"),
        Rule::Add { .. } => tr("Add"),
        Rule::Number { .. } => tr("Number"),
        Rule::Case { .. } => tr("Case"),
        Rule::Extension { .. } => tr("Extension"),
        Rule::Template { .. } => tr("Template"),
    }
}

/// `before`/`after`, as the Add and Number chips say where the text goes.
fn side(after: bool) -> String {
    if after { tr("after") } else { tr("before") }
}

/// A chip's one-line summary of what its rule is set to.
pub fn chip_summary(rule: &Rule) -> String {
    if rule.is_empty() {
        return tr("empty");
    }
    match rule {
        Rule::Replace { find, with, .. } => {
            fill(&tr("{find} → {with}"), &[("find", find), ("with", with)])
        }
        Rule::Add { text, after } => fill(
            &tr("{side} · {text}"),
            &[("side", &side(*after)), ("text", text)],
        ),
        Rule::Number {
            after,
            start,
            digits,
            ..
        } => {
            let number = format!("{start:0width$}", width = usize::from(*digits));
            fill(
                &tr("{side} · {text}"),
                &[("side", &side(*after)), ("text", &number)],
            )
        }
        Rule::Case { to } => match to {
            CaseTo::Lower => tr("lowercase"),
            CaseTo::Upper => tr("UPPERCASE"),
            CaseTo::Title => tr("Title Case"),
            CaseTo::Kebab => tr("kebab-case"),
            CaseTo::Snake => tr("snake_case"),
        },
        Rule::Extension { to, text } => match to {
            ExtTo::Lower => tr("lowercase"),
            ExtTo::Upper => tr("UPPERCASE"),
            ExtTo::Replace => fill(&tr("→ {ext}"), &[("ext", text)]),
            ExtTo::Remove => tr("remove"),
        },
        Rule::Template { text } => match unknown_tags(text).first() {
            Some(tag) => fill(&tr("Unknown {tag}"), &[("tag", &format!("{{{tag}}}"))]),
            None => text.clone(),
        },
    }
}

/// Why a row's new name cannot be used.
pub fn problem_text(p: &Problem, os: Os) -> String {
    match p {
        Problem::Clash => tr("Same name as another item"),
        Problem::Taken => tr("Already in this folder"),
        Problem::Empty => tr("Name can't be empty"),
        Problem::BadChar(c) => {
            let shown = if c.is_control() {
                format!("U+{:04X}", u32::from(*c))
            } else {
                c.to_string()
            };
            let template = match os {
                Os::Mac => tr("Can't contain {char} on macOS"),
                Os::Windows => tr("Can't contain {char} on Windows"),
                Os::Linux => tr("Can't contain {char} on Linux"),
            };
            fill(&template, &[("char", &shown)])
        }
        Problem::Reserved => tr("Windows reserves this name"),
        Problem::TrailingDotSpace => tr("Can't end with a dot or space"),
        Problem::TooLong => tr("Name is too long"),
        Problem::Nested => tr("Inside a folder that's also being renamed"),
    }
}

/// The window's title.
pub fn title_text(items: usize) -> String {
    trn("Rename {n} item", "Rename {n} items", items as u64)
}

/// The footer as `(before, red, after)`; the window draws `red` in the danger colour.
pub fn footer_text(p: &Preview, items: usize) -> (String, String, String) {
    let changing = trn("{n} will change", "{n} will change", p.changing as u64);
    let unchanged = if p.unchanged > 0 {
        let n = trn("{n} unchanged", "{n} unchanged", p.unchanged as u64);
        fill(&tr(" · {text}"), &[("text", &n)])
    } else {
        String::new()
    };
    if p.problems == 0 {
        let head = fill(
            &tr("{items} · {changes}"),
            &[
                ("items", &trn("{n} item", "{n} items", items as u64)),
                ("changes", &changing),
            ],
        );
        return (head + &unchanged, String::new(), String::new());
    }
    (
        fill(&tr("{text} · "), &[("text", &changing)]),
        trn("{n} problem", "{n} problems", p.problems as u64),
        unchanged,
    )
}

/// The Order menu's entries.
pub fn order_label(by: OrderBy) -> String {
    match by {
        OrderBy::Name => tr("Name"),
        OrderBy::Modified => tr("Date"),
        OrderBy::Size => tr("Size"),
    }
}

/// The toast after a finished rename.
pub fn done_text(record: &RenameRecord) -> String {
    match record.items.as_slice() {
        [(_, new, _)] => fill(&tr("Renamed {name}"), &[("name", &file_name(new))]),
        items => trn("Renamed {n} item", "Renamed {n} items", items.len() as u64),
    }
}

/// The toast when a rename failed and was reversed.
pub fn failed_text(path: &Path, reason: &str) -> String {
    fill(
        &tr("Couldn't rename {name}: {reason}. Nothing was changed."),
        &[("name", &file_name(path)), ("reason", reason)],
    )
}

/// The toast when the disk no longer matches the preview.
pub fn changed_text() -> String {
    tr("Something changed in the folder. Check the list and try again.")
}

/// The toast when more items were dropped than a rename takes.
pub fn too_many_text() -> String {
    tr("Too many items to rename at once (5,000 at most)")
}

/// The toast after an Undo.
pub fn undo_text(o: &UndoOutcome) -> String {
    let mut text = match &o.failed {
        Some((path, reason)) => fill(
            &tr("Couldn't put back {name}: {reason}. Nothing was changed."),
            &[("name", &file_name(path)), ("reason", reason)],
        ),
        None => {
            let put_back = trn("Put back {n} name", "Put back {n} names", o.put_back as u64);
            if o.skipped == 0 {
                put_back
            } else {
                let left = trn(
                    "{n} item changed since and was left as it is.",
                    "{n} items changed since and were left as they are.",
                    o.skipped as u64,
                );
                clauses(&[put_back, left])
            }
        }
    };
    if !o.stuck.is_empty() {
        text.push('\n');
        text.push_str(&stuck_text(&o.stuck));
    }
    text
}

/// One line per item a reversal could not put back: where it is now, and what it should be.
pub fn stuck_text(stuck: &[(PathBuf, PathBuf)]) -> String {
    let template = tr("{now} should be {old}");
    stuck
        .iter()
        .map(|(now, old)| {
            fill(
                &template,
                &[("now", &file_name(now)), ("old", &file_name(old))],
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rename::run::ItemPrint;

    fn rule_replace(find: &str, with: &str) -> Rule {
        Rule::Replace {
            find: find.into(),
            with: with.into(),
            match_case: false,
        }
    }

    #[test]
    fn chip_titles() {
        assert_eq!(chip_title(&rule_replace("a", "b")), "Replace");
        assert_eq!(chip_title(&Rule::new_number()), "Number");
        assert_eq!(chip_title(&Rule::Template { text: "x".into() }), "Template");
    }

    #[test]
    fn chip_summaries() {
        assert_eq!(chip_summary(&rule_replace("IMG_", "Trip-")), "IMG_ → Trip-");
        assert_eq!(chip_summary(&rule_replace("find", "with")), "find → with");
        assert_eq!(chip_summary(&Rule::new_number()), "after · 001");
        let before = Rule::Number {
            after: false,
            start: 1,
            digits: 3,
            separator: "-".into(),
        };
        assert_eq!(chip_summary(&before), "before · 001");
        let add = |text: &str, after| Rule::Add {
            text: text.into(),
            after,
        };
        assert_eq!(chip_summary(&add("old-", false)), "before · old-");
        assert_eq!(chip_summary(&add("-v2", true)), "after · -v2");
        let case = |to| chip_summary(&Rule::Case { to });
        assert_eq!(case(CaseTo::Kebab), "kebab-case");
        assert_eq!(case(CaseTo::Snake), "snake_case");
        assert_eq!(case(CaseTo::Lower), "lowercase");
        assert_eq!(case(CaseTo::Upper), "UPPERCASE");
        assert_eq!(case(CaseTo::Title), "Title Case");
        let ext = |to, text: &str| {
            chip_summary(&Rule::Extension {
                to,
                text: text.into(),
            })
        };
        assert_eq!(ext(ExtTo::Lower, ""), "lowercase");
        assert_eq!(ext(ExtTo::Upper, ""), "UPPERCASE");
        assert_eq!(ext(ExtTo::Replace, "jpeg"), "→ jpeg");
        assert_eq!(ext(ExtTo::Remove, ""), "remove");
        assert_eq!(
            chip_summary(&Rule::Template {
                text: "{name}-{n}".into()
            }),
            "{name}-{n}"
        );
        assert_eq!(
            chip_summary(&Rule::Template {
                text: "a{nope}{bad}".into()
            }),
            "Unknown {nope}"
        );
    }

    #[test]
    fn empty_rules_say_empty() {
        assert_eq!(chip_summary(&rule_replace("", "x")), "empty");
        assert_eq!(chip_summary(&Rule::Template { text: "".into() }), "empty");
        assert_eq!(
            chip_summary(&Rule::Extension {
                to: ExtTo::Replace,
                text: "".into()
            }),
            "empty"
        );
    }

    #[test]
    fn problems() {
        assert_eq!(
            problem_text(&Problem::Clash, Os::Mac),
            "Same name as another item"
        );
        assert_eq!(
            problem_text(&Problem::Taken, Os::Mac),
            "Already in this folder"
        );
        assert_eq!(
            problem_text(&Problem::Empty, Os::Mac),
            "Name can't be empty"
        );
        assert_eq!(
            problem_text(&Problem::BadChar(':'), Os::Mac),
            "Can't contain : on macOS"
        );
        assert_eq!(
            problem_text(&Problem::BadChar('*'), Os::Windows),
            "Can't contain * on Windows"
        );
        assert_eq!(
            problem_text(&Problem::BadChar('/'), Os::Linux),
            "Can't contain / on Linux"
        );
        assert_eq!(
            problem_text(&Problem::BadChar('\u{7}'), Os::Windows),
            "Can't contain U+0007 on Windows"
        );
        assert_eq!(
            problem_text(&Problem::Reserved, Os::Windows),
            "Windows reserves this name"
        );
        assert_eq!(
            problem_text(&Problem::TrailingDotSpace, Os::Windows),
            "Can't end with a dot or space"
        );
        assert_eq!(problem_text(&Problem::TooLong, Os::Mac), "Name is too long");
        assert_eq!(
            problem_text(&Problem::Nested, Os::Mac),
            "Inside a folder that's also being renamed"
        );
    }

    #[test]
    fn titles() {
        assert_eq!(title_text(9), "Rename 9 items");
        assert_eq!(title_text(1), "Rename 1 item");
    }

    fn preview(changing: usize, problems: usize, unchanged: usize) -> Preview {
        Preview {
            rows: Vec::new(),
            changing,
            problems,
            unchanged,
        }
    }

    #[test]
    fn footers() {
        assert_eq!(
            footer_text(&preview(9, 0, 0), 9),
            (
                "9 items · 9 will change".into(),
                String::new(),
                String::new()
            )
        );
        assert_eq!(
            footer_text(&preview(7, 0, 2), 9),
            (
                "9 items · 7 will change · 2 unchanged".into(),
                String::new(),
                String::new()
            )
        );
        assert_eq!(
            footer_text(&preview(7, 1, 2), 10),
            (
                "7 will change · ".into(),
                "1 problem".into(),
                " · 2 unchanged".into()
            )
        );
        assert_eq!(footer_text(&preview(1, 2, 0), 3).1, "2 problems");
    }

    #[test]
    fn order_labels() {
        assert_eq!(order_label(OrderBy::Name), "Name");
        assert_eq!(order_label(OrderBy::Modified), "Date");
        assert_eq!(order_label(OrderBy::Size), "Size");
    }

    fn print() -> ItemPrint {
        ItemPrint {
            id: Default::default(),
            len: 0,
            is_dir: false,
        }
    }

    #[test]
    fn done_texts() {
        let one = RenameRecord {
            items: vec![("/t/a".into(), "/t/Trip-1.jpg".into(), print())],
        };
        assert_eq!(done_text(&one), "Renamed Trip-1.jpg");
        let two = RenameRecord {
            items: vec![
                ("/t/a".into(), "/t/x".into(), print()),
                ("/t/b".into(), "/t/y".into(), print()),
            ],
        };
        assert_eq!(done_text(&two), "Renamed 2 items");
    }

    #[test]
    fn failure_and_state_toasts() {
        assert_eq!(
            failed_text(Path::new("/t/a.txt"), "permission denied"),
            "Couldn't rename a.txt: permission denied. Nothing was changed."
        );
        assert_eq!(
            changed_text(),
            "Something changed in the folder. Check the list and try again."
        );
        assert_eq!(
            too_many_text(),
            "Too many items to rename at once (5,000 at most)"
        );
    }

    #[test]
    fn undo_texts() {
        let ok = |put_back, skipped| UndoOutcome {
            put_back,
            skipped,
            ..UndoOutcome::default()
        };
        assert_eq!(undo_text(&ok(1, 0)), "Put back 1 name");
        assert_eq!(undo_text(&ok(3, 0)), "Put back 3 names");
        assert_eq!(
            undo_text(&ok(3, 1)),
            "Put back 3 names; 1 item changed since and was left as it is."
        );
        assert_eq!(
            undo_text(&ok(1, 2)),
            "Put back 1 name; 2 items changed since and were left as they are."
        );
        let failed = UndoOutcome {
            skipped: 1,
            failed: Some(("/t/b".into(), "permission denied".into())),
            ..UndoOutcome::default()
        };
        assert_eq!(
            undo_text(&failed),
            "Couldn't put back b: permission denied. Nothing was changed."
        );
    }

    #[test]
    fn stuck_lines() {
        let stuck = vec![
            ("/t/.vento-tmp-1".into(), "/t/a".into()),
            ("/t/final".into(), "/t/b".into()),
        ];
        assert_eq!(
            stuck_text(&stuck),
            ".vento-tmp-1 should be a\nfinal should be b"
        );
        let failed = UndoOutcome {
            failed: Some(("/t/b".into(), "x".into())),
            stuck,
            ..UndoOutcome::default()
        };
        assert!(undo_text(&failed).ends_with("\n.vento-tmp-1 should be a\nfinal should be b"));
    }
}
