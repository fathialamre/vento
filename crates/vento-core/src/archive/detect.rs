use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// An archive format vento can extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Rar,
    SevenZ,
}

/// What a selection of paths turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Extract these archives, each listed once, a multi-volume RAR set by its
    /// first volume.
    Extract(Vec<PathBuf>),
    /// Compress everything into a new zip called `name`, created inside `dir`.
    Compress { dir: PathBuf, name: OsString },
}

/// Name of the file when it can be handled as text (non-UTF-8 names are never
/// archives, nor parts of a volume set).
fn file_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

/// Whether `ext` is an old-style RAR volume extension: `r` and two digits.
fn is_old_volume_ext(ext: &str) -> bool {
    let bytes = ext.as_bytes();
    bytes.len() == 3
        && bytes[0].eq_ignore_ascii_case(&b'r')
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

/// A `base.partN.rar` name split into `(base, "part", "N", "rar")`, each slice
/// keeping its original case.
fn split_part_name(name: &str) -> Option<(&str, &str, &str, &str)> {
    let (rest, ext) = name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("rar") {
        return None;
    }
    let (base, tail) = rest.rsplit_once('.')?;
    let word = tail.get(..4)?;
    let digits = &tail[4..];
    if base.is_empty()
        || !word.eq_ignore_ascii_case("part")
        || digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((base, word, digits, ext))
}

fn signature_matches(format: Format, head: &[u8]) -> bool {
    match format {
        Format::Zip => head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06"),
        Format::Rar => {
            head.starts_with(b"Rar!\x1a\x07\x00") || head.starts_with(b"Rar!\x1a\x07\x01\x00")
        }
        Format::SevenZ => head.starts_with(b"7z\xbc\xaf\x27\x1c"),
    }
}

/// Which archive format `path` is, if it is one.
///
/// Needs both an archive extension (`.zip`, `.rar`, `.7z`, `.rNN`, any case)
/// and the matching signature in the first bytes, so a `.docx` or a split
/// `.7z.001` piece is not an archive. Names with other extensions cost no I/O;
/// folders and unreadable files give `None`.
pub fn detect(path: &Path) -> Option<Format> {
    let ext = path.extension()?.to_str()?;
    let format = if ext.eq_ignore_ascii_case("zip") {
        Format::Zip
    } else if ext.eq_ignore_ascii_case("rar") || is_old_volume_ext(ext) {
        Format::Rar
    } else if ext.eq_ignore_ascii_case("7z") {
        Format::SevenZ
    } else {
        return None;
    };
    let mut head = Vec::with_capacity(8);
    File::open(path).ok()?.take(8).read_to_end(&mut head).ok()?;
    signature_matches(format, &head).then_some(format)
}

/// What to do with `paths`: extract when every one of them is an archive,
/// otherwise compress them all into one zip next to the first.
pub fn plan(paths: &[PathBuf]) -> Plan {
    let all_archives = !paths.is_empty() && paths.iter().all(|path| detect(path).is_some());
    if !all_archives {
        let dir = paths
            .first()
            .and_then(|first| first.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        return Plan::Compress {
            dir,
            name: zip_name(paths),
        };
    }

    let mut archives: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        let first = if detect(path) == Some(Format::Rar) {
            let first = rar_first_volume(path);
            if first != *path && first.exists() {
                first
            } else {
                path.clone()
            }
        } else {
            path.clone()
        };
        if !archives.contains(&first) {
            archives.push(first);
        }
    }
    Plan::Extract(archives)
}

/// File name for the zip made from `paths`: a single file gives its stem, a
/// single folder its full name, several items `Archive`.
pub fn zip_name(paths: &[PathBuf]) -> OsString {
    let mut name = match paths {
        [only] => {
            let part = if only.is_dir() {
                only.file_name()
            } else {
                only.file_stem()
            };
            part.map(OsString::from)
        }
        _ => None,
    }
    .unwrap_or_else(|| OsString::from("Archive"));
    name.push(".zip");
    name
}

/// The first volume of the RAR set `path` belongs to: `x.part3.rar` gives
/// `x.part1.rar` (the digit width kept), `x.r05` gives `x.rar`; any other path
/// comes back unchanged.
pub fn rar_first_volume(path: &Path) -> PathBuf {
    let Some(name) = file_name(path) else {
        return path.to_path_buf();
    };
    if let Some((base, word, digits, ext)) = split_part_name(name) {
        let first = format!("{base}.{word}{:0>width$}.{ext}", 1, width = digits.len());
        return path.with_file_name(first);
    }
    if let Some((base, ext)) = name.rsplit_once('.')
        && !base.is_empty()
        && is_old_volume_ext(ext)
    {
        let rar = if ext.starts_with('R') { "RAR" } else { "rar" };
        return path.with_file_name(format!("{base}.{rar}"));
    }
    path.to_path_buf()
}

/// `first` followed by each next volume of its set that exists on disk,
/// stopping at the first gap.
pub fn rar_volumes(first: &Path) -> Vec<PathBuf> {
    let mut volumes = vec![first.to_path_buf()];
    let Some(name) = file_name(first) else {
        return volumes;
    };

    if let Some((base, word, digits, ext)) = split_part_name(name) {
        let Ok(start) = digits.parse::<u64>() else {
            return volumes;
        };
        for number in start + 1.. {
            let next = first.with_file_name(format!(
                "{base}.{word}{number:0>width$}.{ext}",
                width = digits.len()
            ));
            if !next.exists() {
                break;
            }
            volumes.push(next);
        }
    } else if let Some((base, ext)) = name.rsplit_once('.')
        && !base.is_empty()
        && ext.eq_ignore_ascii_case("rar")
    {
        let letter = if ext.starts_with('R') { 'R' } else { 'r' };
        for number in 0..100 {
            let next = first.with_file_name(format!("{base}.{letter}{number:02}"));
            if !next.exists() {
                break;
            }
            volumes.push(next);
        }
    }
    volumes
}

/// The archive's name without its extension or volume marker: `Photos.zip`
/// gives `Photos`, `x.part1.rar` and `x.rar` give `x`.
pub fn archive_stem(path: &Path) -> OsString {
    if let Some((base, ..)) = file_name(path).and_then(split_part_name) {
        return OsString::from(base);
    }
    path.file_stem().map(OsString::from).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    const ZIP: &[u8] = b"PK\x03\x04rest";
    const RAR4: &[u8] = b"Rar!\x1a\x07\x00rest";
    const RAR5: &[u8] = b"Rar!\x1a\x07\x01\x00rest";
    const SEVENZ: &[u8] = b"7z\xbc\xaf\x27\x1crest";

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn zip_by_extension_and_signature() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(&write(dir.path(), "a.zip", ZIP)), Some(Format::Zip));
        assert_eq!(
            detect(&write(dir.path(), "empty.zip", b"PK\x05\x06")),
            Some(Format::Zip)
        );
    }

    #[test]
    fn rar4_and_rar5_signatures() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(&write(dir.path(), "a.rar", RAR4)), Some(Format::Rar));
        assert_eq!(detect(&write(dir.path(), "b.rar", RAR5)), Some(Format::Rar));
        assert_eq!(detect(&write(dir.path(), "c.r00", RAR4)), Some(Format::Rar));
    }

    #[test]
    fn sevenz_signature() {
        let dir = tempdir().unwrap();
        assert_eq!(
            detect(&write(dir.path(), "a.7z", SEVENZ)),
            Some(Format::SevenZ)
        );
    }

    #[test]
    fn docx_with_zip_signature_is_not_an_archive() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(&write(dir.path(), "Report.docx", ZIP)), None);
    }

    #[test]
    fn zip_extension_without_signature_is_not_an_archive() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(&write(dir.path(), "a.zip", b"hello world")), None);
        assert_eq!(detect(&write(dir.path(), "b.zip", b"")), None);
        assert_eq!(detect(&write(dir.path(), "c.zip", RAR4)), None);
    }

    #[test]
    fn zip_signature_without_extension_is_not_an_archive() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(&write(dir.path(), "noext", ZIP)), None);
    }

    #[test]
    fn uppercase_extension_counts() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(&write(dir.path(), "A.ZIP", ZIP)), Some(Format::Zip));
        assert_eq!(detect(&write(dir.path(), "B.R01", RAR4)), Some(Format::Rar));
    }

    #[test]
    fn split_7z_piece_is_not_an_archive() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(&write(dir.path(), "x.7z.001", SEVENZ)), None);
    }

    #[test]
    fn folders_and_missing_files_are_not_archives() {
        let dir = tempdir().unwrap();
        let folder = dir.path().join("looks.zip");
        fs::create_dir(&folder).unwrap();
        assert_eq!(detect(&folder), None);
        assert_eq!(detect(&dir.path().join("gone.zip")), None);
    }

    #[test]
    fn plan_all_archives_extracts() {
        let dir = tempdir().unwrap();
        let a = write(dir.path(), "a.zip", ZIP);
        let b = write(dir.path(), "b.7z", SEVENZ);
        assert_eq!(
            plan(&[a.clone(), b.clone()]),
            Plan::Extract(vec![a.clone(), b])
        );
        assert_eq!(plan(&[a.clone(), a.clone()]), Plan::Extract(vec![a]));
    }

    #[test]
    fn plan_mixed_compresses_everything() {
        let dir = tempdir().unwrap();
        let a = write(dir.path(), "a.zip", ZIP);
        let b = write(dir.path(), "b.pdf", b"%PDF");
        assert_eq!(
            plan(&[a, b]),
            Plan::Compress {
                dir: dir.path().to_path_buf(),
                name: "Archive.zip".into()
            }
        );
    }

    #[test]
    fn plan_single_file_names_zip_after_stem() {
        let dir = tempdir().unwrap();
        let f = write(dir.path(), "Report.pdf", b"%PDF");
        assert_eq!(
            plan(&[f]),
            Plan::Compress {
                dir: dir.path().to_path_buf(),
                name: "Report.zip".into()
            }
        );
        assert_eq!(
            zip_name(&[write(dir.path(), ".env", b"A=1")]),
            OsString::from(".env.zip")
        );
    }

    #[test]
    fn plan_single_folder_keeps_full_name() {
        let dir = tempdir().unwrap();
        let folder = dir.path().join("my.photos");
        fs::create_dir(&folder).unwrap();
        assert_eq!(
            plan(&[folder]),
            Plan::Compress {
                dir: dir.path().to_path_buf(),
                name: "my.photos.zip".into()
            }
        );
    }

    #[test]
    fn plan_bare_relative_name_uses_current_dir() {
        assert_eq!(
            plan(&[PathBuf::from("no-such-thing.txt")]),
            Plan::Compress {
                dir: PathBuf::from("."),
                name: "no-such-thing.zip".into()
            }
        );
    }

    #[test]
    fn rar_volumes_collapse_to_first() {
        let dir = tempdir().unwrap();
        let p1 = write(dir.path(), "x.part1.rar", RAR5);
        let p2 = write(dir.path(), "x.part2.rar", RAR5);
        assert_eq!(plan(&[p1.clone(), p2]), Plan::Extract(vec![p1]));

        let first = write(dir.path(), "y.part01.rar", RAR5);
        let second = write(dir.path(), "y.part02.rar", RAR5);
        assert_eq!(plan(&[second]), Plan::Extract(vec![first]));

        let old_first = write(dir.path(), "z.rar", RAR4);
        let old_second = write(dir.path(), "z.r00", RAR4);
        assert_eq!(plan(&[old_second]), Plan::Extract(vec![old_first]));
    }

    #[test]
    fn missing_first_volume_keeps_the_dropped_path() {
        let dir = tempdir().unwrap();
        let p2 = write(dir.path(), "x.part2.rar", RAR5);
        assert_eq!(plan(std::slice::from_ref(&p2)), Plan::Extract(vec![p2]));
    }

    #[test]
    fn rar_first_volume_cases() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(rar_first_volume(&p("/d/x.part3.rar")), p("/d/x.part1.rar"));
        assert_eq!(
            rar_first_volume(&p("/d/x.part003.rar")),
            p("/d/x.part001.rar")
        );
        assert_eq!(rar_first_volume(&p("/d/x.r05")), p("/d/x.rar"));
        assert_eq!(rar_first_volume(&p("/d/x.rar")), p("/d/x.rar"));
        assert_eq!(rar_first_volume(&p("/d/x.zip")), p("/d/x.zip"));
        assert_eq!(rar_first_volume(&p("/d/x.rx5")), p("/d/x.rx5"));
    }

    #[test]
    fn rar_volumes_stop_at_gap() {
        let dir = tempdir().unwrap();
        let p1 = write(dir.path(), "x.part1.rar", RAR5);
        let p2 = write(dir.path(), "x.part2.rar", RAR5);
        write(dir.path(), "x.part4.rar", RAR5);
        assert_eq!(rar_volumes(&p1), vec![p1, p2]);
    }

    #[test]
    fn rar_volumes_follow_the_old_scheme() {
        let dir = tempdir().unwrap();
        let first = write(dir.path(), "x.rar", RAR4);
        let r00 = write(dir.path(), "x.r00", RAR4);
        let r01 = write(dir.path(), "x.r01", RAR4);
        write(dir.path(), "x.r03", RAR4);
        assert_eq!(rar_volumes(&first), vec![first.clone(), r00, r01]);

        let lone = write(dir.path(), "lone.rar", RAR4);
        assert_eq!(rar_volumes(&lone), vec![lone]);
    }

    #[test]
    fn archive_stem_cases() {
        let s = |p: &str| archive_stem(Path::new(p));
        assert_eq!(s("/d/Photos.zip"), OsString::from("Photos"));
        assert_eq!(s("/d/x.part1.rar"), OsString::from("x"));
        assert_eq!(s("/d/x.part01.rar"), OsString::from("x"));
        assert_eq!(s("/d/x.rar"), OsString::from("x"));
        assert_eq!(s("/d/x.r00"), OsString::from("x"));
        assert_eq!(s("/d/a.b.7z"), OsString::from("a.b"));
    }
}
