//! Compressing files and folders into a new zip.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Timelike};
use zip::result::{ZipError, ZipResult};
use zip::write::{FullFileOptions, SimpleFileOptions};
use zip::{CompressionMethod, DateTime, ZipWriter};

use super::job::{ArchiveError, Counting, Made, Progress};
use super::staging::claim_partial_file;
use crate::fileops::{Fingerprint, numbered_name, place_partial, remove_any};

/// Macs scatter these through folders; they carry nothing worth archiving.
const DS_STORE: &str = ".DS_Store";

/// Header id of the Info-ZIP extended timestamp extra field.
const EXTENDED_TIMESTAMP: u16 = 0x5455;
/// Flags byte of that field: only the modified time follows.
const EXTENDED_TIMESTAMP_MODIFIED: u8 = 0x01;

/// Zips `paths` into `dir` under `name` (or the next free `name (2)`, ...).
///
/// Every input becomes a top-level entry named after its own file name, so
/// `Photos/a.jpg` stays inside `Photos/`. Folders keep their empty
/// sub-folders, symlinks are stored as links (never followed), `.DS_Store`
/// files are left out, and unix permissions and modified times are kept.
///
/// The zip is built under a hidden partial name and published in one step; on
/// any error, or when `progress` is cancelled, that partial file is removed and
/// nothing else is touched. Inputs are only read.
pub fn compress(
    paths: &[PathBuf],
    dir: &Path,
    name: &OsStr,
    progress: &Progress,
) -> Result<Made, ArchiveError> {
    if progress.is_cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    let items = collect_inputs(paths, progress)?;
    let total = items.iter().map(Item::size).sum();
    progress.set_total(total);

    let (partial, file) = claim_partial_file(dir, name)?;
    let mut partial = Partial::new(partial);
    write_zip(file, &items, progress)?;
    if progress.is_cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    let output = place_partial(&partial.path, dir, name, false)?;
    partial.published = true;
    let fingerprint = Fingerprint::of(&output)?;
    Ok(Made {
        source: paths[0].clone(),
        output,
        fingerprint,
    })
}

/// A partial file that is removed when dropped, unless it was published.
struct Partial {
    path: PathBuf,
    published: bool,
}

impl Partial {
    fn new(path: PathBuf) -> Self {
        Partial {
            path,
            published: false,
        }
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.published {
            remove_any(&self.path);
        }
    }
}

/// What one zip entry is made from.
enum Kind {
    File { size: u64 },
    Dir,
    Link { target: String },
}

/// One entry to write, noted while walking the inputs so the total is known
/// before any byte is written.
struct Item {
    path: PathBuf,
    /// Entry name with `/` separators and no trailing slash.
    name: String,
    kind: Kind,
    mode: Option<u32>,
    modified: Option<SystemTime>,
}

impl Item {
    fn size(&self) -> u64 {
        match self.kind {
            Kind::File { size } => size,
            Kind::Dir | Kind::Link { .. } => 0,
        }
    }
}

/// Walks `paths` (without following symlinks) into the entries to write.
fn collect_inputs(paths: &[PathBuf], progress: &Progress) -> Result<Vec<Item>, ArchiveError> {
    let mut items = Vec::new();
    let mut top_level = HashSet::new();
    for path in paths {
        let file_name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
        if file_name == DS_STORE {
            continue;
        }
        let name = free_top_level_name(&mut top_level, file_name);
        walk(path, name, &mut items, progress)?;
    }
    if items.is_empty() {
        return Err(ArchiveError::Empty);
    }
    Ok(items)
}

/// `file_name`, or `file_name (2)`, ... when another input already took it, so
/// two inputs from different folders never collide inside the zip.
fn free_top_level_name(taken: &mut HashSet<String>, file_name: &OsStr) -> String {
    let mut name = file_name.to_string_lossy().into_owned();
    let mut n = 2;
    while !taken.insert(name.clone()) {
        name = numbered_name(file_name, n).to_string_lossy().into_owned();
        n += 1;
    }
    name
}

