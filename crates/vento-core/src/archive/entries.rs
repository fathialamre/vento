//! What every archive format shares when extracting: entry names made safe,
//! the reader interface, the output layout and the extract job itself.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::detect::{Format, archive_stem};
use super::job::{ArchiveError, Made, Progress};
use super::staging::Staging;
use super::zip_read::ZipReader;

/// Folder of macOS metadata that Finder and `ditto` add to zips.
const MAC_METADATA_DIR: &str = "__MACOSX";
/// Prefix of the AppleDouble sidecar files holding one file's macOS metadata.
const APPLE_DOUBLE_PREFIX: &str = "._";
/// Free space kept on top of what an archive declares, for file system
/// overhead and for whatever else is writing to the volume meanwhile.
const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

/// Turns a raw entry name into a path relative to the output folder.
///
/// Both `/` and `\` separate folders (zips made on Windows use `\`); `.` and
/// empty components are dropped. Returns `Ok(None)` for entries that are not
/// extracted: macOS metadata (`__MACOSX/` at any depth, `._name` sidecars)
/// and names with nothing left. Refuses with [`ArchiveError::UnsafeEntry`] any
/// name that could reach outside the output folder: a `..` component, an
/// absolute path, a drive prefix such as `C:`, or a NUL byte.
pub(crate) fn safe_relative(raw: &str) -> Result<Option<PathBuf>, ArchiveError> {
    let refuse = || ArchiveError::UnsafeEntry(raw.to_owned());
    if raw.contains('\0') || raw.starts_with(['/', '\\']) {
        return Err(refuse());
    }
    let mut parts = Vec::new();
    for part in raw.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err(refuse()),
            _ if parts.is_empty() && is_drive_prefix(part) => return Err(refuse()),
            _ => parts.push(part),
        }
    }
    let skipped = parts.contains(&MAC_METADATA_DIR)
        || parts
            .last()
            .is_none_or(|leaf| leaf.starts_with(APPLE_DOUBLE_PREFIX));
    Ok((!skipped).then(|| parts.iter().collect()))
}

/// Whether `part` starts like a Windows drive (`C:`, `c:x`).
fn is_drive_prefix(part: &str) -> bool {
    let bytes = part.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// What an entry is. Archives can hold nothing else that vento creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    /// A regular file.
    File,
    /// A folder.
    Dir,
    /// A symbolic link.
    Symlink,
}

/// One kept entry of an archive, as listed before anything is written.
#[derive(Debug, Clone)]
pub(crate) struct EntryInfo {
    /// Where the entry goes, relative to the output folder; made safe by
    /// [`safe_relative`].
    pub path: PathBuf,
    /// What the entry is.
    pub kind: EntryKind,
    /// Declared uncompressed size in bytes.
    pub size: u64,
    /// Unix permission bits, when the archive stores them.
    pub mode: Option<u32>,
    /// Last modification time, when the archive stores it.
    pub mtime: Option<SystemTime>,
}

/// Reads one archive format; the rest of extraction is format-neutral.
pub(crate) trait ArchiveReader {
    /// Lists the entries that will be extracted, in archive order: sidecars
    /// are already left out and every path is safe.
    fn entries(&mut self) -> Result<Vec<EntryInfo>, ArchiveError>;

    /// Hands every kept entry to `sink`, once each, in archive order.
    fn extract(&mut self, sink: &mut Staging<'_>) -> Result<(), ArchiveError>;
}

/// Where the extracted entries end up beside the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Every entry lies under this one top-level item, which is published on
    /// its own (`Photos.zip` holding `Photos/` gives `Photos/`).
    Direct(PathBuf),
    /// The entries go into a new folder with this name, the archive's stem.
    Wrapped(OsString),
}

