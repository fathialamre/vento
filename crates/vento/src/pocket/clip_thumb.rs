//! Thumbnails of the clipboard history's image entries. They are made on the
//! store worker, beside the entry's blobs, so the main thread only ever
//! loads a small file.

use std::fs;
use std::io::{self, Cursor};
use std::path::Path;

use vento_core::clip::{ClipSnapshot, image};
use vento_platform::RgbaImage;

/// The thumbnail's file in an entry's folder. `read_entry` only reads the
/// `<item>-<rep>` blobs, so it never sees this one.
pub(super) const THUMB_FILE: &str = "thumb.png";
/// A thumbnail's shorter side, in pixels: the 28 pt icon box at 2x.
const THUMB_PX: u32 = 56;
/// A thumbnail's longer side is at most this many times its shorter one.
const THUMB_MAX_RATIO: u32 = 8;
/// Pictures with more pixels than this get no thumbnail (256 MB decoded).
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Writes the thumbnail of `s`'s picture into `folder` (the entry's stored
/// folder), through a temporary file so it is never seen half written.
/// Only a PNG is read; a TIFF-only picture gets none, and its row shows the
/// photo icon.
pub(super) fn write_thumb(folder: &Path, s: &ClipSnapshot) -> io::Result<()> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let (bytes, ext) = image(s).ok_or_else(|| bad("no picture"))?;
    if ext != "png" {
        return Err(bad("not a PNG"));
    }
    let full = decode_png(&bytes).ok_or_else(|| bad("cannot decode the PNG"))?;
    save_thumb(
        folder,
        &shrink(&full.pixels, full.width, full.height, THUMB_PX),
    )
}

/// Writes the thumbnail of the picture file `picture` (copied in Finder)
/// into `folder`. Any format macOS reads works, HEIC included.
pub(super) fn write_file_thumb(folder: &Path, picture: &Path) -> io::Result<()> {
    let small = vento_platform::picture_thumbnail(picture, THUMB_PX, THUMB_MAX_RATIO)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cannot read the picture"))?;
    save_thumb(folder, &small)
}

/// Saves `small` as the thumbnail in `folder`, through a temporary file.
fn save_thumb(folder: &Path, small: &RgbaImage) -> io::Result<()> {
    let png = encode_png(small)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cannot encode the thumbnail"))?;
    let tmp = folder.join(format!("{THUMB_FILE}.tmp"));
    fs::write(&tmp, png)?;
    fs::rename(&tmp, folder.join(THUMB_FILE)).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// The RGBA8 pixels of a PNG, whatever its colour type and depth.
fn decode_png(bytes: &[u8]) -> Option<RgbaImage> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = reader.info().size();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let pixels = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[v, a]| [v, v, v, a])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    (pixels.len() == width as usize * height as usize * 4).then_some(RgbaImage {
        width,
        height,
        pixels,
    })
}

/// `image` as PNG bytes.
fn encode_png(image: &RgbaImage) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&image.pixels).ok()?;
    writer.finish().ok()?;
    Some(bytes)
}

/// The RGBA `pixels` of a `w` x `h` image scaled down so its shorter side is
/// `px` and its longer side at most [`THUMB_MAX_RATIO`] times that, each
/// pixel the average of those it covers. Never scaled up.
fn shrink(pixels: &[u8], w: u32, h: u32, px: u32) -> RgbaImage {
    let short = w.min(h);
    let long = w.max(h);
    let unchanged = || RgbaImage {
        width: w,
        height: h,
        pixels: pixels.to_vec(),
    };
    if short == 0 {
        return unchanged();
    }
    let scale = (f64::from(px) / f64::from(short))
        .min(f64::from(px * THUMB_MAX_RATIO) / f64::from(long))
        .min(1.0);
    if scale >= 1.0 {
        return unchanged();
    }
    let out_w = ((f64::from(w) * scale).round() as u32).max(1);
    let out_h = ((f64::from(h) * scale).round() as u32).max(1);
    let mut out = Vec::with_capacity(out_w as usize * out_h as usize * 4);
    for oy in 0..out_h {
        let (y0, y1) = span(oy, out_h, h);
        for ox in 0..out_w {
            let (x0, x1) = span(ox, out_w, w);
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                let row = y as usize * w as usize;
                for x in x0..x1 {
                    let at = (row + x as usize) * 4;
                    for (s, &v) in sum.iter_mut().zip(&pixels[at..at + 4]) {
                        *s += u64::from(v);
                    }
                }
            }
            let n = u64::from((x1 - x0) * (y1 - y0));
            out.extend(sum.iter().map(|s| (s / n) as u8));
        }
    }
    RgbaImage {
        width: out_w,
        height: out_h,
        pixels: out,
    }
}

