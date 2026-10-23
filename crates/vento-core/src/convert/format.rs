//! Target image formats and the ring hint. Names only, no file access.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// An image format Vento converts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Heic,
    Webp,
    Tiff,
    Gif,
    Bmp,
}

/// Extensions that can be read as images but are never offered as a target.
const READ_ONLY_EXTENSIONS: &[&str] = &[
    "avif", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2",
];

impl ImageFormat {
    /// The formats the ring offers, in wheel order.
    pub const TARGETS: &'static [ImageFormat] = &[
        ImageFormat::Jpeg,
        ImageFormat::Png,
        ImageFormat::Heic,
        ImageFormat::Webp,
        ImageFormat::Tiff,
        ImageFormat::Gif,
        ImageFormat::Bmp,
    ];

    /// Label shown on the ring.
    pub fn name(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Png => "PNG",
            ImageFormat::Heic => "HEIC",
            ImageFormat::Webp => "WebP",
            ImageFormat::Tiff => "TIFF",
            ImageFormat::Gif => "GIF",
            ImageFormat::Bmp => "BMP",
        }
    }

    /// Extension of a converted file, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Png => "png",
            ImageFormat::Heic => "heic",
            ImageFormat::Webp => "webp",
            ImageFormat::Tiff => "tiff",
            ImageFormat::Gif => "gif",
            ImageFormat::Bmp => "bmp",
        }
    }

    /// The format an extension names, ignoring case.
    pub fn from_extension(ext: &OsStr) -> Option<ImageFormat> {
        match ext.to_str()?.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "jpe" => Some(ImageFormat::Jpeg),
            "png" => Some(ImageFormat::Png),
            "heic" | "heif" => Some(ImageFormat::Heic),
            "webp" => Some(ImageFormat::Webp),
            "tif" | "tiff" => Some(ImageFormat::Tiff),
            "gif" => Some(ImageFormat::Gif),
            "bmp" => Some(ImageFormat::Bmp),
            _ => None,
        }
    }
}

/// Whether the name says this is an image Vento can read. Looks at the
/// extension only, never at the file.
pub fn looks_like_image(path: &Path) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };
    ImageFormat::from_extension(ext).is_some()
        || ext
            .to_str()
            .is_some_and(|e| READ_ONLY_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The formats to show for these dragged paths: none when no path looks like
/// an image, `targets` minus the format every image already has, otherwise all
/// of `targets`. Non-images do not count towards "already has".
pub fn ring_formats(dragged: &[PathBuf], targets: &[ImageFormat]) -> Vec<ImageFormat> {
    let images: Vec<&PathBuf> = dragged.iter().filter(|p| looks_like_image(p)).collect();
    if images.is_empty() {
        return Vec::new();
    }
    let mut formats = images
        .iter()
        .map(|p| p.extension().and_then(ImageFormat::from_extension));
    let shared = match formats.next() {
        Some(first @ Some(_)) if formats.all(|f| f == first) => first,
        _ => None,
    };
    targets
        .iter()
        .copied()
        .filter(|f| Some(*f) != shared)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn names_and_extensions() {
        let pairs = [
            (ImageFormat::Jpeg, "JPEG", "jpg"),
            (ImageFormat::Png, "PNG", "png"),
            (ImageFormat::Heic, "HEIC", "heic"),
            (ImageFormat::Webp, "WebP", "webp"),
            (ImageFormat::Tiff, "TIFF", "tiff"),
            (ImageFormat::Gif, "GIF", "gif"),
            (ImageFormat::Bmp, "BMP", "bmp"),
        ];
        for (format, name, ext) in pairs {
            assert_eq!(format.name(), name);
            assert_eq!(format.extension(), ext);
        }
    }

    #[test]
    fn from_extension_is_case_insensitive() {
        assert_eq!(
            ImageFormat::from_extension(OsStr::new("JPG")),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            ImageFormat::from_extension(OsStr::new("Jpeg")),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            ImageFormat::from_extension(OsStr::new("HEIF")),
            Some(ImageFormat::Heic)
        );
        assert_eq!(
            ImageFormat::from_extension(OsStr::new("tif")),
            Some(ImageFormat::Tiff)
        );
        assert_eq!(ImageFormat::from_extension(OsStr::new("txt")), None);
    }

    #[test]
    fn targets_order() {
        assert_eq!(
            ImageFormat::TARGETS,
            [
                ImageFormat::Jpeg,
                ImageFormat::Png,
                ImageFormat::Heic,
                ImageFormat::Webp,
                ImageFormat::Tiff,
                ImageFormat::Gif,
                ImageFormat::Bmp,
            ]
        );
    }

    #[test]
    fn ring_hides_the_shared_format() {
        let ring = ring_formats(&paths(&["a.png", "b.PNG"]), ImageFormat::TARGETS);
        let expected: Vec<_> = ImageFormat::TARGETS
            .iter()
            .copied()
            .filter(|f| *f != ImageFormat::Png)
            .collect();
        assert_eq!(ring, expected);
    }

    #[test]
    fn ring_shows_all_for_mixed() {
        let ring = ring_formats(&paths(&["a.png", "b.jpg"]), ImageFormat::TARGETS);
        assert_eq!(ring, ImageFormat::TARGETS);
    }

    #[test]
    fn ring_shows_all_for_readable_non_target() {
        let ring = ring_formats(&paths(&["a.avif"]), ImageFormat::TARGETS);
        assert_eq!(ring, ImageFormat::TARGETS);
    }

    #[test]
    fn ring_ignores_non_images_when_deciding() {
        let ring = ring_formats(&paths(&["a.png", "notes.txt"]), ImageFormat::TARGETS);
        let expected: Vec<_> = ImageFormat::TARGETS
            .iter()
            .copied()
            .filter(|f| *f != ImageFormat::Png)
            .collect();
        assert_eq!(ring, expected);
    }

    #[test]
    fn ring_is_empty_without_images() {
        let ring = ring_formats(&paths(&["notes.txt", "folder"]), ImageFormat::TARGETS);
        assert!(ring.is_empty());
    }
}
