//! Between Linux clipboard MIME types and the pasteboard type names
//! `vento_core::clip` works with.
//!
//! A Linux clipboard holds one item in many types; macOS (and so the
//! history) has one item per copied file. Files come in as one item per file
//! with a `public.file-url` each, text, links and pictures as the core's
//! types, and every other MIME type (`text/html`, ...) under its own name, so
//! a copy taken back from the history is the copy it was.

use vento_core::clip::{
    ClipRep, ClipSnapshot, SECRET_MARKERS, TYPE_FILE_URL, TYPE_PNG, TYPE_TEXT, TYPE_TIFF, TYPE_URL,
    file_url_to_path,
};

/// GNOME's file copy: `copy` (or `cut`), then one URI per line.
pub(super) const GNOME_FILES: &str = "x-special/gnome-copied-files";
pub(super) const URI_LIST: &str = "text/uri-list";
const UTF8_TEXT: &str = "text/plain;charset=utf-8";
/// Text types, best first. `STRING` is Latin-1 and left alone.
const TEXT_TYPES: [&str; 3] = [UTF8_TEXT, "UTF8_STRING", "text/plain"];
/// What password managers set on a secret copy (KeePassXC and others).
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";
const PNG: &str = "image/png";
const TIFF: &str = "image/tiff";

/// Types that come out as a core type, that only X11 itself uses, or other
/// picture types of a picture kept as PNG or TIFF (a browser offers BMP,
/// JPEG and more of the same picture: they would only add size).
fn translated_or_internal(ty: &str) -> bool {
    TEXT_TYPES.iter().any(|t| t.eq_ignore_ascii_case(ty))
        || [GNOME_FILES, URI_LIST, PNG, TIFF, PASSWORD_HINT].contains(&ty)
        || ty.starts_with("image/")
        // TARGETS, TIMESTAMP, MULTIPLE, SAVE_TARGETS, STRING, TEXT, ...
        || !ty.contains('/')
}

/// The best text type in `types`, spelled as the clipboard spells it (the
/// charset's case varies: GNOME offers `text/plain;charset=UTF-8`).
fn text_type(types: &[String]) -> Option<&str> {
    TEXT_TYPES.iter().find_map(|want| {
        types
            .iter()
            .find(|have| have.eq_ignore_ascii_case(want))
            .map(String::as_str)
    })
}

