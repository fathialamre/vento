//! Where screenshots are kept on systems with a Pictures folder.

use std::path::PathBuf;

/// The `Screenshots` folder in the Pictures folder `pictures` (the user's
/// own, when the system names one), else in `home/Pictures`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn in_pictures(pictures: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    pictures
        .unwrap_or_else(|| home.unwrap_or_default().join("Pictures"))
        .join("Screenshots")
}

/// The folder screenshots are kept in on Linux and Windows.
#[cfg(any(target_os = "linux", windows))]
pub(crate) fn screenshot_folder() -> PathBuf {
    let pictures = directories::UserDirs::new().and_then(|d| d.picture_dir().map(PathBuf::from));
    in_pictures(pictures, std::env::home_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshots_go_in_the_pictures_folder() {
        let pictures = PathBuf::from("/home/a/Bilder");
        assert_eq!(
            in_pictures(Some(pictures), Some(PathBuf::from("/home/a"))),
            PathBuf::from("/home/a/Bilder/Screenshots")
        );
    }

    #[test]
    fn without_a_pictures_folder_they_go_under_home() {
        assert_eq!(
            in_pictures(None, Some(PathBuf::from("/home/a"))),
            PathBuf::from("/home/a/Pictures/Screenshots")
        );
    }
}
