//! The Windows clipboard, and between its formats and the type names
//! `vento_core::clip` works with.
//!
//! Files come in as one item per file with a `public.file-url` each (from
//! `CF_HDROP`); text, links and pictures as the core's types (pictures as
//! PNG, from the `PNG` format or else the bitmap, `CF_DIBV5` or `CF_DIB`);
//! HTML and RTF under their own names with a `windows:` prefix, so a copy
//! taken back from the history is the copy it was. A copy a password manager
//! keeps out of Windows' own clipboard history carries a secret marker.

use std::ptr;
use std::time::Duration;

use vento_core::clip::{
    ClipRep, ClipSnapshot, RepData, SECRET_MARKERS, TYPE_FILE_URL, TYPE_PNG, TYPE_TEXT, TYPE_TIFF,
    TYPE_URL, capture, file_url_to_path, path_to_file_url,
};
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};

use crate::PlatformError;

/// The names [`types`] gives the standard formats vento reads.
const HDROP: &str = "CF_HDROP";
const UNICODE_TEXT: &str = "CF_UNICODETEXT";
const DIB: &str = "CF_DIB";
const DIBV5: &str = "CF_DIBV5";
/// Registered formats: a picture as PNG (browsers, Office, the Snipping
/// Tool), a link (browsers), what Explorer's copy means.
const PNG: &str = "PNG";
const URL: &str = "UniformResourceLocatorW";
const DROP_EFFECT: &str = "Preferred DropEffect";
/// What password managers set to keep a copy out of clipboard history.
const EXCLUDE: &str = "ExcludeClipboardContentFromMonitorProcessing";
const NO_HISTORY: &str = "CanIncludeInClipboardHistory";
const VIEWER_IGNORE: &str = "Clipboard Viewer Ignore";
/// The other formats kept, under `windows:` and their name.
const KEPT: [&str; 3] = [
    "HTML Format",
    "Rich Text Format",
    "Rich Text Format Without Objects",
];
const KEPT_PREFIX: &str = "windows:";
/// `DROPEFFECT_COPY`: pasting copies the files, never moves them.
const DROPEFFECT_COPY: u32 = 1;

/// Whether `types` (as [`types`] names them) mark a secret copy, by name
/// only: a `CanIncludeInClipboardHistory` of 0 is read in [`plan`].
fn named_secret(types: &[String]) -> bool {
    types.iter().any(|t| t == EXCLUDE || t == VIEWER_IGNORE)
}

/// The core type names a clipboard holding `types` reads as, without
/// reading any data.
pub(super) fn core_types(types: &[String]) -> Vec<String> {
    let has = |t: &str| types.iter().any(|have| have == t);
    let mut out = Vec::new();
    if named_secret(types) || has(NO_HISTORY) {
        out.push(SECRET_MARKERS[0].to_owned());
    }
    if has(HDROP) {
        out.push(TYPE_FILE_URL.to_owned());
    }
    if has(UNICODE_TEXT) {
        out.push(TYPE_TEXT.to_owned());
    }
    if has(URL) {
        out.push(TYPE_URL.to_owned());
    }
    if has(PNG) || has(DIBV5) || has(DIB) {
        out.push(TYPE_PNG.to_owned());
    }
    out.extend(
        KEPT.iter()
            .filter(|k| has(k))
            .map(|k| format!("{KEPT_PREFIX}{k}")),
    );
    out
}