fn walk(
    path: &Path,
    name: String,
    items: &mut Vec<Item>,
    progress: &Progress,
) -> Result<(), ArchiveError> {
    if progress.is_cancelled() {
        return Err(ArchiveError::Cancelled);
    }
    let meta = fs::symlink_metadata(path)?;
    let kind = meta.file_type();
    let modified = meta.modified().ok();
    let mode = unix_mode(&meta);
    if kind.is_symlink() {
        let target = fs::read_link(path)?.to_string_lossy().into_owned();
        items.push(Item {
            path: path.to_path_buf(),
            name,
            kind: Kind::Link { target },
            mode: None,
            modified,
        });
    } else if kind.is_dir() {
        let mut children = fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        children.sort();
        items.push(Item {
            path: path.to_path_buf(),
            name: name.clone(),
            kind: Kind::Dir,
            mode,
            modified,
        });
        for child in children.iter().filter(|child| *child != DS_STORE) {
            let child_name = format!("{name}/{}", child.to_string_lossy());
            walk(&path.join(child), child_name, items, progress)?;
        }
    } else if kind.is_file() {
        items.push(Item {
            path: path.to_path_buf(),
            name,
            kind: Kind::File { size: meta.len() },
            mode,
            modified,
        });
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("cannot compress special file {}", path.display()),
        )
        .into());
    }
    Ok(())
}

#[cfg(unix)]
fn unix_mode(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.mode() & 0o7777)
}

#[cfg(not(unix))]
fn unix_mode(_meta: &fs::Metadata) -> Option<u32> {
    None
}

/// Writes every item into a zip over `file`, then flushes it to disk.
fn write_zip(file: File, items: &[Item], progress: &Progress) -> Result<(), ArchiveError> {
    let mut zip = ZipWriter::new(file);
    for item in items {
        if progress.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        write_item(&mut zip, item, progress)?;
    }
    let file = zip.finish().map_err(zip_error)?;
    file.sync_all()?;
    Ok(())
}

fn write_item(
    zip: &mut ZipWriter<File>,
    item: &Item,
    progress: &Progress,
) -> Result<(), ArchiveError> {
    let mut options = file_options(item).map_err(zip_error)?;
    match &item.kind {
        Kind::Dir => zip.add_directory(&item.name, options).map_err(zip_error),
        Kind::Link { target } => zip
            .add_symlink(&item.name, target, options)
            .map_err(zip_error),
        Kind::File { size } => {
            progress.set_current(leaf(&item.name));
            options = options.large_file(*size >= u64::from(u32::MAX));
            zip.start_file(&item.name, options).map_err(zip_error)?;
            let mut source = File::open(&item.path)?;
            io::copy(&mut source, &mut Counting::new(&mut *zip, progress))
                .map_err(ArchiveError::from_io)?;
            Ok(())
        }
    }
}

