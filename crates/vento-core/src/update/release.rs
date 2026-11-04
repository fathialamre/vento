//! The latest-release JSON from GitHub and the checksum list that goes with it.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::version::Version;

/// The name of the checksum file attached to every release.
const SUMS_NAME: &str = "SHA256SUMS.txt";

/// One file attached to a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    /// The file name, such as `vento-0.3.0-macos-arm64.zip`.
    pub name: String,
    /// Where to download it from (HTTPS).
    pub url: String,
    /// Its size in bytes, as GitHub reports it.
    pub size: u64,
}

/// A published, final release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The version of the tag.
    pub version: Version,
    /// The release notes (Markdown), as written on GitHub.
    pub notes: String,
    /// Every file attached to the release.
    pub assets: Vec<Asset>,
}

/// A newer release that has what this platform needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    /// The version the app would become.
    pub version: Version,
    /// The release notes (Markdown).
    pub notes: String,
    /// The archive to download.
    pub archive: Asset,
    /// The checksum list to verify the archive against.
    pub sums: Asset,
}

/// A platform that releases carry a build for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetPlatform {
    /// macOS on Apple silicon.
    MacArm64,
    /// Windows, x64 and ARM64 alike: the setup installs the build that fits.
    WindowsSetup,
}

impl AssetPlatform {
    /// The end of the archive's file name for this platform.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::MacArm64 => "macos-arm64.zip",
            Self::WindowsSetup => "windows-setup.exe",
        }
    }
}

/// Why a response was not a usable release.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    /// The text is not the JSON of a release (an error body, a rate limit).
    #[error("not a release document: {0}")]
    Json(#[from] serde_json::Error),
    /// A draft, a prerelease, or a tag that is not a plain `vX.Y.Z`.
    #[error("not a final release")]
    NotARelease,
}

/// Why a downloaded file failed its checksum check.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The checksum list has no line for this file name.
    #[error("no checksum listed for the file")]
    NotListed,
    /// The file's hash differs from the listed one.
    #[error("checksum does not match")]
    Mismatch,
    /// The file could not be read.
    #[error("could not read the file: {0}")]
    Io(#[from] io::Error),
}

/// The fields of the GitHub response that matter here.
#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

/// One entry of `assets` in the GitHub response.
#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Reads the body of `GET /repos/{owner}/{repo}/releases/latest`.
/// A draft, a prerelease, or a tag that does not parse is `NotARelease`.
pub fn parse_latest(json: &str) -> Result<Release, ReleaseError> {
    let raw: RawRelease = serde_json::from_str(json)?;
    if raw.draft || raw.prerelease {
        return Err(ReleaseError::NotARelease);
    }
    let version = Version::parse(&raw.tag_name).ok_or(ReleaseError::NotARelease)?;
    Ok(Release {
        version,
        notes: raw.body.unwrap_or_default(),
        assets: raw
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
            })
            .collect(),
    })
}

/// The archive name for a version, such as `vento-0.3.0-macos-arm64.zip`.
pub fn asset_name(v: Version, p: AssetPlatform) -> String {
    format!("vento-{v}-{}", p.suffix())
}

impl Release {
    /// The update this release offers to `running`: only when it is newer and
    /// has both this platform's archive and the checksum list.
    pub fn update_for(&self, running: Version, p: AssetPlatform) -> Option<Update> {
        if self.version <= running {
            return None;
        }
        let wanted = asset_name(self.version, p);
        let find = |name: &str| self.assets.iter().find(|a| a.name == name).cloned();
        Some(Update {
            version: self.version,
            notes: self.notes.clone(),
            archive: find(&wanted)?,
            sums: find(SUMS_NAME)?,
        })
    }
}

/// Reads a `shasum` list (`<hex>  <file>` per line, or `<hex> *<file>` for
/// binary mode) into file name to lowercase hex. Blank and malformed lines
/// are skipped.
pub fn parse_sums(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let hash_end = line
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(line.len());
        let (hash, rest) = line.split_at(hash_end);
        let name = rest.strip_prefix("  ").or_else(|| rest.strip_prefix(" *"));
        match name {
            Some(name) if !hash.is_empty() && !name.is_empty() => {
                map.insert(name.to_string(), hash.to_ascii_lowercase());
            }
            _ => {}
        }
    }
    map
}

