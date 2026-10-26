//! WebP encoding for the Convert action, with libwebp: neither ImageIO (on
//! the Mac) nor the `image` crate (on Linux) writes lossy WebP. The colour
//! profile, Exif and XMP go into an extended `RIFF/WEBP` container.

use std::ffi::c_int;
use std::{ptr, slice};

use libwebp_sys::{WebPEncodeRGBA, WebPFree};

/// Lossy quality, 0 to 100: visually lossless, like JPEG and HEIC's 0.9.
const QUALITY: f32 = 90.0;

/// `VP8X` flags, from the WebP container specification.
const ICC_FLAG: u8 = 0x20;
const ALPHA_FLAG: u8 = 0x10;
const EXIF_FLAG: u8 = 0x08;
const XMP_FLAG: u8 = 0x04;

/// Where the Exif block starts in a JPEG `APP1` segment.
const EXIF_PREFIX: &[u8] = b"Exif\0\0";
/// Where the XMP packet starts in a JPEG `APP1` segment.
const XMP_PREFIX: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// What a WebP file carries besides its pixels, as raw blocks.
#[derive(Default)]
pub(crate) struct Metadata<'a> {
    /// The ICC profile the pixels are in.
    pub icc: Option<&'a [u8]>,
    /// Exif, from the TIFF header on.
    pub exif: Option<&'a [u8]>,
    /// An XMP packet.
    pub xmp: Option<&'a [u8]>,
}

/// Encodes premultiplied RGBA pixels, `width * 4` bytes a row, as a WebP
/// file carrying `metadata`; the Exif pixel size is set to the image's.
/// Un-premultiplies `pixels` in place. `None` when libwebp fails.
#[cfg(target_os = "macos")]
pub(crate) fn encode(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    metadata: &Metadata,
) -> Option<Vec<u8>> {
    unpremultiply(pixels);
    encode_rgba(pixels, width, height, metadata)
}

/// [`encode`] for straight (not premultiplied) RGBA pixels.
pub(crate) fn encode_rgba(
    pixels: &[u8],
    width: usize,
    height: usize,
    metadata: &Metadata,
) -> Option<Vec<u8>> {
    let stride = width.checked_mul(4)?;
    if pixels.len() < stride.checked_mul(height)? {
        return None;
    }
    let (w, h, stride) = (
        c_int::try_from(width).ok()?,
        c_int::try_from(height).ok()?,
        c_int::try_from(stride).ok()?,
    );
    let mut out: *mut u8 = ptr::null_mut();
    // SAFETY: `pixels` holds `height` rows of `stride` bytes; libwebp
    // allocates `out`, which is freed below.
    let len = unsafe { WebPEncodeRGBA(pixels.as_ptr(), w, h, stride, QUALITY, &mut out) };
    if out.is_null() {
        return None;
    }
    // SAFETY: libwebp wrote `len` bytes at `out`.
    let simple = (len > 0).then(|| unsafe { slice::from_raw_parts(out, len) }.to_vec());
    // SAFETY: `out` came from libwebp and is not used again.
    unsafe { WebPFree(out.cast()) };
    let simple = simple?;
    if metadata.icc.is_none() && metadata.exif.is_none() && metadata.xmp.is_none() {
        return Some(simple);
    }
    let exif = metadata.exif.map(|block| {
        let mut block = block.to_vec();
        set_exif_size(&mut block, width, height);
        block
    });
    let metadata = Metadata {
        exif: exif.as_deref(),
        ..*metadata
    };
    with_metadata(&simple, width, height, &metadata)
}