/// The last component of an entry name.
fn leaf(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn zip_error(error: ZipError) -> ArchiveError {
    match error {
        ZipError::Io(e) => ArchiveError::from_io(e),
        other => ArchiveError::Io(io::Error::other(other)),
    }
}

/// Options for one entry: deflate, permissions, and the modified time in both
/// forms zip readers look for.
///
/// The DOS date and time field has no time zone and is read as local time, so it
/// is written in local time. The Info-ZIP extended timestamp field holds exact
/// UTC seconds and is preferred by extractors that understand it.
fn file_options(item: &Item) -> ZipResult<FullFileOptions<'static, 'static>> {
    let mut options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    if let Some(mode) = item.mode {
        options = options.unix_permissions(mode);
    }
    let mut options = options.into_full_options();
    if let Some(modified) = item.modified {
        options = options.last_modified_time(dos_time(modified));
        if let Ok(seconds) = i32::try_from(unix_seconds(modified)) {
            let mut data = vec![EXTENDED_TIMESTAMP_MODIFIED];
            data.extend_from_slice(&seconds.to_le_bytes());
            options.add_extra_field(EXTENDED_TIMESTAMP, data, false)?;
        }
    }
    Ok(options)
}

/// Whole seconds since the Unix epoch, negative before it.
fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// The zip DOS timestamp (local time, 2 s resolution) for `time`; the zip
/// default when it falls outside what the format can store (1980 to 2107).
fn dos_time(time: SystemTime) -> DateTime {
    let local = chrono::DateTime::<chrono::Local>::from(time);
    u16::try_from(local.year())
        .ok()
        .and_then(|year| {
            DateTime::from_date_and_time(
                year,
                local.month() as u8,
                local.day() as u8,
                local.hour() as u8,
                local.minute() as u8,
                local.second() as u8,
            )
            .ok()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileops::Fingerprint;
    use std::fs::{self, File};
    use std::io::Read;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, UNIX_EPOCH};

    fn open(zip: &Path) -> zip::ZipArchive<File> {
        zip::ZipArchive::new(File::open(zip).unwrap()).unwrap()
    }

    fn names(zip: &Path) -> Vec<String> {
        let mut archive = open(zip);
        (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
            .collect()
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn compress_one_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, b"hello hello hello hello").unwrap();
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_709_642_096))
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let progress = Progress::new();
        let made = compress(
            std::slice::from_ref(&file),
            dir.path(),
            OsStr::new("a.zip"),
            &progress,
        )
        .unwrap();

        assert_eq!(made.output, dir.path().join("a.zip"));
        assert_eq!(made.source, file);
        let mut zip = open(&made.output);
        assert_eq!(zip.len(), 1);
        let mut entry = zip.by_index(0).unwrap();
        assert_eq!(entry.name().unwrap(), "a.txt");
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello hello hello hello");
        #[cfg(unix)]
        assert_eq!(entry.unix_mode().unwrap() & 0o7777, 0o755);
        let mtime = fs::metadata(&file).unwrap().modified().unwrap();
        // The extended timestamp field holds the exact UTC mtime.
        let extended = entry
            .extra_data_fields()
            .find_map(|field| match field {
                zip::ExtraField::ExtendedTimestamp(stamp) => stamp.mod_time(),
                _ => None,
            })
            .expect("extended timestamp present");
        assert_eq!(i64::from(extended), 1_709_642_096);
        assert!(i64::from(extended).abs_diff(unix_seconds(mtime)) <= 1);
        // The DOS field reads as local time.
        let stamp = entry.last_modified().unwrap();
        let dos = chrono::NaiveDate::from_ymd_opt(
            stamp.year().into(),
            stamp.month().into(),
            stamp.day().into(),
        )
        .unwrap()
        .and_hms_opt(
            stamp.hour().into(),
            stamp.minute().into(),
            stamp.second().into(),
        )
        .unwrap();
        let local = chrono::DateTime::<chrono::Local>::from(mtime).naive_local();
        assert!((dos - local).num_seconds().abs() <= 2);
    }

    #[test]
    fn compress_folder_keeps_top_level_and_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("Photos");
        fs::create_dir_all(photos.join("empty")).unwrap();
        fs::write(photos.join("a.jpg"), b"jpg").unwrap();

        let made = compress(
            &[photos],
            dir.path(),
            OsStr::new("Photos.zip"),
            &Progress::new(),
        )
        .unwrap();

        let mut found = names(&made.output);
        found.sort();
        assert_eq!(found, ["Photos/", "Photos/a.jpg", "Photos/empty/"]);
    }

    #[test]
    fn compress_names_each_input_by_its_own_parent() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("one")).unwrap();
        fs::create_dir_all(dir.path().join("two")).unwrap();
        let a = dir.path().join("one/a.txt");
        let b = dir.path().join("two/b.txt");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();

        let made = compress(
            &[a, b],
            dir.path(),
            OsStr::new("Archive.zip"),
            &Progress::new(),
        )
        .unwrap();

        assert_eq!(names(&made.output), ["a.txt", "b.txt"]);
    }

    #[test]
    fn compress_same_name_from_two_folders_keeps_both() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("one")).unwrap();
        fs::create_dir_all(dir.path().join("two")).unwrap();
        let a = dir.path().join("one/x.txt");
        let b = dir.path().join("two/x.txt");
        fs::write(&a, b"first").unwrap();
        fs::write(&b, b"second").unwrap();

        let made = compress(
            &[a, b],
            dir.path(),
            OsStr::new("Archive.zip"),
            &Progress::new(),
        )
        .unwrap();

        assert_eq!(names(&made.output), ["x.txt", "x (2).txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn compress_stores_symlinks_as_links() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Links");
        fs::create_dir(&folder).unwrap();
        std::os::unix::fs::symlink("target.txt", folder.join("link")).unwrap();

        let made = compress(
            &[folder],
            dir.path(),
            OsStr::new("Links.zip"),
            &Progress::new(),
        )
        .unwrap();

        let mut zip = open(&made.output);
        let mut entry = zip.by_name("Links/link").unwrap();
        assert!(entry.is_symlink());
        let mut target = String::new();
        entry.read_to_string(&mut target).unwrap();
        assert_eq!(target, "target.txt");
    }

    #[test]
    fn compress_leaves_out_ds_store() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Docs");
        fs::create_dir_all(folder.join("sub")).unwrap();
        fs::write(folder.join("a.txt"), b"a").unwrap();
        fs::write(folder.join(".DS_Store"), b"junk").unwrap();
        fs::write(folder.join("sub/.DS_Store"), b"junk").unwrap();

        let made = compress(
            &[folder],
            dir.path(),
            OsStr::new("Docs.zip"),
            &Progress::new(),
        )
        .unwrap();

        let mut found = names(&made.output);
        found.sort();
        assert_eq!(found, ["Docs/", "Docs/a.txt", "Docs/sub/"]);
    }

    #[test]
    fn compress_name_clash_gets_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, b"a").unwrap();
        let old = dir.path().join("Archive.zip");
        fs::write(&old, b"not really a zip").unwrap();

        let made = compress(
            &[file],
            dir.path(),
            OsStr::new("Archive.zip"),
            &Progress::new(),
        )
        .unwrap();

        assert_eq!(made.output, dir.path().join("Archive (2).zip"));
        assert_eq!(fs::read(&old).unwrap(), b"not really a zip");
        assert_eq!(names(&made.output), ["a.txt"]);
        assert_eq!(
            listing(dir.path()),
            ["Archive (2).zip", "Archive.zip", "a.txt"]
        );
    }

    #[test]
    fn compress_cancel_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, vec![7u8; 100_000]).unwrap();
        let before = listing(dir.path());
        let progress = Progress::new();
        progress.cancel();

        let result = compress(&[file], dir.path(), OsStr::new("a.zip"), &progress);

        assert!(matches!(result, Err(ArchiveError::Cancelled)));
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn compress_cancel_midway_removes_partial() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.bin");
        // Incompressible data, so compressing it takes long enough to cancel.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let data: Vec<u8> = (0..32 * 1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect();
        fs::write(&file, data).unwrap();
        let before = listing(dir.path());
        let progress = Progress::new();

        let finished = std::sync::atomic::AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                while progress.snapshot().done == 0 && !finished.load(Ordering::Relaxed) {
                    std::thread::yield_now();
                }
                progress.cancel();
            });
            let result = compress(&[file], dir.path(), OsStr::new("big.zip"), &progress);
            finished.store(true, Ordering::Relaxed);
            result
        });

        assert!(matches!(result, Err(ArchiveError::Cancelled)));
        assert_eq!(listing(dir.path()), before);
        assert!(progress.snapshot().done < progress.snapshot().total);
    }

    #[cfg(unix)]
    #[test]
    fn compress_into_read_only_folder_fails_clean() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), b"a").unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::set_permissions(&out, fs::Permissions::from_mode(0o555)).unwrap();

        let result = compress(
            &[src.join("a.txt")],
            &out,
            OsStr::new("a.zip"),
            &Progress::new(),
        );

        fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(ArchiveError::Io(_))));
        assert!(listing(&out).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn compress_unreadable_input_removes_partial() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        let locked = dir.path().join("locked.txt");
        fs::write(&good, b"good").unwrap();
        fs::write(&locked, b"locked").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let before = listing(dir.path());

        let result = compress(
            &[good, locked.clone()],
            dir.path(),
            OsStr::new("a.zip"),
            &Progress::new(),
        );

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(result, Err(ArchiveError::Io(_))));
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn compress_nothing_but_ds_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join(".DS_Store");
        fs::write(&junk, b"junk").unwrap();

        let result = compress(&[junk], dir.path(), OsStr::new("a.zip"), &Progress::new());

        assert!(matches!(result, Err(ArchiveError::Empty)));
        assert_eq!(listing(dir.path()), [".DS_Store"]);
    }

    #[test]
    fn dos_time_outside_zip_range_uses_default() {
        let before_1980 = UNIX_EPOCH + Duration::from_secs(86_400);
        assert_eq!(dos_time(before_1980), DateTime::default());
        assert_eq!(
            dos_time(UNIX_EPOCH - Duration::from_secs(5)),
            DateTime::default()
        );
    }

    #[test]
    fn progress_reaches_total() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Data");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a"), vec![1u8; 3000]).unwrap();
        fs::write(folder.join("b"), vec![2u8; 5000]).unwrap();
        let progress = Progress::new();

        compress(&[folder], dir.path(), OsStr::new("Data.zip"), &progress).unwrap();

        let snap = progress.snapshot();
        assert_eq!(snap.total, 8000);
        assert_eq!(snap.done, snap.total);
    }

    #[test]
    fn made_fingerprint_matches_output() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, b"a").unwrap();

        let made = compress(&[file], dir.path(), OsStr::new("a.zip"), &Progress::new()).unwrap();

        assert_eq!(Fingerprint::of(&made.output).unwrap(), made.fingerprint);
    }
}