/// Decides where `entries` go: [`Layout::Direct`] when they all share one
/// first component, else [`Layout::Wrapped`] in a folder named `stem`.
///
/// A lone top-level symlink is wrapped too: published on its own, a relative
/// link would point at whatever lies beside it.
pub(crate) fn layout(entries: &[EntryInfo], stem: &OsStr) -> Result<Layout, ArchiveError> {
    let first = entries.first().ok_or(ArchiveError::Empty)?;
    let top = first_component(&first.path);
    let shared = top.is_some() && entries.iter().all(|e| first_component(&e.path) == top);
    let top_is_link = entries
        .iter()
        .any(|e| e.kind == EntryKind::Symlink && e.path.parent() == Some(Path::new("")));
    match top {
        Some(top) if shared && !top_is_link => Ok(Layout::Direct(PathBuf::from(top))),
        _ => Ok(Layout::Wrapped(stem.to_os_string())),
    }
}

fn first_component(path: &Path) -> Option<&OsStr> {
    path.iter().next()
}

/// Refuses with [`ArchiveError::NotEnoughSpace`] unless the volume holding
/// `dir` has room for `needed` bytes plus a 64 MiB margin.
pub(crate) fn check_space(dir: &Path, needed: u64) -> Result<(), ArchiveError> {
    let available = fs4::available_space(dir)?;
    if needed.saturating_add(SPACE_MARGIN) > available {
        return Err(ArchiveError::NotEnoughSpace);
    }
    Ok(())
}

/// Opens `path` with the reader for `format`. The reader borrows `password`
/// rather than copying it.
pub(crate) fn open_reader<'p>(
    path: &Path,
    format: Format,
    password: Option<&'p [u8]>,
) -> Result<Box<dyn ArchiveReader + 'p>, ArchiveError> {
    match format {
        Format::Zip => Ok(Box::new(ZipReader::open(path, password)?)),
        Format::SevenZ => Err(ArchiveError::Unsupported("7z".to_owned())),
        Format::Rar => Err(ArchiveError::Unsupported("rar".to_owned())),
    }
}

/// Extracts `archive` into the folder it sits in.
///
/// The entries all go into a new folder named after the archive
/// (`Photos.zip` gives `Photos/`), unless they already share one top-level
/// item, which is then extracted directly. Either way the result gets a free
/// name (`Photos (2)`) and nothing that exists is replaced. macOS metadata
/// entries are skipped.
///
/// The archive is listed first: an unsafe entry name, an empty archive or too
/// little disk space fails before anything is written. Everything is then
/// written into a hidden partial folder, `mark` is called on every item
/// created there (and on the partial folder itself), and the result is
/// published in one step. On any error, or when `progress` is cancelled, the
/// partial folder is removed and nothing else is touched. The archive is only
/// read.
pub fn extract_one(
    archive: &Path,
    format: Format,
    password: Option<&[u8]>,
    progress: &Progress,
    mark: &dyn Fn(&Path) -> io::Result<()>,
) -> Result<Made, ArchiveError> {
    if progress.is_cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    let mut reader = open_reader(archive, format, password)?;
    let entries = reader.entries()?;
    let layout = layout(&entries, &archive_stem(archive))?;
    let dir = folder_of(archive);
    let total = entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .fold(0u64, |sum, e| sum.saturating_add(e.size));
    check_space(dir, total)?;
    progress.set_total(total);

    let mut staging = Staging::new(dir, archive, progress, mark)?;
    reader.extract(&mut staging)?;
    let (output, fingerprint) = staging.publish(layout)?;
    log::info!("extracted {} to {}", archive.display(), output.display());
    Ok(Made {
        source: archive.to_path_buf(),
        output,
        fingerprint,
    })
}

