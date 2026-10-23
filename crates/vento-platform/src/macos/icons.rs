//! Finder file icons and picture thumbnails rendered to RGBA8 pixels.

use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext, NSImage,
    NSImageInterpolation, NSScreen, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::RgbaImage;

/// `NSWorkspace.iconForFile` of `path`, drawn at `px`×`px` points times the
/// main screen's backing scale into an RGBA8 bitmap. `None` if the path
/// doesn't exist or rendering fails.
pub(super) fn file_icon(path: &Path, px: u32) -> Option<RgbaImage> {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("file_icon called off the main thread; ignored");
        return None;
    };
    if px == 0 || !path.exists() {
        return None;
    }
    let path = path.to_str()?;
    let scale = NSScreen::mainScreen(mtm)
        .map(|s| s.backingScaleFactor())
        .unwrap_or(1.0);
    let side = (f64::from(px) * scale).round() as usize;
    if side == 0 {
        return None;
    }
    autoreleasepool(|_| render(path, f64::from(px), side))
}

fn render(path: &str, points: f64, side: usize) -> Option<RgbaImage> {
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
    rasterize(
        &icon,
        NSSize::new(points, points),
        side,
        side,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(points, points)),
    )
}

/// A thumbnail of the picture file at `path`: its shorter side `short_px`
/// pixels, its longer one in proportion but at most `max_ratio` times as
/// long (the picture is cropped to fit). `None` if the file is missing or
/// not a picture macOS can read. Any thread: it draws into its own bitmap.
pub(crate) fn picture_thumbnail(path: &Path, short_px: u32, max_ratio: u32) -> Option<RgbaImage> {
    if short_px == 0 || max_ratio == 0 {
        return None;
    }
    let path = path.to_str()?;
    autoreleasepool(|_| {
        let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path))?;
        let size = image.size();
        if !(size.width > 0.0 && size.height > 0.0) {
            return None;
        }
        let (short, long) = (size.width.min(size.height), size.width.max(size.height));
        let short_px = f64::from(short_px);
        let long_px = (long / short * short_px)
            .round()
            .min(short_px * f64::from(max_ratio));
        let (w, h) = if size.width >= size.height {
            (long_px, short_px)
        } else {
            (short_px, long_px)
        };
        // Fill the whole bitmap, keeping the picture's shape: what overflows
        // the capped side is cut off evenly.
        let scale = (w / size.width).max(h / size.height);
        let (dw, dh) = (size.width * scale, size.height * scale);
        let rect = NSRect::new(
            NSPoint::new((w - dw) / 2.0, (h - dh) / 2.0),
            NSSize::new(dw, dh),
        );
        rasterize(&image, NSSize::new(w, h), w as usize, h as usize, rect)
    })
}

/// Draws `image` into `rect` of a `width`×`height`-pixel RGBA8 bitmap whose
/// point size is `points`, and returns its straight-alpha pixels.
fn rasterize(
    image: &NSImage,
    points: NSSize,
    width: usize,
    height: usize,
    rect: NSRect,
) -> Option<RgbaImage> {
    if width == 0 || height == 0 {
        return None;
    }
    let (w, h) = (width as isize, height as isize);
    // Premultiplied RGBA, alpha last: the only 8-bit RGBA layout a bitmap
    // graphics context accepts. Rows are un-premultiplied on the way out.
    // SAFETY: null planes make the rep allocate (and own) its buffer; every
    // other argument describes a valid single-plane 8-bit RGBA layout.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat(0),
            w * 4,
            32,
        )
    }?;
    // The rep's point size sets the context's scale, so the image picks
    // its best representation for the bitmap's pixels.
    rep.setSize(points);
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;

    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    ctx.setImageInterpolation(NSImageInterpolation::High);
    image.drawInRect(rect);
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    let data = rep.bitmapData();
    let stride = usize::try_from(rep.bytesPerRow()).ok()?;
    if data.is_null() || stride < width * 4 {
        return None;
    }
    // SAFETY: the rep owns `stride * height` bytes at `data` and outlives
    // this borrow.
    let bytes = unsafe { std::slice::from_raw_parts(data, stride * height) };
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in bytes.chunks_exact(stride) {
        pixels.extend_from_slice(&row[..width * 4]);
    }
    if !rep
        .bitmapFormat()
        .contains(NSBitmapFormat::AlphaNonpremultiplied)
    {
        unpremultiply(&mut pixels);
    }
    Some(RgbaImage {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

/// Divide each RGBA8 pixel's colour by its alpha, rounding to nearest.
fn unpremultiply(pixels: &mut [u8]) {
    for p in pixels.as_chunks_mut::<4>().0 {
        let a = u16::from(p[3]);
        if a == 0 {
            p[..3].fill(0);
            continue;
        }
        for c in &mut p[..3] {
            *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_file_thumbnail_keeps_its_shape() {
        let dir = std::env::temp_dir().join(format!("vento-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A 4×2 opaque red PNG.
        let file = dir.join("red.png");
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, 4, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 0, 0, 255].repeat(8)).unwrap();
        }
        std::fs::write(&file, png).unwrap();

        let t = picture_thumbnail(&file, 56, 8).expect("a thumbnail");
        assert_eq!((t.width, t.height), (112, 56));
        let mid = ((28 * 112 + 56) * 4) as usize;
        assert_eq!(&t.pixels[mid..mid + 4], [255, 0, 0, 255]);
        // Very wide pictures stop at the ratio.
        let t = picture_thumbnail(&file, 10, 1).unwrap();
        assert_eq!((t.width, t.height), (10, 10));

        std::fs::write(dir.join("bad.png"), b"not a picture").unwrap();
        assert!(picture_thumbnail(&dir.join("bad.png"), 56, 8).is_none());
        assert!(picture_thumbnail(&dir.join("gone.png"), 56, 8).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unpremultiply_restores_straight_alpha() {
        let mut px = [64, 32, 0, 128, 10, 10, 10, 0, 200, 100, 50, 255];
        unpremultiply(&mut px);
        assert_eq!(px, [128, 64, 0, 128, 0, 0, 0, 0, 200, 100, 50, 255]);
    }
}
