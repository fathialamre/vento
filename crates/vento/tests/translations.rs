//! Keeps the translation files complete.
//!
//! Every text marked for translation (`@tr(...)` in `.slint` files and
//! `tr(...)`, `trc(...)`, `trn(...)` calls in Rust) must be in
//! `lang/vento.pot`, and every entry there must be translated in every
//! `lang/<code>/LC_MESSAGES/vento.po`, with the same placeholders.
//!
//! After adding or changing a marked text, regenerate the files with
//!
//! ```text
//! VENTO_UPDATE_POT=1 cargo test -p vento --test translations
//! ```
//!
//! which rewrites the template, adds the new entries (with an empty
//! translation) to each `.po` and drops entries no code uses any more. Then
//! fill in the empty translations and run the tests again without the
//! variable.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use vento_core::i18n::po::{self, Catalog};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn updating() -> bool {
    std::env::var_os("VENTO_UPDATE_POT").is_some()
}

// ---------------------------------------------------------------------------
// Finding the marked texts

/// A marked text and where it was first seen.
struct Found {
    plural: Option<String>,
    loc: String,
}

/// (msgctxt, msgid): the empty context means none.
type Key = (String, String);

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    Rust,
    Slint,
}

struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
    lang: Lang,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, lang: Lang) -> Self {
        Lexer {
            b: src.as_bytes(),
            i: 0,
            lang,
        }
    }

    fn at(&self, off: usize) -> Option<u8> {
        self.b.get(self.i + off).copied()
    }

    /// Skips whitespace and comments.
    fn skip_trivia(&mut self) {
        loop {
            match (self.at(0), self.at(1)) {
                (Some(c), _) if c.is_ascii_whitespace() => self.i += 1,
                (Some(b'/'), Some(b'/')) => {
                    while self.at(0).is_some_and(|c| c != b'\n') {
                        self.i += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => self.skip_block_comment(),
                _ => break,
            }
        }
    }

    fn skip_block_comment(&mut self) {
        // Rust block comments nest; Slint's do not.
        let nests = self.lang == Lang::Rust;
        let mut depth = 0usize;
        while self.i < self.b.len() {
            if self.at(0) == Some(b'/') && self.at(1) == Some(b'*') {
                depth += 1;
                self.i += 2;
            } else if self.at(0) == Some(b'*') && self.at(1) == Some(b'/') {
                self.i += 2;
                depth -= 1;
                if depth == 0 || !nests {
                    return;
                }
            } else {
                self.i += 1;
            }
        }
    }

    /// Reads a `"..."` literal at the cursor and resolves its escapes. With
    /// no literal there, returns `None` and does not move.
    fn string(&mut self) -> Option<String> {
        if self.at(0) != Some(b'"') {
            return None;
        }
        let mut j = self.i + 1;
        let mut out: Vec<u8> = Vec::new();
        while j < self.b.len() {
            match self.b[j] {
                b'"' => {
                    self.i = j + 1;
                    return String::from_utf8(out).ok();
                }
                b'\\' => {
                    j += 1;
                    match self.b.get(j).copied()? {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'0' => out.push(0),
                        b'\n' | b'\r' => {
                            // A backslash at the end of a line swallows the
                            // newline and the next line's indentation.
                            while self.b.get(j + 1).is_some_and(|c| c.is_ascii_whitespace()) {
                                j += 1;
                            }
                        }
                        b'x' => {
                            let hex = std::str::from_utf8(self.b.get(j + 1..j + 3)?).ok()?;
                            out.push(u8::from_str_radix(hex, 16).ok()?);
                            j += 2;
                        }
                        b'u' if self.b.get(j + 1) == Some(&b'{') => {
                            let end = j + self.b[j..].iter().position(|&c| c == b'}')?;
                            let hex = std::str::from_utf8(&self.b[j + 2..end]).ok()?;
                            let ch = char::from_u32(u32::from_str_radix(hex, 16).ok()?)?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                            j = end;
                        }
                        other => out.push(other),
                    }
                    j += 1;
                }
                c => {
                    out.push(c);
                    j += 1;
                }
            }
        }
        None
    }

    /// Steps over a literal or comment that must not be searched for calls.
    fn skip_string_raw(&mut self) {
        // At a `"`; the escapes only matter for finding the closing quote.
        self.i += 1;
        while let Some(c) = self.at(0) {
            self.i += 1;
            match c {
                b'\\' => self.i += 1,
                b'"' => return,
                _ => {}
            }
        }
    }

    /// Steps over a Rust raw string whose `r` is already consumed.
    fn skip_raw_string(&mut self) {
        let mut hashes = 0;
        while self.at(0) == Some(b'#') {
            hashes += 1;
            self.i += 1;
        }
        if self.at(0) != Some(b'"') {
            return;
        }
        self.i += 1;
        while self.i < self.b.len() {
            if self.at(0) == Some(b'"') && (1..=hashes).all(|k| self.at(k) == Some(b'#')) {
                self.i += 1 + hashes;
                return;
            }
            self.i += 1;
        }
    }

    /// Steps over a Rust char literal, or a lifetime's quote.
    fn skip_quote(&mut self) {
        if self.at(1) == Some(b'\\') {
            self.i += 3;
            while self.at(0).is_some_and(|c| c != b'\'') {
                self.i += 1;
            }
            self.i += 1;
            return;
        }
        let width = match self.at(1) {
            Some(c) if c >= 0xF0 => 4,
            Some(c) if c >= 0xE0 => 3,
            Some(c) if c >= 0xC0 => 2,
            _ => 1,
        };
        if self.at(1 + width) == Some(b'\'') {
            self.i += 2 + width;
        } else {
            self.i += 1;
        }
    }

    fn ident(&mut self) -> &'a str {
        let start = self.i;
        while self
            .at(0)
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.i += 1;
        }
        std::str::from_utf8(&self.b[start..self.i]).unwrap_or("")
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.b[self.i..].starts_with(token.as_bytes()) {
            self.i += token.len();
            true
        } else {
            false
        }
    }

    fn line(&self, at: usize) -> usize {
        1 + self.b[..at].iter().filter(|&&c| c == b'\n').count()
    }
}

