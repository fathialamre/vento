//! The six rename rules, applied in order to a name, and the rule history behind Undo.

use serde::{Deserialize, Serialize};

use super::name::{NameParts, nfc};

/// Most entries the rule history keeps.
const HISTORY_CAP: usize = 100;

/// The letter case a [`Rule::Case`] rewrites the stem to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseTo {
    /// all lower case
    Lower,
    /// ALL UPPER CASE
    Upper,
    /// Each Word Capitalised
    Title,
    /// words-joined-by-hyphens
    Kebab,
    /// words_joined_by_underscores
    Snake,
}

/// What a [`Rule::Extension`] does to the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtTo {
    /// lower-case it
    Lower,
    /// upper-case it
    Upper,
    /// swap it for the rule's text
    Replace,
    /// drop it
    Remove,
}

/// One step of a rename: rules run top to bottom, each seeing the previous result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    /// Replaces every occurrence of `find` in the stem; a lone `*` replaces
    /// the whole stem.
    Replace {
        find: String,
        with: String,
        match_case: bool,
    },
    /// Puts `text` before or after the stem.
    Add { text: String, after: bool },
    /// Adds a running number, `start` for the first item.
    Number {
        after: bool,
        start: u32,
        digits: u8,
        separator: String,
    },
    /// Rewrites the stem's letter case.
    Case { to: CaseTo },
    /// Changes the extension; folders are left alone.
    Extension { to: ExtTo, text: String },
    /// Builds the whole stem from `text` and its `{…}` tags.
    Template { text: String },
}

impl Rule {
    /// A Number rule with its defaults: after the name, from 1, three digits, `-`.
    pub fn new_number() -> Rule {
        Rule::Number {
            after: true,
            start: 1,
            digits: 3,
            separator: "-".into(),
        }
    }

    /// Whether the rule has nothing to act on yet (and so changes no name).
    pub fn is_empty(&self) -> bool {
        match self {
            Rule::Replace { find, .. } => find.is_empty(),
            Rule::Add { text, .. } | Rule::Template { text } => text.is_empty(),
            Rule::Extension { to, text } => *to == ExtTo::Replace && text.is_empty(),
            Rule::Number { .. } | Rule::Case { .. } => false,
        }
    }
}

/// What a rule may need to know about the item besides its name.
pub struct Ctx<'a> {
    /// The item's 0-based place among the items the rules apply to.
    pub counter: usize,
    /// The name of the folder holding the item.
    pub folder: &'a str,
    /// The item's modification day, as `YYYY-MM-DD`.
    pub modified_day: &'a str,
}

/// Runs `rules` in order over `parts`. Every rule acts on the stem, except
/// Extension, which acts on the extension and does nothing for folders.
pub fn apply_rules(rules: &[Rule], parts: &NameParts, is_dir: bool, ctx: &Ctx) -> NameParts {
    let mut out = parts.clone();
    for rule in rules.iter().filter(|r| !r.is_empty()) {
        match rule {
            Rule::Replace {
                find,
                with,
                match_case,
            } => out.stem = replace(&out.stem, find, with, *match_case),
            Rule::Add { text, after } => {
                out.stem = if *after {
                    format!("{}{text}", out.stem)
                } else {
                    format!("{text}{}", out.stem)
                }
            }
            Rule::Number {
                after,
                start,
                digits,
                separator,
            } => {
                let n = u64::from(*start) + ctx.counter as u64;
                let n = format!("{n:0>width$}", width = usize::from(*digits));
                out.stem = if *after {
                    format!("{}{separator}{n}", out.stem)
                } else {
                    format!("{n}{separator}{}", out.stem)
                };
            }
            Rule::Case { to } => out.stem = restyle(&out.stem, *to),
            Rule::Extension { to, text } => {
                if !is_dir && out.ext.is_some() {
                    out.ext = match to {
                        ExtTo::Lower => out.ext.map(|e| e.to_lowercase()),
                        ExtTo::Upper => out.ext.map(|e| e.to_uppercase()),
                        ExtTo::Replace => Some(text.clone()),
                        ExtTo::Remove => None,
                    };
                }
            }
            Rule::Template { text } => out.stem = expand(text, &out.stem, ctx),
        }
    }
    out
}

