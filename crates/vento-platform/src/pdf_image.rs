//! Writes the PDF action's drawn pages as image files, where vento encodes
//! them itself (Linux and Windows): PNG, or JPEG on white, recording the
//! resolution they were drawn at.

use std::io::Cursor;

use image::codecs::jpeg::{JpegEncoder, PixelDensity};
use image::{ImageEncoder, RgbImage};
use vento_core::pdf::{ImageKind, ImageOptions, PdfError};

/// The most pixels a page is drawn with (1 GiB of RGBA), as on the Mac: an
/// A0 page at 300 DPI fits; a larger page fails rather than ask for many
/// gigabytes.
pub(crate) const MAX_PIXELS: usize = 1 << 28;

/// The quality JPEG pages are written at, as Convert writes JPEGs.
const JPEG_QUALITY: u8 = 90;

/// `page`, drawn at `opts.dpi`, as the file contents `opts.kind` asks for.
pub(crate) fn encode(page: &RgbImage, opts: &ImageOptions) -> Result<Vec<u8>, PdfError> {
    let encode_failed =
        |e: &dyn std::fmt::Display| PdfError::Backend(format!("couldn't write the image: {e}"));
    match opts.kind {
        ImageKind::Png => png_with_dpi(page, opts.dpi).map_err(|e| encode_failed(&e)),
        ImageKind::Jpeg => jpeg_with_dpi(page, opts.dpi).map_err(|e| encode_failed(&e)),
    }
}

/// `image` as a PNG that records `dpi`.
fn png_with_dpi(image: &RgbImage, dpi: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width(), image.height());
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let per_metre = (f64::from(dpi) / 0.0254).round() as u32;
        encoder.set_pixel_dims(Some(png::PixelDimensions {
            xppu: per_metre,
            yppu: per_metre,
            unit: png::Unit::Meter,
        }));
        let mut writer = encoder.write_header()?;
        writer.write_image_data(image.as_raw())?;
    }
    Ok(out)
}

/// `image` as a JPEG that records `dpi`.
fn jpeg_with_dpi(image: &RgbImage, dpi: u32) -> Result<Vec<u8>, image::ImageError> {
    let mut out = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    encoder.set_pixel_density(PixelDensity::dpi(u16::try_from(dpi).unwrap_or(u16::MAX)));
    encoder.write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out.into_inner())
}
