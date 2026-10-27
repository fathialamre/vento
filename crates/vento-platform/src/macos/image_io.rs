//! Reading and writing image files with ImageIO, for the Convert action.
//! Core Foundation, Core Graphics and ImageIO are thread-safe, so both
//! functions run on any thread.

use std::ffi::c_void;
use std::fs::{self, File};
use std::path::Path;
use std::sync::OnceLock;
use std::{ptr, slice};

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{
    CFDictionary, CFMutableData, CFMutableDictionary, CFNumber, CFRetained, CFString, CFType,
    CFURL, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGBitmapContextGetColorSpace,
    CGBitmapContextGetData, CGColorSpace, CGColorSpaceModel, CGContext, CGImage, CGImageAlphaInfo,
    kCGColorSpaceSRGB,
};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationLossyCompressionQuality,
    kCGImagePropertyDPIHeight, kCGImagePropertyDPIWidth, kCGImagePropertyExifDictionary,
    kCGImagePropertyExifPixelXDimension, kCGImagePropertyExifPixelYDimension,
    kCGImagePropertyGPSDictionary, kCGImagePropertyIPTCDictionary, kCGImagePropertyOrientation,
    kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth, kCGImagePropertyTIFFDictionary,
    kCGImagePropertyTIFFOrientation, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceThumbnailMaxPixelSize,
};
use vento_core::convert::{ConvertError, ImageFormat};

use crate::webp;

/// Property dictionaries as ImageIO hands them out: string keys, any values.
type Properties = CFDictionary<CFString, CFType>;

/// JPEG and HEIC quality: visually lossless, about half the size of 1.0.
const LOSSY_QUALITY: f64 = 0.9;

/// The longest side a WebP image can have.
const WEBP_MAX_SIDE: usize = 16383;

