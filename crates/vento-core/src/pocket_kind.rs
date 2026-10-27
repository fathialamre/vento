//! What kind of thing a Pocket item is, from its extension: the groups a full
//! Pocket's Stacks layout and its kind filter are made of.

use std::path::Path;

/// The kind of a Pocket item. Folders are told apart by the filesystem, every
/// other kind by the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PocketKind {
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Folder,
    /// No extension, or one vento does not know.
    Other,
}

impl PocketKind {
    /// Every kind, in the order a full Pocket lists them.
    pub const ALL: [Self; 7] = [
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::Document,
        Self::Archive,
        Self::Folder,
        Self::Other,
    ];

    /// The name the Pocket shows for the kind's group.
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "Images",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Document => "Documents",
            Self::Archive => "Archives",
            Self::Folder => "Folders",
            Self::Other => "Other",
        }
    }
}

const IMAGES: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "heic", "heif", "webp", "svg", "tif", "tiff", "bmp", "ico",
    "avif", "raw", "dng", "cr2", "nef", "arw",
];
const VIDEOS: &[&str] = &["mov", "mp4", "m4v", "mkv", "avi", "webm"];
const AUDIO: &[&str] = &["mp3", "m4a", "wav", "aiff", "flac", "ogg"];
const DOCUMENTS: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pages", "numbers", "key", "txt", "md",
    "rtf", "csv", "odt", "ods", "odp", "epub",
];
const ARCHIVES: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "dmg", "iso",
];

/// The kind of the item at `path`; `is_dir` says whether it is a folder (a
/// folder is a folder whatever its name ends in, so `Photos.app` is one).
pub fn kind_of(path: &Path, is_dir: bool) -> PocketKind {
    if is_dir {
        return PocketKind::Folder;
    }
    let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
        return PocketKind::Other;
    };
    let extension = extension.to_ascii_lowercase();
    let has = |list: &[&str]| list.contains(&extension.as_str());
    if has(IMAGES) {
        PocketKind::Image
    } else if has(VIDEOS) {
        PocketKind::Video
    } else if has(AUDIO) {
        PocketKind::Audio
    } else if has(DOCUMENTS) {
        PocketKind::Document
    } else if has(ARCHIVES) {
        PocketKind::Archive
    } else {
        PocketKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_the_extension_or_a_folder() {
        use std::path::Path;
        assert_eq!(kind_of(Path::new("/a/Logo.PNG"), false), PocketKind::Image);
        assert_eq!(kind_of(Path::new("/a/clip.mov"), false), PocketKind::Video);
        assert_eq!(kind_of(Path::new("/a/song.mp3"), false), PocketKind::Audio);
        assert_eq!(
            kind_of(Path::new("/a/report.pdf"), false),
            PocketKind::Document
        );
        assert_eq!(kind_of(Path::new("/a/src.zip"), false), PocketKind::Archive);
        assert_eq!(kind_of(Path::new("/a/project"), true), PocketKind::Folder);
        assert_eq!(kind_of(Path::new("/a/Makefile"), false), PocketKind::Other);
        assert_eq!(kind_of(Path::new("/a/thing.xyz"), false), PocketKind::Other);
    }
}
