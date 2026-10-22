//! General pasteboard writes.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};
use vento_core::clip::{ClipRep, ClipSnapshot};

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

pub(super) fn file_url(path: &Path) -> Result<Retained<NSURL>, PlatformError> {
    let s = path.to_str().ok_or_else(|| {
        PlatformError::Failed(format!("path is not valid UTF-8: {}", path.display()))
    })?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(s)))
}

/// Every type name of every item on `pb`, in order, duplicates kept.
pub(super) fn types_of(pb: &NSPasteboard) -> Vec<String> {
    let Some(items) = pb.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .flat_map(|item| {
            item.types()
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// All items on `pb` with every representation that has data. A type whose
/// data cannot be had is left out, and an item left with none is dropped.
pub(super) fn read_from(pb: &NSPasteboard) -> ClipSnapshot {
    let Some(items) = pb.pasteboardItems() else {
        return ClipSnapshot::default();
    };
    let items = items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| {
                    item.dataForType(&ty).map(|data| ClipRep {
                        ty: ty.to_string(),
                        data: data.to_vec(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .filter(|reps| !reps.is_empty())
        .collect();
    ClipSnapshot { items }
}

/// Replace the contents of `pb` with `s`, one pasteboard item per snapshot
/// item (items without representations are skipped); returns the change
/// count after writing. An empty snapshot just clears `pb`.
pub(super) fn write_to(pb: &NSPasteboard, s: &ClipSnapshot) -> Result<i64, PlatformError> {
    let cleared = pb.clearContents();
    let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = s
        .items
        .iter()
        .filter(|reps| !reps.is_empty())
        .map(|reps| {
            let item = NSPasteboardItem::new();
            for rep in reps {
                let ty = NSString::from_str(&rep.ty);
                if !item.setData_forType(&NSData::with_bytes(&rep.data), &ty) {
                    log::warn!("clipboard item rejected the type {}", rep.ty);
                }
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    if objects.is_empty() {
        return Ok(cleared as i64);
    }
    if pb.writeObjects(&NSArray::from_retained_slice(&objects)) {
        Ok(pb.changeCount() as i64)
    } else {
        Err(PlatformError::Failed(
            "clipboard rejected the snapshot".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::TempPasteboard;
    use super::*;

    fn rep(ty: &str, data: &[u8]) -> ClipRep {
        ClipRep {
            ty: ty.into(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn snapshot_round_trips_through_real_pasteboard() {
        let pb = TempPasteboard::new();
        let written = ClipSnapshot {
            items: vec![
                vec![
                    rep("public.utf8-plain-text", b"A1"),
                    rep("public.png", &[1, 2, 3]),
                    rep("com.microsoft.excel.xml", b"<x/>"),
                ],
                vec![rep("public.file-url", b"file:///tmp/v.txt")],
            ],
        };
        write_to(&pb.0, &written).unwrap();
        let read = read_from(&pb.0);
        // AppKit may add derived types, so every written rep must be there
        // with identical bytes, not the lists equal.
        assert_eq!(read.items.len(), written.items.len());
        for (want, got) in written.items.iter().zip(&read.items) {
            for r in want {
                let found = got.iter().find(|g| g.ty == r.ty);
                assert_eq!(found, Some(r), "missing or changed {}", r.ty);
            }
        }
    }

    #[test]
    fn types_of_lists_markers() {
        let pb = TempPasteboard::new();
        let s = ClipSnapshot {
            items: vec![vec![
                rep("public.utf8-plain-text", b"secret"),
                rep("org.nspasteboard.ConcealedType", b""),
            ]],
        };
        write_to(&pb.0, &s).unwrap();
        let types = types_of(&pb.0);
        assert!(types.iter().any(|t| t == "org.nspasteboard.ConcealedType"));
        assert!(types.iter().any(|t| t == "public.utf8-plain-text"));
    }

    #[test]
    fn read_keeps_a_zero_length_marker() {
        // The secret re-check runs on the snapshot's own type names, so an
        // empty marker must survive the read.
        let pb = TempPasteboard::new();
        let s = ClipSnapshot {
            items: vec![vec![
                rep("public.utf8-plain-text", b"secret"),
                rep("org.nspasteboard.ConcealedType", b""),
            ]],
        };
        write_to(&pb.0, &s).unwrap();
        let read = read_from(&pb.0);
        assert!(
            read.items[0].contains(&rep("org.nspasteboard.ConcealedType", b"")),
            "{read:?}"
        );
    }

    #[test]
    fn write_returns_new_change_count() {
        let pb = TempPasteboard::new();
        let before = pb.0.changeCount() as i64;
        let s = ClipSnapshot {
            items: vec![vec![rep("public.utf8-plain-text", b"hi")]],
        };
        let after = write_to(&pb.0, &s).unwrap();
        assert_eq!(after, pb.0.changeCount() as i64);
        assert_ne!(after, before);
    }

    #[test]
    fn empty_snapshot_clears_and_items_without_reps_are_skipped() {
        let pb = TempPasteboard::new();
        let s = ClipSnapshot {
            items: vec![vec![rep("public.utf8-plain-text", b"old")]],
        };
        write_to(&pb.0, &s).unwrap();
        let before = pb.0.changeCount() as i64;
        let after = write_to(&pb.0, &ClipSnapshot::default()).unwrap();
        assert_ne!(after, before);
        assert!(read_from(&pb.0).items.is_empty());

        let s = ClipSnapshot {
            items: vec![vec![], vec![rep("public.utf8-plain-text", b"kept")]],
        };
        write_to(&pb.0, &s).unwrap();
        let read = read_from(&pb.0);
        assert_eq!(read.items.len(), 1);
        assert!(read.items[0].contains(&rep("public.utf8-plain-text", b"kept")));
    }
}