/// The format the file at `path` decodes as, judged by its content, not its
/// name. `None` for a readable image of another format (AVIF, RAW).
pub(crate) fn probe_image(path: &Path) -> Result<Option<ImageFormat>, ConvertError> {
    // Worker threads live long: drain what ImageIO autoreleases per file.
    autoreleasepool(|_| {
        let source = open_source(path)?;
        // SAFETY: `source` is a valid image source.
        let Some(uti) = (unsafe { source.r#type() }) else {
            return Ok(None);
        };
        Ok(format_of_type(&uti.to_string()))
    })
}

/// Writes the first frame of `src`, upright, into the existing file `dst` as
/// `format`, keeping the source's metadata and colour profile (GIF and BMP,
/// which carry no profile, get the colours matched to sRGB instead).
pub(crate) fn write_image(src: &Path, dst: &Path, format: ImageFormat) -> Result<(), ConvertError> {
    // Worker threads live long: drain what ImageIO autoreleases per file.
    autoreleasepool(|_| write(src, dst, format))
}

fn write(src: &Path, dst: &Path, format: ImageFormat) -> Result<(), ConvertError> {
    if format == ImageFormat::Heic && !heic_encoder_works() {
        return Err(heic_unsupported());
    }
    let source = open_source(src)?;
    // SAFETY: `source` is a valid image source; no options.
    let source_props = unsafe { source.properties_at_index(0, None) }
        // SAFETY: ImageIO property dictionaries have string keys.
        .map(|props| unsafe { CFRetained::cast_unchecked::<Properties>(props) });
    let image = upright_first_frame(&source, source_props.as_deref())?;
    encode_with(&image, source_props.as_deref(), dst, format, None)
}

/// Writes `image` into `dst` as `format`, without metadata; `dpi`, when
/// given, is recorded as the image's resolution (except in WebP, written
/// without it). `dst` may or may not exist;
/// its contents are replaced. JPEG goes on white, as Convert's does. The
/// caller drains the autorelease pool.
pub(crate) fn encode(
    image: &CGImage,
    dst: &Path,
    format: ImageFormat,
    dpi: Option<u32>,
) -> Result<(), ConvertError> {
    if format == ImageFormat::Heic && !heic_encoder_works() {
        return Err(heic_unsupported());
    }
    encode_with(image, None, dst, format, dpi)
}

fn heic_unsupported() -> ConvertError {
    ConvertError::Unsupported("HEIC is not supported on this Mac".into())
}

/// [`encode`], carrying over `source`'s metadata (the image's own, when it
/// came from a file).
fn encode_with(
    image: &CGImage,
    source: Option<&Properties>,
    dst: &Path,
    format: ImageFormat,
    dpi: Option<u32>,
) -> Result<(), ConvertError> {
    if format == ImageFormat::Webp {
        return write_webp(image, source, dst);
    }
    let redrawn = match format {
        // JPEG keeps no transparency: lay the picture on white.
        ImageFormat::Jpeg if has_alpha(image) => Some(redraw(image, Background::White, false)?),
        // GIF and BMP carry no colour profile: match the pixels to sRGB.
        ImageFormat::Bmp => Some(redraw(image, Background::White, true)?),
        ImageFormat::Gif => Some(redraw(image, Background::Clear, true)?),
        _ => None,
    };
    let image = redrawn.as_deref().unwrap_or(image);
    let size = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let props = destination_properties(source, format, size);
    if let Some(dpi) = dpi {
        let dpi = CFNumber::new_i64(i64::from(dpi));
        // SAFETY: the keys are ImageIO's own statics.
        unsafe {
            props.set(kCGImagePropertyDPIWidth, &dpi);
            props.set(kCGImagePropertyDPIHeight, &dpi);
        }
    }
    let failed = || ConvertError::Encode(format!("couldn't write {}", format.name()));
    let uti = CFString::from_static_str(type_of_format(format));
    let url = CFURL::from_file_path(dst).ok_or_else(failed)?;
    // SAFETY: valid URL and type; no options.
    let dest = unsafe { CGImageDestination::with_url(&url, &uti, 1, None) }.ok_or_else(failed)?;
    // SAFETY: `props` holds ImageIO property keys and plist values.
    unsafe { dest.add_image(image, Some(props.as_opaque())) };
    // SAFETY: exactly one image was added, as declared.
    if unsafe { dest.finalize() } {
        Ok(())
    } else {
        Err(failed())
    }
}

/// WebP goes through libwebp, as ImageIO reads it but can't write it. The
/// pixels are drawn in the image's own colour space (or sRGB, as `redraw`
/// falls back to), and that space's profile goes into the file.
fn write_webp(
    image: &CGImage,
    source: Option<&Properties>,
    dst: &Path,
) -> Result<(), ConvertError> {
    let failed = || ConvertError::Encode("couldn't write WebP".into());
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    if w > WEBP_MAX_SIDE || h > WEBP_MAX_SIDE {
        return Err(ConvertError::Unsupported(format!(
            "WebP can't be larger than {WEBP_MAX_SIDE} pixels a side"
        )));
    }
    let context =
        drawing_context(image, CGImageAlphaInfo::PremultipliedLast, false).ok_or_else(failed)?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
    CGContext::clear_rect(Some(&context), rect);
    CGContext::draw_image(Some(&context), rect, Some(image));
    let icc = CGBitmapContextGetColorSpace(Some(&context))
        .and_then(|space| CGColorSpace::icc_data(Some(&space)))
        .map(|data| data.to_vec());
    let data = CGBitmapContextGetData(Some(&context)).cast::<u8>();
    if data.is_null() {
        return Err(failed());
    }
    // SAFETY: the context owns `h` rows of `w * 4` bytes, as `bitmap_context`
    // set it up, and outlives the slice.
    let pixels = unsafe { slice::from_raw_parts_mut(data, w * h * 4) };
    let (exif, xmp) = source
        .map(|source| metadata_blocks(source, (w, h)))
        .unwrap_or_default();
    let metadata = webp::Metadata {
        icc: icc.as_deref(),
        exif: exif.as_deref(),
        xmp: xmp.as_deref(),
    };
    let webp = webp::encode(pixels, w, h, &metadata).ok_or_else(failed)?;
    fs::write(dst, webp)?;
    Ok(())
}

/// The source's Exif and XMP as the raw blocks a WebP file carries, saying
/// upright. ImageIO hands metadata out only as dictionaries, so it writes
/// them into a 1×1 JPEG in memory and the blocks are read back from that.
fn metadata_blocks(
    source: &Properties,
    size: (usize, usize),
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    // SAFETY: the keys are ImageIO's own statics.
    let keys = unsafe {
        [
            kCGImagePropertyExifDictionary,
            kCGImagePropertyTIFFDictionary,
            kCGImagePropertyGPSDictionary,
            kCGImagePropertyIPTCDictionary,
        ]
    };
    if !keys.iter().any(|key| source.get(key).is_some()) {
        return (None, None);
    }
    let jpeg = || {
        // SAFETY: a valid system colour space name.
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
        let context = bitmap_context(1, 1, &space, CGImageAlphaInfo::NoneSkipLast)?;
        let image = CGBitmapContextCreateImage(Some(&context))?;
        let props = destination_properties(Some(source), ImageFormat::Jpeg, size);
        let data = CFMutableData::new(None, 0)?;
        let uti = CFString::from_static_str(type_of_format(ImageFormat::Jpeg));
        // SAFETY: valid data and type; no options.
        let dest = unsafe { CGImageDestination::with_data(&data, &uti, 1, None) }?;
        // SAFETY: `props` holds ImageIO property keys and plist values.
        unsafe { dest.add_image(&image, Some(props.as_opaque())) };
        // SAFETY: exactly one image was added, as declared.
        unsafe { dest.finalize() }.then(|| data.to_vec())
    };
    jpeg().map_or((None, None), |bytes| webp::jpeg_metadata(&bytes))
}

fn open_source(path: &Path) -> Result<CFRetained<CGImageSource>, ConvertError> {
    let source = CFURL::from_file_path(path)
        // SAFETY: valid URL; no options.
        .and_then(|url| unsafe { CGImageSource::with_url(&url, None) })
        // SAFETY: `source` is a valid image source.
        .filter(|source| unsafe { source.count() } > 0);
    match source {
        Some(source) => Ok(source),
        // ImageIO says the same for a locked file as for a text file; only
        // the second is not an image.
        None => match File::open(path) {
            Err(e) => Err(ConvertError::Io(e)),
            Ok(_) => Err(ConvertError::NotAnImage),
        },
    }
}

/// The error for a file that probed as an image but won't decode.
fn unreadable() -> ConvertError {
    ConvertError::Encode("couldn't read the image".into())
}

/// The first frame at full size with its orientation applied to the pixels,
/// so it shows upright without any orientation tag.
fn upright_first_frame(
    source: &CGImageSource,
    props: Option<&Properties>,
) -> Result<CFRetained<CGImage>, ConvertError> {
    let dimension = |key: &CFString| props.and_then(|p| number(p, key)).unwrap_or(0);
    // SAFETY: the keys are ImageIO's own statics.
    let longest = unsafe {
        dimension(kCGImagePropertyPixelWidth).max(dimension(kCGImagePropertyPixelHeight))
    };
    let longest = if longest > 0 {
        longest
    } else {
        // No size in the metadata: decode the frame to learn it.
        // SAFETY: `source` is a valid image source; no options.
        let image = unsafe { source.image_at_index(0, None) }.ok_or_else(unreadable)?;
        CGImage::width(Some(&image)).max(CGImage::height(Some(&image))) as i64
    };
    let yes = CFNumber::new_i32(1);
    let max = CFNumber::new_i64(longest);
    // SAFETY: the keys are ImageIO's own statics.
    let keys: [&CFString; 3] = unsafe {
        [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceThumbnailMaxPixelSize,
        ]
    };
    let values: [&CFType; 3] = [&yes, &yes, &max];
    let options = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    // SAFETY: `options` holds thumbnail option keys with number values.
    unsafe { source.thumbnail_at_index(0, Some(options.as_opaque())) }.ok_or_else(unreadable)
}

fn has_alpha(image: &CGImage) -> bool {
    !matches!(
        CGImage::alpha_info(Some(image)),
        CGImageAlphaInfo::None | CGImageAlphaInfo::NoneSkipLast | CGImageAlphaInfo::NoneSkipFirst
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Background {
    /// Transparent pixels become white; the result has no alpha.
    White,
    /// Transparency is kept.
    Clear,
}

/// `image` drawn into an 8-bit RGB bitmap: in sRGB when `srgb` (for formats
/// that carry no colour profile) or when its own space is not RGB, else in
/// its own space.
fn redraw(
    image: &CGImage,
    background: Background,
    srgb: bool,
) -> Result<CFRetained<CGImage>, ConvertError> {
    let failed = || ConvertError::Encode("couldn't draw the image".into());
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let alpha = match background {
        Background::White => CGImageAlphaInfo::NoneSkipLast,
        Background::Clear => CGImageAlphaInfo::PremultipliedLast,
    };
    let context = drawing_context(image, alpha, srgb).ok_or_else(failed)?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
    if background == Background::White {
        CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(Some(&context), rect);
    }
    CGContext::draw_image(Some(&context), rect, Some(image));
    CGBitmapContextCreateImage(Some(&context)).ok_or_else(failed)
}

/// An 8-bit RGB bitmap the size of `image` to draw it in: in sRGB when
/// `srgb` or when the image's own space is not RGB, else in its own space.
fn drawing_context(
    image: &CGImage,
    alpha: CGImageAlphaInfo,
    srgb: bool,
) -> Option<CFRetained<CGContext>> {
    let own = CGImage::color_space(Some(image))
        .filter(|space| CGColorSpace::model(Some(space)) == CGColorSpaceModel::RGB);
    // SAFETY: a valid system colour space name.
    let srgb_space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let candidates = [own.filter(|_| !srgb), Some(srgb_space)];
    // An exotic own space (extended range, say) has no 8-bit bitmap; sRGB does.
    candidates
        .iter()
        .flatten()
        .find_map(|space| bitmap_context(w, h, space, alpha))
}

fn bitmap_context(
    width: usize,
    height: usize,
    space: &CGColorSpace,
    alpha: CGImageAlphaInfo,
) -> Option<CFRetained<CGContext>> {
    // SAFETY: a null buffer makes the context allocate (and own) its pixels;
    // 8 bits per component and 4 bytes per pixel match `alpha`'s layouts.
    unsafe {
        CGBitmapContextCreate(
            ptr::null_mut::<c_void>(),
            width,
            height,
            8,
            width * 4,
            Some(space),
            alpha.0,
        )
    }
}

/// The source's metadata, upright and with the output's pixel `size`, plus
/// the encoder settings for `format`.
fn destination_properties(
    source: Option<&Properties>,
    format: ImageFormat,
    size: (usize, usize),
) -> CFRetained<CFMutableDictionary<CFString, CFType>> {
    let props = source
        .and_then(copy_mutable)
        .unwrap_or_else(CFMutableDictionary::empty);
    let upright = CFNumber::new_i32(1);
    // SAFETY: the keys are ImageIO's own statics.
    let (orientation, tiff_key, tiff_orientation) = unsafe {
        (
            kCGImagePropertyOrientation,
            kCGImagePropertyTIFFDictionary,
            kCGImagePropertyTIFFOrientation,
        )
    };
    // The pixels are already turned; a leftover tag would turn them again.
    props.set(orientation, &upright);
    let tiff = props
        .get(tiff_key)
        .and_then(|v| v.downcast::<CFDictionary>().ok())
        // SAFETY: ImageIO property dictionaries have string keys.
        .and_then(|d| copy_mutable(unsafe { d.cast_unchecked::<CFString, CFType>() }));
    if let Some(tiff) = tiff {
        tiff.set(tiff_orientation, &upright);
        props.set(tiff_key, &tiff);
    }
    // SAFETY: the keys are ImageIO's own statics.
    let (exif_key, exif_width, exif_height) = unsafe {
        (
            kCGImagePropertyExifDictionary,
            kCGImagePropertyExifPixelXDimension,
            kCGImagePropertyExifPixelYDimension,
        )
    };
    // Turning the pixels may have swapped the sides.
    let exif = props
        .get(exif_key)
        .and_then(|v| v.downcast::<CFDictionary>().ok())
        // SAFETY: ImageIO property dictionaries have string keys.
        .and_then(|d| copy_mutable(unsafe { d.cast_unchecked::<CFString, CFType>() }));
    if let Some(exif) = exif {
        exif.set(exif_width, &CFNumber::new_i64(size.0 as i64));
        exif.set(exif_height, &CFNumber::new_i64(size.1 as i64));
        props.set(exif_key, &exif);
    }
    if matches!(format, ImageFormat::Jpeg | ImageFormat::Heic) {
        let quality = CFNumber::new_f64(LOSSY_QUALITY);
        // SAFETY: an ImageIO static.
        props.set(
            unsafe { kCGImageDestinationLossyCompressionQuality },
            &quality,
        );
    }
    props
}

fn copy_mutable(dict: &Properties) -> Option<CFRetained<CFMutableDictionary<CFString, CFType>>> {
    // SAFETY: the copy keeps the original's string keys and CF values.
    unsafe { CFMutableDictionary::new_copy(None, 0, Some(dict.as_opaque())) }
        .map(|d| unsafe { CFRetained::cast_unchecked(d) })
}

fn number(props: &Properties, key: &CFString) -> Option<i64> {
    props.get(key)?.downcast::<CFNumber>().ok()?.as_i64()
}

/// Whether this Mac can encode HEIC. ImageIO lists the type even where no
/// HEVC encoder runs (some virtual machines), so a 1×1 image is encoded in
/// memory once to find out.
fn heic_encoder_works() -> bool {
    static WORKS: OnceLock<bool> = OnceLock::new();
    *WORKS.get_or_init(|| autoreleasepool(|_| trial_heic_encode().is_some()))
}

fn trial_heic_encode() -> Option<()> {
    // SAFETY: a valid system colour space name.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let context = bitmap_context(1, 1, &space, CGImageAlphaInfo::NoneSkipLast)?;
    let image = CGBitmapContextCreateImage(Some(&context))?;
    let data = CFMutableData::new(None, 0)?;
    let uti = CFString::from_static_str(type_of_format(ImageFormat::Heic));
    // SAFETY: valid data and type; no options.
    let dest = unsafe { CGImageDestination::with_data(&data, &uti, 1, None) }?;
    // SAFETY: no properties.
    unsafe { dest.add_image(&image, None) };
    // SAFETY: exactly one image was added, as declared.
    (unsafe { dest.finalize() } && !data.is_empty()).then_some(())
}

/// The uniform type identifier ImageIO writes `format` as.
fn type_of_format(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "public.jpeg",
        ImageFormat::Png => "public.png",
        ImageFormat::Heic => "public.heic",
        ImageFormat::Webp => "org.webmproject.webp",
        ImageFormat::Tiff => "public.tiff",
        ImageFormat::Gif => "com.compuserve.gif",
        ImageFormat::Bmp => "com.microsoft.bmp",
    }
}

fn format_of_type(uti: &str) -> Option<ImageFormat> {
    Some(match uti {
        "public.jpeg" => ImageFormat::Jpeg,
        "public.png" => ImageFormat::Png,
        "public.heic" | "public.heif" => ImageFormat::Heic,
        "org.webmproject.webp" => ImageFormat::Webp,
        "public.tiff" => ImageFormat::Tiff,
        "com.compuserve.gif" => ImageFormat::Gif,
        "com.microsoft.bmp" => ImageFormat::Bmp,
        _ => return None,
    })
}

/// Builds test images with ImageIO itself, so no binary fixtures are kept.
#[cfg(test)]
pub(super) mod test_support {
    use std::path::Path;

    use super::*;
    use objc2_core_foundation::CFBoolean;
    use objc2_image_io::{kCGImagePropertyExifDateTimeOriginal, kCGImagePropertyExifDictionary};

    pub(super) enum Fill {
        /// Opaque, in the fixture's colour space, components 0 to 1.
        Color([f64; 3]),
        /// Fully transparent.
        Clear,
    }

    /// An image file to write: a single colour, with optional metadata.
    pub(super) struct Fixture {
        pub width: usize,
        pub height: usize,
        pub fill: Fill,
        pub space: &'static CFString,
        pub sixteen_bit: bool,
        pub frames: usize,
        /// The top-level orientation tag.
        pub orientation: Option<i32>,
        /// `{Exif}.DateTimeOriginal`.
        pub date: Option<&'static str>,
    }

    impl Fixture {
        pub fn new(width: usize, height: usize) -> Fixture {
            Fixture {
                width,
                height,
                fill: Fill::Color([0.8, 0.3, 0.1]),
                // SAFETY: a Core Graphics static.
                space: unsafe { kCGColorSpaceSRGB },
                sixteen_bit: false,
                frames: 1,
                orientation: None,
                date: None,
            }
        }

        pub fn write(&self, path: &Path, format: ImageFormat) {
            let image = self.image();
            let props = CFMutableDictionary::<CFString, CFType>::empty();
            if let Some(orientation) = self.orientation {
                let orientation = CFNumber::new_i32(orientation);
                props.set(unsafe { kCGImagePropertyOrientation }, &orientation);
                let tiff = CFMutableDictionary::<CFString, CFType>::empty();
                tiff.set(unsafe { kCGImagePropertyTIFFOrientation }, &orientation);
                props.set(unsafe { kCGImagePropertyTIFFDictionary }, &tiff);
            }
            if let Some(date) = self.date {
                let exif = CFMutableDictionary::<CFString, CFType>::empty();
                exif.set(
                    unsafe { kCGImagePropertyExifDateTimeOriginal },
                    &CFString::from_str(date),
                );
                props.set(unsafe { kCGImagePropertyExifDictionary }, &exif);
            }
            let url = CFURL::from_file_path(path).unwrap();
            let uti = CFString::from_static_str(type_of_format(format));
            let dest = unsafe { CGImageDestination::with_url(&url, &uti, self.frames, None) }
                .expect("destination");
            for _ in 0..self.frames {
                unsafe { dest.add_image(&image, Some(props.as_opaque())) };
            }
            assert!(unsafe { dest.finalize() }, "fixture {format:?} not written");
        }

        fn image(&self) -> CFRetained<CGImage> {
            let (bits, alpha) = match self.fill {
                Fill::Clear => (8, CGImageAlphaInfo::PremultipliedLast),
                Fill::Color(_) => (8, CGImageAlphaInfo::NoneSkipLast),
            };
            let bits = if self.sixteen_bit { 16 } else { bits };
            let space = CGColorSpace::with_name(Some(self.space)).unwrap();
            let context = unsafe {
                CGBitmapContextCreate(
                    ptr::null_mut(),
                    self.width,
                    self.height,
                    bits,
                    self.width * 4 * bits / 8,
                    Some(&space),
                    alpha.0,
                )
            }
            .expect("fixture bitmap");
            if let Fill::Color([r, g, b]) = self.fill {
                CGContext::set_fill_color_space(Some(&context), Some(&space));
                unsafe { CGContext::set_fill_color(Some(&context), [r, g, b, 1.0].as_ptr()) };
                let size = CGSize::new(self.width as f64, self.height as f64);
                CGContext::fill_rect(Some(&context), CGRect::new(CGPoint::new(0.0, 0.0), size));
            }
            CGBitmapContextCreateImage(Some(&context)).unwrap()
        }
    }

    fn source(path: &Path) -> CFRetained<CGImageSource> {
        open_source(path).expect("readable image")
    }

    pub(super) fn properties(path: &Path) -> CFRetained<Properties> {
        let props = unsafe { source(path).properties_at_index(0, None) }.expect("properties");
        unsafe { CFRetained::cast_unchecked(props) }
    }

    pub(super) fn dictionary(props: &Properties, key: &CFString) -> Option<CFRetained<Properties>> {
        let dict = props.get(key)?.downcast::<CFDictionary>().ok()?;
        Some(unsafe { CFRetained::cast_unchecked(dict) })
    }

    pub(super) fn string(props: &Properties, key: &CFString) -> Option<String> {
        Some(props.get(key)?.downcast::<CFString>().ok()?.to_string())
    }

    pub(super) fn boolean(props: &Properties, key: &CFString) -> Option<bool> {
        Some(props.get(key)?.downcast::<CFBoolean>().ok()?.as_bool())
    }

    pub(super) fn frame_count(path: &Path) -> usize {
        unsafe { source(path).count() }
    }

    /// The top-left pixel of the first frame drawn in sRGB, as 8-bit
    /// premultiplied RGBA.
    pub(in crate::macos) fn corner_pixel(path: &Path) -> [u8; 4] {
        corner_pixel_in(path, unsafe { kCGColorSpaceSRGB })
    }

    /// The top-left pixel of the first frame drawn in the named colour space,
    /// as 8-bit premultiplied RGBA.
    pub(super) fn corner_pixel_in(path: &Path, space: &CFString) -> [u8; 4] {
        let image = unsafe { source(path).image_at_index(0, None) }.expect("frame");
        let space = CGColorSpace::with_name(Some(space)).unwrap();
        let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
        let mut pixels = vec![0u8; w * h * 4];
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                w,
                h,
                8,
                w * 4,
                Some(&space),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .expect("bitmap");
        let size = CGSize::new(w as f64, h as f64);
        CGContext::draw_image(
            Some(&context),
            CGRect::new(CGPoint::new(0.0, 0.0), size),
            Some(&image),
        );
        drop(context);
        [pixels[0], pixels[1], pixels[2], pixels[3]]
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use objc2_core_graphics::kCGColorSpaceDisplayP3;
    use objc2_image_io::{
        kCGImagePropertyDepth, kCGImagePropertyExifDateTimeOriginal,
        kCGImagePropertyExifDictionary, kCGImagePropertyExifPixelXDimension,
        kCGImagePropertyExifPixelYDimension, kCGImagePropertyHasAlpha, kCGImagePropertyOrientation,
        kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth, kCGImagePropertyProfileName,
        kCGImagePropertyTIFFDictionary, kCGImagePropertyTIFFOrientation,
    };
    use vento_core::convert::{ConvertError, ImageFormat};

    use super::test_support::{
        Fill, Fixture, boolean, corner_pixel, corner_pixel_in, dictionary, frame_count, properties,
        string,
    };
    use super::{number, probe_image, write_image};

    /// Converts `src` into a fresh empty file in `dir`, as the batch does.
    fn convert(src: &Path, dir: &Path, format: ImageFormat) -> PathBuf {
        let dst = dir.join(format!(".out.{}.vento-partial-1", format.extension()));
        fs::write(&dst, b"").unwrap();
        write_image(src, &dst, format).unwrap();
        dst
    }

    fn size(path: &Path) -> (i64, i64) {
        let props = properties(path);
        (
            number(&props, unsafe { kCGImagePropertyPixelWidth }).unwrap(),
            number(&props, unsafe { kCGImagePropertyPixelHeight }).unwrap(),
        )
    }

    #[test]
    fn every_target_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture::new(30, 20).write(&src, ImageFormat::Png);
        for &format in ImageFormat::TARGETS {
            // A Mac (or CI virtual machine) without a working HEVC encoder.
            if format == ImageFormat::Heic && !super::heic_encoder_works() {
                continue;
            }
            let out = convert(&src, dir.path(), format);
            assert_eq!(probe_image(&out).unwrap(), Some(format), "{format:?}");
            assert_eq!(size(&out), (30, 20), "{format:?}");
        }
    }

    #[test]
    fn probe_reads_content_not_extension() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.png");
        Fixture::new(8, 8).write(&src, ImageFormat::Jpeg);
        assert_eq!(probe_image(&src).unwrap(), Some(ImageFormat::Jpeg));
    }

    #[test]
    fn non_image_is_not_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("notes.png");
        fs::write(&src, "just some words, not pixels\n").unwrap();
        assert!(matches!(probe_image(&src), Err(ConvertError::NotAnImage)));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("locked.png");
        Fixture::new(8, 8).write(&src, ImageFormat::Png);
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        let result = probe_image(&src);
        fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(result, Err(ConvertError::Io(_))), "{result:?}");
    }

    #[test]
    fn webp_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture::new(30, 20).write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Webp);
        assert_eq!(probe_image(&out).unwrap(), Some(ImageFormat::Webp));
        assert_eq!(size(&out), (30, 20));
    }

    #[test]
    fn webp_keeps_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture {
            fill: Fill::Clear,
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Webp);
        let props = properties(&out);
        assert_eq!(
            boolean(&props, unsafe { kCGImagePropertyHasAlpha }),
            Some(true)
        );
        assert_eq!(corner_pixel(&out)[3], 0);
    }

    #[test]
    fn webp_orientation_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.jpg");
        Fixture {
            orientation: Some(6),
            ..Fixture::new(40, 20)
        }
        .write(&src, ImageFormat::Jpeg);
        let out = convert(&src, dir.path(), ImageFormat::Webp);
        assert_eq!(size(&out), (20, 40));
        let props = properties(&out);
        let top = number(&props, unsafe { kCGImagePropertyOrientation });
        assert!(matches!(top, None | Some(1)), "orientation {top:?}");
        if let Some(tiff) = dictionary(&props, unsafe { kCGImagePropertyTIFFDictionary }) {
            let inner = number(&tiff, unsafe { kCGImagePropertyTIFFOrientation });
            assert!(
                matches!(inner, None | Some(1)),
                "TIFF orientation {inner:?}"
            );
        }
    }

    #[test]
    fn webp_keeps_exif_date() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.tiff");
        let date = "2024:05:06 07:08:09";
        Fixture {
            date: Some(date),
            orientation: Some(6),
            ..Fixture::new(16, 8)
        }
        .write(&src, ImageFormat::Tiff);
        let out = convert(&src, dir.path(), ImageFormat::Webp);
        let props = properties(&out);
        let exif =
            dictionary(&props, unsafe { kCGImagePropertyExifDictionary }).expect("Exif dictionary");
        assert_eq!(
            string(&exif, unsafe { kCGImagePropertyExifDateTimeOriginal }).as_deref(),
            Some(date)
        );
        // The Exif block says upright and the right size, as the pixels are.
        let top = number(&props, unsafe { kCGImagePropertyOrientation });
        assert!(matches!(top, None | Some(1)), "orientation {top:?}");
        assert_eq!(size(&out), (8, 16));
        let exif_size = (
            number(&exif, unsafe { kCGImagePropertyExifPixelXDimension }),
            number(&exif, unsafe { kCGImagePropertyExifPixelYDimension }),
        );
        assert_eq!(exif_size, (Some(8), Some(16)));
    }

    #[test]
    fn webp_keeps_display_p3_profile() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        // Display P3's pure red lies outside sRGB: going through sRGB would
        // clip it.
        let p3 = unsafe { kCGColorSpaceDisplayP3 };
        Fixture {
            space: p3,
            fill: Fill::Color([1.0, 0.0, 0.0]),
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Webp);
        let profile = string(&properties(&out), unsafe { kCGImagePropertyProfileName });
        assert!(
            profile.as_deref().is_some_and(|p| p.contains("P3")),
            "profile {profile:?}"
        );
        let (want, got) = (corner_pixel_in(&src, p3), corner_pixel_in(&out, p3));
        for i in 0..3 {
            assert!(want[i].abs_diff(got[i]) <= 3, "want {want:?}, got {got:?}");
        }
    }

    #[test]
    fn exif_pixel_size_matches_rotated_output_heic() {
        // A Mac (or CI virtual machine) without a working HEVC encoder.
        if !super::heic_encoder_works() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.jpg");
        Fixture {
            orientation: Some(6),
            ..Fixture::new(40, 20)
        }
        .write(&src, ImageFormat::Jpeg);
        let out = convert(&src, dir.path(), ImageFormat::Heic);
        let exif = dictionary(&properties(&out), unsafe { kCGImagePropertyExifDictionary })
            .expect("Exif dictionary");
        let exif_size = (
            number(&exif, unsafe { kCGImagePropertyExifPixelXDimension }),
            number(&exif, unsafe { kCGImagePropertyExifPixelYDimension }),
        );
        assert_eq!(exif_size, (Some(20), Some(40)));
    }

    #[test]
    fn orientation_is_applied_to_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.jpg");
        Fixture {
            orientation: Some(6),
            ..Fixture::new(40, 20)
        }
        .write(&src, ImageFormat::Jpeg);
        let out = convert(&src, dir.path(), ImageFormat::Jpeg);
        assert_eq!(size(&out), (20, 40));
        let props = properties(&out);
        let top = number(&props, unsafe { kCGImagePropertyOrientation });
        assert!(matches!(top, None | Some(1)), "orientation {top:?}");
        if let Some(tiff) = dictionary(&props, unsafe { kCGImagePropertyTIFFDictionary }) {
            let inner = number(&tiff, unsafe { kCGImagePropertyTIFFOrientation });
            assert!(
                matches!(inner, None | Some(1)),
                "TIFF orientation {inner:?}"
            );
        }
    }

    #[test]
    fn transparency_becomes_white_in_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture {
            fill: Fill::Clear,
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Jpeg);
        let [r, g, b, _] = corner_pixel(&out);
        assert!(r >= 250 && g >= 250 && b >= 250, "corner {r} {g} {b}");
    }

    #[test]
    fn transparency_is_kept_in_png() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture {
            fill: Fill::Clear,
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Png);
        let props = properties(&out);
        assert_eq!(
            boolean(&props, unsafe { kCGImagePropertyHasAlpha }),
            Some(true)
        );
        assert_eq!(corner_pixel(&out)[3], 0);
    }

    #[test]
    fn display_p3_profile_is_kept_in_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture {
            space: unsafe { kCGColorSpaceDisplayP3 },
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Jpeg);
        let profile = string(&properties(&out), unsafe { kCGImagePropertyProfileName });
        assert!(
            profile.as_deref().is_some_and(|p| p.contains("P3")),
            "profile {profile:?}"
        );
    }

    #[test]
    fn gif_output_is_srgb() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture {
            space: unsafe { kCGColorSpaceDisplayP3 },
            fill: Fill::Color([0.2, 0.6, 0.2]),
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Gif);
        let profile = string(&properties(&out), unsafe { kCGImagePropertyProfileName });
        assert!(
            profile.as_deref().is_none_or(|p| p.contains("sRGB")),
            "profile {profile:?}"
        );
        // The colour looks the same, so the pixels were matched to sRGB
        // rather than copied raw from Display P3.
        let (want, got) = (corner_pixel(&src), corner_pixel(&out));
        for i in 0..3 {
            assert!(want[i].abs_diff(got[i]) <= 4, "want {want:?}, got {got:?}");
        }
    }

    #[test]
    fn bmp_output_is_srgb() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture {
            space: unsafe { kCGColorSpaceDisplayP3 },
            fill: Fill::Color([0.2, 0.6, 0.2]),
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Png);
        let out = convert(&src, dir.path(), ImageFormat::Bmp);
        let profile = string(&properties(&out), unsafe { kCGImagePropertyProfileName });
        assert!(
            profile.as_deref().is_none_or(|p| p.contains("sRGB")),
            "profile {profile:?}"
        );
        // The colour looks the same, so the pixels were matched to sRGB
        // rather than copied raw from Display P3.
        let (want, got) = (corner_pixel(&src), corner_pixel(&out));
        for i in 0..3 {
            assert!(want[i].abs_diff(got[i]) <= 4, "want {want:?}, got {got:?}");
        }
    }

    #[test]
    fn animated_gif_gives_one_frame() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.gif");
        Fixture {
            frames: 2,
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Gif);
        assert_eq!(frame_count(&src), 2);
        let out = convert(&src, dir.path(), ImageFormat::Tiff);
        assert_eq!(frame_count(&out), 1);
    }

    #[test]
    fn sixteen_bit_png_converts_to_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        Fixture {
            sixteen_bit: true,
            ..Fixture::new(24, 12)
        }
        .write(&src, ImageFormat::Png);
        assert_eq!(
            number(&properties(&src), unsafe { kCGImagePropertyDepth }),
            Some(16)
        );
        let out = convert(&src, dir.path(), ImageFormat::Jpeg);
        assert_eq!(probe_image(&out).unwrap(), Some(ImageFormat::Jpeg));
        assert_eq!(size(&out), (24, 12));
    }

    #[test]
    fn exif_date_survives_to_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.tiff");
        let date = "2024:05:06 07:08:09";
        Fixture {
            date: Some(date),
            ..Fixture::new(16, 16)
        }
        .write(&src, ImageFormat::Tiff);
        let out = convert(&src, dir.path(), ImageFormat::Jpeg);
        let exif = dictionary(&properties(&out), unsafe { kCGImagePropertyExifDictionary })
            .expect("Exif dictionary");
        assert_eq!(
            string(&exif, unsafe { kCGImagePropertyExifDateTimeOriginal }).as_deref(),
            Some(date)
        );
    }
}