/// The Exif (from the TIFF header on) and XMP blocks of a JPEG file.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn jpeg_metadata(jpeg: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let (mut exif, mut xmp) = (None, None);
    if !jpeg.starts_with(&[0xff, 0xd8]) {
        return (None, None);
    }
    let mut at = 2;
    // Metadata segments come before the first scan.
    while let [0xff, marker, hi, lo, ..] = jpeg[at.min(jpeg.len())..] {
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        let Some(payload) = jpeg.get(at + 4..at + 2 + len) else {
            break;
        };
        if marker == 0xe1 {
            if let Some(block) = payload.strip_prefix(EXIF_PREFIX) {
                exif.get_or_insert_with(|| block.to_vec());
            } else if let Some(packet) = payload.strip_prefix(XMP_PREFIX) {
                xmp.get_or_insert_with(|| packet.to_vec());
            }
        }
        at += 2 + len;
    }
    (exif, xmp)
}

/// `webp`, a file as libwebp writes it, rewrapped in an extended container
/// that also carries `metadata`. `None` when `webp` doesn't parse.
fn with_metadata(webp: &[u8], width: usize, height: usize, metadata: &Metadata) -> Option<Vec<u8>> {
    let Metadata { icc, exif, xmp } = *metadata;
    let body = webp
        .strip_prefix(b"RIFF")?
        .get(4..)?
        .strip_prefix(b"WEBP")?;
    let mut flags = 0;
    let mut image = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let fourcc: &[u8; 4] = rest.get(..4)?.try_into().ok()?;
        let size = u32::from_le_bytes(rest.get(4..8)?.try_into().ok()?) as usize;
        let payload = rest.get(8..8 + size)?;
        match fourcc {
            // libwebp's own VP8X: only its alpha flag carries over.
            b"VP8X" => flags |= payload.first()? & ALPHA_FLAG,
            b"ALPH" => {
                flags |= ALPHA_FLAG;
                chunk(&mut image, fourcc, payload);
            }
            b"VP8 " => chunk(&mut image, fourcc, payload),
            // Lossless keeps alpha in the bitstream: bit 28 after the signature.
            b"VP8L" => {
                let header = u32::from_le_bytes(payload.get(1..5)?.try_into().ok()?);
                if header & (1 << 28) != 0 {
                    flags |= ALPHA_FLAG;
                }
                chunk(&mut image, fourcc, payload);
            }
            _ => {}
        }
        rest = rest.get(8 + size + size % 2..).unwrap_or_default();
    }
    if image.is_empty() {
        return None;
    }
    if icc.is_some() {
        flags |= ICC_FLAG;
    }
    if exif.is_some() {
        flags |= EXIF_FLAG;
    }
    if xmp.is_some() {
        flags |= XMP_FLAG;
    }
    let mut vp8x = [0u8; 10];
    vp8x[0] = flags;
    vp8x[4..7].copy_from_slice(&canvas_side(width)?);
    vp8x[7..10].copy_from_slice(&canvas_side(height)?);
    let mut body = b"WEBP".to_vec();
    chunk(&mut body, b"VP8X", &vp8x);
    if let Some(icc) = icc {
        chunk(&mut body, b"ICCP", icc);
    }
    body.extend_from_slice(&image);
    if let Some(exif) = exif {
        chunk(&mut body, b"EXIF", exif);
    }
    if let Some(xmp) = xmp {
        chunk(&mut body, b"XMP ", xmp);
    }
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&u32::try_from(body.len()).ok()?.to_le_bytes());
    file.extend_from_slice(&body);
    Some(file)
}

/// A canvas side as `VP8X` stores it: minus one, 24-bit little-endian.
fn canvas_side(side: usize) -> Option<[u8; 3]> {
    let minus_one = u32::try_from(side.checked_sub(1)?).ok()?;
    let [a, b, c, high] = minus_one.to_le_bytes();
    (high == 0).then_some([a, b, c])
}

/// Appends a RIFF chunk: fourcc, little-endian payload size, payload, and a
/// pad byte when the size is odd.
fn chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
}