/// Where the data of a planned representation comes from.
enum Source {
    /// Already known: a file taken from the drop list, or a marker.
    Known(Vec<u8>),
    /// UTF-16 text in this format, as UTF-8.
    Text(&'static str),
    /// A bitmap in this format, as PNG.
    Bitmap(&'static str),
    /// To be read as it is in this format.
    Read(String),
}

/// The items a clipboard holding `types` reads as: per item, its core
/// type names and where each one's data comes from. Only the file list
/// and the history flag are read here, with `read`.
fn plan(types: &[String], read: &impl Fn(&str) -> Option<Vec<u8>>) -> Vec<Vec<(String, Source)>> {
    let has = |t: &str| types.iter().any(|have| have == t);
    let secret = named_secret(types)
        || (has(NO_HISTORY) && read(NO_HISTORY).is_some_and(|d| d.iter().all(|&b| b == 0)));
    let marker: Vec<(String, Source)> = if secret {
        vec![(SECRET_MARKERS[0].to_owned(), Source::Known(Vec::new()))]
    } else {
        Vec::new()
    };
    let files = has(HDROP)
        .then(|| read(HDROP))
        .flatten()
        .map(|d| drop_list(&d))
        .unwrap_or_default();
    if !files.is_empty() {
        let mut items: Vec<Vec<(String, Source)>> = files
            .into_iter()
            .map(|p| {
                let url = path_to_file_url(std::path::Path::new(&p));
                vec![(TYPE_FILE_URL.to_owned(), Source::Known(url.into_bytes()))]
            })
            .collect();
        items[0].extend(marker);
        return items;
    }
    let mut item = marker;
    if has(UNICODE_TEXT) {
        item.push((TYPE_TEXT.to_owned(), Source::Text(UNICODE_TEXT)));
    }
    if has(URL) {
        item.push((TYPE_URL.to_owned(), Source::Text(URL)));
    }
    if has(PNG) {
        item.push((TYPE_PNG.to_owned(), Source::Read(PNG.to_owned())));
    } else if has(DIBV5) {
        item.push((TYPE_PNG.to_owned(), Source::Bitmap(DIBV5)));
    } else if has(DIB) {
        item.push((TYPE_PNG.to_owned(), Source::Bitmap(DIB)));
    }
    for kept in KEPT.iter().filter(|k| has(k)) {
        item.push((
            format!("{KEPT_PREFIX}{kept}"),
            Source::Read((*kept).to_owned()),
        ));
    }
    if item.is_empty() {
        Vec::new()
    } else {
        vec![item]
    }
}

/// A type's data, as [`capture`] takes it.
struct Bytes(Vec<u8>);

impl RepData for Bytes {
    fn byte_len(&self) -> usize {
        self.0.len()
    }

    fn copy_bytes(&self) -> Vec<u8> {
        self.0.clone()
    }
}

/// The snapshot of a clipboard holding `types`, cut to `limit` bytes as
/// [`capture`] cuts it; each format is read with `read` only when needed.
pub(super) fn snapshot(
    types: &[String],
    limit: usize,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> ClipSnapshot {
    let plan = plan(types, &read);
    let names: Vec<Vec<String>> = plan
        .iter()
        .map(|item| item.iter().map(|(name, _)| name.clone()).collect())
        .collect();
    capture(&names, limit, |i, j| match &plan[i][j].1 {
        Source::Known(data) => Some(Bytes(data.clone())),
        Source::Text(format) => read(format).map(|d| Bytes(from_wide(&d).into_bytes())),
        Source::Bitmap(format) => read(format).and_then(|d| dib_to_png(&d)).map(Bytes),
        Source::Read(format) => read(format).map(Bytes),
    })
}

/// What to put on the clipboard for `snapshot`, as (format name, data):
/// the core's types in their Windows forms, then the kept formats.
pub(super) fn offers(snapshot: &ClipSnapshot) -> Vec<(String, Vec<u8>)> {
    let reps = || snapshot.items.iter().flatten();
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut offer = |format: &str, data: Vec<u8>| {
        if !out.iter().any(|(have, _)| have == format) {
            out.push((format.to_owned(), data));
        }
    };
    let files: Vec<String> = reps()
        .filter(|r| r.ty == TYPE_FILE_URL)
        .filter_map(|r| file_url_to_path(&r.data))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if !files.is_empty() {
        offer(HDROP, drop_files(&files));
        offer(DROP_EFFECT, DROPEFFECT_COPY.to_le_bytes().to_vec());
        offer(UNICODE_TEXT, to_wide(&files.join("\r\n")));
    }
    if let Some(text) = reps().find(|r| r.ty == TYPE_TEXT) {
        offer(UNICODE_TEXT, to_wide(&String::from_utf8_lossy(&text.data)));
    }
    if let Some(link) = reps().find(|r| r.ty == TYPE_URL) {
        let link = String::from_utf8_lossy(&link.data);
        offer(URL, to_wide(&link));
        offer(UNICODE_TEXT, to_wide(&link));
    }
    let picture = reps()
        .find(|r| r.ty == TYPE_PNG)
        .map(|r| r.data.clone())
        .or_else(|| {
            reps()
                .find(|r| r.ty == TYPE_TIFF)
                .and_then(|r| tiff_to_png(&r.data))
        });
    if let Some(png) = picture {
        if let Some(dib) = png_to_dib(&png) {
            offer(DIB, dib);
        }
        offer(PNG, png);
    }
    if reps().any(|r| SECRET_MARKERS.contains(&r.ty.as_str())) {
        offer(EXCLUDE, vec![0]);
        offer(NO_HISTORY, 0u32.to_le_bytes().to_vec());
    }
    for rep in reps() {
        if let Some(name) = rep.ty.strip_prefix(KEPT_PREFIX) {
            offer(name, rep.data.clone());
        }
    }
    out
}

/// UTF-16LE bytes, up to the first NUL, as text.
pub(super) fn from_wide(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// `text` as NUL-terminated UTF-16LE bytes.
fn to_wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The paths of a `CF_HDROP`: a `DROPFILES` header, then the paths, each
/// NUL-terminated, then one more NUL.
pub(super) fn drop_list(data: &[u8]) -> Vec<String> {
    let word = |at: usize| {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (Some(offset), Some(wide)) = (word(0), word(16)) else {
        return Vec::new();
    };
    let Some(list) = data.get(offset as usize..) else {
        return Vec::new();
    };
    let text = if wide != 0 {
        let units: Vec<u16> = list
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(list).into_owned()
    };
    text.split('\0')
        .take_while(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `paths` as a `CF_HDROP`, in UTF-16.
fn drop_files(paths: &[String]) -> Vec<u8> {
    // pFiles, then pt (x, y), fNC, fWide.
    let mut out = Vec::new();
    for word in [20u32, 0, 0, 0, 1] {
        out.extend(word.to_le_bytes());
    }
    for path in paths {
        out.extend(
            path.encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes),
        );
    }
    out.extend([0, 0]);
    out
}

/// A `CF_DIB` or `CF_DIBV5` bitmap as PNG. Its alpha counts only when some
/// pixel has any: a 32-bit bitmap that leaves it at 0 is opaque.
pub(super) fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let header = u32::from_le_bytes(dib.get(..4)?.try_into().ok()?) as usize;
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32::from_le_bytes(dib.get(16..20)?.try_into().ok()?);
    let colours = u32::from_le_bytes(dib.get(32..36)?.try_into().ok()?) as usize;
    // BI_BITFIELDS after a plain 40-byte header: three masks follow it.
    let masks = if header == 40 && compression == 3 {
        12
    } else {
        0
    };
    let palette = if bits <= 8 {
        4 * if colours == 0 { 1 << bits } else { colours }
    } else {
        4 * colours
    };
    let offset = 14 + header + masks + palette;
    let mut file = Vec::with_capacity(14 + dib.len());
    file.extend(b"BM");
    file.extend(((14 + dib.len()) as u32).to_le_bytes());
    file.extend([0; 4]);
    file.extend((offset as u32).to_le_bytes());
    file.extend(dib);
    let image = image::load_from_memory_with_format(&file, image::ImageFormat::Bmp).ok()?;
    let mut rgba = image.to_rgba8();
    if bits == 32 && rgba.pixels().all(|p| p[3] == 0) {
        for p in rgba.pixels_mut() {
            p[3] = 255;
        }
    }
    let mut png = Vec::new();
    rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// A PNG as a `CF_DIB`: 32-bit, bottom row first, for apps that take only
/// bitmaps (Paint).
fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    let rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()?
        .to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut dib = Vec::with_capacity(40 + (w * h * 4) as usize);
    dib.extend(40u32.to_le_bytes());
    dib.extend((w as i32).to_le_bytes());
    dib.extend((h as i32).to_le_bytes());
    dib.extend(1u16.to_le_bytes());
    dib.extend(32u16.to_le_bytes());
    // BI_RGB, then the image size and the rest left at 0.
    dib.extend([0u8; 4]);
    dib.extend((w * h * 4).to_le_bytes());
    dib.extend([0u8; 16]);
    for row in rgba.rows().rev() {
        for p in row {
            dib.extend([p[2], p[1], p[0], p[3]]);
        }
    }
    Some(dib)
}

/// A TIFF (a Mac's picture copy) as PNG.
fn tiff_to_png(tiff: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory_with_format(tiff, image::ImageFormat::Tiff).ok()?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// The clipboard, open; closed when dropped.
struct Open;

impl Open {
    /// Opens the clipboard for `owner`, trying for a moment while another
    /// app holds it.
    fn new(owner: HWND) -> Option<Self> {
        for _ in 0..20 {
            // SAFETY: plain call; the clipboard is closed on drop.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Some(Open);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: opened in `new`.
        unsafe { CloseClipboard() };
    }
}

/// The id of the format named `name`, as [`types`] names it.
pub(super) fn format_id(name: &str) -> u32 {
    match name {
        HDROP => u32::from(CF_HDROP),
        UNICODE_TEXT => u32::from(CF_UNICODETEXT),
        DIB => u32::from(CF_DIB),
        DIBV5 => u32::from(CF_DIBV5),
        _ => {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `wide` is NUL-terminated.
            unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
        }
    }
}

/// The name of the format `id`: the standard ones vento reads by their
/// `CF_` names, the registered ones by theirs; `None` for the rest.
fn format_name(id: u32) -> Option<String> {
    match id {
        _ if id == u32::from(CF_HDROP) => Some(HDROP.into()),
        _ if id == u32::from(CF_UNICODETEXT) => Some(UNICODE_TEXT.into()),
        _ if id == u32::from(CF_DIB) => Some(DIB.into()),
        _ if id == u32::from(CF_DIBV5) => Some(DIBV5.into()),
        0xC000..=0xFFFF => {
            let mut name = [0u16; 128];
            // SAFETY: the buffer's length is passed with it.
            let len = unsafe { GetClipboardFormatNameW(id, name.as_mut_ptr(), name.len() as i32) };
            (len > 0).then(|| String::from_utf16_lossy(&name[..len as usize]))
        }
        _ => None,
    }
}

/// The names of the formats on the open clipboard.
fn types(_open: &Open) -> Vec<String> {
    let mut out = Vec::new();
    let mut id = 0;
    loop {
        // SAFETY: the clipboard is open (`_open`).
        id = unsafe { EnumClipboardFormats(id) };
        if id == 0 {
            break;
        }
        out.extend(format_name(id));
    }
    out
}

/// The data of the format `name` on the open clipboard.
fn read(_open: &Open, name: &str) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open; the global is only read while locked.
    unsafe {
        let handle = GetClipboardData(format_id(name));
        if handle.is_null() {
            return None;
        }
        let size = GlobalSize(handle);
        let data = GlobalLock(handle);
        if data.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
    }
}

/// The clipboard's change count.
pub(super) fn change_count() -> i64 {
    // SAFETY: plain call.
    i64::from(unsafe { GetClipboardSequenceNumber() })
}

/// The core types on the clipboard, without reading any data but the
/// history flag.
pub(super) fn current_types(owner: HWND) -> Vec<String> {
    Open::new(owner).map_or_else(Vec::new, |open| core_types(&types(&open)))
}

/// The clipboard as a snapshot, cut to `limit` bytes.
pub(super) fn read_snapshot(owner: HWND, limit: usize) -> ClipSnapshot {
    let Some(open) = Open::new(owner) else {
        log::warn!("cannot open the clipboard: another app holds it");
        return ClipSnapshot::default();
    };
    let names = types(&open);
    snapshot(&names, limit, |name| read(&open, name))
}

/// Puts `offers` on the clipboard in place of what it holds; returns the
/// new change count.
pub(super) fn write(owner: HWND, offers: &[(String, Vec<u8>)]) -> Result<i64, PlatformError> {
    let open = Open::new(owner)
        .ok_or_else(|| PlatformError::Failed("another app holds the clipboard".into()))?;
    // SAFETY: the clipboard is open and owned by `owner`; each global goes to
    // the clipboard, or is freed when it does not take it.
    unsafe {
        if EmptyClipboard() == 0 {
            return Err(PlatformError::Failed(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        let mut set = 0;
        for (name, data) in offers {
            let global = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1));
            if global.is_null() {
                continue;
            }
            let at = GlobalLock(global);
            if at.is_null() {
                GlobalFree(global);
                continue;
            }
            ptr::copy_nonoverlapping(data.as_ptr(), at.cast::<u8>(), data.len());
            GlobalUnlock(global);
            if SetClipboardData(format_id(name), global as HANDLE).is_null() {
                GlobalFree(global);
            } else {
                set += 1;
            }
        }
        drop(open);
        if set == 0 && !offers.is_empty() {
            return Err(PlatformError::Failed("the clipboard took nothing".into()));
        }
    }
    Ok(change_count())
}

/// One representation, for the platform's copies.
pub(super) fn rep(ty: &str, data: Vec<u8>) -> ClipRep {
    ClipRep {
        ty: ty.to_owned(),
        data,
    }
}

#[cfg(test)]
mod tests {
    use vento_core::clip::MAX_SNAPSHOT_BYTES;

    use super::*;

    fn names(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| (*s).to_owned()).collect()
    }

    fn reader(data: Vec<(&'static str, Vec<u8>)>) -> impl Fn(&str) -> Option<Vec<u8>> {
        move |t| {
            data.iter()
                .find(|(have, _)| *have == t)
                .map(|(_, d)| d.clone())
        }
    }

    #[test]
    fn a_files_copy_becomes_one_item_per_file() {
        let list = drop_files(&[r"C:\a b.txt".into(), r"D:\مرحبا.png".into()]);
        assert_eq!(drop_list(&list), [r"C:\a b.txt", r"D:\مرحبا.png"]);
        let s = snapshot(
            &names(&[HDROP, DROP_EFFECT, "Shell IDList Array"]),
            MAX_SNAPSHOT_BYTES,
            reader(vec![(HDROP, list)]),
        );
        assert_eq!(
            s.items,
            [
                vec![rep(TYPE_FILE_URL, b"file:///C:/a%20b.txt".to_vec())],
                vec![rep(
                    TYPE_FILE_URL,
                    path_to_file_url(std::path::Path::new(r"D:\مرحبا.png")).into_bytes()
                )]
            ]
        );
    }

    #[test]
    fn text_links_and_html_are_kept() {
        let s = snapshot(
            &names(&[UNICODE_TEXT, URL, "HTML Format", "Locale"]),
            MAX_SNAPSHOT_BYTES,
            reader(vec![
                (UNICODE_TEXT, to_wide("مرحبا")),
                (URL, to_wide("https://example.com/")),
                ("HTML Format", b"<b>hi</b>".to_vec()),
            ]),
        );
        assert_eq!(
            s.items,
            [vec![
                rep(TYPE_TEXT, "مرحبا".into()),
                rep(TYPE_URL, b"https://example.com/".to_vec()),
                rep("windows:HTML Format", b"<b>hi</b>".to_vec()),
            ]]
        );
    }

    #[test]
    fn a_bitmap_comes_in_as_png_and_goes_back_out() {
        let png = {
            let image = image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]));
            let mut out = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        let dib = png_to_dib(&png).unwrap();
        let back = dib_to_png(&dib).unwrap();
        let image = image::load_from_memory(&back).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (3, 2));
        assert_eq!(image.get_pixel(0, 0).0, [10, 20, 30, 255]);
        // A bitmap with no PNG beside it is read as one.
        let s = snapshot(
            &names(&[DIB]),
            MAX_SNAPSHOT_BYTES,
            reader(vec![(DIB, dib.clone())]),
        );
        assert_eq!(s.items[0][0].ty, TYPE_PNG);
        // And a PNG goes out with a bitmap for Paint.
        let o = offers(&ClipSnapshot {
            items: vec![vec![rep(TYPE_PNG, png)]],
        });
        let out: Vec<&str> = o.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(out, [DIB, PNG]);
    }

    #[test]
    fn a_bitmap_with_no_alpha_is_opaque() {
        let mut dib = png_to_dib(&{
            let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([9, 8, 7, 255]));
            let mut out = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        })
        .unwrap();
        // As screenshots often are: alpha left at 0.
        let last = dib.len() - 1;
        dib[last] = 0;
        let back = image::load_from_memory(&dib_to_png(&dib).unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(back.get_pixel(0, 0).0, [9, 8, 7, 255]);
    }

    #[test]
    fn a_password_managers_copy_is_secret_and_stays_secret() {
        let t = names(&[UNICODE_TEXT, EXCLUDE]);
        assert_eq!(core_types(&t)[0], SECRET_MARKERS[0]);
        let s = snapshot(
            &t,
            MAX_SNAPSHOT_BYTES,
            reader(vec![(UNICODE_TEXT, to_wide("hunter2"))]),
        );
        assert_eq!(s.items[0][0].ty, SECRET_MARKERS[0]);
        let o = offers(&s);
        assert!(o.iter().any(|(n, _)| n == EXCLUDE));
        // Kept out of history by its flag alone, set to 0.
        let flagged = snapshot(
            &names(&[UNICODE_TEXT, NO_HISTORY]),
            MAX_SNAPSHOT_BYTES,
            reader(vec![(NO_HISTORY, vec![0; 4]), (UNICODE_TEXT, to_wide("x"))]),
        );
        assert_eq!(flagged.items[0][0].ty, SECRET_MARKERS[0]);
        let allowed = snapshot(
            &names(&[UNICODE_TEXT, NO_HISTORY]),
            MAX_SNAPSHOT_BYTES,
            reader(vec![
                (NO_HISTORY, vec![1, 0, 0, 0]),
                (UNICODE_TEXT, to_wide("x")),
            ]),
        );
        assert_eq!(allowed.items[0][0].ty, TYPE_TEXT);
    }

    #[test]
    fn files_go_out_as_a_drop_list_to_copy_and_as_paths() {
        let s = ClipSnapshot {
            items: vec![vec![rep(TYPE_FILE_URL, b"file:///C:/a%20b.txt".to_vec())]],
        };
        let o = offers(&s);
        let get = |n: &str| o.iter().find(|(have, _)| have == n).map(|(_, d)| d.clone());
        assert_eq!(drop_list(&get(HDROP).unwrap()), [r"C:\a b.txt"]);
        assert_eq!(get(DROP_EFFECT).unwrap(), 1u32.to_le_bytes());
        assert_eq!(from_wide(&get(UNICODE_TEXT).unwrap()), r"C:\a b.txt");
    }

    #[test]
    fn other_platforms_types_are_not_offered() {
        let s = ClipSnapshot {
            items: vec![vec![
                rep(TYPE_TEXT, b"x".to_vec()),
                rep("com.apple.flat-rtfd", b"...".to_vec()),
                rep("text/html", b"<i>x</i>".to_vec()),
            ]],
        };
        let o = offers(&s);
        let out: Vec<&str> = o.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(out, [UNICODE_TEXT]);
    }
}
