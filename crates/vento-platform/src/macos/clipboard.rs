//! General pasteboard writes.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSString, NSURL};

use crate::PlatformError;

pub(super) fn copy_text(text: &str) -> Result<(), PlatformError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    let ok =
        pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    if ok {
        Ok(())
    } else {
        Err(PlatformError::Failed("clipboard rejected the text".into()))
    }
}

pub(super) fn copy_files(paths: &[PathBuf]) -> Result<(), PlatformError> {
    let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
        .iter()
        .map(|p| file_url(p).map(ProtocolObject::from_retained))
        .collect::<Result<_, _>>()?;
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    if pasteboard.writeObjects(&NSArray::from_retained_slice(&urls)) {
        Ok(())
    } else {
        Err(PlatformError::Failed("clipboard rejected the files".into()))
    }
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, PlatformError> {
    let s = path.to_str().ok_or_else(|| {
        PlatformError::Failed(format!("path is not valid UTF-8: {}", path.display()))
    })?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(s)))
}
