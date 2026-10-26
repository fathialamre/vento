//! Reads and writes image files for the Convert action. The `image` crate
//! reads JPEG, PNG, TIFF, GIF, BMP and WebP and writes all but WebP, which
//! libwebp writes as on the Mac (`crate::webp`).

use std::io::Cursor;
use std::path::Path;

use image::codecs::bmp::BmpEncoder;
use image::codecs::gif::GifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageReader, RgbImage};
use vento_core::convert::{ConvertError, ImageFormat};

use crate::webp;

/// JPEG's quality, 1 to 100: the Mac's 0.9.
const JPEG_QUALITY: u8 = 90;
/// How hard the GIF encoder looks for the 256 colours, 1 (best) to 30.
const GIF_SPEED: i32 = 10;

/// The formats this system writes, in the ring's order.
pub(crate) fn targets() -> Vec<ImageFormat> {
    ImageFormat::TARGETS
        .iter()
        .copied()
        .filter(|&f| f != ImageFormat::Heic)
        .collect()
}

/// The format the file at `path` decodes as, judged by its content: `None`
/// for a readable image of a format Convert does not write, `Err(NotAnImage)`
/// when its header does not decode.
pub(crate) fn probe_image(path: &Path) -> Result<Option<ImageFormat>, ConvertError> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let format = reader.format().ok_or(ConvertError::NotAnImage)?;
    reader
        .into_decoder()
        .map_err(|_| ConvertError::NotAnImage)?;
    Ok(format_of(format))
}

/// Writes the first frame of `src`, upright, into the existing file `dst` as
/// `format`, keeping the colour profile and Exif where `format` holds them
/// (and XMP in WebP). JPEG and BMP get white behind transparent areas.
pub(crate) fn write_image(src: &Path, dst: &Path, format: ImageFormat) -> Result<(), ConvertError> {
    let source = read(src)?;
    let bytes = encode(&source, format)?;
    std::fs::write(dst, bytes)?;
    Ok(())
}

/// A decoded first frame, upright, and what came with it.
struct Source {
    image: DynamicImage,
    icc: Option<Vec<u8>>,
    /// Exif from the TIFF header on, saying "upright" and the upright size.
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
}

fn read(path: &Path) -> Result<Source, ConvertError> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| ConvertError::NotAnImage)?;
    // Missing or unreadable metadata is no reason to fail the conversion.
    let icc = decoder.icc_profile().ok().flatten();
    let mut exif = decoder.exif_metadata().ok().flatten();
    let xmp = decoder.xmp_metadata().ok().flatten();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| ConvertError::NotAnImage)?;
    image.apply_orientation(orientation);
    if let Some(exif) = exif.as_mut() {
        // The pixels are upright now: the tag must not turn them again.
        let _ = Orientation::remove_from_exif_chunk(exif);
        webp::set_exif_size(exif, image.width() as usize, image.height() as usize);
    }
    Ok(Source {
        image,
        icc,
        exif,
        xmp,
    })
}

fn encode(source: &Source, format: ImageFormat) -> Result<Vec<u8>, ConvertError> {
    let image = &source.image;
    let (width, height) = (image.width(), image.height());
    let mut out = Vec::new();
    match format {
        ImageFormat::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
            with_metadata(&mut encoder, source);
            let rgb = on_white(image);
            encoder
                .write_image(rgb.as_raw(), width, height, ExtendedColorType::Rgb8)
                .map_err(encode_failed)?;
        }
        ImageFormat::Png => {
            let mut encoder = PngEncoder::new(&mut out);
            with_metadata(&mut encoder, source);
            let image = storable(image);
            encoder
                .write_image(image.as_bytes(), width, height, image.color().into())
                .map_err(encode_failed)?;
        }
        ImageFormat::Tiff => {
            let mut cursor = Cursor::new(&mut out);
            let mut encoder = TiffEncoder::new(&mut cursor);
            with_metadata(&mut encoder, source);
            let image = storable(image);
            encoder
                .write_image(image.as_bytes(), width, height, image.color().into())
                .map_err(encode_failed)?;
        }
        ImageFormat::Gif => {
            let rgba = image.to_rgba8();
            GifEncoder::new_with_speed(&mut out, GIF_SPEED)
                .encode(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
                .map_err(encode_failed)?;
        }
        ImageFormat::Bmp => {
            let rgb = on_white(image);
            BmpEncoder::new(&mut out)
                .write_image(rgb.as_raw(), width, height, ExtendedColorType::Rgb8)
                .map_err(encode_failed)?;
        }
        ImageFormat::Webp => {
            let rgba = image.to_rgba8();
            let metadata = webp::Metadata {
                icc: source.icc.as_deref(),
                exif: source.exif.as_deref(),
                xmp: source.xmp.as_deref(),
            };
            out = webp::encode_rgba(rgba.as_raw(), width as usize, height as usize, &metadata)
                .ok_or_else(|| ConvertError::Encode("WebP encoding failed".into()))?;
        }
        ImageFormat::Heic => {
            return Err(ConvertError::Unsupported(
                "HEIC is not supported on this system".into(),
            ));
        }
    }
    Ok(out)
}

/// Gives `encoder` the profile and Exif, where it can hold them.
fn with_metadata(encoder: &mut impl ImageEncoder, source: &Source) {
    if let Some(icc) = &source.icc {
        let _ = encoder.set_icc_profile(icc.clone());
    }
    if let Some(exif) = &source.exif {
        let _ = encoder.set_exif_metadata(exif.clone());
    }
}

