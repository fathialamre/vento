//! Turning a winit window into a Pocket overlay: above the menu bar, on
//! every Space, out of the window cycle.

use objc2_app_kit::{NSStatusWindowLevel, NSWindowCollectionBehavior};
use raw_window_handle::RawWindowHandle;

use crate::PlatformError;

/// Set the window behind `handle` to status-window level, join it to all
/// Spaces (full-screen ones included) as a stationary, cycle-ignoring
/// window, drop its shadow, and keep AppKit from pushing it below the menu
/// bar.
pub(super) fn make_overlay(handle: RawWindowHandle) -> Result<(), PlatformError> {
    let window = super::patch::window_of(handle, "make_overlay")?;
    window.setLevel(NSStatusWindowLevel);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    window.setHasShadow(false);
    super::patch::keep_frame(&window)
}
