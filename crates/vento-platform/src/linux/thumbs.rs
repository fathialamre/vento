//! Picture thumbnails, decoded with `image` (PNG and JPEG, the formats it
//! is built with in the app).

use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::RgbaImage;

/// The thumbnail size for a `w`×`h` picture: the shorter side `short_px`,
/// the longer in proportion but at most `max_ratio` times as long.
fn thumb_size(w: u32, h: u32, short_px: u32, max_ratio: u32) -> (u32, u32) {
    let long = |long: u32, short: u32| {
        let scaled = (f64::from(long) * f64::from(short_px) / f64::from(short)).round() as u32;
        scaled.clamp(short_px, short_px.saturating_mul(max_ratio))
    };
    if w <= h {
        (short_px, long(h, w))
    } else {
        (long(w, h), short_px)
    }
}

/// A thumbnail of the picture file at `path`, upright (its EXIF
/// orientation applied), its shorter side `short_px` pixels and its longer
/// one in proportion, cropped about the centre to at most `max_ratio` times
/// as long. `None` if it is missing or not a picture `image` reads. Any
/// thread.
pub(crate) fn picture_thumbnail(path: &Path, short_px: u32, max_ratio: u32) -> Option<RgbaImage> {
    if short_px == 0 || max_ratio == 0 {
        return None;
    }
    let mut decoder = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let orientation = decoder.orientation().ok();
    let mut picture = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        picture.apply_orientation(orientation);
    }
    let (w, h) = (picture.width(), picture.height());
    if w == 0 || h == 0 {
        return None;
    }
    // Scale so the shorter side fits, then crop the longer one.
    let scale = f64::from(short_px) / f64::from(w.min(h));
    let sw = ((f64::from(w) * scale).round() as u32).max(short_px.min(w.max(1)));
    let sh = ((f64::from(h) * scale).round() as u32).max(short_px.min(h.max(1)));
    let scaled = picture.thumbnail_exact(sw, sh).to_rgba8();
    let (tw, th) = thumb_size(w, h, short_px, max_ratio);
    let (tw, th) = (tw.min(sw), th.min(sh));
    let cropped = image::imageops::crop_imm(&scaled, (sw - tw) / 2, (sh - th) / 2, tw, th);
    let pixels = cropped.to_image().into_raw();
    Some(RgbaImage {
        width: tw,
        height: th,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_short_side_fits_and_the_long_one_is_capped() {
        assert_eq!(thumb_size(400, 200, 56, 2), (112, 56));
        assert_eq!(thumb_size(200, 400, 56, 2), (56, 112));
        assert_eq!(
            thumb_size(1000, 100, 56, 2),
            (112, 56),
            "a panorama is cropped"
        );
        assert_eq!(thumb_size(300, 300, 56, 2), (56, 56));
    }

    fn picture(dir: &Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        // Left half red, right half blue, to see the crop.
        let img = image::RgbaImage::from_fn(w, h, |x, _| {
            if x < w / 2 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        img.save(&path).unwrap();
        path
    }

    #[test]
    fn pictures_become_thumbnails_of_the_asked_size() {
        let dir = tempfile::tempdir().unwrap();
        let t = picture_thumbnail(&picture(dir.path(), "wide.png", 400, 200), 56, 2).unwrap();
        assert_eq!((t.width, t.height, t.pixels.len()), (112, 56, 112 * 56 * 4));
        assert_eq!(
            &t.pixels[0..4],
            &[255, 0, 0, 255],
            "the left edge stays red"
        );
    }

    #[test]
    fn a_long_picture_is_cropped_about_its_centre() {
        let dir = tempfile::tempdir().unwrap();
        let t = picture_thumbnail(&picture(dir.path(), "pano.png", 1000, 100), 50, 2).unwrap();
        assert_eq!((t.width, t.height), (100, 50));
        // The centre 100 of 500 scaled pixels: red then blue, half each.
        let px = |x: u32| &t.pixels[(x * 4) as usize..(x * 4 + 4) as usize];
        assert_eq!(px(10)[0], 255);
        assert_eq!(px(90)[2], 255);
    }

    #[test]
    fn missing_or_unreadable_files_have_no_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        assert!(picture_thumbnail(&dir.path().join("gone.png"), 56, 2).is_none());
        let text = dir.path().join("note.png");
        std::fs::write(&text, "not a picture").unwrap();
        assert!(picture_thumbnail(&text, 56, 2).is_none());
        assert!(picture_thumbnail(&text, 0, 2).is_none());
    }
}