/// One marked text before it gets a file location.
struct Call {
    ctx: String,
    msgid: String,
    plural: Option<String>,
    line: usize,
}

/// Finds `tr("..")`, `trc("..", "..")`, `trn("..", "..", n)` and
/// `trn_shown("..", "..", n, shown)` calls whose text arguments are string
/// literals.
fn scan_rust(src: &str) -> Vec<Call> {
    let mut lx = Lexer::new(src, Lang::Rust);
    let mut calls = Vec::new();
    while lx.i < lx.b.len() {
        match lx.at(0).unwrap() {
            b'/' if matches!(lx.at(1), Some(b'/' | b'*')) => lx.skip_trivia(),
            b'"' => lx.skip_string_raw(),
            b'\'' => lx.skip_quote(),
            c if c.is_ascii_alphanumeric() || c == b'_' => {
                let at = lx.i;
                let id = lx.ident();
                if matches!(id, "r" | "br" | "cr") && matches!(lx.at(0), Some(b'"' | b'#')) {
                    lx.skip_raw_string();
                } else if matches!(id, "tr" | "trc" | "trn" | "trn_shown") {
                    let line = lx.line(at);
                    let save = lx.i;
                    if let Some(call) = rust_args(&mut lx, id, line) {
                        calls.push(call);
                    } else {
                        lx.i = save;
                    }
                }
            }
            _ => lx.i += 1,
        }
    }
    calls
}

fn rust_args(lx: &mut Lexer, func: &str, line: usize) -> Option<Call> {
    lx.skip_trivia();
    if !lx.eat("(") {
        return None;
    }
    lx.skip_trivia();
    let first = lx.string()?;
    let call = |ctx: String, msgid: String, plural: Option<String>| Call {
        ctx,
        msgid,
        plural,
        line,
    };
    if func == "tr" {
        return Some(call(String::new(), first, None));
    }
    lx.skip_trivia();
    if !lx.eat(",") {
        return None;
    }
    lx.skip_trivia();
    let second = lx.string()?;
    Some(match func {
        "trc" => call(first, second, None),
        _ => call(String::new(), first, Some(second)),
    })
}

/// Finds `@tr("text")`, `@tr("ctx" => "text")` and
/// `@tr("one" | "other" % n)` in Slint source.
fn scan_slint(src: &str) -> Vec<Call> {
    let mut lx = Lexer::new(src, Lang::Slint);
    let mut calls = Vec::new();
    while lx.i < lx.b.len() {
        match lx.at(0).unwrap() {
            b'/' if matches!(lx.at(1), Some(b'/' | b'*')) => lx.skip_trivia(),
            b'"' => lx.skip_string_raw(),
            b'@' => {
                let at = lx.i;
                lx.i += 1;
                if lx.ident() == "tr" {
                    let line = lx.line(at);
                    let save = lx.i;
                    if let Some(call) = slint_args(&mut lx, line) {
                        calls.push(call);
                    } else {
                        lx.i = save;
                    }
                }
            }
            _ => lx.i += 1,
        }
    }
    calls
}

