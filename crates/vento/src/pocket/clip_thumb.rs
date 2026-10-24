//! Thumbnails of the clipboard history's image entries. They are made on the
//! store worker, beside the entry's blobs, so the main thread only ever
//! loads a small file. macOS decodes only about as many pixels as the
//! thumbnail needs, so even a huge picture costs a few megabytes here.

use std::fs;
use std::io;
use std::path::Path;

use vento_core::clip::{ClipSnapshot, TYPE_PNG, TYPE_TIFF};
use vento_platform::RgbaImage;

/// The thumbnail's file in an entry's folder. `read_entry` only reads the
/// `<item>-<rep>` blobs, so it never sees this one.
pub(super) const THUMB_FILE: &str = "thumb.png";
/// A thumbnail's shorter side, in pixels: the 28 pt icon box at 2x.
const THUMB_PX: u32 = 56;
/// A thumbnail's longer side is at most this many times its shorter one.
const THUMB_MAX_RATIO: u32 = 8;

/// Writes the thumbnail of `s`'s picture into `folder` (the entry's stored
/// folder), through a temporary file so it is never seen half written.
/// The PNG is read if there is one, else the TIFF; the bytes are read in
/// place, not copied.
pub(super) fn write_thumb(folder: &Path, s: &ClipSnapshot) -> io::Result<()> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let item = s.items.first().ok_or_else(|| bad("no picture"))?;
    let rep = [TYPE_PNG, TYPE_TIFF]
        .into_iter()
        .find_map(|ty| item.iter().find(|r| r.ty == ty))
        .ok_or_else(|| bad("no picture"))?;
    let small = vento_platform::picture_data_thumbnail(&rep.data, THUMB_PX, THUMB_MAX_RATIO)
        .ok_or_else(|| bad("cannot read the picture"))?;
    save_thumb(folder, &small)
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

#[cfg(test)]
mod tests {
    use super::*;

    use vento_core::clip::ClipRep;

    fn flat(width: u32, height: u32) -> RgbaImage {
        RgbaImage {
            width,
            height,
            pixels: vec![200; width as usize * height as usize * 4],
        }
    }

    fn picture(reps: &[(&str, Vec<u8>)]) -> ClipSnapshot {
        ClipSnapshot {
            items: vec![
                reps.iter()
                    .map(|(ty, data)| ClipRep {
                        ty: (*ty).to_owned(),
                        data: data.clone(),
                    })
                    .collect(),
            ],
        }
    }

    /// The size and pixels of the thumbnail written into `folder`.
    fn read_thumb(folder: &Path) -> RgbaImage {
        let bytes = fs::read(folder.join(THUMB_FILE)).unwrap();
        let mut reader = png::Decoder::new(io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgba);
        RgbaImage {
            width: info.width,
            height: info.height,
            pixels,
        }
    }

    /// A baseline, uncompressed little-endian RGB TIFF of one grey level.
    fn grey_tiff(width: u16, height: u16, v: u8) -> Vec<u8> {
        let pixels = usize::from(width) * usize::from(height) * 3;
        // Header, then the pixels, then the bits per sample, then the IFD.
        let bits_at = 8 + pixels as u32;
        let ifd_at = bits_at + 6;
        let mut t = b"II*\0".to_vec();
        t.extend(ifd_at.to_le_bytes());
        t.extend(std::iter::repeat_n(v, pixels));
        t.extend([8, 0, 8, 0, 8, 0]);
        let entries: [(u16, u16, u32, u32); 9] = [
            (256, 3, 1, u32::from(width)),
            (257, 3, 1, u32::from(height)),
            (258, 3, 3, bits_at),
            (259, 3, 1, 1),
            (262, 3, 1, 2),
            (273, 4, 1, 8),
            (277, 3, 1, 3),
            (278, 3, 1, u32::from(height)),
            (279, 4, 1, pixels as u32),
        ];
        t.extend((entries.len() as u16).to_le_bytes());
        for (tag, ty, count, value) in entries {
            t.extend(tag.to_le_bytes());
            t.extend(ty.to_le_bytes());
            t.extend(count.to_le_bytes());
            t.extend(value.to_le_bytes());
        }
        t.extend(0u32.to_le_bytes());
        t
    }

    #[test]
    fn thumb_is_a_small_png_beside_the_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let png = encode_png(&flat(400, 200)).unwrap();
        write_thumb(tmp.path(), &picture(&[(TYPE_PNG, png)])).unwrap();
        let thumb = read_thumb(tmp.path());
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
    fn tiff_only_picture_gets_a_thumb() {
        let tmp = tempfile::tempdir().unwrap();
        write_thumb(
            tmp.path(),
            &picture(&[(TYPE_TIFF, grey_tiff(300, 100, 90))]),
        )
        .unwrap();
        let thumb = read_thumb(tmp.path());
        assert_eq!((thumb.width, thumb.height), (168, 56));
        let at = (28 * 168 + 84) * 4;
        assert_eq!(thumb.pixels[at + 3], 255);
    }

    #[test]
    fn png_is_preferred_over_tiff() {
        let tmp = tempfile::tempdir().unwrap();
        let png = encode_png(&flat(400, 200)).unwrap();
        let reps = [(TYPE_TIFF, grey_tiff(300, 100, 90)), (TYPE_PNG, png)];
        write_thumb(tmp.path(), &picture(&reps)).unwrap();
        assert_eq!(read_thumb(tmp.path()).width, 112);
    }

    #[test]
    fn copied_picture_file_gets_a_thumb() {
        let tmp = tempfile::tempdir().unwrap();
        let photo = tmp.path().join("photo.png");
        fs::write(&photo, encode_png(&flat(400, 200)).unwrap()).unwrap();
        let folder = tmp.path().join("7");
        fs::create_dir(&folder).unwrap();
        write_file_thumb(&folder, &photo).unwrap();
        let thumb = read_thumb(&folder);
        assert_eq!((thumb.width, thumb.height), (112, 56));
        assert!(write_file_thumb(&folder, &tmp.path().join("gone.png")).is_err());
    }

    #[test]
    fn no_thumb_without_a_readable_picture() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(write_thumb(tmp.path(), &picture(&[(TYPE_TIFF, vec![1, 2, 3])])).is_err());
        assert!(write_thumb(tmp.path(), &picture(&[(TYPE_PNG, vec![1, 2, 3])])).is_err());
        assert!(write_thumb(tmp.path(), &picture(&[("public.utf8-plain-text", vec![1])])).is_err());
        assert!(write_thumb(tmp.path(), &ClipSnapshot { items: vec![] }).is_err());
        assert!(fs::read_dir(tmp.path()).unwrap().next().is_none());
    }
}
