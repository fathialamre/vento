//! Helpers shared by the tests of the archive modules.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, ZipWriter};

/// The names directly inside `dir`, sorted.
pub(crate) fn listing(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// `path` with its parts joined by `/` on every system.
pub(crate) fn slashed(path: &Path) -> String {
    let parts: Vec<_> = path.iter().map(|part| part.to_string_lossy()).collect();
    parts.join("/")
}

/// Every path below `dir`, relative to it and [`slashed`], sorted.
pub(crate) fn tree(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            out.push(slashed(path.strip_prefix(root).unwrap()));
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// A `mark` callback that accepts everything.
pub(crate) fn no_mark(_: &Path) -> io::Result<()> {
    Ok(())
}

/// One entry of a test zip.
pub(crate) enum ZipEntry<'a> {
    File(&'a str, &'a [u8]),
    Dir(&'a str),
    Link(&'a str, &'a str),
}

/// Options for an entry that is stored, not compressed.
pub(crate) fn stored() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

/// Writes a zip holding `entries` at `path`.
pub(crate) fn make_zip(path: &Path, entries: &[ZipEntry]) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    for entry in entries {
        match entry {
            ZipEntry::File(name, data) => {
                zip.start_file(*name, stored()).unwrap();
                zip.write_all(data).unwrap();
            }
            ZipEntry::Dir(name) => zip.add_directory(*name, stored()).unwrap(),
            ZipEntry::Link(name, target) => zip.add_symlink(*name, *target, stored()).unwrap(),
        }
    }
    zip.finish().unwrap();
}

/// Writes a zip at `path` with one AES-encrypted file, `secret.txt`, under
/// `password`.
pub(crate) fn make_encrypted_with(path: &Path, password: &str) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    zip.start_file(
        "secret.txt",
        SimpleFileOptions::default().with_aes_encryption(AesMode::Aes256, password),
    )
    .unwrap();
    zip.write_all(b"top secret").unwrap();
    zip.finish().unwrap();
}

/// [`make_encrypted_with`] under the password `hunter2`.
pub(crate) fn make_encrypted(path: &Path) {
    make_encrypted_with(path, "hunter2");
}