fn slint_args(lx: &mut Lexer, line: usize) -> Option<Call> {
    lx.skip_trivia();
    if !lx.eat("(") {
        return None;
    }
    lx.skip_trivia();
    let first = lx.string()?;
    lx.skip_trivia();
    let (ctx, msgid) = if lx.eat("=>") {
        lx.skip_trivia();
        let text = lx.string()?;
        lx.skip_trivia();
        (first, text)
    } else {
        (String::new(), first)
    };
    let plural = if lx.eat("|") {
        lx.skip_trivia();
        Some(lx.string()?)
    } else {
        None
    };
    Some(Call {
        ctx,
        msgid,
        plural,
        line,
    })
}

fn files_under(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// Every marked text in the sources, by (context, text).
fn scan_sources() -> BTreeMap<Key, Found> {
    let root = repo_root();
    let mut sources: Vec<(PathBuf, Lang)> = Vec::new();
    let mut slint = Vec::new();
    files_under(&root.join("crates/vento/ui"), "slint", &mut slint);
    sources.extend(slint.into_iter().map(|p| (p, Lang::Slint)));
    for krate in ["vento", "vento-core", "vento-platform"] {
        let mut rust = Vec::new();
        files_under(
            &root.join("crates").join(krate).join("src"),
            "rs",
            &mut rust,
        );
        sources.extend(rust.into_iter().map(|p| (p, Lang::Rust)));
    }
    // Relative, slash-separated names, sorted: the first place a text is
    // used is the same on every machine.
    let mut sources: Vec<(String, PathBuf, Lang)> = sources
        .into_iter()
        .map(|(p, l)| {
            let rel = p.strip_prefix(&root).unwrap_or(&p);
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            (name, p, l)
        })
        .collect();
    sources.sort_by(|a, b| a.0.cmp(&b.0));

    let mut found: BTreeMap<Key, Found> = BTreeMap::new();
    for (name, path, lang) in sources {
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {name}: {e}"));
        let calls = match lang {
            Lang::Rust => scan_rust(&text),
            Lang::Slint => scan_slint(&text),
        };
        for c in calls {
            let loc = format!("{name}:{}", c.line);
            let key = (c.ctx.clone(), c.msgid.clone());
            match found.get(&key) {
                Some(prev) if prev.plural != c.plural => panic!(
                    "{loc}: {:?} is marked as {} here but as {} at {}",
                    c.msgid,
                    kind(&c.plural),
                    kind(&prev.plural),
                    prev.loc
                ),
                Some(_) => {}
                None => {
                    found.insert(
                        key,
                        Found {
                            plural: c.plural,
                            loc,
                        },
                    );
                }
            }
        }
    }
    found
}

fn kind(plural: &Option<String>) -> String {
    match plural {
        Some(p) => format!("a plural with {p:?}"),
        None => "a plain text".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Writing the template and the translations

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `keyword "text"`, or the gettext multi-line form (an empty first line,
/// then one line per line of text) when the text has a newline inside it.
fn field(out: &mut String, keyword: &str, text: &str) {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.len() <= 1 {
        out.push_str(&format!("{keyword} {}\n", quote(text)));
        return;
    }
    out.push_str(&format!("{keyword} \"\"\n"));
    for l in lines {
        out.push_str(&quote(l));
        out.push('\n');
    }
}

struct OutEntry<'a> {
    loc: &'a str,
    ctx: &'a str,
    msgid: &'a str,
    plural: Option<&'a str>,
    forms: &'a [String],
}

fn write_entry(out: &mut String, e: &OutEntry) {
    out.push('\n');
    out.push_str(&format!("#: {}\n", e.loc));
    if !e.ctx.is_empty() {
        field(out, "msgctxt", e.ctx);
    }
    field(out, "msgid", e.msgid);
    match e.plural {
        Some(p) => {
            field(out, "msgid_plural", p);
            for (i, f) in e.forms.iter().enumerate() {
                field(out, &format!("msgstr[{i}]"), f);
            }
        }
        None => field(out, "msgstr", e.forms.first().map_or("", String::as_str)),
    }
}

const POT_HEADER: &str = "Project-Id-Version: vento\n\
MIME-Version: 1.0\n\
Content-Type: text/plain; charset=UTF-8\n\
Content-Transfer-Encoding: 8bit\n";

/// A whole file: the header, then every entry sorted by context and text.
fn render(
    header: &str,
    found: &BTreeMap<Key, Found>,
    forms_for: &dyn Fn(&Key, &Found) -> Vec<String>,
) -> String {
    let mut out = String::new();
    field(&mut out, "msgid", "");
    field(&mut out, "msgstr", header);
    for (key, f) in found {
        let forms = forms_for(key, f);
        write_entry(
            &mut out,
            &OutEntry {
                loc: &f.loc,
                ctx: &key.0,
                msgid: &key.1,
                plural: f.plural.as_deref(),
                forms: &forms,
            },
        );
    }
    out
}

fn write_if_changed(path: &Path, text: &str) {
    if fs::read_to_string(path).ok().as_deref() != Some(text) {
        fs::write(path, text).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    }
}

fn pot_path() -> PathBuf {
    repo_root().join("lang/vento.pot")
}

/// `lang/<code>/LC_MESSAGES/vento.po` of every language, by code.
fn po_files() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(read) = fs::read_dir(repo_root().join("lang")) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path().join("LC_MESSAGES/vento.po");
        if path.is_file() {
            out.push((entry.file_name().to_string_lossy().into_owned(), path));
        }
    }
    out.sort();
    out
}

fn update_files(found: &BTreeMap<Key, Found>) {
    write_if_changed(
        &pot_path(),
        &render(POT_HEADER, found, &|_, f| {
            vec![String::new(); if f.plural.is_some() { 2 } else { 1 }]
        }),
    );
    for (code, path) in po_files() {
        let text = fs::read_to_string(&path).unwrap();
        let old = po::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let n = old.nplurals();
        let rendered = render(old.header(), found, &|key, f| {
            let kept = old.get(&key.0, &key.1).map(|e| e.forms.clone());
            match (kept, f.plural.is_some()) {
                (Some(forms), _) if !forms.is_empty() => forms,
                (_, true) => vec![String::new(); n],
                _ => vec![String::new()],
            }
        });
        write_if_changed(&path, &rendered);
        eprintln!("updated {code}");
    }
}

// ---------------------------------------------------------------------------
// Reading the files back

fn load(path: &Path) -> Catalog {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    po::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The `{...}` tokens of a text, as a set.
fn placeholders(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        out.insert(format!("{{{}}}", &after[..close]));
        rest = &after[close + 1..];
    }
    out
}

/// Compares a translation's `{...}` tokens with the English text's. A plain
/// entry must match exactly. In a plural entry a form may leave out `{n}`
/// ("one file" needs no number), but must keep every other placeholder and
/// must not add one the English lacks. Returns what is wrong, if anything.
fn placeholder_problem(english: &str, translated: &str, plural: bool) -> Option<String> {
    let want = placeholders(english);
    let got = placeholders(translated);
    if plural {
        let missing: Vec<_> = want
            .iter()
            .filter(|p| p.as_str() != "{n}" && !got.contains(*p))
            .collect();
        let extra: Vec<_> = got.difference(&want).collect();
        if missing.is_empty() && extra.is_empty() {
            return None;
        }
        return Some(format!(
            "missing {missing:?}, extra {extra:?}; the English has {want:?}"
        ));
    }
    (got != want).then(|| format!("has {got:?}, the English has {want:?}"))
}

fn report(problems: Vec<String>, what: &str) {
    assert!(
        problems.is_empty(),
        "{what}:\n  {}\n\nIf texts were added or changed, run \
         `VENTO_UPDATE_POT=1 cargo test -p vento --test translations`, \
         translate the new entries, and run again.",
        problems.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// The checks

#[test]
fn marked_strings_match_the_template() {
    let found = scan_sources();
    if updating() {
        update_files(&found);
    }
    let pot = load(&pot_path());
    let in_template: BTreeSet<(String, String, Option<String>)> = pot
        .entries()
        .iter()
        .map(|e| (e.ctx.clone(), e.msgid.clone(), e.msgid_plural.clone()))
        .collect();
    let in_code: BTreeSet<(String, String, Option<String>)> = found
        .iter()
        .map(|(k, f)| (k.0.clone(), k.1.clone(), f.plural.clone()))
        .collect();
    let mut problems = Vec::new();
    for (ctx, id, plural) in in_code.difference(&in_template) {
        problems.push(format!(
            "marked in code but not in vento.pot: {ctx:?} {id:?} {plural:?}"
        ));
    }
    for (ctx, id, plural) in in_template.difference(&in_code) {
        problems.push(format!(
            "in vento.pot but not marked in code: {ctx:?} {id:?} {plural:?}"
        ));
    }
    report(problems, "lang/vento.pot is out of date");
}

#[test]
fn every_language_is_complete() {
    if updating() {
        return;
    }
    let pot = load(&pot_path());
    let files = po_files();
    assert!(
        !files.is_empty(),
        "no lang/<code>/LC_MESSAGES/vento.po found"
    );
    let mut problems = Vec::new();
    for (code, path) in files {
        let cat = load(&path);
        for t in pot.entries() {
            let name = format!("[{code}] {:?} {:?}", t.ctx, t.msgid);
            let Some(e) = cat.get(&t.ctx, &t.msgid) else {
                problems.push(format!("{name}: missing"));
                continue;
            };
            if e.msgid_plural != t.msgid_plural {
                problems.push(format!(
                    "{name}: msgid_plural is {:?}, the template has {:?}",
                    e.msgid_plural, t.msgid_plural
                ));
                continue;
            }
            let want = if t.msgid_plural.is_some() {
                cat.nplurals()
            } else {
                1
            };
            if e.forms.len() != want {
                problems.push(format!("{name}: has {} forms, needs {want}", e.forms.len()));
            } else if let Some(i) = e.forms.iter().position(|f| f.trim().is_empty()) {
                problems.push(format!("{name}: translation {i} is empty"));
            }
        }
    }
    report(problems, "translations are incomplete");
}

#[test]
fn placeholders_survive_translation() {
    if updating() {
        return;
    }
    let pot = load(&pot_path());
    let mut problems = Vec::new();
    for (code, path) in po_files() {
        let cat = load(&path);
        for t in pot.entries() {
            let Some(e) = cat.get(&t.ctx, &t.msgid) else {
                continue; // reported by every_language_is_complete
            };
            let plural = t.msgid_plural.is_some();
            let source = t.msgid_plural.as_deref().unwrap_or(&t.msgid);
            for (i, f) in e.forms.iter().enumerate() {
                if f.is_empty() {
                    continue; // reported by every_language_is_complete
                }
                if let Some(why) = placeholder_problem(source, f, plural) {
                    problems.push(format!("[{code}] {:?} form {i}: {why}", t.msgid));
                }
            }
        }
    }
    report(problems, "placeholders differ from the English text");
}

// ---------------------------------------------------------------------------
// No bare text in the windows

/// Properties whose value a person reads or hears.
const TEXT_PROPERTIES: &[&str] = &[
    "text",
    "label",
    "title",
    "accessible-label",
    "accessible-value",
    "accessible-description",
    "accessible-placeholder-text",
    "placeholder-text",
];

/// Literals that need no translation: the name, a separator and a
/// capped counter.
const BARE_ALLOWED: &[&str] = &["Vento", ":", "99+"];

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// The text of a Slint string literal at the cursor with its `\{...}`
/// interpolations cut out, and whether it had any.
fn slint_literal(lx: &mut Lexer) -> (String, bool) {
    // At the opening quote.
    lx.i += 1;
    let mut text = String::new();
    let mut interpolated = false;
    while let Some(c) = lx.at(0) {
        match c {
            b'"' => {
                lx.i += 1;
                break;
            }
            b'\\' if lx.at(1) == Some(b'{') => {
                interpolated = true;
                lx.i += 2;
                let mut depth = 1;
                while let Some(c) = lx.at(0) {
                    lx.i += 1;
                    match c {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'\\' => lx.i += 2,
            _ => {
                let start = lx.i;
                lx.i += 1;
                while lx.at(0).is_some_and(|c| c & 0xC0 == 0x80) {
                    lx.i += 1;
                }
                text.push_str(std::str::from_utf8(&lx.b[start..lx.i]).unwrap_or(""));
            }
        }
    }
    (text, interpolated)
}

/// Reads a value up to its `;` and reports the string literals in it that
/// are not inside `@tr(...)`.
fn scan_value(lx: &mut Lexer, out: &mut Vec<(usize, String)>) {
    let mut depth = 0i32;
    while let Some(c) = lx.at(0) {
        match c {
            b'/' if matches!(lx.at(1), Some(b'/' | b'*')) => lx.skip_trivia(),
            b'"' => {
                let line = lx.line(lx.i);
                let (text, interpolated) = slint_literal(lx);
                let allowed = BARE_ALLOWED.contains(&text.as_str());
                if !allowed && (interpolated || text.chars().any(|c| c.is_alphabetic())) {
                    out.push((line, text));
                }
            }
            b'@' => {
                lx.i += 1;
                while lx.at(0).is_some_and(is_ident_byte) {
                    lx.i += 1;
                }
                lx.skip_trivia();
                if lx.at(0) == Some(b'(') {
                    // `@tr(...)` is marked for translation; `@image-url(...)`
                    // and the like hold no text.
                    let mut depth = 0;
                    while let Some(c) = lx.at(0) {
                        match c {
                            b'"' => {
                                slint_literal(lx);
                                continue;
                            }
                            b'(' => depth += 1,
                            b')' => depth -= 1,
                            _ => {}
                        }
                        lx.i += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                lx.i += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 {
                    return;
                }
                lx.i += 1;
            }
            b';' if depth == 0 => {
                lx.i += 1;
                return;
            }
            _ => lx.i += 1,
        }
    }
}

/// The string literals in `.slint` source that a person reads but that are
/// not marked with `@tr`: the values of the text properties and of
/// `[string]` lists. Each is a line and the literal's text.
fn bare_slint_text(src: &str) -> Vec<(usize, String)> {
    let mut lx = Lexer::new(src, Lang::Slint);
    let mut out = Vec::new();
    while lx.i < lx.b.len() {
        match lx.at(0).unwrap() {
            b'/' if matches!(lx.at(1), Some(b'/' | b'*')) => lx.skip_trivia(),
            b'"' => lx.skip_string_raw(),
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let after_dot = lx.i > 0 && lx.b[lx.i - 1] == b'.';
                let start = lx.i;
                while lx.at(0).is_some_and(is_ident_byte) {
                    lx.i += 1;
                }
                let name = std::str::from_utf8(&lx.b[start..lx.i]).unwrap_or("");
                if after_dot {
                    continue;
                }
                if name == "property" {
                    lx.skip_trivia();
                    if lx.b[lx.i..].starts_with(b"<[string]>") {
                        lx.i += "<[string]>".len();
                        lx.skip_trivia();
                        while lx.at(0).is_some_and(is_ident_byte) {
                            lx.i += 1;
                        }
                        lx.skip_trivia();
                        if lx.at(0) == Some(b':') {
                            lx.i += 1;
                            scan_value(&mut lx, &mut out);
                        }
                    }
                } else if name == "items" || TEXT_PROPERTIES.contains(&name) {
                    lx.skip_trivia();
                    if lx.at(0) == Some(b':') && lx.at(1) != Some(b':') {
                        lx.i += 1;
                        scan_value(&mut lx, &mut out);
                    }
                }
            }
            _ => lx.i += 1,
        }
    }
    out
}

#[test]
fn no_bare_text_in_slint() {
    let mut files = Vec::new();
    files_under(&repo_root().join("crates/vento/ui"), "slint", &mut files);
    files.sort();
    assert!(!files.is_empty(), "no .slint files found");
    let mut problems = Vec::new();
    for path in files {
        let src = fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        for (line, text) in bare_slint_text(&src) {
            problems.push(format!("{name}:{line}: {text:?}"));
        }
    }
    assert!(
        problems.is_empty(),
        "text a person reads must be marked with @tr(...):\n  {}",
        problems.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// The scanner itself

fn keys(calls: Vec<Call>) -> Vec<(String, String, Option<String>)> {
    calls
        .into_iter()
        .map(|c| (c.ctx, c.msgid, c.plural))
        .collect()
}

fn t(ctx: &str, id: &str, plural: Option<&str>) -> (String, String, Option<String>) {
    (ctx.into(), id.into(), plural.map(Into::into))
}

#[test]
fn scans_rust_calls() {
    let src = r##"
        // tr("in a comment")
        /* trc("a", "b") */
        let a = tr("Plain");
        let b = trc("ctx", "Open");
        let c = trn(
            "{n} file",
            "{n} files",
            count,
        );
        let d = tr(variable);
        let e = "tr(\"inside a string\")";
        let f = r#"tr("raw")"#;
        let g = str("not a call");
        fn tr(msgid: &'static str) -> String {}
        let h = tr("Quote \" and \\ and \n");
        let i = 'x'; let j: &'static str = tr("After a lifetime");
    "##;
    assert_eq!(
        keys(scan_rust(src)),
        vec![
            t("", "Plain", None),
            t("ctx", "Open", None),
            t("", "{n} file", Some("{n} files")),
            t("", "Quote \" and \\ and \n", None),
            t("", "After a lifetime", None),
        ]
    );
}

#[test]
fn scans_slint_calls() {
    let src = r#"
        // @tr("in a comment")
        text: @tr("Plain");
        text: @tr("Moved {} to {}", a, b);
        text: @tr("menu" => "Open");
        text: @tr("{n} item" | "{n} items" % root.count);
        text: "@tr(\"in a string\")";
        text: @tr(variable);
    "#;
    assert_eq!(
        keys(scan_slint(src)),
        vec![
            t("", "Plain", None),
            t("", "Moved {} to {}", None),
            t("menu", "Open", None),
            t("", "{n} item", Some("{n} items")),
        ]
    );
}

#[test]
fn writes_gettext_formatting() {
    let mut out = String::new();
    field(&mut out, "msgstr", "a\nb \"q\"\n");
    assert_eq!(out, "msgstr \"\"\n\"a\\n\"\n\"b \\\"q\\\"\\n\"\n");
    let mut one = String::new();
    field(&mut one, "msgid", "plain");
    assert_eq!(one, "msgid \"plain\"\n");
    assert_eq!(
        placeholders("Moved {n} to {dest} {n}"),
        BTreeSet::from(["{n}".to_string(), "{dest}".to_string()])
    );
}

#[test]
fn plural_forms_may_leave_out_the_count_only() {
    let en = "Moved {n} items to {dest}";
    // A form without {n} passes, and so does one with it.
    assert_eq!(placeholder_problem(en, "نُقل عنصر إلى {dest}", true), None);
    assert_eq!(
        placeholder_problem(en, "نُقل {n} عنصرا إلى {dest}", true),
        None
    );
    // A form missing {dest} fails.
    assert!(placeholder_problem(en, "نُقل {n} عنصرا", true).is_some());
    // A form with an extra or misspelled name fails.
    assert!(placeholder_problem(en, "نُقل {n} عنصرا إلى {dst}", true).is_some());
    assert!(placeholder_problem(en, "نُقل {n} عنصرا إلى {dest} {other}", true).is_some());
    // Plain entries stay strict: {n} cannot be left out there.
    assert!(placeholder_problem("Moved {n}", "نُقل", false).is_some());
    assert_eq!(placeholder_problem("Moved {n}", "نُقل {n}", false), None);
}

#[test]
fn finds_bare_text_in_slint() {
    let src = r#"
        // text: "in a comment";
        Text { text: "Bare"; }
        Text { text: @tr("Marked"); }
        Text { text: root.on ? "Yes" : @tr("No"); }
        Text { text: root.on ? @tr("Yes {}", "inner") : "No"; }
        Text { text: "Vento"; }
        Text { text: ":"; }
        Text { text: root.count > 99 ? "99+" : root.count; }
        Text { text: "\{root.a} \{root.b}"; }
        Text { text: root.a + "x"; }
        Foo { label: "Label"; accessible-label: "Name"; title: "T"; }
        Foo { items: ["One", @tr("Two")]; }
        property <[string]> names: ["A", "B"];
        in property <[string]> from-rust;
        Foo { commands: "M 1 0 L 4"; text: root.name; }
        Foo { x: self.text; font-family: "Inter"; source: @image-url("a.svg"); }
    "#;
    let lines: Vec<(usize, String)> = bare_slint_text(src);
    let found: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(
        found,
        vec![
            "Bare", "Yes", "No", " ", "x", "Label", "Name", "T", "One", "A", "B"
        ]
    );
}

// ---------------------------------------------------------------------------
// Right to left

/// The `.slint` files whose rows never mirror: the wheel and its ring are a
/// gesture around the pointer, where an action is where it is.
const LTR_FILES: &[&str] = &["wheel.slint"];

/// `src` with its comments and the insides of its string literals blanked
/// out (each kept byte becomes a space, newlines stay), so braces and words
/// in them don't count as code and lines keep their numbers.
fn code_only(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    out[i] = b' ';
                    i += 1;
                }
                if b[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The byte just past the `}` that closes the block opened at `open`.
fn block_end(code: &[u8], open: usize) -> usize {
    let mut depth = 0;
    for (i, &c) in code.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    code.len()
}

/// The direct statements of the block opened at `open`: for each, its text
/// before any `{` of its own, and where that `{` is.
fn statements(code: &[u8], open: usize) -> Vec<(String, Option<usize>)> {
    let end = block_end(code, open) - 1;
    let mut out = Vec::new();
    let mut start = open + 1;
    let mut i = open + 1;
    while i < end {
        match code[i] {
            b'{' => {
                let head = String::from_utf8_lossy(&code[start..i]).trim().to_string();
                out.push((head, Some(i)));
                i = block_end(code, i);
                start = i;
            }
            b';' => {
                let head = String::from_utf8_lossy(&code[start..i]).trim().to_string();
                out.push((head, None));
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out
}

/// Whether a statement head opens a child element (`Text`, `id := Foo`,
/// `if c: Foo`, `for x[i] in m: Foo`) rather than a callback body.
fn is_child(head: &str) -> bool {
    if head.contains("=>") {
        return false;
    }
    let last = head
        .rsplit(|c: char| c.is_whitespace() || c == ':')
        .next()
        .unwrap_or("");
    last.starts_with(|c: char| c.is_ascii_uppercase())
}

/// Whether the element whose block opens at `open` sets a `layout-order`
/// that depends on `Theme.rtl`.
fn orders_by_direction(code: &[u8], open: usize) -> bool {
    statements(code, open).iter().any(|(head, block)| {
        block.is_none() && head.starts_with("layout-order") && head.contains("Theme.rtl")
    })
}

/// The lines of `src` with a `HorizontalLayout` (an element or a component
/// inheriting it) that has two or more children not all reordered by
/// `Theme.rtl` through `layout-order`, unless the line says why the row
/// stays left to right with `// ltr: <reason>`.
fn unmirrored_rows(src: &str) -> Vec<usize> {
    let code = code_only(src);
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let word = b"HorizontalLayout";
    let mut i = 0;
    while let Some(at) = code[i..].windows(word.len()).position(|w| w == word) {
        let at = i + at;
        i = at + word.len();
        let before = at.checked_sub(1).map(|j| code[j]);
        let after = code.get(i).copied();
        if before.is_some_and(is_ident_byte) || after.is_some_and(is_ident_byte) {
            continue;
        }
        let line = code[..at].iter().filter(|&&c| c == b'\n').count();
        let marked = lines[line]
            .find("//")
            .is_some_and(|c| lines[line][c + 2..].trim_start().starts_with("ltr:"));
        let Some(open) = code[i..].iter().position(|&c| c == b'{').map(|p| i + p) else {
            continue;
        };
        let children: Vec<usize> = statements(&code, open)
            .into_iter()
            .filter_map(|(head, block)| block.filter(|_| is_child(&head)))
            .collect();
        let mirrored =
            children.len() < 2 || children.iter().all(|&c| orders_by_direction(&code, c));
        if !mirrored && !marked {
            out.push(line + 1);
        }
    }
    out
}

#[test]
fn rows_mirror() {
    let mut files = Vec::new();
    files_under(&repo_root().join("crates/vento/ui"), "slint", &mut files);
    files.sort();
    assert!(!files.is_empty(), "no .slint files found");
    let mut problems = Vec::new();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if LTR_FILES.contains(&name.as_str()) {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        for line in unmirrored_rows(&src) {
            problems.push(format!("{name}:{line}"));
        }
    }
    assert!(
        problems.is_empty(),
        "a row must run right to left in a right-to-left language: give each of its children \
         a `layout-order` that reverses them when `Theme.rtl`, or say why it stays left to \
         right with `// ltr: <reason>`:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn finds_unmirrored_rows() {
    let src = r#"
        HorizontalLayout { Text { } Image { } }
        HorizontalLayout { Text { } }
        // A HorizontalLayout { A { } B { } } in a comment.
        HorizontalLayout { // ltr: digits read left to right
            Text { } Text { }
        }
        component Wide inherits HorizontalLayout {
            in property <bool> on;
            changed on => { debug("{"); }
            if root.on: Text { layout-order: Theme.rtl ? 2 : 0; }
            name := Text { text: "}"; layout-order: Theme.rtl ? 1 : 0; }
        }
        HorizontalLayout {
            for x[i] in 3: Rectangle { layout-order: Theme.rtl ? -i : 0; }
            Text { layout-order: 1; }
        }
        MyHorizontalLayoutish { A { } B { } }
        HorizontalLayout { A { Inner { layout-order: Theme.rtl ? 1 : 0; } } B { } }
    "#;
    assert_eq!(unmirrored_rows(src), vec![2, 14, 19]);
}
