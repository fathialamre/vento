//! Drags in other apps. Windows keeps no drag pasteboard that another app
//! can read, as macOS does; what shows a drag is the mouse capture: while
//! OLE's `DoDragDrop` runs, it holds the capture in a window of its own,
//! of class `CLIPBRDWNDCLASS`, on the thread the drag started from. That
//! tells a drag of files (or text, or anything OLE carries) from a text
//! selection or a window being moved. What a drag carries shows only to
//! the window it is over: the wheel learns its files once it is under the
//! pointer (winit's `HoveredFile`).

use windows_sys::Win32::UI::WindowsAndMessaging::{GUITHREADINFO, GetGUIThreadInfo};

/// The class of the window OLE captures the mouse with during a drag.
const DRAG_CAPTURE_CLASS: &str = "CLIPBRDWNDCLASS";

/// Whether an OLE drag is under way in the app in front.
pub(super) fn active() -> bool {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is sized; thread 0 is the one in front.
    if unsafe { GetGUIThreadInfo(0, &mut info) } == 0 || info.hwndCapture.is_null() {
        return false;
    }
    super::class_of(info.hwndCapture) == DRAG_CAPTURE_CLASS
}

/// Counts drags as they start, for `InputSample::drag_change_count`.
#[derive(Debug, Default)]
pub(super) struct DragCounter {
    count: i64,
    dragging: bool,
}

impl DragCounter {
    /// Takes whether a drag is under way now; returns the count, which
    /// moved on if a drag started since the last call.
    pub fn feed(&mut self, dragging: bool) -> i64 {
        if dragging && !self.dragging {
            self.count += 1;
        }
        self.dragging = dragging;
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_moves_once_per_drag() {
        let mut c = DragCounter::default();
        assert_eq!(c.feed(false), 0);
        assert_eq!(c.feed(true), 1);
        assert_eq!(c.feed(true), 1);
        assert_eq!(c.feed(false), 1);
        assert_eq!(c.feed(true), 2);
    }

    #[test]
    fn no_drag_runs_during_the_tests() {
        assert!(!active());
    }
}