/// Rewrites the Exif `PixelXDimension` and `PixelYDimension` in a TIFF
/// block. Leaves what it can't follow as it is.
pub(crate) fn set_exif_size(tiff: &mut [u8], width: usize, height: usize) {
    let big_endian = match tiff.get(..2) {
        Some(b"MM") => true,
        Some(b"II") => false,
        _ => return,
    };
    let read = |tiff: &[u8], at: usize, len: usize| {
        let bytes = tiff.get(at..at.checked_add(len)?)?;
        let fold = |n: usize, b: &u8| n << 8 | usize::from(*b);
        Some(if big_endian {
            bytes.iter().fold(0, fold)
        } else {
            bytes.iter().rev().fold(0, fold)
        })
    };
    // An IFD: an entry count, then twelve bytes an entry: tag, type, value
    // count, and the value itself when it fits in four bytes.
    let entries = |tiff: &[u8], ifd: usize| {
        let count = read(tiff, ifd, 2).unwrap_or(0);
        (0..count).map(move |i| ifd + 2 + i * 12)
    };
    let Some(ifd0) = read(tiff, 4, 4) else {
        return;
    };
    let exif_ifd = entries(tiff, ifd0)
        .find(|&entry| read(tiff, entry, 2) == Some(0x8769))
        .and_then(|entry| read(tiff, entry + 8, 4));
    let Some(exif_ifd) = exif_ifd else {
        return;
    };
    for entry in entries(tiff, exif_ifd) {
        let side = match read(tiff, entry, 2) {
            Some(0xa002) => width,
            Some(0xa003) => height,
            _ => continue,
        };
        // One SHORT or one LONG.
        let len = match (read(tiff, entry + 2, 2), read(tiff, entry + 4, 4)) {
            (Some(3), Some(1)) => 2,
            (Some(4), Some(1)) => 4,
            _ => continue,
        };
        let Some(value) = tiff.get_mut(entry + 8..entry + 8 + len) else {
            continue;
        };
        for (i, byte) in value.iter_mut().enumerate() {
            let shift = if big_endian { len - 1 - i } else { i };
            *byte = (side >> (8 * shift)) as u8;
        }
    }
}

