//! HEIC for the Convert action, read and written through libheif (AVIF is
//! read too). Reading needs its libde265 plugin and writing its x265 one;
//! without them HEIC is not offered.

use std::path::Path;
use std::sync::OnceLock;

use image::{DynamicImage, RgbImage, RgbaImage};
use libheif_rs::{
    Channel, ColorProfileRaw, ColorSpace, CompressionFormat, EncoderQuality, HeifContext, Image,
    LibHeif, RgbChroma, color_profile_types,
};
use vento_core::convert::ConvertError;

/// HEIC's quality, 0 to 100: the Mac's 0.9.
const QUALITY: u8 = 90;
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
    /// AVIF, or another HEIF Convert does not write.
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

/// libheif, loaded once for the life of the process. Each `LibHeif` loads
/// it and its plugins and unloads them when dropped, and doing both from two
/// threads at once crashes (the wheel asks [`can_write`] on the main thread
/// while a conversion runs on a worker).
fn lib() -> &'static LibHeif {
    static LIB: OnceLock<LibHeif> = OnceLock::new();
    LIB.get_or_init(LibHeif::new)
}

/// Whether libheif can write HEIC here (its x265 plugin is installed).
/// Asked once: the wheel's ring asks on every open.
pub(super) fn can_write() -> bool {
    static CAN: OnceLock<bool> = OnceLock::new();
    *CAN.get_or_init(|| lib().encoder_for_format(CompressionFormat::Hevc).is_ok())
}

/// Whether the HEIF file at `path` opens: its header and primary image.
pub(super) fn opens(path: &Path) -> Result<(), ConvertError> {
    lib();
    let bytes = std::fs::read(path)?;
    let context = HeifContext::read_from_bytes(&bytes).map_err(|_| ConvertError::NotAnImage)?;
    context
        .primary_image_handle()
        .map_err(|_| ConvertError::NotAnImage)?;
    Ok(())
}

/// A HEIF file's primary image, upright, and what came with it.
pub(super) struct Decoded {
    pub image: DynamicImage,
    pub icc: Option<Vec<u8>>,
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
}

/// The primary image of the HEIF file at `path`, upright (libheif applies
/// the file's rotation and mirroring), with its ICC profile, Exif (from the
/// TIFF header on) and XMP.
pub(super) fn read(path: &Path) -> Result<Decoded, ConvertError> {
    let lib = lib();
    let bytes = std::fs::read(path)?;
    let context = HeifContext::read_from_bytes(&bytes).map_err(|_| ConvertError::NotAnImage)?;
    let handle = context
        .primary_image_handle()
        .map_err(|_| ConvertError::NotAnImage)?;
    let alpha = handle.has_alpha_channel();
    let chroma = if alpha {
        RgbChroma::Rgba
    } else {
        RgbChroma::Rgb
    };
    let decoded = lib
        .decode(&handle, ColorSpace::Rgb(chroma), None)
        .map_err(|e| ConvertError::Unsupported(format!("cannot read this HEIF image: {e}")))?;
    let planes = decoded.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| ConvertError::Unsupported("libheif gave no pixels".into()))?;
    let channels = if alpha { 4 } else { 3 };
    let row = plane.width as usize * channels;
    let mut pixels = Vec::with_capacity(row * plane.height as usize);
    for y in 0..plane.height as usize {
        let start = y * plane.stride;
        pixels.extend_from_slice(
            plane
                .data
                .get(start..start + row)
                .ok_or_else(|| ConvertError::Unsupported("libheif gave too few pixels".into()))?,
        );
    }
    let (width, height) = (plane.width, plane.height);
    let image = if alpha {
        RgbaImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgba8)
    } else {
        RgbImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgb8)
    }
    .ok_or(ConvertError::NotAnImage)?;
    let mut exif = None;
    let mut xmp = None;
    for item in handle.all_metadata() {
        if item.item_type.0 == *b"Exif" {
            // Four bytes give where the TIFF header starts after them.
            let skip = item
                .raw_data
                .get(..4)
                .map_or(0, |s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
            exif = exif.or_else(|| item.raw_data.get(4 + skip..).map(<[u8]>::to_vec));
        } else if item.content_type == "application/rdf+xml" {
            xmp = xmp.or(Some(item.raw_data));
        }
    }
    Ok(Decoded {
        image,
        icc: handle.color_profile_raw().map(|profile| profile.data),
        exif,
        xmp,
    })
}

/// `image` as a HEIC file at quality 90, carrying the profile and metadata.
pub(super) fn encode(
    image: &DynamicImage,
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
) -> Result<Vec<u8>, ConvertError> {
    let failed = |e: libheif_rs::HeifError| ConvertError::Encode(format!("HEIC: {e}"));
    let lib = lib();
    let mut encoder = lib
        .encoder_for_format(CompressionFormat::Hevc)
        .map_err(|_| ConvertError::Unsupported("HEIC needs libheif's x265 plugin".into()))?;
    encoder
        .set_quality(EncoderQuality::Lossy(QUALITY))
        .map_err(failed)?;
    let (width, height) = (image.width(), image.height());
    let alpha = image.color().has_alpha();
    let (chroma, pixels, channels) = if alpha {
        (RgbChroma::Rgba, image.to_rgba8().into_raw(), 4)
    } else {
        (RgbChroma::Rgb, image.to_rgb8().into_raw(), 3)
    };
    let mut heif = Image::new(width, height, ColorSpace::Rgb(chroma)).map_err(failed)?;
    heif.create_plane(Channel::Interleaved, width, height, 8)
        .map_err(failed)?;
    {
        let planes = heif.planes_mut();
        let plane = planes
            .interleaved
            .ok_or_else(|| ConvertError::Encode("HEIC: no plane".into()))?;
        let row = width as usize * channels;
        for (y, source) in pixels.chunks_exact(row).enumerate() {
            plane.data[y * plane.stride..][..row].copy_from_slice(source);
        }
    }
    if let Some(icc) = icc {
        heif.set_color_profile_raw(&ColorProfileRaw::new(
            color_profile_types::PROF,
            icc.to_vec(),
        ))
        .map_err(failed)?;
    }
    let mut context = HeifContext::new().map_err(failed)?;
    let handle = context
        .encode_image(&heif, &mut encoder, None)
        .map_err(failed)?;
    if let Some(exif) = exif {
        context.add_exif_metadata(&handle, exif).map_err(failed)?;
    }
    if let Some(xmp) = xmp {
        context.add_xmp_metadata(&handle, xmp).map_err(failed)?;
    }
    context.write_to_bytes().map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compatible.len();
        let mut head = (size as u32).to_be_bytes().to_vec();
        head.extend_from_slice(b"ftyp");
        head.extend_from_slice(major);
        head.extend_from_slice(&[0; 4]);
        for brand in compatible {
            head.extend_from_slice(*brand);
        }
        head.resize(HEAD_LEN, 0);
        head
    }

    #[test]
    fn heic_avif_and_the_rest_are_told_apart() {
        assert_eq!(kind(&ftyp(b"heic", &[b"mif1", b"heic"])), Some(Kind::Heic));
        // A generic major brand: the compatible ones say HEVC.
        assert_eq!(kind(&ftyp(b"mif1", &[b"mif1", b"heic"])), Some(Kind::Heic));
        assert_eq!(kind(&ftyp(b"avif", &[b"mif1", b"avif"])), Some(Kind::Other));
        assert_eq!(
            kind(b"\x89PNG\r\n\x1a\n not a heif file at all, no box"),
            None
        );
        assert_eq!(kind(&[]), None);
    }
}