/// `image` in a colour type PNG and TIFF store: floating-point images (an
/// HDR source) become 16 bits a channel.
fn storable(image: &DynamicImage) -> std::borrow::Cow<'_, DynamicImage> {
    use std::borrow::Cow;
    match image {
        DynamicImage::ImageRgb32F(_) => Cow::Owned(DynamicImage::ImageRgb16(image.to_rgb16())),
        DynamicImage::ImageRgba32F(_) => Cow::Owned(DynamicImage::ImageRgba16(image.to_rgba16())),
        _ => Cow::Borrowed(image),
    }
}

/// `image` as 8-bit RGB, transparent areas laid on white.
fn on_white(image: &DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let a = u16::from(a);
        let over = |c: u8| ((u16::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        image::Rgb([over(r), over(g), over(b)])
    })
}

fn format_of(format: image::ImageFormat) -> Option<ImageFormat> {
    match format {
        image::ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
        image::ImageFormat::Png => Some(ImageFormat::Png),
        image::ImageFormat::WebP => Some(ImageFormat::Webp),
        image::ImageFormat::Tiff => Some(ImageFormat::Tiff),
        image::ImageFormat::Gif => Some(ImageFormat::Gif),
        image::ImageFormat::Bmp => Some(ImageFormat::Bmp),
        _ => None,
    }
}

fn encode_failed(e: image::ImageError) -> ConvertError {
    ConvertError::Encode(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// Exif saying "turn 90° clockwise to show" (orientation 6), from the
    /// TIFF header on.
    const EXIF_ROTATE_90: [u8; 26] = [
        b'I', b'I', 0x2a, 0, 8, 0, 0, 0, // header, IFD0 at 8
        1, 0, // one entry
        0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, // Orientation = 6
        0, 0, 0, 0, // no next IFD
    ];
    /// Stands in for a colour profile: the codecs carry its bytes as they are.
    const ICC: &[u8] = b"not really a profile, but carried as one";

    /// A 4 x 2 picture: opaque red on the left half, transparent on the right.
    fn half_clear() -> RgbaImage {
        RgbaImage::from_fn(4, 2, |x, _| {
            if x < 2 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 0])
            }
        })
    }

    fn convert(dir: &Path, src: &Path, format: ImageFormat) -> std::path::PathBuf {
        let dst = dir.join(format!("out.{format:?}"));
        std::fs::write(&dst, b"").unwrap();
        write_image(src, &dst, format).unwrap();
        dst
    }

    fn decoder(path: &Path) -> impl ImageDecoder {
        ImageReader::open(path)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap()
    }

    #[test]
    fn every_target_writes_a_file_of_its_format() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        half_clear().save(&src).unwrap();
        for format in targets() {
            let out = convert(dir.path(), &src, format);
            assert_eq!(probe_image(&out).unwrap(), Some(format), "{format:?}");
            let image = image::open(&out).unwrap();
            assert_eq!((image.width(), image.height()), (4, 2), "{format:?}");
        }
    }

    #[test]
    fn jpeg_and_bmp_lay_transparency_on_white() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        half_clear().save(&src).unwrap();
        for format in [ImageFormat::Jpeg, ImageFormat::Bmp] {
            let image = image::open(convert(dir.path(), &src, format)).unwrap().to_rgb8();
            let [r, g, b] = image.get_pixel(3, 0).0;
            assert!(r > 240 && g > 240 && b > 240, "{format:?}: {r} {g} {b}");
            let [r, g, b] = image.get_pixel(0, 0).0;
            assert!(r > 200 && g < 50 && b < 50, "{format:?}: {r} {g} {b}");
        }
        // PNG keeps it.
        let png = image::open(convert(dir.path(), &src, ImageFormat::Png)).unwrap().to_rgba8();
        assert_eq!(png.get_pixel(3, 0).0[3], 0);
    }

    #[test]
    fn a_turned_photo_comes_out_upright_with_its_profile_and_exif() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.jpg");
        let rgb = DynamicImage::ImageRgba8(half_clear()).to_rgb8();
        let mut file = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut file, 95);
        encoder.set_icc_profile(ICC.to_vec()).unwrap();
        encoder.set_exif_metadata(EXIF_ROTATE_90.to_vec()).unwrap();
        encoder
            .write_image(rgb.as_raw(), 4, 2, ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::write(&src, file).unwrap();

        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Webp] {
            let out = convert(dir.path(), &src, format);
            let mut decoder = decoder(&out);
            assert_eq!(decoder.dimensions(), (2, 4), "{format:?}: turned");
            assert_eq!(decoder.icc_profile().unwrap().as_deref(), Some(ICC), "{format:?}");
            let exif = decoder.exif_metadata().unwrap().expect("Exif kept");
            assert_eq!(
                Orientation::from_exif_chunk(&exif),
                Some(Orientation::NoTransforms),
                "{format:?}: upright"
            );
        }
    }

    #[test]
    fn a_text_file_is_not_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("notes.png");
        std::fs::write(&src, "hello").unwrap();
        assert!(matches!(probe_image(&src), Err(ConvertError::NotAnImage)));
    }

    #[test]
    fn heic_is_not_offered_without_libheif() {
        assert!(!targets().contains(&ImageFormat::Heic));
        assert_eq!(targets().len(), ImageFormat::TARGETS.len() - 1);
    }
}
