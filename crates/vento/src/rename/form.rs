//! What the Rename window shows, made from the core's rules and preview:
//! a rule as its popover's form and back, the rule a tool adds, and the
//! list's rows.

use vento_core::rename::{Batch, CaseTo, ExtTo, Os, Preview, Rule, problem_text};

use crate::ui::{RenameRow, RuleForm};

/// The rule kinds, in toolbar order (`RuleForm::kind`).
const REPLACE: i32 = 0;
const ADD: i32 = 1;
const NUMBER: i32 = 2;
const CASE: i32 = 3;
const EXTENSION: i32 = 4;
const TEMPLATE: i32 = 5;

/// The Case popover's choices, in its order (`RuleForm::choice`).
const CASES: [CaseTo; 5] = [
    CaseTo::Lower,
    CaseTo::Upper,
    CaseTo::Title,
    CaseTo::Kebab,
    CaseTo::Snake,
];
/// The Extension popover's choices, in its order.
const EXTENSIONS: [ExtTo; 4] = [ExtTo::Lower, ExtTo::Upper, ExtTo::Replace, ExtTo::Remove];

/// The popover's form for `rule`; fields the kind does not use are left
/// at their defaults.
pub fn rule_to_form(rule: &Rule) -> RuleForm {
    match rule {
        Rule::Replace {
            find,
            with,
            match_case,
        } => RuleForm {
            kind: REPLACE,
            find: find.into(),
            with: with.into(),
            match_case: *match_case,
            ..RuleForm::default()
        },
        Rule::Add { text, after } => RuleForm {
            kind: ADD,
            text: text.into(),
            after: *after,
            ..RuleForm::default()
        },
        Rule::Number {
            after,
            start,
            digits,
            separator,
        } => RuleForm {
            kind: NUMBER,
            after: *after,
            start: i32::try_from(*start).unwrap_or(i32::MAX),
            digits: i32::from(*digits),
            separator: separator.into(),
            ..RuleForm::default()
        },
        Rule::Case { to } => RuleForm {
            kind: CASE,
            choice: CASES.iter().position(|c| c == to).unwrap_or(0) as i32,
            ..RuleForm::default()
        },
        Rule::Extension { to, text } => RuleForm {
            kind: EXTENSION,
            choice: EXTENSIONS.iter().position(|e| e == to).unwrap_or(0) as i32,
            text: text.into(),
            ..RuleForm::default()
        },
        Rule::Template { text } => RuleForm {
            kind: TEMPLATE,
            text: text.into(),
            ..RuleForm::default()
        },
    }
}

/// The rule the popover's form describes. Digits are kept to 1–6 and Start
/// to 0 or more, whatever the window sent; an Extension's new text loses
/// its leading dots.
pub fn form_to_rule(form: &RuleForm) -> Rule {
    let pick = |n: usize| usize::try_from(form.choice).unwrap_or(0).min(n - 1);
    match form.kind {
        REPLACE => Rule::Replace {
            find: form.find.to_string(),
            with: form.with.to_string(),
            match_case: form.match_case,
        },
        ADD => Rule::Add {
            text: form.text.to_string(),
            after: form.after,
        },
        NUMBER => Rule::Number {
            after: form.after,
            start: u32::try_from(form.start).unwrap_or(0),
            digits: form.digits.clamp(1, 6) as u8,
            separator: form.separator.to_string(),
        },
        CASE => Rule::Case {
            to: CASES[pick(CASES.len())],
        },
        EXTENSION => Rule::Extension {
            to: EXTENSIONS[pick(EXTENSIONS.len())],
            text: form.text.trim_start_matches('.').to_string(),
        },
        _ => Rule::Template {
            text: form.text.to_string(),
        },
    }
}

/// The rule a tool adds, by its kind; an unknown kind adds a Template.
pub fn default_rule(kind: i32) -> Rule {
    match kind {
        REPLACE => Rule::Replace {
            find: String::new(),
            with: String::new(),
            match_case: false,
        },
        ADD => Rule::Add {
            text: String::new(),
            after: false,
        },
        NUMBER => Rule::new_number(),
        CASE => Rule::Case { to: CaseTo::Lower },
        EXTENSION => Rule::Extension {
            to: ExtTo::Lower,
            text: String::new(),
        },
        _ => Rule::Template {
            text: String::new(),
        },
    }
}

