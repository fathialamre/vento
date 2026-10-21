//! Lookups the Info window needs: a file's kind, whether a folder is a
//! package, how to keep cloud files from downloading, and which volumes make
//! up the one the user sees.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSNumber, NSString, NSURL, NSURLIsPackageKey, NSURLLocalizedTypeDescriptionKey,
};
use vento_core::sizes::VolumeScope;

use super::clipboard::file_url;
use crate::PlatformError;

/// `setiopolicy_np`'s policy type for materialising dataless (cloud) files.
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: i32 = 3;
/// Applies the policy to the calling thread only.
const IOPOL_SCOPE_THREAD: i32 = 1;
/// Reading a dataless file fails instead of downloading it.
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: i32 = 1;

unsafe extern "C" {
    // `libc` does not declare this one; see `sys/resource.h`.
    fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
}

/// The one Foundation resource value `key` of `path` as an object, `None` if
/// it can't be read.
fn resource_value(
    path: &Path,
    key: &objc2_foundation::NSURLResourceKey,
) -> Option<Retained<AnyObject>> {
    let url: Retained<NSURL> = file_url(path).ok()?;
    let mut value: Option<Retained<AnyObject>> = None;
    // SAFETY: `value` is an out-pointer for an object; callers cast it to
    // the documented type of `key` with `downcast`.
    unsafe { url.getResourceValue_forKey_error(&mut value, key) }.ok()?;
    value
}

/// The localised kind Finder shows for `path` ("PNG image", "Folder"), or
/// `None` if the system doesn't know. Safe on any thread.
pub(super) fn kind_description(path: &Path) -> Option<String> {
    autoreleasepool(|_| {
        // SAFETY: the key is a static string constant.
        let value = resource_value(path, unsafe { NSURLLocalizedTypeDescriptionKey })?;
        let kind = value.downcast::<NSString>().ok()?.to_string();
        (!kind.is_empty()).then_some(kind)
    })
}

/// Whether `path` is a package: a folder Finder shows as one file (an app,
/// a bundle). False for files and missing paths, without calling into
/// Foundation. Safe on any thread.
pub(crate) fn is_package(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    autoreleasepool(|_| {
        // SAFETY: the key is a static string constant.
        resource_value(path, unsafe { NSURLIsPackageKey })
            .and_then(|v| v.downcast::<NSNumber>().ok())
            .is_some_and(|n| n.boolValue())
    })
}

/// Makes reads on the calling thread fail on cloud-only files rather than
/// download them. Affects this thread only, so call it on the worker that
/// walks.
pub(crate) fn block_cloud_downloads_on_this_thread() -> Result<(), PlatformError> {
    // SAFETY: plain integer arguments, matching the declaration above.
    let rc = unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_THREAD,
            IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(PlatformError::Failed(format!(
            "cannot block cloud downloads: {}",
            std::io::Error::last_os_error()
        )))
    }
}

/// How a walk starting at `start` treats volumes. The user's files live on
/// the Data volume, which the system volume at `/` shows through firmlinks;
/// when `start` is on either, both count as one and the Data volume's
/// mount point is not entered a second time. Anything else is left to the
/// default (stay on `start`'s device).
pub(crate) fn volume_scope(start: &Path) -> VolumeScope {
    let dev = |p: &str| std::fs::metadata(p).ok().map(|m| m.dev());
    let (Some(start_dev), Some(root), Some(data)) = (
        std::fs::metadata(start).ok().map(|m| m.dev()),
        dev("/"),
        dev("/System/Volumes/Data"),
    ) else {
        return VolumeScope::default();
    };
    if start_dev != root && start_dev != data {
        return VolumeScope::default();
    }
    VolumeScope {
        also_same: vec![root, data],
        skip: vec![PathBuf::from("/System/Volumes/Data")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vento-info-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn apps_are_packages() {
        let dir = scratch("pkg");
        std::fs::create_dir_all(dir.join("Foo.app/Contents")).unwrap();
        std::fs::create_dir_all(dir.join("plain")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        assert!(is_package(&dir.join("Foo.app")));
        assert!(!is_package(&dir.join("plain")));
        assert!(!is_package(&dir.join("a.txt")));
        assert!(!is_package(&dir.join("missing")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn kind_of_a_png() {
        let dir = scratch("kind");
        std::fs::write(dir.join("x.png"), "").unwrap();
        let kind = kind_description(&dir.join("x.png")).expect("a kind");
        assert!(kind.contains("PNG"), "{kind}");
        let folder = kind_description(&dir).expect("a kind");
        assert!(!folder.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cloud_downloads_can_be_blocked() {
        let result = std::thread::spawn(block_cloud_downloads_on_this_thread)
            .join()
            .unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn the_data_volume_counts_as_root() {
        let scope = volume_scope(Path::new("/Users"));
        assert_eq!(scope.skip, vec![PathBuf::from("/System/Volumes/Data")]);
        assert_eq!(scope.also_same.len(), 2);
    }
}
