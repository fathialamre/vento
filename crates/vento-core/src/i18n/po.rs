//! A reader for gettext `.po` and `.pot` files: `msgctxt`, `msgid`,
//! `msgid_plural`, `msgstr` and `msgstr[N]`, with multi-line strings,
//! escapes, comments and the header's `Plural-Forms`.
//!
//! Flags such as `#, fuzzy` are comments here and are not acted on, and
//! obsolete `#~` entries are skipped.

use std::collections::HashMap;

use super::plural::{self, Expr};

/// One message of a catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `msgctxt`, or an empty string when the entry has none.
    pub ctx: String,
    pub msgid: String,
    pub msgid_plural: Option<String>,
    /// The `msgstr`, or the `msgstr[N]` forms in order. Entries that are
    /// not yet translated hold empty strings.
    pub forms: Vec<String>,
}

/// A parsed `.po` or `.pot` file.
#[derive(Debug)]
pub struct Catalog {
    header: String,
    nplurals: usize,
    plural: Option<Expr>,
    entries: Vec<Entry>,
    /// ctx -> msgid -> position in `entries`; looked up without allocating.
    index: HashMap<String, HashMap<String, usize>>,
}

impl Catalog {
    /// The header's text (the `msgstr` of the empty `msgid`).
    pub fn header(&self) -> &str {
        &self.header
    }

    /// Number of plural forms the header declares; 2 when it declares none.
    pub fn nplurals(&self) -> usize {
        self.nplurals
    }

    /// Which plural form to use for `n`, clamped to the declared forms. With
    /// no `Plural-Forms` header this is the English rule.
    pub fn plural_form(&self, n: u64) -> usize {
        let idx = match &self.plural {
            Some(e) => e.eval(n),
            None => usize::from(n != 1),
        };
        idx.min(self.nplurals.saturating_sub(1))
    }

    /// Every message, in file order (the header is not one of them).
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, ctx: &str, msgid: &str) -> Option<&Entry> {
        let i = *self.index.get(ctx)?.get(msgid)?;
        self.entries.get(i)
    }

    /// The translation of a plain entry, or `None` when the entry is
    /// missing or its translation is empty.
    pub fn translate(&self, ctx: &str, msgid: &str) -> Option<&str> {
        self.get(ctx, msgid)?
            .forms
            .first()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// The translation of a plural entry for the count `n`, or `None` when
    /// the entry is missing or that form is empty.
    pub fn translate_plural(&self, ctx: &str, msgid: &str, n: u64) -> Option<&str> {
        self.get(ctx, msgid)?
            .forms
            .get(self.plural_form(n))
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
}

#[derive(Default)]
struct Builder {
    ctx: Option<String>,
    msgid: Option<String>,
    msgid_plural: Option<String>,
    forms: Vec<String>,
}

#[derive(Clone, Copy)]
enum Field {
    Ctx,
    Id,
    IdPlural,
    Str(usize),
}

/// Parses the text of a `.po` or `.pot` file.
pub fn parse(text: &str) -> Result<Catalog, String> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut header: Option<String> = None;
    let mut cur = Builder::default();
    let mut field: Option<Field> = None;

    let mut flush = |cur: &mut Builder, line: usize| -> Result<(), String> {
        let b = std::mem::take(cur);
        let Some(msgid) = b.msgid else {
            if b.ctx.is_some() {
                return Err(format!("line {line}: msgctxt without msgid"));
            }
            return Ok(());
        };
        if b.forms.is_empty() {
            return Err(format!("line {line}: entry {msgid:?} has no msgstr"));
        }
        let ctx = b.ctx.unwrap_or_default();
        if msgid.is_empty() && ctx.is_empty() {
            header = Some(b.forms.into_iter().next().unwrap_or_default());
        } else {
            entries.push(Entry {
                ctx,
                msgid,
                msgid_plural: b.msgid_plural,
                forms: b.forms,
            });
        }
        Ok(())
    };

    for (i, raw) in text.lines().enumerate() {
        let no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('"') {
            let s = unquote(line, no)?;
            match (field, &mut cur) {
                (Some(Field::Ctx), b) => b.ctx.get_or_insert_with(String::new).push_str(&s),
                (Some(Field::Id), b) => b.msgid.get_or_insert_with(String::new).push_str(&s),
                (Some(Field::IdPlural), b) => {
                    b.msgid_plural.get_or_insert_with(String::new).push_str(&s)
                }
                (Some(Field::Str(n)), b) => b.forms[n].push_str(&s),
                (None, _) => return Err(format!("line {no}: string without a keyword")),
            }
            continue;
        }
        let (kw, rest) = match line.find(|c: char| c.is_whitespace() || c == '"') {
            Some(p) => line.split_at(p),
            None => (line, ""),
        };
        let value = unquote(rest.trim(), no)?;
        match kw {
            "msgctxt" => {
                if cur.msgid.is_some() {
                    flush(&mut cur, no)?;
                }
                cur.ctx = Some(value);
                field = Some(Field::Ctx);
            }
            "msgid" => {
                if cur.msgid.is_some() {
                    flush(&mut cur, no)?;
                }
                cur.msgid = Some(value);
                field = Some(Field::Id);
            }
            "msgid_plural" => {
                if cur.msgid.is_none() {
                    return Err(format!("line {no}: msgid_plural without msgid"));
                }
                cur.msgid_plural = Some(value);
                field = Some(Field::IdPlural);
            }
            "msgstr" => {
                if cur.msgid.is_none() {
                    return Err(format!("line {no}: msgstr without msgid"));
                }
                cur.forms = vec![value];
                field = Some(Field::Str(0));
            }
            _ => {
                let idx = kw
                    .strip_prefix("msgstr[")
                    .and_then(|r| r.strip_suffix(']'))
                    .and_then(|n| n.parse::<usize>().ok())
                    .ok_or_else(|| format!("line {no}: unknown keyword {kw:?}"))?;
                if cur.msgid.is_none() {
                    return Err(format!("line {no}: msgstr[{idx}] without msgid"));
                }
                if idx != cur.forms.len() {
                    return Err(format!("line {no}: msgstr[{idx}] is out of order"));
                }
                cur.forms.push(value);
                field = Some(Field::Str(idx));
            }
        }
    }
    flush(&mut cur, text.lines().count() + 1)?;

    let header = header.unwrap_or_default();
    let (nplurals, plural) = parse_plural_forms(&header)?;

    let mut index: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        let prev = index
            .entry(e.ctx.clone())
            .or_default()
            .insert(e.msgid.clone(), i);
        if prev.is_some() {
            return Err(format!(
                "duplicate entry {:?} (context {:?})",
                e.msgid, e.ctx
            ));
        }
    }
    Ok(Catalog {
        header,
        nplurals,
        plural,
        entries,
        index,
    })
}