/// The SHA-256 of a file as lowercase hex, read in chunks.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Checks `file` against the line for `name` in the checksum list `sums`.
pub fn verify(file: &Path, name: &str, sums: &str) -> Result<(), VerifyError> {
    let listed = parse_sums(sums)
        .remove(name)
        .ok_or(VerifyError::NotListed)?;
    if sha256_file(file)? == listed {
        Ok(())
    } else {
        Err(VerifyError::Mismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::version::Version;
    use std::io::Write;

    const JSON: &str = r####"{"tag_name":"v0.3.0","body":"### Added\n- Updates: x.","draft":false,"prerelease":false,
     "assets":[{"name":"vento-0.3.0-macos-arm64.zip","browser_download_url":"https://e/z","size":10},
               {"name":"SHA256SUMS.txt","browser_download_url":"https://e/s","size":1}]}"####;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn newer_release_with_asset_is_an_update() {
        let r = parse_latest(JSON).unwrap();
        assert_eq!(r.version, v("0.3.0"));
        let u = r.update_for(v("0.2.0"), AssetPlatform::MacArm64).unwrap();
        assert_eq!(u.version, v("0.3.0"));
        assert_eq!(u.notes, "### Added\n- Updates: x.");
        assert_eq!(u.archive.url, "https://e/z");
        assert_eq!(u.archive.size, 10);
        assert_eq!(u.sums.url, "https://e/s");
    }

    #[test]
    fn same_or_older_is_no_update() {
        let r = parse_latest(JSON).unwrap();
        assert!(r.update_for(v("0.3.0"), AssetPlatform::MacArm64).is_none());
        assert!(r.update_for(v("0.4.0"), AssetPlatform::MacArm64).is_none());
    }

    #[test]
    fn missing_platform_asset_is_no_update() {
        let json = JSON.replace("vento-0.3.0-macos-arm64.zip", "vento-0.3.0-windows-x64.zip");
        let r = parse_latest(&json).unwrap();
        assert!(r.update_for(v("0.2.0"), AssetPlatform::MacArm64).is_none());
    }

    #[test]
    fn windows_takes_the_setup_and_the_mac_the_zip() {
        let json = JSON.replace(
            r#"{"name":"SHA256SUMS.txt""#,
            r#"{"name":"vento-0.3.0-windows-setup.exe","browser_download_url":"https://e/w","size":20},
               {"name":"SHA256SUMS.txt""#,
        );
        let r = parse_latest(&json).unwrap();
        let w = r
            .update_for(v("0.2.0"), AssetPlatform::WindowsSetup)
            .unwrap();
        assert_eq!(w.archive.name, "vento-0.3.0-windows-setup.exe");
        assert_eq!(w.archive.url, "https://e/w");
        assert_eq!(w.sums.url, "https://e/s");
        let m = r.update_for(v("0.2.0"), AssetPlatform::MacArm64).unwrap();
        assert_eq!(m.archive.url, "https://e/z");
        // A release with only the Mac's archive has nothing for Windows.
        let mac_only = parse_latest(JSON).unwrap();
        assert!(
            mac_only
                .update_for(v("0.2.0"), AssetPlatform::WindowsSetup)
                .is_none()
        );
    }

    #[test]
    fn missing_sums_is_no_update() {
        let json = JSON.replace("SHA256SUMS.txt", "other.txt");
        let r = parse_latest(&json).unwrap();
        assert!(r.update_for(v("0.2.0"), AssetPlatform::MacArm64).is_none());
    }

    #[test]
    fn prerelease_and_draft_are_not_releases() {
        let pre = JSON.replace(r#""prerelease":false"#, r#""prerelease":true"#);
        assert!(matches!(parse_latest(&pre), Err(ReleaseError::NotARelease)));
        let draft = JSON.replace(r#""draft":false"#, r#""draft":true"#);
        assert!(matches!(
            parse_latest(&draft),
            Err(ReleaseError::NotARelease)
        ));
    }

    #[test]
    fn suffixed_tag_is_not_a_release() {
        let json = JSON.replace("v0.3.0\"", "v0.4.0-beta\"");
        assert!(matches!(
            parse_latest(&json),
            Err(ReleaseError::NotARelease)
        ));
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_latest("{").is_err());
        assert!(parse_latest(r#"{"message":"API rate limit exceeded"}"#).is_err());
    }

    #[test]
    fn asset_name_is_versioned() {
        assert_eq!(
            asset_name(v("0.3.0"), AssetPlatform::MacArm64),
            "vento-0.3.0-macos-arm64.zip"
        );
        assert_eq!(
            asset_name(v("0.3.0"), AssetPlatform::WindowsSetup),
            "vento-0.3.0-windows-setup.exe"
        );
    }

    #[test]
    fn sums_parse_shasum_format() {
        let m = parse_sums("ABC123  vento-0.3.0-macos-arm64.zip\n\nbad line\n");
        assert_eq!(m["vento-0.3.0-macos-arm64.zip"], "abc123");
        assert_eq!(m.len(), 1);
    }

    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn hello_file() -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn sha256_file_hashes_contents() {
        let f = hello_file();
        assert_eq!(sha256_file(f.path()).unwrap(), HELLO);
    }

    #[test]
    fn verify_matches_and_mismatches() {
        let f = hello_file();
        let ok = format!("{HELLO}  a.zip\n");
        assert!(verify(f.path(), "a.zip", &ok).is_ok());
        let bad = format!("{}  a.zip\n", "0".repeat(64));
        assert!(matches!(
            verify(f.path(), "a.zip", &bad),
            Err(VerifyError::Mismatch)
        ));
        assert!(matches!(
            verify(f.path(), "b.zip", &ok),
            Err(VerifyError::NotListed)
        ));
    }

    #[test]
    fn verify_missing_file_is_io() {
        let ok = format!("{HELLO}  a.zip\n");
        assert!(matches!(
            verify(std::path::Path::new("/nonexistent/vento-x"), "a.zip", &ok),
            Err(VerifyError::Io(_))
        ));
    }
}
