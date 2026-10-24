//! Finder file icons rendered to RGBA8 pixels.

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
    let scale = NSScreen::mainScreen(mtm)
        .map(|s| s.backingScaleFactor())
        .unwrap_or(1.0);
    file_icon_at(path, px, scale)
}

/// [`file_icon`] drawn at `scale` pixels per point instead of the main
/// screen's. Any thread: `iconForFile` is thread-safe, and it draws into
/// its own bitmap.
pub(crate) fn file_icon_at(path: &Path, px: u32, scale: f64) -> Option<RgbaImage> {
    if px == 0 || !path.exists() {
        return None;
    }
    let path = path.to_str()?;
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
pub(super) fn unpremultiply(pixels: &mut [u8]) {
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
    fn unpremultiply_restores_straight_alpha() {
        let mut px = [64, 32, 0, 128, 10, 10, 10, 0, 200, 100, 50, 255];
        unpremultiply(&mut px);
        assert_eq!(px, [128, 64, 0, 128, 0, 0, 0, 0, 200, 100, 50, 255]);
    }
}