/// The source pixels `[start, end)` that output pixel `i` of `out` covers
/// along a side `len` long; never empty.
fn span(i: u32, out: u32, len: u32) -> (u32, u32) {
    let at = |k: u32| (u64::from(k) * u64::from(len) / u64::from(out)) as u32;
    let start = at(i);
    (start, at(i + 1).max(start + 1).min(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    use vento_core::clip::{ClipRep, TYPE_PNG, TYPE_TIFF};

    fn flat(width: u32, height: u32) -> RgbaImage {
        RgbaImage {
            width,
            height,
            pixels: vec![200; width as usize * height as usize * 4],
        }
    }

    fn picture(ty: &str, data: Vec<u8>) -> ClipSnapshot {
        ClipSnapshot {
            items: vec![vec![ClipRep {
                ty: ty.to_owned(),
                data,
            }]],
        }
    }

    #[test]
    fn thumb_is_a_small_png_beside_the_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let png = encode_png(&flat(400, 200)).unwrap();
        write_thumb(tmp.path(), &picture(TYPE_PNG, png)).unwrap();
        let thumb = decode_png(&fs::read(tmp.path().join(THUMB_FILE)).unwrap()).unwrap();
        assert_eq!((thumb.width, thumb.height), (112, 56));
        assert!(thumb.pixels.iter().all(|&p| p == 200));
        // No temporary file left behind.
        let names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [THUMB_FILE]);
    }

    #[test]
    fn copied_picture_file_gets_a_thumb() {
        let tmp = tempfile::tempdir().unwrap();
        let photo = tmp.path().join("photo.png");
        fs::write(&photo, encode_png(&flat(400, 200)).unwrap()).unwrap();
        let folder = tmp.path().join("7");
        fs::create_dir(&folder).unwrap();
        write_file_thumb(&folder, &photo).unwrap();
        let thumb = decode_png(&fs::read(folder.join(THUMB_FILE)).unwrap()).unwrap();
        assert_eq!((thumb.width, thumb.height), (112, 56));
        assert!(write_file_thumb(&folder, &tmp.path().join("gone.png")).is_err());
    }

    #[test]
    fn no_thumb_without_a_readable_png() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(write_thumb(tmp.path(), &picture(TYPE_TIFF, vec![1, 2, 3])).is_err());
        assert!(write_thumb(tmp.path(), &picture(TYPE_PNG, vec![1, 2, 3])).is_err());
        assert!(fs::read_dir(tmp.path()).unwrap().next().is_none());
    }

    #[test]
    fn rgb_pngs_decode_to_opaque_rgba() {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3, 4, 5, 6]).unwrap();
        writer.finish().unwrap();
        let image = decode_png(&bytes).unwrap();
        assert_eq!(image.pixels, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn shrink_keeps_the_short_side_at_the_thumbnail_size() {
        let shrink_flat = |w, h| shrink(&flat(w, h).pixels, w, h, 56);
        let small = shrink_flat(1120, 560);
        assert_eq!((small.width, small.height), (112, 56));
        assert_eq!(small.pixels.len(), 112 * 56 * 4);
        // A flat colour stays that colour.
        assert!(small.pixels.iter().all(|&p| p == 200));
        // Very long pictures are cut down on their long side too.
        let long = shrink_flat(4000, 100);
        assert_eq!((long.width, long.height), (448, 11));
        // Already small: unchanged.
        let tiny = shrink_flat(40, 30);
        assert_eq!((tiny.width, tiny.height), (40, 30));
    }

    #[test]
    fn shrink_averages_the_pixels_it_covers() {
        // 4 x 2, left half black, right half white, down to 2 x 1.
        let mut pixels = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 0 } else { 255 };
                pixels.extend([v, v, v, 255]);
            }
        }
        let small = shrink(&pixels, 4, 2, 1);
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.pixels, [0, 0, 0, 255, 255, 255, 255, 255]);
    }
}
