//! Translation of the text Rust code makes: a lookup in the `.po` catalogs
//! that are embedded in the binary. Slint reads the same files for the text
//! in `.slint` files.
//!
//! The current language is one process-wide atomic index, so any thread may
//! switch it or format text: toasts and archive jobs format on workers.
//! English is the source language and needs no catalog: a missing catalog,
//! entry or translation falls back to the English text in the code.

pub mod dates;
pub mod errors;
pub mod plural;
pub mod po;

use std::cell::Cell;
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
    /// The font family the windows use for this language, one of those
    /// bundled in `crates/vento/ui/fonts`; empty for the system's own font.
    pub font: &'static str,
}

/// Every language, English first. English is also the fallback.
pub const LANGUAGES: &[Language] = &[
    Language {
        code: "en",
        name: "English",
        rtl: false,
        font: "",
    },
    Language {
        code: "ar",
        name: "العربية",
        rtl: true,
        font: "IBM Plex Sans Arabic",
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The language this thread reads in place of the process-wide one, as
    /// an index into [`LANGUAGES`]; see [`with_language`]. In this crate's
    /// own unit tests every thread starts on English, so a test that
    /// switches the process-wide language cannot disturb the others.
    static OVERRIDE: Cell<Option<usize>> = const { Cell::new(if cfg!(test) { Some(0) } else { None }) };
}

/// The index in [`LANGUAGES`] of `code`; an unknown code is English.
fn index_of(code: &str) -> usize {
    LANGUAGES.iter().position(|l| l.code == code).unwrap_or(0)
}

/// Switches the language. An unknown code means English.
pub fn set_language(code: &str) {
    CURRENT.store(index_of(code), Ordering::Relaxed);
}

/// Runs `f` with this thread reading the language `code` (English for an
/// unknown one) instead of the process-wide one, and puts back what it read
/// before, also when `f` panics. Other threads are not affected, so tests
/// that need Arabic can run beside tests that need English.
pub fn with_language<R>(code: &str, f: impl FnOnce() -> R) -> R {
    /// Puts the previous override back when dropped.
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|o| o.set(self.0));
        }
    }
    let _restore = Restore(OVERRIDE.with(|o| o.replace(Some(index_of(code)))));
    f()
}

/// The language the `language` setting asks for. `"auto"` takes the first
/// of the `preferred` languages (the system's, best first) that Vento has;
/// none means English. A code matches on its language part, so `"AR"` and
/// `"ar-LY"` both mean Arabic. A value Vento has no language for is logged
/// once and means English, the default.
pub fn resolve(setting: &str, preferred: &[String]) -> &'static Language {
    if setting != "auto" {
        if let Some(language) = lookup(setting) {
            return language;
        }
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| log::warn!("unknown language {setting:?} in settings, using English"));
        return &LANGUAGES[0];
    }
    preferred
        .iter()
        .find_map(|code| lookup(code))
        .unwrap_or(&LANGUAGES[0])
}

/// The language whose code is the language part of `tag` (`"ar-LY"`,
/// `"ar_LY.UTF-8"`, `"AR"`); `None` for `"auto"` or a language Vento lacks.
pub fn lookup(tag: &str) -> Option<&'static Language> {
    let end = tag.find(['-', '_', '@', '.']).unwrap_or(tag.len());
    let part = tag[..end].to_ascii_lowercase();
    LANGUAGES.iter().find(|l| l.code == part)
}

/// The system's preferred languages, best first, as the platform words
/// them (`"ar-LY"` on macOS, `"ar_LY.UTF-8"` on Linux).
pub fn system_languages() -> Vec<String> {
    sys_locale::get_locales().collect()
}

/// The language in use.
pub fn current() -> &'static Language {
    let index = OVERRIDE
        .with(Cell::get)
        .unwrap_or_else(|| CURRENT.load(Ordering::Relaxed));
    &LANGUAGES[index.min(LANGUAGES.len() - 1)]
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

/// Like [`trn`], but fills `{n}` with `shown` instead of the plain count:
/// the count as the sentence writes it (`"12,408"`, with thousands
/// separators). `n` alone picks the form.
pub fn trn_shown(singular: &'static str, plural: &'static str, n: u64, shown: &str) -> String {
    let template = template_for(catalog(), singular, plural, n);
    fill(template, &[("n", shown)])
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
    fill(
        template_for(cat, singular, plural, n),
        &[("n", &n.to_string())],
    )
}

/// The text of the form `n` takes: the catalog's, else the English one.
fn template_for<'a>(
    cat: Option<&'a Catalog>,
    singular: &'a str,
    plural: &'a str,
    n: u64,
) -> &'a str {
    cat.and_then(|c| c.translate_plural("", singular, n))
        .unwrap_or(if n == 1 { singular } else { plural })
}

