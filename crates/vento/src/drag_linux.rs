//! Linux stand-in for the `drag` crate, which needs GTK there: the same
//! types the Pocket uses, and a [`start_drag`] that says dragging out is
//! not available yet, so the Pocket keeps its drag-out logic unchanged.
//! Values the Pocket only receives from a real drag are never made here.
#![allow(dead_code)]

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

/// The image shown under the cursor.
pub enum Image {
    Raw(Vec<u8>),
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

/// Never starts a drag: the error is what the Pocket logs.
pub fn start_drag<W, F>(
    _window: &W,
    _item: DragItem,
    _image: Image,
    _on_drop: F,
    _options: Options,
) -> Result<(), String>
where
    W: HasWindowHandle,
    F: Fn(DragResult, CursorPosition) + Send + 'static,
{
    Err("dragging out of the Pocket is not available on Linux yet".into())
}
