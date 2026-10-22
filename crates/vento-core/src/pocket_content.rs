//! Dropped text, links and images, saved as files the Pocket owns.
//!
//! Every writer creates `dir` if missing, picks a free name through
//! [`unique_destination`] and writes with `File::create_new`, so nothing that
//! already exists is ever replaced.

use crate::fileops::unique_destination;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Image extensions the Pocket accepts.
const IMAGE_EXTS: [&str; 5] = ["png", "tiff", "jpg", "gif", "heic"];
/// Longest host kept in a link's file name, so the name stays within limits.
const MAX_HOST_CHARS: usize = 100;
/// How many names are tried when others keep appearing under us.
const NAME_ATTEMPTS: u32 = 1000;

/// Saves `text` as `Text <timestamp>.txt` (UTF-8) in `dir`.
pub fn save_text(dir: &Path, text: &str, now: SystemTime) -> io::Result<PathBuf> {
    let name = format!("Text {}.txt", timestamp(now));
    write_new(dir, &name, text.as_bytes())
}

/// Saves `url` as a `.webloc` (XML property list with a `URL` key) named
/// `<host> <timestamp>.webloc`, or `Link <timestamp>.webloc` without a host.
pub fn save_url(dir: &Path, url: &str, now: SystemTime) -> io::Result<PathBuf> {
    let host = host_of(url.trim());
    let host = if host.is_empty() { "Link" } else { &host };
    let name = format!("{host} {}.webloc", timestamp(now));
    write_new(dir, &name, &webloc_bytes(url))
}

/// The contents [`save_url`] writes for `url`.
pub fn webloc_bytes(url: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>URL</key><string>{}</string>\n\
         </dict>\n\
         </plist>\n",
        xml_escape(url.trim())
    )
    .into_bytes()
}

/// Saves `bytes` as `Image <timestamp>.<ext>` in `dir`. `ext` must be one of
/// `png`, `tiff`, `jpg`, `gif`, `heic`; anything else is `InvalidInput`.
pub fn save_image(dir: &Path, bytes: &[u8], ext: &str, now: SystemTime) -> io::Result<PathBuf> {
    if !IMAGE_EXTS.contains(&ext) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported image type: {ext}"),
        ));
    }
    let name = format!("Image {}.{ext}", timestamp(now));
    write_new(dir, &name, bytes)
}

/// Creates a new file for `name` (renamed if taken) in `dir` and writes
/// `bytes`. A file whose write fails is removed: this call created it.
fn write_new(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let name = OsString::from(name);
    for _ in 0..NAME_ATTEMPTS {
        let path = unique_destination(dir, &name);
        match fs::File::create_new(&path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(bytes).and_then(|()| file.flush()) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            // Someone took the name between the check and the create.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for the saved item",
    ))
}

/// `YYYY-MM-DD HH.MM.SS` in UTC (dots, not colons: safe in file names).
fn timestamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let in_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}.{:02}.{:02}",
        in_day / 3600,
        in_day % 3600 / 60,
        in_day % 60
    )
}

/// Proleptic Gregorian `(year, month, day)` for days since 1970-01-01
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The host of `url` for use in a file name: the text between `://` and the
/// next `/`, `?`, `#` or `:`, without any `user@` prefix. Empty when there is
/// none. Control characters and path separators are dropped.
fn host_of(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Userinfo can itself contain ':'; drop it before cutting at the port.
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let host = authority.split(':').next().unwrap_or_default();
    host.chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .take(MAX_HOST_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    /// 2026-10-20 05:30:00 UTC.
    fn fixed() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_792_474_200)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn timestamp_is_utc_with_dots() {
        assert_eq!(timestamp(fixed()), "2026-10-20 05.30.00");
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00.00.00");
        // Leap day and the last second of a year.
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_799);
        assert_eq!(timestamp(leap), "2024-02-28 23.59.59");
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(timestamp(leap), "2024-02-29 00.00.00");
        let end = UNIX_EPOCH + Duration::from_secs(1_798_761_599);
        assert_eq!(timestamp(end), "2026-12-31 23.59.59");
    }

    #[test]
    fn save_text_writes_utf8_with_timestamp_name() {
        let d = tempdir().unwrap();
        let dir = d.path().join("saved");
        let path = save_text(&dir, "مرحبا بالعالم", fixed()).unwrap();
        assert_eq!(path, dir.join("Text 2026-10-20 05.30.00.txt"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "مرحبا بالعالم");
    }

    #[test]
    fn save_url_writes_webloc_plist() {
        let d = tempdir().unwrap();
        let path = save_url(d.path(), "https://example.com/a?b=1&c", fixed()).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, "example.com 2026-10-20 05.30.00.webloc");
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("<key>URL</key><string>https://example.com/a?b=1&amp;c</string>"));
        assert!(body.starts_with("<?xml"));
        assert!(body.contains("<plist version=\"1.0\">"));
    }

    #[test]
    fn save_url_escapes_all_xml_specials() {
        let d = tempdir().unwrap();
        let path = save_url(d.path(), "http://h/<a>\"b\"'c'&", fixed()).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("<string>http://h/&lt;a&gt;&quot;b&quot;&apos;c&apos;&amp;</string>")
        );
    }

    #[test]
    fn save_url_without_host_uses_link() {
        let d = tempdir().unwrap();
        for (i, url) in ["not a url", "https:///path", "mailto:a@b.c", ""]
            .into_iter()
            .enumerate()
        {
            let path = save_url(d.path(), url, fixed()).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with("Link 2026-10-20 05.30.00"), "{i}: {name}");
        }
    }

    #[test]
    fn host_extraction() {
        assert_eq!(host_of("https://example.com"), "example.com");
        assert_eq!(host_of("https://example.com:8080/x"), "example.com");
        assert_eq!(host_of("https://example.com?q=1"), "example.com");
        assert_eq!(host_of("https://example.com#frag"), "example.com");
        assert_eq!(host_of("https://user:pw@example.com/x"), "example.com");
        assert_eq!(host_of("https://user@example.com:80"), "example.com");
        assert_eq!(host_of("https://a/b@c"), "a");
        assert_eq!(host_of("nohost"), "");
        assert_eq!(host_of("https://"), "");
    }

    #[test]
    fn save_image_writes_bytes_with_ext() {
        let d = tempdir().unwrap();
        for ext in IMAGE_EXTS {
            let path = save_image(d.path(), &[1, 2, 3], ext, fixed()).unwrap();
            assert_eq!(fs::read(&path).unwrap(), [1, 2, 3]);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with("Image 2026-10-20 05.30.00") && name.ends_with(ext));
        }
    }

    #[test]
    fn save_image_rejects_unknown_ext() {
        let d = tempdir().unwrap();
        for ext in ["exe", "", "png/../x", "PNG", ".png"] {
            let err = save_image(d.path(), &[1], ext, fixed()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{ext:?}");
        }
        assert!(names(d.path()).is_empty());
    }

    #[test]
    fn same_second_twice_gets_unique_names() {
        let d = tempdir().unwrap();
        let first = save_text(d.path(), "one", fixed()).unwrap();
        let second = save_text(d.path(), "two", fixed()).unwrap();
        assert_eq!(first, d.path().join("Text 2026-10-20 05.30.00.txt"));
        assert_eq!(second, d.path().join("Text 2026-10-20 05.30.00 (2).txt"));
        assert_eq!(fs::read_to_string(&first).unwrap(), "one");
        assert_eq!(fs::read_to_string(&second).unwrap(), "two");
    }
}