/// Whether `text` reads right to left as a line of its own: its first
/// strong letter is Hebrew, Arabic or another right-to-left script, or the
/// right-to-left mark. A file name in Arabic does, whatever the interface
/// language; digits, spaces and symbols decide nothing.
pub fn starts_right_to_left(text: &str) -> bool {
    for c in text.chars() {
        let rtl = matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
            || c == '\u{200F}';
        if rtl {
            return true;
        }
        if c.is_alphabetic() || c == '\u{200E}' {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    /// Runs `f` reading the process-wide language, which `set_language`
    /// sets, as the app does. Every test thread starts on an English
    /// override (see [`OVERRIDE`]), so only this thread sees the switch,
    /// and the lock keeps two such tests apart.
    fn with_process_language<R>(f: impl FnOnce() -> R) -> R {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = OVERRIDE.with(|o| o.replace(None));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        set_language("en");
        OVERRIDE.with(|o| o.set(before));
        result.unwrap_or_else(|e| std::panic::resume_unwind(e))
    }

    #[test]
    fn falls_back_to_english_and_switches() {
        with_process_language(|| {
            set_language("en");
            assert_eq!(tr("Language"), "Language");
            set_language("ar");
            assert_eq!(tr("Language"), "اللغة");
            assert_eq!(current().code, "ar");
            set_language("xx");
            assert_eq!(current().code, "en");
        });
    }

    #[test]
    fn with_language_reads_that_language_on_this_thread_only() {
        assert_eq!(current().code, "en");
        with_language("ar", || {
            assert_eq!(current().code, "ar");
            assert_eq!(tr("Language"), "اللغة");
            // Another thread is not affected.
            std::thread::spawn(|| assert_eq!(tr("Language"), "Language"))
                .join()
                .unwrap();
            // Nested: the inner one wins, the outer one comes back.
            with_language("en", || assert_eq!(tr("Language"), "Language"));
            assert_eq!(current().code, "ar");
        });
        assert_eq!(current().code, "en");
        with_language("xx", || assert_eq!(current().code, "en"));
    }

    #[test]
    fn with_language_is_undone_by_a_panic() {
        let result = std::panic::catch_unwind(|| {
            with_language("ar", || {
                assert_eq!(current().code, "ar");
                panic!("on purpose");
            })
        });
        assert!(result.is_err());
        assert_eq!(current().code, "en");
    }

    #[test]
    fn with_language_wins_over_the_process_language() {
        with_process_language(|| {
            set_language("ar");
            assert_eq!(current().code, "ar");
            with_language("en", || assert_eq!(current().code, "en"));
            assert_eq!(current().code, "ar");
        });
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
    fn a_shown_count_fills_the_count_but_the_plain_one_picks_the_form() {
        assert_eq!(
            trn_shown("{n} file", "{n} files", 12_408, "12,408"),
            "12,408 files"
        );
        assert_eq!(trn_shown("{n} file", "{n} files", 1, "1"), "1 file");
        with_language("ar", || {
            assert_eq!(trn_shown("{n} file", "{n} files", 2, "2"), "ملفان");
            assert_eq!(
                trn_shown("{n} file", "{n} files", 12_408, "12,408"),
                "12,408 ملفات"
            );
        });
    }

    #[test]
    fn fill_leaves_unknown_and_unclosed_names() {
        assert_eq!(fill("a {x} b {y", &[("x", "1")]), "a 1 b {y");
        assert_eq!(fill("{n}", &[("n", "{n}")]), "{n}");
        assert_eq!(fill("{missing} and {}", &[]), "{missing} and {}");
    }

    #[test]
    fn automatic_takes_the_first_preferred_language_vento_has() {
        assert_eq!(
            resolve("auto", &["fr-FR".into(), "ar-LY".into()]).code,
            "ar"
        );
        assert_eq!(resolve("auto", &["fr-FR".into()]).code, "en");
        assert_eq!(resolve("auto", &[]).code, "en");
        assert_eq!(resolve("auto", &["ar_LY.UTF-8".into()]).code, "ar");
        assert_eq!(resolve("auto", &["ar@tashkeel".into()]).code, "ar");
    }

    #[test]
    fn a_saved_language_matches_loosely() {
        assert_eq!(resolve("AR", &[]).code, "ar");
        assert_eq!(resolve("ar-LY", &[]).code, "ar");
        assert_eq!(resolve("en", &["ar".into()]).code, "en");
        // A language Vento does not have acts as Automatic.
        assert_eq!(resolve("xx", &["ar".into()]).code, "en");
        assert_eq!(resolve("xx", &[]).code, "en");
    }

    #[test]
    fn languages_are_english_then_arabic() {
        let codes: Vec<_> = LANGUAGES.iter().map(|l| (l.code, l.rtl)).collect();
        assert_eq!(codes, vec![("en", false), ("ar", true)]);
    }

    #[test]
    fn every_language_but_english_has_a_catalog() {
        // A language added to `LANGUAGES` without an arm in `catalog()`
        // would read English whatever its `.po` says.
        for language in LANGUAGES.iter().filter(|l| l.code != "en") {
            let word = with_language(language.code, || tr("Language"));
            assert_ne!(word, "Language", "{} has no catalog", language.code);
        }
    }

    #[test]
    fn fill_replaces_named_placeholders() {
        assert_eq!(
            fill("Moved {n} to {dest}", &[("n", "3"), ("dest", "Photos")]),
            "Moved 3 to Photos"
        );
    }

    #[test]
    fn a_line_reads_right_to_left_from_its_first_strong_letter() {
        assert!(starts_right_to_left("تقرير المبيعات.pdf"));
        assert!(starts_right_to_left("2024 تقرير.pdf"));
        assert!(starts_right_to_left("\u{200F}Vento يعمل"));
        assert!(!starts_right_to_left("Vento يعمل"));
        assert!(!starts_right_to_left("Report تقرير.pdf"));
        assert!(!starts_right_to_left("Photo 1.png"));
        assert!(!starts_right_to_left("~/Pictures"));
        assert!(!starts_right_to_left("2024"));
        assert!(!starts_right_to_left(""));
    }
}
