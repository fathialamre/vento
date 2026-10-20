//! Finder file icons rendered to RGBA8 pixels.

use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext,
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
    let n = side as isize;
    // Premultiplied RGBA, alpha last: the only 8-bit RGBA layout a bitmap
    // graphics context accepts. Rows are un-premultiplied on the way out.
    // SAFETY: null planes make the rep allocate (and own) its buffer; every
    // other argument describes a valid single-plane 8-bit RGBA layout.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            n,
            n,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat(0),
            n * 4,
            32,
        )
    }?;
    // The rep's point size sets the context's scale, so the image picks
    // its best representation for `side` pixels.
    rep.setSize(NSSize::new(points, points));
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;

    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    ctx.setImageInterpolation(NSImageInterpolation::High);
    icon.drawInRect(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(points, points),
    ));
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    let data = rep.bitmapData();
    let stride = usize::try_from(rep.bytesPerRow()).ok()?;
    if data.is_null() || stride < side * 4 {
        return None;
    }
    // SAFETY: the rep owns `stride * side` bytes at `data` and outlives
    // this borrow.
    let bytes = unsafe { std::slice::from_raw_parts(data, stride * side) };
    let mut pixels = Vec::with_capacity(side * side * 4);
    for row in bytes.chunks_exact(stride) {
        pixels.extend_from_slice(&row[..side * 4]);
    }
    if !rep
        .bitmapFormat()
        .contains(NSBitmapFormat::AlphaNonpremultiplied)
    {
        unpremultiply(&mut pixels);
    }
    Some(RgbaImage {
        width: side as u32,
        height: side as u32,
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
    fn unpremultiply_restores_straight_alpha() {
        let mut px = [64, 32, 0, 128, 10, 10, 10, 0, 200, 100, 50, 255];
        unpremultiply(&mut px);
        assert_eq!(px, [128, 64, 0, 128, 0, 0, 0, 0, 200, 100, 50, 255]);
    }
}
