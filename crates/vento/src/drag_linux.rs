//! Linux stand-in for the `drag` crate, which needs GTK there: the same
//! types and `start_drag` the Pocket uses, on top of the platform's XDND
//! drag, so the Pocket keeps its drag-out logic unchanged.

use std::path::PathBuf;

use raw_window_handle::HasWindowHandle;

/// How a drag ended.
#[derive(Debug, Clone, Copy)]
pub enum DragResult {
    Dropped,
    Cancel,
}

/// What a drag carries.
pub enum DragItem {
    Files(Vec<PathBuf>),
}

/// The image shown beside the cursor: PNG bytes.
pub enum Image {
    Raw(Vec<u8>),
}

/// The PNG `bytes` as RGBA pixels; `None` for anything but 8-bit RGBA.
fn decode(bytes: &[u8]) -> Option<vento_platform::RgbaImage> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .ok()?;
    let mut pixels = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    pixels.truncate(info.buffer_size());
    Some(vento_platform::RgbaImage {
        width: info.width,
        height: info.height,
        pixels,
    })
}

/// Where the drag ended, logical global coordinates.
#[derive(Debug, Clone)]
pub struct CursorPosition {
    pub x: i32,
    pub y: i32,
}

/// Braced, so `Options::default()` reads as it does with the crate.
#[derive(Default)]
pub struct Options {}

/// Starts dragging `item` out while the press on `window` that started it
/// is held; `on_drop` hears how and where it ended, on another thread.
pub fn start_drag<W, F>(
    _window: &W,
    item: DragItem,
    image: Image,
    on_drop: F,
    _options: Options,
) -> Result<(), String>
where
    W: HasWindowHandle,
    F: Fn(DragResult, CursorPosition) + Send + 'static,
{
    let DragItem::Files(paths) = item;
    let Image::Raw(png) = image;
    vento_platform::start_file_drag(
        paths,
        decode(&png),
        Box::new(move |end| {
            let result = if end.dropped {
                DragResult::Dropped
            } else {
                DragResult::Cancel
            };
            let at = CursorPosition {
                x: end.at.x.round() as i32,
                y: end.at.y.round() as i32,
            };
            on_drop(result, at);
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_decode_to_their_pixels() {
        let icon = vento_platform::RgbaImage {
            width: 2,
            height: 1,
            pixels: vec![255, 0, 0, 255, 0, 0, 255, 128],
        };
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, icon.width, icon.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&icon.pixels).unwrap();
        writer.finish().unwrap();
        assert_eq!(decode(&png), Some(icon));
        assert_eq!(decode(b"not a png"), None);
    }
}