/// The folder `path` sits in; `.` for a bare file name.
fn folder_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(raw: &[&str]) -> Vec<EntryInfo> {
        raw.iter()
            .filter_map(|name| safe_relative(name).unwrap())
            .map(|path| EntryInfo {
                path,
                kind: EntryKind::File,
                size: 0,
                mode: None,
                mtime: None,
            })
            .collect()
    }

    #[test]
    fn safe_relative_keeps_plain_names() {
        assert_eq!(
            safe_relative("Photos/a.jpg").unwrap(),
            Some(PathBuf::from("Photos/a.jpg"))
        );
        assert_eq!(
            safe_relative("./a//b/").unwrap(),
            Some(PathBuf::from("a/b"))
        );
        assert_eq!(safe_relative("").unwrap(), None);
        assert_eq!(safe_relative("./").unwrap(), None);
    }

    #[test]
    fn safe_relative_rejects_escape() {
        for raw in [
            "../a",
            "a/../../b",
            "a/..",
            "/etc/passwd",
            "\\x",
            "C:\\x",
            "c:x",
            "a\0b",
        ] {
            assert!(
                matches!(safe_relative(raw), Err(ArchiveError::UnsafeEntry(_))),
                "{raw:?} should be refused"
            );
        }
    }

    #[test]
    fn safe_relative_skips_mac_sidecars() {
        assert_eq!(safe_relative("__MACOSX/a").unwrap(), None);
        assert_eq!(safe_relative("x/__MACOSX/a").unwrap(), None);
        assert_eq!(safe_relative("x/._a").unwrap(), None);
        assert_eq!(safe_relative("._a").unwrap(), None);
        assert_eq!(
            safe_relative("x/a._b").unwrap(),
            Some(PathBuf::from("x/a._b"))
        );
    }

    #[test]
    fn windows_backslash_names_become_folders() {
        assert_eq!(
            safe_relative("dir\\file.txt").unwrap(),
            Some(PathBuf::from("dir/file.txt"))
        );
    }

    #[test]
    fn layout_single_top_folder_is_direct() {
        let entries = kept(&["Photos/", "Photos/a.jpg", "Photos/sub/b.jpg"]);
        assert_eq!(
            layout(&entries, OsStr::new("Archive")).unwrap(),
            Layout::Direct(PathBuf::from("Photos"))
        );
        let single = kept(&["a.txt"]);
        assert_eq!(
            layout(&single, OsStr::new("Archive")).unwrap(),
            Layout::Direct(PathBuf::from("a.txt"))
        );
    }

    #[test]
    fn layout_many_top_items_wraps_in_stem() {
        let entries = kept(&["a.txt", "b.txt"]);
        assert_eq!(
            layout(&entries, OsStr::new("Archive")).unwrap(),
            Layout::Wrapped(OsString::from("Archive"))
        );
        let mixed = kept(&["Photos/a.jpg", "notes.txt"]);
        assert_eq!(
            layout(&mixed, OsStr::new("Archive")).unwrap(),
            Layout::Wrapped(OsString::from("Archive"))
        );
    }

    #[test]
    fn layout_ignores_skipped_sidecars() {
        let entries = kept(&["Photos/a", "__MACOSX/Photos/._a", "Photos/._b"]);
        assert_eq!(
            layout(&entries, OsStr::new("Archive")).unwrap(),
            Layout::Direct(PathBuf::from("Photos"))
        );
    }

    #[test]
    fn layout_of_nothing_is_empty() {
        assert!(matches!(
            layout(&[], OsStr::new("Archive")),
            Err(ArchiveError::Empty)
        ));
    }

    #[test]
    fn layout_wraps_a_lone_symlink() {
        let mut entries = kept(&["link"]);
        entries[0].kind = EntryKind::Symlink;
        assert_eq!(
            layout(&entries, OsStr::new("Archive")).unwrap(),
            Layout::Wrapped(OsString::from("Archive"))
        );
    }

    #[test]
    fn not_enough_space_refuses_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            check_space(dir.path(), u64::MAX / 2),
            Err(ArchiveError::NotEnoughSpace)
        ));
        assert!(check_space(dir.path(), 0).is_ok());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