/// Turns premultiplied RGBA into straight RGBA, as libwebp expects.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn unpremultiply(pixels: &mut [u8]) {
    for [r, g, b, a] in pixels.as_chunks_mut::<4>().0 {
        let alpha = u32::from(*a);
        if alpha == 255 {
            continue;
        }
        for c in [r, g, b] {
            *c = match alpha {
                0 => 0,
                _ => ((u32::from(*c) * 255 + alpha / 2) / alpha).min(255) as u8,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le32(bytes: &[u8]) -> u32 {
        u32::from_le_bytes(bytes.try_into().unwrap())
    }

    /// A RIFF file with one chunk per entry, as libwebp would lay it out.
    fn riff(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (fourcc, payload) in chunks {
            body.extend_from_slice(*fourcc);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);
        file
    }

    /// The chunks of a RIFF/WEBP file: fourcc, declared size, payload.
    fn chunks(file: &[u8]) -> Vec<([u8; 4], u32, Vec<u8>)> {
        assert_eq!(&file[0..4], b"RIFF");
        assert_eq!(le32(&file[4..8]) as usize, file.len() - 8, "RIFF size");
        assert_eq!(&file[8..12], b"WEBP");
        let mut out = Vec::new();
        let mut at = 12;
        while at < file.len() {
            let fourcc: [u8; 4] = file[at..at + 4].try_into().unwrap();
            let size = le32(&file[at + 4..at + 8]);
            let payload = file[at + 8..at + 8 + size as usize].to_vec();
            at += 8 + size as usize + size as usize % 2;
            out.push((fourcc, size, payload));
        }
        assert_eq!(at, file.len(), "chunks end with the file");
        out
    }

    #[test]
    fn metadata_goes_into_an_extended_container() {
        let simple = riff(&[(b"VP8 ", b"pixels")]);
        let exif = b"MM\0*odd";
        let xmp = b"<x:xmpmeta/>";
        let metadata = Metadata {
            icc: None,
            exif: Some(exif),
            xmp: Some(xmp),
        };
        let file = with_metadata(&simple, 300, 2, &metadata).unwrap();
        let chunks = chunks(&file);
        let names: Vec<&[u8; 4]> = chunks.iter().map(|(name, _, _)| name).collect();
        assert_eq!(names, [b"VP8X", b"VP8 ", b"EXIF", b"XMP "]);
        // Sizes are the unpadded payload lengths; odd ones get a pad byte.
        assert_eq!(chunks[2].1, 7);
        assert_eq!(chunks[2].2, exif);
        assert_eq!(file.len() % 2, 0);
        assert_eq!(chunks[1].2, b"pixels");
        assert_eq!(chunks[3].2, xmp);
        let vp8x = &chunks[0].2;
        assert_eq!(vp8x.len(), 10);
        assert_eq!(vp8x[0], 0x08 | 0x04, "EXIF and XMP flags, no alpha");
        assert_eq!(vp8x[1..4], [0, 0, 0]);
        // Canvas size minus one, 24-bit little-endian: 299 and 1.
        assert_eq!(vp8x[4..7], [0x2b, 0x01, 0x00]);
        assert_eq!(vp8x[7..10], [0x01, 0x00, 0x00]);
    }

    #[test]
    fn alpha_flag_follows_the_alpha_chunk() {
        // libwebp writes its own VP8X for alpha; it is replaced, not kept.
        let simple = riff(&[
            (b"VP8X", &[0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            (b"ALPH", b"a"),
            (b"VP8 ", b"pixels"),
        ]);
        let metadata = Metadata {
            exif: Some(b"II*\0"),
            ..Metadata::default()
        };
        let file = with_metadata(&simple, 1, 1, &metadata).unwrap();
        let chunks = chunks(&file);
        let names: Vec<&[u8; 4]> = chunks.iter().map(|(name, _, _)| name).collect();
        assert_eq!(names, [b"VP8X", b"ALPH", b"VP8 ", b"EXIF"]);
        assert_eq!(chunks[0].2[0], 0x10 | 0x08, "alpha and EXIF flags");
    }

    /// A TIFF block with an Exif IFD holding a SHORT `PixelXDimension` and
    /// a LONG `PixelYDimension`, both 1.
    fn tiff_with_size(big_endian: bool) -> Vec<u8> {
        let u16b = |n: u16| {
            if big_endian {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let u32b = |n: u32| {
            if big_endian {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let mut tiff = if big_endian {
            b"MM".to_vec()
        } else {
            b"II".to_vec()
        };
        tiff.extend_from_slice(&u16b(42));
        tiff.extend_from_slice(&u32b(8));
        // IFD0 at 8: one entry, the Exif IFD pointer, then no next IFD.
        tiff.extend_from_slice(&u16b(1));
        tiff.extend_from_slice(&u16b(0x8769));
        tiff.extend_from_slice(&u16b(4));
        tiff.extend_from_slice(&u32b(1));
        tiff.extend_from_slice(&u32b(26));
        tiff.extend_from_slice(&u32b(0));
        // The Exif IFD at 26; a SHORT sits in the first two value bytes.
        tiff.extend_from_slice(&u16b(2));
        for (tag, kind) in [(0xa002, 3), (0xa003, 4)] {
            tiff.extend_from_slice(&u16b(tag));
            tiff.extend_from_slice(&u16b(kind));
            tiff.extend_from_slice(&u32b(1));
            let one = if kind == 3 && big_endian { 1 << 16 } else { 1 };
            tiff.extend_from_slice(&u32b(one));
        }
        tiff.extend_from_slice(&u32b(0));
        tiff
    }

    #[test]
    fn exif_size_is_rewritten() {
        let mut tiff = tiff_with_size(true);
        set_exif_size(&mut tiff, 0x1234, 0x0567);
        assert_eq!(tiff[36..40], [0x12, 0x34, 0, 0], "SHORT width");
        assert_eq!(tiff[48..52], [0, 0, 0x05, 0x67], "LONG height");
        let mut tiff = tiff_with_size(false);
        set_exif_size(&mut tiff, 0x1234, 0x0567);
        assert_eq!(tiff[36..40], [0x34, 0x12, 0, 0], "SHORT width");
        assert_eq!(tiff[48..52], [0x67, 0x05, 0, 0], "LONG height");
    }

    #[test]
    fn junk_exif_is_left_alone() {
        let junk = b"MM\0*\xff\xff\xff\xff\0\x01";
        let mut tiff = junk.to_vec();
        set_exif_size(&mut tiff, 1, 1);
        assert_eq!(tiff, junk);
    }

    #[test]
    fn profile_follows_vp8x() {
        let simple = riff(&[
            (b"VP8X", &[0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            (b"ALPH", b"a"),
            (b"VP8 ", b"pixels"),
        ]);
        let metadata = Metadata {
            icc: Some(b"icc"),
            ..Metadata::default()
        };
        let file = with_metadata(&simple, 1, 1, &metadata).unwrap();
        let chunks = chunks(&file);
        let names: Vec<&[u8; 4]> = chunks.iter().map(|(name, _, _)| name).collect();
        assert_eq!(names, [b"VP8X", b"ICCP", b"ALPH", b"VP8 "]);
        assert_eq!(chunks[1].2, b"icc");
        assert_eq!(chunks[0].2[0], 0x20 | 0x10, "ICC and alpha flags");
    }

    #[test]
    fn junk_is_not_rewrapped() {
        let metadata = Metadata {
            icc: Some(b"icc"),
            ..Metadata::default()
        };
        assert_eq!(with_metadata(b"not a webp file", 1, 1, &metadata), None);
    }

    #[test]
    fn unpremultiply_restores_straight_colour() {
        let mut pixels = [100, 50, 0, 128, 7, 8, 9, 0, 10, 20, 30, 255];
        unpremultiply(&mut pixels);
        assert_eq!(pixels, [199, 100, 0, 128, 0, 0, 0, 0, 10, 20, 30, 255]);
    }

    #[test]
    fn jpeg_metadata_finds_exif_and_xmp() {
        let mut jpeg = vec![0xff, 0xd8];
        let mut segment = |marker: u8, payload: &[u8]| {
            jpeg.extend_from_slice(&[0xff, marker]);
            jpeg.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
            jpeg.extend_from_slice(payload);
        };
        segment(0xe0, b"JFIF\0rest");
        segment(0xe1, b"Exif\0\0MM\0*tiff");
        segment(0xe1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>");
        segment(0xda, b"scan");
        jpeg.extend_from_slice(&[0xff, 0xe1, 0, 8, b'E', b'x', b'i', b'f', 0, 0]);
        let (exif, xmp) = jpeg_metadata(&jpeg);
        assert_eq!(exif.as_deref(), Some(&b"MM\0*tiff"[..]));
        assert_eq!(xmp.as_deref(), Some(&b"<x:xmpmeta/>"[..]));
    }

    #[test]
    fn jpeg_without_metadata_has_none() {
        assert_eq!(jpeg_metadata(&[0xff, 0xd8, 0xff, 0xd9]), (None, None));
        assert_eq!(jpeg_metadata(b"junk"), (None, None));
    }

    #[test]
    fn encodes_a_simple_file_without_metadata() {
        let pixels = [200, 100, 50, 255].repeat(4 * 3);
        let file = encode_rgba(&pixels, 4, 3, &Metadata::default()).unwrap();
        let chunks = chunks(&file);
        assert_eq!(chunks.len(), 1);
        assert_eq!(&chunks[0].0, b"VP8 ");
    }
}