fn parse_plural_forms(header: &str) -> Result<(usize, Option<Expr>), String> {
    let Some(line) = header
        .lines()
        .find_map(|l| l.trim().strip_prefix("Plural-Forms:"))
    else {
        return Ok((2, None));
    };
    let mut nplurals = None;
    let mut expr = None;
    for part in line.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("nplurals=") {
            nplurals = Some(
                v.trim()
                    .parse::<usize>()
                    .map_err(|_| format!("bad nplurals in Plural-Forms: {v:?}"))?,
            );
        } else if let Some(v) = part.strip_prefix("plural=") {
            expr = Some(plural::compile(v)?);
        }
    }
    match (nplurals, expr) {
        (Some(n), Some(e)) if n > 0 => Ok((n, Some(e))),
        _ => Err(format!("incomplete Plural-Forms header: {}", line.trim())),
    }
}

/// Reads one `"..."` literal and resolves its escapes.
fn unquote(s: &str, line: usize) -> Result<String, String> {
    let inner = s
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .filter(|_| s.len() >= 2)
        .ok_or_else(|| format!("line {line}: expected a quoted string"))?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                other => {
                    return Err(format!(
                        "line {line}: unsupported escape \\{}",
                        other.unwrap_or(' ')
                    ));
                }
            },
            '"' => return Err(format!("line {line}: unescaped quote inside a string")),
            c => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# translator comment
msgid ""
msgstr ""
"Content-Type: text/plain; charset=UTF-8\n"
"Plural-Forms: nplurals=3; plural=(n==1 ? 0 : n==2 ? 1 : 2);\n"

#: somewhere.slint:3
msgid "Plain"
msgstr "Simple"

msgctxt "menu"
msgid "Open"
msgstr "Ouvrir"

msgid "{n} file"
msgid_plural "{n} files"
msgstr[0] "one {n}"
msgstr[1] "two {n}"
msgstr[2] "many {n}"

#~ msgid "Gone"
#~ msgstr "Parti"

msgid "Multi"
msgstr ""
"line one\n"
"say \"hi\" \\ done"

msgid "Empty"
msgstr ""
"#;

    #[test]
    fn reads_plain_context_and_plural_entries() {
        let c = parse(SAMPLE).unwrap();
        assert_eq!(c.nplurals(), 3);
        assert_eq!(c.plural_form(1), 0);
        assert_eq!(c.plural_form(2), 1);
        assert_eq!(c.plural_form(7), 2);
        assert_eq!(c.translate("", "Plain"), Some("Simple"));
        assert_eq!(c.translate("menu", "Open"), Some("Ouvrir"));
        assert_eq!(c.translate("", "Open"), None);
        assert_eq!(c.translate("", "Gone"), None);
        assert_eq!(c.translate_plural("", "{n} file", 1), Some("one {n}"));
        assert_eq!(c.translate_plural("", "{n} file", 2), Some("two {n}"));
        assert_eq!(c.translate_plural("", "{n} file", 50), Some("many {n}"));
        assert_eq!(
            c.translate("", "Multi"),
            Some("line one\nsay \"hi\" \\ done")
        );
        // An empty translation counts as untranslated.
        assert_eq!(c.translate("", "Empty"), None);
        let plural = c.entries().iter().find(|e| e.msgid == "{n} file").unwrap();
        assert_eq!(plural.msgid_plural.as_deref(), Some("{n} files"));
        assert_eq!(plural.forms.len(), 3);
    }

    #[test]
    fn rejects_malformed_files() {
        assert!(parse("msgid \"a\"\nmsgstr \"b\"\nmsgid \"a\"\nmsgstr \"c\"\n").is_err());
        assert!(parse("msgstr \"orphan\"\n").is_err());
        assert!(parse("\"stray\"\n").is_err());
        assert!(parse("msgid \"a\nmsgstr \"b\"\n").is_err());
        assert!(parse("msgid \"a\"\nmsgstr \"\\q\"\n").is_err());
        assert!(
            parse("msgid \"\"\nmsgstr \"Plural-Forms: nplurals=2; plural=(n ==);\\n\"\n").is_err()
        );
    }
}
