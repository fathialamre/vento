//! Picture thumbnails made with ImageIO. It decodes only about as many
//! pixels as the thumbnail needs (JPEG and HEIC at a reduced scale, PNG and
//! TIFF shrunk as they stream in), so a huge picture never sits in memory
//! at full size. Core Graphics and ImageIO are thread-safe: any thread.

use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{
    CFData, CFDictionary, CFIndex, CFNumber, CFRetained, CFString, CFType, CFURL, CGPoint, CGRect,
    CGSize, kCFAllocatorNull,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_image_io::{
    CGImageSource, kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
    kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceThumbnailMaxPixelSize,
};

use super::icons::unpremultiply;
use crate::RgbaImage;

/// The longest side ImageIO is asked to decode to. Only very long strips
/// reach it; their short side is then stretched a little to fill.
const MAX_DECODE_SIDE: f64 = 4096.0;

/// A thumbnail of the picture file at `path`: its shorter side `short_px`
/// pixels, its longer one in proportion but at most `max_ratio` times as
/// long (the picture is cropped to fit). `None` if the file is missing or
/// not a picture macOS can read.
pub(crate) fn picture_thumbnail(path: &Path, short_px: u32, max_ratio: u32) -> Option<RgbaImage> {
    // Worker threads live long: drain what ImageIO autoreleases per picture.
    autoreleasepool(|_| {
        let url = CFURL::from_file_path(path)?;
        // SAFETY: a valid file URL; no options.
        let source = unsafe { CGImageSource::with_url(&url, None) }?;
        thumbnail(&source, short_px, max_ratio)
    })
}

/// [`picture_thumbnail`] of the encoded picture `bytes` (PNG, TIFF, JPEG,
/// HEIC: any format macOS reads), without copying them.
pub(crate) fn picture_data_thumbnail(
    bytes: &[u8],
    short_px: u32,
    max_ratio: u32,
) -> Option<RgbaImage> {
    let len = CFIndex::try_from(bytes.len()).ok()?;
    autoreleasepool(|_| {
        // SAFETY: `bytes` is valid for `len` bytes, and `kCFAllocatorNull`
        // leaves them to their owner. The data, the source reading it and
        // the thumbnail are all released (and the pool drained) before this
        // returns, while `bytes` is still borrowed.
        let data =
            unsafe { CFData::with_bytes_no_copy(None, bytes.as_ptr(), len, kCFAllocatorNull) }?;
        // SAFETY: valid data; no options.
        let source = unsafe { CGImageSource::with_data(&data, None) }?;
        thumbnail(&source, short_px, max_ratio)
    })
}

fn thumbnail(source: &CGImageSource, short_px: u32, max_ratio: u32) -> Option<RgbaImage> {
    if short_px == 0 || max_ratio == 0 {
        return None;
    }
    // SAFETY: `source` is a valid image source.
    if unsafe { source.count() } == 0 {
        return None;
    }
    let short_px = f64::from(short_px);
    let image = decode_small(source, short_px, f64::from(max_ratio))?;
    let (iw, ih) = (
        CGImage::width(Some(&image)) as f64,
        CGImage::height(Some(&image)) as f64,
    );
    if !(iw > 0.0 && ih > 0.0) {
        return None;
    }
    let (short, long) = (iw.min(ih), iw.max(ih));
    let long_px = (long / short * short_px)
        .round()
        .min(short_px * f64::from(max_ratio));
    let (w, h) = if iw >= ih {
        (long_px, short_px)
    } else {
        (short_px, long_px)
    };
    // Fill the whole bitmap, keeping the picture's shape: what overflows the
    // capped side is cut off evenly.
    let scale = (w / iw).max(h / ih);
    let (dw, dh) = (iw * scale, ih * scale);
    let rect = CGRect::new(
        CGPoint::new((w - dw) / 2.0, (h - dh) / 2.0),
        CGSize::new(dw, dh),
    );
    draw(&image, w as usize, h as usize, rect)
}

/// The first frame, upright, decoded just big enough that its shorter side
/// covers `short_px` (or as big as it is, when smaller).
fn decode_small(
    source: &CGImageSource,
    short_px: f64,
    max_ratio: f64,
) -> Option<CFRetained<CGImage>> {
    // SAFETY: `source` is a valid image source; no options. Reading the
    // properties parses the header only.
    let props = unsafe { source.properties_at_index(0, None) }
        // SAFETY: ImageIO property dictionaries have string keys.
        .map(|p| unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(p) });
    let dimension = |key: &CFString| {
        props
            .as_deref()
            .and_then(|p| p.get(key)?.downcast::<CFNumber>().ok()?.as_f64())
            .filter(|&v| v > 0.0)
    };
    // SAFETY: the keys are ImageIO's own statics.
    let size = unsafe {
        dimension(kCGImagePropertyPixelWidth).zip(dimension(kCGImagePropertyPixelHeight))
    };
    // The longer side that gives the shorter one `short_px`; without a size
    // in the metadata, the longest a thumbnail keeps.
    let longest = size.map_or(short_px * max_ratio, |(w, h)| {
        w.max(h) / w.min(h) * short_px
    });
    let yes = CFNumber::new_i32(1);
    let max = CFNumber::new_f64(longest.ceil().clamp(1.0, MAX_DECODE_SIDE));
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
    unsafe { source.thumbnail_at_index(0, Some(options.as_opaque())) }
}