/// Replaces every non-overlapping occurrence of `find`, scanning left to right.
/// A lone `*` stands for the whole stem.
fn replace(stem: &str, find: &str, with: &str, match_case: bool) -> String {
    if find == "*" {
        return with.to_string();
    }
    if match_case {
        return stem.replace(find, with);
    }
    let want = find.to_lowercase();
    let len = find.chars().count();
    let chars: Vec<char> = stem.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let end = i + len;
        if end <= chars.len() && chars[i..end].iter().collect::<String>().to_lowercase() == want {
            out.push_str(with);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Rewrites `stem` in the given letter case.
fn restyle(stem: &str, to: CaseTo) -> String {
    match to {
        CaseTo::Lower => stem.to_lowercase(),
        CaseTo::Upper => stem.to_uppercase(),
        CaseTo::Title => stem
            .split(' ')
            .map(title_word)
            .collect::<Vec<_>>()
            .join(" "),
        CaseTo::Kebab => words(stem).join("-"),
        CaseTo::Snake => words(stem).join("_"),
    }
}

/// Upper-cases the first cased letter of `word` and lower-cases the rest.
fn title_word(word: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    for c in word.chars() {
        if !done && (c.is_lowercase() || c.is_uppercase()) {
            out.extend(c.to_uppercase());
            done = true;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Splits on space, `_`, `-`, `.` and lower-to-upper boundaries; lower-cases, drops empties.
fn words(stem: &str) -> Vec<String> {
    let mut words = vec![String::new()];
    let mut prev_lower = false;
    for c in stem.chars() {
        if matches!(c, ' ' | '_' | '-' | '.') {
            words.push(String::new());
            prev_lower = false;
            continue;
        }
        if prev_lower && c.is_uppercase() {
            words.push(String::new());
        }
        prev_lower = c.is_lowercase();
        if let Some(w) = words.last_mut() {
            w.extend(c.to_lowercase());
        }
    }
    words.retain(|w| !w.is_empty());
    words
}

/// What a `{…}` tag stands for, or `None` for a tag that is not known.
fn tag_value(tag: &str, stem: &str, ctx: &Ctx) -> Option<String> {
    match tag {
        "name" => Some(stem.to_string()),
        "n" => Some((ctx.counter + 1).to_string()),
        "folder" => Some(ctx.folder.to_string()),
        "modified" => Some(ctx.modified_day.to_string()),
        _ => {
            let k: usize = tag.strip_prefix("n:")?.parse().ok()?;
            (1..=6)
                .contains(&k)
                .then(|| format!("{:0>k$}", ctx.counter + 1))
        }
    }
}

/// Expands the tags of `template`; unknown tags stay as typed.
fn expand(template: &str, stem: &str, ctx: &Ctx) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        match rest.find('}') {
            Some(close) => {
                let tag = &rest[1..close];
                match tag_value(tag, stem, ctx) {
                    Some(v) => out.push_str(&v),
                    None => out.push_str(&rest[..=close]),
                }
                rest = &rest[close + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    nfc(&out)
}

/// The `{…}` tags in `template` that are not known, without their braces.
pub fn unknown_tags(template: &str) -> Vec<String> {
    let dummy = Ctx {
        counter: 0,
        folder: "",
        modified_day: "",
    };
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        rest = &rest[open..];
        let Some(close) = rest.find('}') else { break };
        let tag = &rest[1..close];
        if tag_value(tag, "", &dummy).is_none() {
            found.push(tag.to_string());
        }
        rest = &rest[close + 1..];
    }
    found
}

/// The rule lists before each change, so the window's Undo can step back.
#[derive(Debug, Default)]
pub struct RuleHistory {
    stack: Vec<Vec<Rule>>,
}

impl RuleHistory {
    /// Remembers the rules as they were before a change; the oldest entry goes past 100.
    pub fn record(&mut self, before: Vec<Rule>) {
        self.stack.push(before);
        if self.stack.len() > HISTORY_CAP {
            self.stack.remove(0);
        }
    }

    /// Takes back the latest entry, or `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Option<Vec<Rule>> {
        self.stack.pop()
    }

    /// Forgets every entry.
    pub fn clear(&mut self) {
        self.stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::super::name::{join_name, split_name};
    use super::*;

    fn run(rules: &[Rule], name: &str, counter: usize) -> String {
        let ctx = Ctx {
            counter,
            folder: "Trip",
            modified_day: "2026-10-14",
        };
        join_name(&apply_rules(rules, &split_name(name, false), false, &ctx))
    }

    #[test]
    fn replace_every_occurrence_case_insensitive_by_default() {
        let r = Rule::Replace {
            find: "img".into(),
            with: "Trip-".into(),
            match_case: false,
        };
        assert_eq!(run(&[r], "IMG_img.JPG", 0), "Trip-_Trip-.JPG");
        let r = Rule::Replace {
            find: "img".into(),
            with: "".into(),
            match_case: true,
        };
        assert_eq!(run(&[r], "IMG_img.JPG", 0), "IMG_.JPG");
    }

    #[test]
    fn replace_star_swaps_the_whole_stem() {
        let r = Rule::Replace {
            find: "*".into(),
            with: "Trip".into(),
            match_case: false,
        };
        assert_eq!(run(std::slice::from_ref(&r), "IMG_001.jpg", 0), "Trip.jpg");
        assert_eq!(run(&[r], "notes", 0), "Trip");
        let r = Rule::Replace {
            find: "a*b".into(),
            with: "x".into(),
            match_case: true,
        };
        assert_eq!(run(&[r], "a*b c.txt", 0), "x c.txt");
    }

    #[test]
    fn replace_never_touches_the_extension() {
        let r = Rule::Replace {
            find: "jpg".into(),
            with: "x".into(),
            match_case: false,
        };
        assert_eq!(run(&[r], "jpg.jpg", 0), "x.jpg");
    }

    #[test]
    fn add_before_and_after() {
        let add = |text: &str, after| Rule::Add {
            text: text.into(),
            after,
        };
        assert_eq!(run(&[add("old-", false)], "a.txt", 0), "old-a.txt");
        assert_eq!(run(&[add("-v2", true)], "a.txt", 0), "a-v2.txt");
    }

    #[test]
    fn number_pads_and_overflows() {
        let n = Rule::new_number();
        assert_eq!(run(std::slice::from_ref(&n), "a.jpg", 0), "a-001.jpg");
        let n = Rule::Number {
            after: false,
            start: 998,
            digits: 3,
            separator: "_".into(),
        };
        assert_eq!(run(&[n], "a.jpg", 2), "1000_a.jpg");
    }

    #[test]
    fn case_styles() {
        let c = |to| Rule::Case { to };
        assert_eq!(
            run(&[c(CaseTo::Kebab)], "MyFile name_v2.TXT", 0),
            "my-file-name-v2.TXT"
        );
        assert_eq!(run(&[c(CaseTo::Snake)], "MyFile name", 0), "my_file_name");
        assert_eq!(run(&[c(CaseTo::Title)], "hello world", 0), "Hello World");
        assert_eq!(run(&[c(CaseTo::Upper)], "صورة a", 0), "صورة A");
    }

    #[test]
    fn extension_rule_and_folders() {
        let e = |to, t: &str| Rule::Extension { to, text: t.into() };
        assert_eq!(run(&[e(ExtTo::Lower, "")], "A.JPG", 0), "A.jpg");
        assert_eq!(run(&[e(ExtTo::Replace, "jpeg")], "a.jpg", 0), "a.jpeg");
        assert_eq!(run(&[e(ExtTo::Remove, "")], "a.jpg", 0), "a");
        let ctx = Ctx {
            counter: 0,
            folder: "x",
            modified_day: "",
        };
        let dir = apply_rules(
            &[e(ExtTo::Upper, "")],
            &split_name("Photos.2026", true),
            true,
            &ctx,
        );
        assert_eq!(join_name(&dir), "Photos.2026");
    }

    #[test]
    fn template_tokens() {
        let t = Rule::Template {
            text: "{folder}-{n:3}-{name}-{modified}{n}".into(),
        };
        assert_eq!(run(&[t], "IMG.heic", 4), "Trip-005-IMG-2026-10-145.heic");
        assert_eq!(unknown_tags("{name}{date}{n:9}"), ["date", "n:9"]);
        assert_eq!(
            run(
                &[Rule::Template {
                    text: "{date}".into()
                }],
                "a.txt",
                0
            ),
            "{date}.txt"
        );
    }

    #[test]
    fn rules_apply_in_order() {
        let rules = [
            Rule::Replace {
                find: "IMG_".into(),
                with: "".into(),
                match_case: true,
            },
            Rule::new_number(),
        ];
        assert_eq!(run(&rules, "IMG_4821.heic", 0), "4821-001.heic");
    }

    #[test]
    fn empty_rules_do_nothing() {
        let r = Rule::Replace {
            find: "".into(),
            with: "x".into(),
            match_case: false,
        };
        assert!(r.is_empty());
        let add = Rule::Add {
            text: "".into(),
            after: true,
        };
        assert_eq!(run(&[add], "a.txt", 0), "a.txt");
    }

    #[test]
    fn history_undoes_last_change() {
        let mut h = RuleHistory::default();
        h.record(vec![]);
        h.record(vec![Rule::new_number()]);
        assert_eq!(h.undo(), Some(vec![Rule::new_number()]));
        assert_eq!(h.undo(), Some(vec![]));
        assert_eq!(h.undo(), None);
    }

    #[test]
    fn history_keeps_the_latest_hundred() {
        let mut h = RuleHistory::default();
        for i in 0..101u32 {
            h.record(vec![Rule::Number {
                after: true,
                start: i,
                digits: 1,
                separator: "".into(),
            }]);
        }
        let mut n = 0;
        while h.undo().is_some() {
            n += 1;
        }
        assert_eq!(n, 100);
    }
}
