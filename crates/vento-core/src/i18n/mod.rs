//! Translation of the text Rust code makes: a lookup in the `.po` catalogs
//! that are embedded in the binary. Slint reads the same files for the text
//! in `.slint` files.
//!
//! The current language is one process-wide atomic index, so any thread may
//! switch it or format text: toasts and archive jobs format on workers.
//! English is the source language and needs no catalog: a missing catalog,
//! entry or translation falls back to the English text in the code.

pub mod plural;
pub mod po;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use po::Catalog;

/// A language the interface can be shown in.
#[derive(Debug, PartialEq, Eq)]
pub struct Language {
    /// The code used in `settings.toml` and in `lang/<code>/`.
    pub code: &'static str,
    /// The language's name in that language.
    pub name: &'static str,
    /// Whether the language is written right to left.
    pub rtl: bool,
}

/// Every language, English first. English is also the fallback.
pub const LANGUAGES: &[Language] = &[
    Language {
        code: "en",
        name: "English",
        rtl: false,
    },
    Language {
        code: "ar",
        name: "العربية",
        rtl: true,
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Switches the language. An unknown code means English.
pub fn set_language(code: &str) {
    let idx = LANGUAGES.iter().position(|l| l.code == code).unwrap_or(0);
    CURRENT.store(idx, Ordering::Relaxed);
}

/// The language in use.
pub fn current() -> &'static Language {
    &LANGUAGES[CURRENT.load(Ordering::Relaxed).min(LANGUAGES.len() - 1)]
}

/// The catalog of the language in use; `None` for English.
fn catalog() -> Option<&'static Catalog> {
    static ARABIC: OnceLock<Catalog> = OnceLock::new();
    match current().code {
        "ar" => Some(ARABIC.get_or_init(|| {
            load(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../lang/ar/LC_MESSAGES/vento.po"
            )))
        })),
        _ => None,
    }
}

/// Parses an embedded catalog. A broken one is logged and acts as empty,
/// so the text stays English; the completeness test catches it before then.
fn load(text: &str) -> Catalog {
    po::parse(text).unwrap_or_else(|e| {
        log::error!("translation catalog is invalid: {e}");
        po::parse("").expect("an empty catalog parses")
    })
}

/// Translates `msgid`. Without a translation this is `msgid` itself.
pub fn tr(msgid: &'static str) -> String {
    translate(catalog(), "", msgid)
}

/// Translates `msgid` in the context `ctx`, to tell apart one English text
/// that means two things.
pub fn trc(ctx: &'static str, msgid: &'static str) -> String {
    translate(catalog(), ctx, msgid)
}

/// Translates a counted sentence and fills `{n}` with the count. The
/// language's own rule picks the form: `singular` and `plural` are the
/// English texts and key the entry.
pub fn trn(singular: &'static str, plural: &'static str, n: u64) -> String {
    translate_plural(catalog(), singular, plural, n)
}

/// Replaces each `{name}` in `template` with its value from `args`. A name
/// that is not in `args` is left as it is, and replaced text is not
/// searched again.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match args.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out.push_str(v),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

fn translate(cat: Option<&Catalog>, ctx: &str, msgid: &str) -> String {
    cat.and_then(|c| c.translate(ctx, msgid))
        .unwrap_or(msgid)
        .to_string()
}

fn translate_plural(cat: Option<&Catalog>, singular: &str, plural: &str, n: u64) -> String {
    let template = cat
        .and_then(|c| c.translate_plural("", singular, n))
        .unwrap_or(if n == 1 { singular } else { plural });
    fill(template, &[("n", &n.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn falls_back_to_english_and_switches() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_language("en");
        assert_eq!(tr("Language"), "Language");
        set_language("ar");
        assert_eq!(tr("Language"), "اللغة");
        assert_eq!(current().code, "ar");
        set_language("xx");
        assert_eq!(current().code, "en");
    }

    const SAMPLE: &str = r#"msgid ""
msgstr ""
"Plural-Forms: nplurals=2; plural=(n==1 ? 0 : 1);\n"

msgid "Open"
msgstr "OPEN"

msgid "Blank"
msgstr ""

msgid "{n} file"
msgid_plural "{n} files"
msgstr[0] "one file"
msgstr[1] "{n} FILES"

msgid "{n} dir"
msgid_plural "{n} dirs"
msgstr[0] "one dir"
msgstr[1] ""
"#;

    #[test]
    fn missing_or_empty_translations_fall_back_to_english() {
        let c = po::parse(SAMPLE).unwrap();
        assert_eq!(translate(Some(&c), "", "Open"), "OPEN");
        assert_eq!(translate(Some(&c), "", "Blank"), "Blank");
        assert_eq!(translate(Some(&c), "", "Unknown"), "Unknown");
        assert_eq!(translate(Some(&c), "ctx", "Open"), "Open");
        assert_eq!(translate(None, "", "Open"), "Open");
    }

    #[test]
    fn counted_sentences_pick_the_form_and_fill_the_count() {
        let c = po::parse(SAMPLE).unwrap();
        let f = |n| translate_plural(Some(&c), "{n} file", "{n} files", n);
        assert_eq!((f(1), f(5)), ("one file".into(), "5 FILES".into()));
        // An empty form falls back to the English rule and text.
        let d = |n| translate_plural(Some(&c), "{n} dir", "{n} dirs", n);
        assert_eq!((d(1), d(2)), ("one dir".into(), "2 dirs".into()));
        // No catalog: English singular for exactly one, plural otherwise.
        let e = |n| translate_plural(None, "{n} file", "{n} files", n);
        assert_eq!(
            (e(1), e(0), e(3)),
            ("1 file".into(), "0 files".into(), "3 files".into())
        );
    }

    #[test]
    fn fill_leaves_unknown_and_unclosed_names() {
        assert_eq!(fill("a {x} b {y", &[("x", "1")]), "a 1 b {y");
        assert_eq!(fill("{n}", &[("n", "{n}")]), "{n}");
        assert_eq!(fill("{missing} and {}", &[]), "{missing} and {}");
    }

    #[test]
    fn languages_are_english_then_arabic() {
        let codes: Vec<_> = LANGUAGES.iter().map(|l| (l.code, l.rtl)).collect();
        assert_eq!(codes, vec![("en", false), ("ar", true)]);
    }

    #[test]
    fn fill_replaces_named_placeholders() {
        assert_eq!(
            fill("Moved {n} to {dest}", &[("n", "3"), ("dest", "Photos")]),
            "Moved 3 to Photos"
        );
    }
}