/// The URIs of a `text/uri-list`, comments and blank lines left out.
fn uris(list: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(list)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The core type names a clipboard holding `types` reads as, without
/// reading any data. A password manager's copy carries a secret marker.
pub(super) fn core_types(types: &[String]) -> Vec<String> {
    let has = |t: &str| types.iter().any(|have| have == t);
    let mut out = Vec::new();
    if has(PASSWORD_HINT) {
        out.push(SECRET_MARKERS[0].to_owned());
    }
    if has(GNOME_FILES) || has(URI_LIST) {
        out.push(TYPE_FILE_URL.to_owned());
    }
    if text_type(types).is_some() {
        out.push(TYPE_TEXT.to_owned());
    }
    if has(PNG) {
        out.push(TYPE_PNG.to_owned());
    }
    if has(TIFF) {
        out.push(TYPE_TIFF.to_owned());
    }
    out.extend(types.iter().filter(|t| !translated_or_internal(t)).cloned());
    out
}

/// The snapshot of a clipboard holding `types`, reading each type's data
/// with `read`. Types that can't be read are left out.
pub(super) fn snapshot(types: &[String], read: impl Fn(&str) -> Option<Vec<u8>>) -> ClipSnapshot {
    let has = |t: &str| types.iter().any(|have| have == t);
    let mut marker = Vec::new();
    if has(PASSWORD_HINT) {
        marker.push(ClipRep {
            ty: SECRET_MARKERS[0].to_owned(),
            data: Vec::new(),
        });
    }
    // GNOME's list says copy or cut on its first line; the URI list is the
    // same files without it.
    let listed = has(GNOME_FILES)
        .then(|| read(GNOME_FILES))
        .flatten()
        .map(|data| uris(&data).into_iter().skip(1).collect::<Vec<_>>())
        .or_else(|| {
            has(URI_LIST)
                .then(|| read(URI_LIST))
                .flatten()
                .map(|d| uris(&d))
        })
        .unwrap_or_default();
    let (files, links): (Vec<String>, Vec<String>) = listed
        .into_iter()
        .partition(|u| file_url_to_path(u.as_bytes()).is_some());
    if !files.is_empty() {
        let mut items: Vec<Vec<ClipRep>> = files
            .into_iter()
            .map(|uri| {
                vec![ClipRep {
                    ty: TYPE_FILE_URL.to_owned(),
                    data: uri.into_bytes(),
                }]
            })
            .collect();
        items[0].extend(marker);
        return ClipSnapshot { items };
    }

    let mut item = marker;
    if let Some(text) = text_type(types).and_then(&read) {
        item.push(ClipRep {
            ty: TYPE_TEXT.to_owned(),
            data: text,
        });
    }
    if let Some(link) = links.into_iter().next() {
        item.push(ClipRep {
            ty: TYPE_URL.to_owned(),
            data: link.into_bytes(),
        });
    }
    for (mime, ty) in [(PNG, TYPE_PNG), (TIFF, TYPE_TIFF)] {
        if let Some(data) = has(mime).then(|| read(mime)).flatten() {
            item.push(ClipRep {
                ty: ty.to_owned(),
                data,
            });
        }
    }
    for ty in types.iter().filter(|t| !translated_or_internal(t)) {
        if let Some(data) = read(ty) {
            item.push(ClipRep {
                ty: ty.clone(),
                data,
            });
        }
    }
    let items = if item.is_empty() {
        Vec::new()
    } else {
        vec![item]
    };
    ClipSnapshot { items }
}

/// What to offer for `snapshot`, as (MIME type or X11 target, data): the
/// core's types in their Linux forms (files for Files and as text for a
/// terminal), then every MIME type kept as it was.
pub(super) fn offers(snapshot: &ClipSnapshot) -> Vec<(String, Vec<u8>)> {
    let reps = || snapshot.items.iter().flatten();
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut offer = |ty: &str, data: Vec<u8>| {
        if !out.iter().any(|(have, _)| have == ty) {
            out.push((ty.to_owned(), data));
        }
    };
    let files: Vec<&[u8]> = reps()
        .filter(|r| r.ty == TYPE_FILE_URL)
        .map(|r| r.data.as_slice())
        .filter(|u| file_url_to_path(u).is_some())
        .collect();
    if !files.is_empty() {
        let uris: Vec<String> = files
            .iter()
            .map(|u| String::from_utf8_lossy(u).into_owned())
            .collect();
        let paths: Vec<String> = files
            .iter()
            .filter_map(|u| file_url_to_path(u))
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        offer(
            GNOME_FILES,
            format!("copy\n{}", uris.join("\n")).into_bytes(),
        );
        offer(URI_LIST, format!("{}\r\n", uris.join("\r\n")).into_bytes());
        let text = paths.join("\n").into_bytes();
        offer(UTF8_TEXT, text.clone());
        offer("UTF8_STRING", text.clone());
        offer("text/plain", text);
    }
    if let Some(text) = reps().find(|r| r.ty == TYPE_TEXT) {
        offer(UTF8_TEXT, text.data.clone());
        offer("UTF8_STRING", text.data.clone());
        offer("text/plain", text.data.clone());
    }
    if let Some(link) = reps().find(|r| r.ty == TYPE_URL) {
        let mut list = link.data.clone();
        list.extend_from_slice(b"\r\n");
        offer(URI_LIST, list);
        offer(UTF8_TEXT, link.data.clone());
        offer("UTF8_STRING", link.data.clone());
    }
    for (ty, mime) in [(TYPE_PNG, PNG), (TYPE_TIFF, TIFF)] {
        if let Some(rep) = reps().find(|r| r.ty == ty) {
            offer(mime, rep.data.clone());
        }
    }
    if reps().any(|r| SECRET_MARKERS.contains(&r.ty.as_str())) {
        offer(PASSWORD_HINT, b"secret".to_vec());
    }
    for rep in reps().filter(|r| r.ty.contains('/')) {
        offer(&rep.ty, rep.data.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| (*s).to_owned()).collect()
    }

    fn reader(data: &[(&str, &str)]) -> impl Fn(&str) -> Option<Vec<u8>> {
        let data: Vec<(String, Vec<u8>)> = data
            .iter()
            .map(|(t, d)| ((*t).to_owned(), d.as_bytes().to_vec()))
            .collect();
        move |t| {
            data.iter()
                .find(|(have, _)| have == t)
                .map(|(_, d)| d.clone())
        }
    }

    fn rep(ty: &str, data: &str) -> ClipRep {
        ClipRep {
            ty: ty.to_owned(),
            data: data.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_files_copy_becomes_one_item_per_file() {
        let t = types(&["TARGETS", GNOME_FILES, URI_LIST, UTF8_TEXT]);
        let s = snapshot(
            &t,
            reader(&[(
                GNOME_FILES,
                "copy\nfile:///home/me/a.txt\nfile:///home/me/My%20B.png",
            )]),
        );
        assert_eq!(
            s.items,
            [
                vec![rep(TYPE_FILE_URL, "file:///home/me/a.txt")],
                vec![rep(TYPE_FILE_URL, "file:///home/me/My%20B.png")]
            ]
        );
    }

    #[test]
    fn a_uri_list_alone_also_gives_files() {
        let t = types(&[URI_LIST]);
        let s = snapshot(
            &t,
            reader(&[(URI_LIST, "# from somewhere\r\nfile:///tmp/x\r\n")]),
        );
        assert_eq!(s.items, [vec![rep(TYPE_FILE_URL, "file:///tmp/x")]]);
    }

    #[test]
    fn text_copies_keep_text_and_their_other_types() {
        let t = types(&["TARGETS", "UTF8_STRING", UTF8_TEXT, "text/html", "STRING"]);
        let s = snapshot(
            &t,
            reader(&[
                (UTF8_TEXT, "مرحبا"),
                ("UTF8_STRING", "ignored"),
                ("text/html", "<b>مرحبا</b>"),
            ]),
        );
        assert_eq!(
            s.items,
            [vec![
                rep(TYPE_TEXT, "مرحبا"),
                rep("text/html", "<b>مرحبا</b>")
            ]]
        );
    }

    #[test]
    fn a_link_copy_gives_text_and_a_url() {
        let t = types(&[URI_LIST, UTF8_TEXT]);
        let s = snapshot(
            &t,
            reader(&[
                (URI_LIST, "https://example.com/\r\n"),
                (UTF8_TEXT, "https://example.com/"),
            ]),
        );
        assert_eq!(
            s.items,
            [vec![
                rep(TYPE_TEXT, "https://example.com/"),
                rep(TYPE_URL, "https://example.com/")
            ]]
        );
    }

    #[test]
    fn pictures_and_secrets_are_recognised() {
        let t = types(&[PNG, PASSWORD_HINT]);
        let s = snapshot(&t, reader(&[(PNG, "PNGDATA"), (PASSWORD_HINT, "secret")]));
        assert_eq!(
            s.items,
            [vec![rep(SECRET_MARKERS[0], ""), rep(TYPE_PNG, "PNGDATA")]]
        );
        assert_eq!(core_types(&t), [SECRET_MARKERS[0], TYPE_PNG]);
    }

    /// The types `snapshot` reads for a clipboard holding `t`, in order.
    fn reads(t: &[&str]) -> Vec<String> {
        let asked = std::cell::RefCell::new(Vec::new());
        snapshot(&types(t), |ty| {
            asked.borrow_mut().push(ty.to_owned());
            Some(b"x".to_vec())
        });
        asked.into_inner()
    }

    #[test]
    fn an_empty_clipboard_is_an_empty_snapshot() {
        assert!(
            snapshot(&types(&["TARGETS", "TIMESTAMP"]), reader(&[]))
                .items
                .is_empty()
        );
        assert!(reads(&["TARGETS", "TIMESTAMP", "MULTIPLE"]).is_empty());
    }

    #[test]
    fn only_the_best_text_type_is_read() {
        let t = [
            "text/plain",
            UTF8_TEXT,
            "UTF8_STRING",
            "text/html",
            "MULTIPLE",
        ];
        assert_eq!(reads(&t), [UTF8_TEXT, "text/html"]);
    }

    #[test]
    fn the_charset_is_matched_whatever_its_case() {
        assert_eq!(
            reads(&["text/plain;charset=UTF-8", "TARGETS"]),
            ["text/plain;charset=UTF-8"]
        );
        let t = types(&["text/plain;charset=UTF-8"]);
        assert_eq!(core_types(&t), [TYPE_TEXT]);
    }

    #[test]
    fn a_picture_is_read_once() {
        assert_eq!(
            reads(&["image/bmp", PNG, "image/jpeg", "text/html"]),
            [PNG, "text/html"]
        );
    }

    #[test]
    fn files_are_offered_for_files_and_as_paths() {
        let s = ClipSnapshot {
            items: vec![
                vec![rep(TYPE_FILE_URL, "file:///home/me/a.txt")],
                vec![rep(TYPE_FILE_URL, "file:///home/me/My%20B.png")],
            ],
        };
        let o = offers(&s);
        let get = |t: &str| {
            o.iter()
                .find(|(have, _)| have == t)
                .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        };
        assert_eq!(
            get(GNOME_FILES).unwrap(),
            "copy\nfile:///home/me/a.txt\nfile:///home/me/My%20B.png"
        );
        assert_eq!(
            get(URI_LIST).unwrap(),
            "file:///home/me/a.txt\r\nfile:///home/me/My%20B.png\r\n"
        );
        assert_eq!(get(UTF8_TEXT).unwrap(), "/home/me/a.txt\n/home/me/My B.png");
    }

    #[test]
    fn a_snapshot_read_back_offers_what_it_was() {
        let t = types(&[UTF8_TEXT, "text/html"]);
        let read = reader(&[(UTF8_TEXT, "hi"), ("text/html", "<i>hi</i>")]);
        let o = offers(&snapshot(&t, read));
        let names: Vec<&str> = o.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(names, [UTF8_TEXT, "UTF8_STRING", "text/plain", "text/html"]);
    }

    #[test]
    fn secret_snapshots_stay_secret_when_written() {
        let s = ClipSnapshot {
            items: vec![vec![rep(SECRET_MARKERS[0], ""), rep(TYPE_TEXT, "pw")]],
        };
        assert!(
            offers(&s)
                .iter()
                .any(|(t, d)| t == PASSWORD_HINT && d == b"secret")
        );
    }

    #[test]
    fn mac_only_types_are_not_offered() {
        let s = ClipSnapshot {
            items: vec![vec![rep(TYPE_TEXT, "x"), rep("com.apple.flat-rtfd", "...")]],
        };
        assert!(offers(&s).iter().all(|(t, _)| !t.starts_with("com.apple")));
    }
}
