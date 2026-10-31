//! HEIF on Windows: not yet. libheif does not come with Windows, and WIC
//! reads HEIC only with the HEIF extension from the Store. Convert neither
//! offers HEIC nor reads HEIF; such files are still told apart, so they are
//! refused as HEIF rather than as "not an image". The same interface as
//! `linux/heif.rs`, for the shared `image_io`.

use std::path::Path;

use image::DynamicImage;
use vento_core::convert::ConvertError;

/// How much of a file [`kind`] needs: the `ftyp` box with its brands.
pub(super) const HEAD_LEN: usize = 64;
/// `ftyp` brands of HEVC-coded HEIF, which is what HEIC means.
const HEVC_BRANDS: [&[u8; 4]; 6] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"];
/// `ftyp` brands of any HEIF file: HEVC's, AVIF's and the generic ones.
const HEIF_BRANDS: [&[u8; 4]; 10] = [
    b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1", b"avif", b"avis",
];

/// What kind of HEIF file starts with `head`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Heic,
    /// AVIF, or another HEIF.
    Other,
}

/// What `head`, a file's first [`HEAD_LEN`] bytes, is, by the brands of its
/// `ftyp` box; `None` when not HEIF.
pub(super) fn kind(head: &[u8]) -> Option<Kind> {
    if !brands(head).any(|b| HEIF_BRANDS.contains(&b)) {
        return None;
    }
    Some(if brands(head).any(|b| HEVC_BRANDS.contains(&b)) {
        Kind::Heic
    } else {
        Kind::Other
    })
}

/// The major and compatible brands of the `ftyp` box `head` starts with.
fn brands(head: &[u8]) -> impl Iterator<Item = &[u8; 4]> {
    let size = head
        .get(..4)
        .map_or(0, |s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
    let ftyp = if head.get(4..8) == Some(b"ftyp") {
        &head[..size.min(head.len())]
    } else {
        &[]
    };
    let major = ftyp.get(8..12).and_then(|b| <&[u8; 4]>::try_from(b).ok());
    let (compatible, _) = ftyp.get(16..).unwrap_or_default().as_chunks::<4>();
    major.into_iter().chain(compatible)
}

/// Why HEIF fails here.
fn unsupported() -> ConvertError {
    ConvertError::Unsupported("HEIC and AVIF are not supported on Windows yet".into())
}

/// Never: HEIC is not offered on Windows.
pub(super) fn can_write() -> bool {
    false
}

/// Always fails: HEIF is not read on Windows.
pub(super) fn opens(_path: &Path) -> Result<(), ConvertError> {
    Err(unsupported())
}

/// What `read` would hand back; never made on Windows.
#[allow(dead_code)]
pub(super) struct Decoded {
    pub image: DynamicImage,
    pub icc: Option<Vec<u8>>,
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
}

/// Always fails: HEIF is not read on Windows.
pub(super) fn read(_path: &Path) -> Result<Decoded, ConvertError> {
    Err(unsupported())
}

/// Always fails: HEIC is not written on Windows.
pub(super) fn encode(
    _image: &DynamicImage,
    _icc: Option<&[u8]>,
    _exif: Option<&[u8]>,
    _xmp: Option<&[u8]>,
) -> Result<Vec<u8>, ConvertError> {
    Err(unsupported())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heif_files_are_known_but_not_read() {
        let mut head = 24u32.to_be_bytes().to_vec();
        head.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        assert_eq!(kind(&head), Some(Kind::Heic));
        assert_eq!(kind(b"\x89PNG\r\n\x1a\n"), None);
        assert!(!can_write());
        assert!(matches!(
            opens(Path::new("x.heic")),
            Err(ConvertError::Unsupported(_))
        ));
    }
}