/// Draws `image` into `rect` of a `width`×`height` sRGB bitmap and returns
/// its straight-alpha RGBA8 pixels, rows top to bottom.
fn draw(image: &CGImage, width: usize, height: usize, rect: CGRect) -> Option<RgbaImage> {
    if width == 0 || height == 0 {
        return None;
    }
    // SAFETY: a valid system colour space name.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let mut pixels = vec![0u8; width * height * 4];
    {
        // SAFETY: `pixels` holds `height` rows of `width * 4` bytes and
        // outlives the context, dropped at the end of this block. Premultiplied
        // RGBA, alpha last: the 8-bit RGBA layout a bitmap context accepts.
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                Some(&space),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }?;
        CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
        CGContext::draw_image(Some(&context), rect, Some(image));
    }
    unpremultiply(&mut pixels);
    Some(RgbaImage {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use objc2_core_foundation::CFMutableData;
    use objc2_core_graphics::CGBitmapContextCreateImage;
    use objc2_image_io::CGImageDestination;

    use super::*;

    /// A `width`×`height` opaque picture of `rgb`, encoded as the UTI `ty`.
    fn encoded(ty: &'static str, width: usize, height: usize, rgb: [u8; 3]) -> Vec<u8> {
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        let context = unsafe {
            CGBitmapContextCreate(
                ptr::null_mut(),
                width,
                height,
                8,
                width * 4,
                Some(&space),
                CGImageAlphaInfo::NoneSkipLast.0,
            )
        }
        .unwrap();
        let [r, g, b] = rgb.map(|c| f64::from(c) / 255.0);
        CGContext::set_rgb_fill_color(Some(&context), r, g, b, 1.0);
        let size = CGSize::new(width as f64, height as f64);
        CGContext::fill_rect(Some(&context), CGRect::new(CGPoint::new(0.0, 0.0), size));
        let image = CGBitmapContextCreateImage(Some(&context)).unwrap();
        let data = CFMutableData::new(None, 0).unwrap();
        let dest = unsafe {
            CGImageDestination::with_data(&data, &CFString::from_static_str(ty), 1, None)
        }
        .unwrap();
        unsafe { dest.add_image(&image, None) };
        assert!(unsafe { dest.finalize() });
        data.to_vec()
    }

    fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut enc = png::Encoder::new(&mut bytes, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&rgba.repeat((width * height) as usize))
            .unwrap();
        w.finish().unwrap();
        bytes
    }

    fn middle(t: &RgbaImage) -> [u8; 4] {
        let at = ((t.height / 2 * t.width + t.width / 2) * 4) as usize;
        t.pixels[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn picture_file_thumbnail_keeps_its_shape() {
        let dir = tempfile::tempdir().unwrap();
        // A 4×2 opaque red PNG.
        let file = dir.path().join("red.png");
        std::fs::write(&file, png(4, 2, [255, 0, 0, 255])).unwrap();

        let t = picture_thumbnail(&file, 56, 8).expect("a thumbnail");
        assert_eq!((t.width, t.height), (112, 56));
        assert_eq!(middle(&t), [255, 0, 0, 255]);
        // Very wide pictures stop at the ratio.
        let t = picture_thumbnail(&file, 10, 1).unwrap();
        assert_eq!((t.width, t.height), (10, 10));

        std::fs::write(dir.path().join("bad.png"), b"not a picture").unwrap();
        assert!(picture_thumbnail(&dir.path().join("bad.png"), 56, 8).is_none());
        assert!(picture_thumbnail(&dir.path().join("gone.png"), 56, 8).is_none());
    }

    #[test]
    fn data_thumbnail_reads_png_tiff_and_jpeg() {
        let tiff = encoded("public.tiff", 300, 900, [0, 0, 255]);
        let t = picture_data_thumbnail(&tiff, 56, 8).expect("a TIFF thumbnail");
        assert_eq!((t.width, t.height), (56, 168));
        assert_eq!(middle(&t), [0, 0, 255, 255]);

        let jpeg = encoded("public.jpeg", 2000, 1000, [255, 255, 255]);
        let t = picture_data_thumbnail(&jpeg, 56, 8).expect("a JPEG thumbnail");
        assert_eq!((t.width, t.height), (112, 56));

        // Transparency comes through as straight alpha.
        let t = picture_data_thumbnail(&png(40, 40, [0, 255, 0, 128]), 56, 8).unwrap();
        assert_eq!((t.width, t.height), (56, 56));
        assert_eq!(middle(&t), [0, 255, 0, 128]);

        // Long strips are cropped to the ratio.
        let t = picture_data_thumbnail(&png(4000, 100, [9, 9, 9, 255]), 56, 8).unwrap();
        assert_eq!((t.width, t.height), (448, 56));

        assert!(picture_data_thumbnail(b"not a picture", 56, 8).is_none());
        assert!(picture_data_thumbnail(&[], 56, 8).is_none());
    }
}