/// The list's rows: each item's name, the name the rules give it, and why
/// that cannot be used. A name that is not valid UTF-8 is shown as best it
/// can be and never changes.
pub fn rows_of(batch: &Batch, p: &Preview, os: Os) -> Vec<RenameRow> {
    batch
        .items
        .iter()
        .zip(&p.rows)
        .map(|(item, row)| {
            let old = item.name.clone().unwrap_or_else(|| {
                item.path
                    .file_name()
                    .unwrap_or(item.path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            });
            let new = row.new_name.as_deref().unwrap_or(&old);
            RenameRow {
                is_dir: item.is_dir,
                new: new.into(),
                changed: row.new_name.is_some(),
                problem: row
                    .problem
                    .as_ref()
                    .map(|problem| problem_text(problem, os))
                    .unwrap_or_default()
                    .into(),
                old: old.into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use vento_core::rename::{Item, preview};

    use super::*;

    fn item(path: &str) -> Item {
        let path = PathBuf::from(path);
        Item {
            name: path.file_name().map(|n| n.to_str().unwrap().to_string()),
            path,
            is_dir: false,
            len: 0,
            modified: SystemTime::UNIX_EPOCH,
            modified_day: "1970-01-01".into(),
        }
    }

    #[test]
    fn form_roundtrip_every_kind() {
        for k in 0..6 {
            let r = default_rule(k);
            assert_eq!(form_to_rule(&rule_to_form(&r)), r, "kind {k}");
        }
        let edited = [
            Rule::Replace {
                find: "IMG_".into(),
                with: "Trip-".into(),
                match_case: true,
            },
            Rule::Add {
                text: "x".into(),
                after: true,
            },
            Rule::Number {
                after: false,
                start: 7,
                digits: 2,
                separator: "".into(),
            },
            Rule::Case { to: CaseTo::Snake },
            Rule::Extension {
                to: ExtTo::Replace,
                text: "jpg".into(),
            },
            Rule::Extension {
                to: ExtTo::Remove,
                text: String::new(),
            },
            Rule::Template {
                text: "{name}-{n:3}".into(),
            },
        ];
        for r in edited {
            assert_eq!(form_to_rule(&rule_to_form(&r)), r);
        }
    }

    #[test]
    fn digits_from_form_clamp() {
        let mut form = rule_to_form(&Rule::new_number());
        form.digits = 0;
        assert!(matches!(
            form_to_rule(&form),
            Rule::Number { digits: 1, .. }
        ));
        form.digits = 9;
        assert!(matches!(
            form_to_rule(&form),
            Rule::Number { digits: 6, .. }
        ));
        form.start = -5;
        assert!(matches!(form_to_rule(&form), Rule::Number { start: 0, .. }));
    }

    #[test]
    fn extension_text_loses_leading_dots() {
        let mut form = rule_to_form(&default_rule(EXTENSION));
        form.choice = 2;
        form.text = "..jpg".into();
        assert_eq!(
            form_to_rule(&form),
            Rule::Extension {
                to: ExtTo::Replace,
                text: "jpg".into()
            }
        );
    }

    #[test]
    fn a_tool_adds_its_kind() {
        assert!(matches!(default_rule(REPLACE), Rule::Replace { .. }));
        assert!(matches!(default_rule(ADD), Rule::Add { .. }));
        assert_eq!(default_rule(NUMBER), Rule::new_number());
        assert!(matches!(default_rule(CASE), Rule::Case { .. }));
        assert!(matches!(default_rule(EXTENSION), Rule::Extension { .. }));
        assert!(matches!(default_rule(TEMPLATE), Rule::Template { .. }));
    }

    #[test]
    fn rows_show_problem_text_and_unchanged_names() {
        let batch = Batch {
            items: vec![item("/d/a-1.txt"), item("/d/a_1.txt")],
            taken: Default::default(),
        };
        let rules = [Rule::Replace {
            find: "-".into(),
            with: "_".into(),
            match_case: false,
        }];
        let p = preview(&batch, &rules, true, Os::Linux);
        let rows = rows_of(&batch, &p, Os::Linux);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].old, "a-1.txt");
        assert_eq!(rows[0].new, "a_1.txt");
        assert!(rows[0].changed);
        assert_eq!(rows[0].problem, "Same name as another item");
        assert!(!rows[1].changed);
        assert_eq!(rows[1].new, rows[1].old);
        assert_eq!(rows[1].problem, "");
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_stays_as_it_is() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let mut odd = item("/d/x");
        odd.path = PathBuf::from("/d").join(OsStr::from_bytes(b"a\xffb.txt"));
        odd.name = None;
        let batch = Batch {
            items: vec![odd],
            taken: Default::default(),
        };
        let rules = [Rule::Case { to: CaseTo::Upper }];
        let p = preview(&batch, &rules, true, Os::Linux);
        let rows = rows_of(&batch, &p, Os::Linux);
        assert_eq!(rows[0].old, "a\u{fffd}b.txt");
        assert_eq!(rows[0].new, rows[0].old);
        assert!(!rows[0].changed);
        assert_eq!(rows[0].problem, "");
    }
}
