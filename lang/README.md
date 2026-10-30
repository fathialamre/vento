# Translations

Vento's interface text lives in two files per language, gettext style:

- `vento.pot`: the template. Every text marked for translation (`@tr(...)` in
  the `.slint` files, `tr(...)`, `trc(...)` and `trn(...)` in Rust), in English.
- `<code>/LC_MESSAGES/vento.po`: one language's translation of it, for example
  `ar/LC_MESSAGES/vento.po`.

English is the source language and has no `.po` file. The translations are
compiled into the app, so a moved or copied `Vento.app` keeps them.

## Adding a language

1. Copy `vento.pot` to `lang/<code>/LC_MESSAGES/vento.po` (`<code>` is the
   two-letter language code) and translate every entry. Fill in the header too,
   above all its `Plural-Forms` line.
2. Add the language to `LANGUAGES` in `crates/vento-core/src/i18n/mod.rs`: its
   code, its own name ("Deutsch", "العربية"), whether it is written right to
   left, and its font (empty for the system's own; a bundled font is added
   under `crates/vento/ui/fonts` and imported in `theme.slint`, as IBM Plex
   Sans Arabic is for Arabic). In the same file, give `catalog()` an arm that loads the new `.po`.
   Also add the code to `CFBundleLocalizations` in `scripts/bundle-macos.sh`.
3. Build. The Settings window lists the language and Automatic can pick it.

Then run the tests (`cargo test --workspace --locked`). The translations test
fails until the new file is complete.

## Writing a translation

- **Every entry needs a translation.** An empty one fails the test.
- **Keep every placeholder.** `{name}`, `{n}`, `{dest}` and the others must
  appear in the translation as they do in the English. A plural form may leave
  out `{n}` where the language words the count without it ("عنصر واحد").
- **Give every plural form.** A counted sentence is translated whole, once for
  each form the language has, as its `Plural-Forms` header says: Arabic has six
  (`msgstr[0]` to `msgstr[5]`), English two. A sentence that holds a name instead
  of a count ("Moved Photos.zip to Desktop") is a separate, non-plural entry.
- **Right-to-left languages: start a line with U+200F when it opens with Latin
  text.** The text engine takes a line's direction from its first strong
  letter, so "Vento يعمل" would read left to right. Put a right-to-left mark
  (U+200F) first. This covers a line that opens with a placeholder for a name
  (`{app}`, `{name}`), which can hold Latin text. A line that opens with a
  number placeholder (`{n}`) needs no mark. The translations test fails for a
  right-to-left entry that is missing one.
- **Right-to-left languages: put U+200E before a shortcut placeholder.** In
  "Press {} to open the history" the `{}` is a shortcut such as ⌃⌥V, whose
  symbols take the sentence's direction and come out reversed (V⌥⌃). A
  left-to-right mark (U+200E) just before the placeholder keeps them in order.
- **Leave "Vento", theme names, image format names, modifier names and symbols
  as they are.** File names Vento creates, log lines and settings keys stay in
  English too.
- Digits stay Western (0 to 9).

## Regenerating the files

After you add, change or remove a marked text, run

```sh
VENTO_UPDATE_POT=1 cargo test -p vento --test translations
```

It rewrites `vento.pot`, adds each new entry to every `.po` with an empty
translation, and drops entries no code uses any more. Fill in the empty ones, then
run the tests again without the variable.
