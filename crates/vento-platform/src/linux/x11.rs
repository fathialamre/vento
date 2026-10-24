//! Vento's own X11 windows (it runs under XWayland on GNOME).

use std::cell::OnceCell;

use raw_window_handle::RawWindowHandle;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, MapState};
use x11rb::rust_connection::RustConnection;

use crate::PlatformError;

thread_local! {
    /// A connection of our own to the X server the windows live on.
    static CONN: OnceCell<Option<RustConnection>> = const { OnceCell::new() };
}

/// The X11 window id of `handle`; `Unsupported` for a window that is not
/// an X11 one (vento running as a Wayland client).
fn window_id(handle: RawWindowHandle) -> Result<u32, PlatformError> {
    match handle {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window)
            .map_err(|_| PlatformError::Failed(format!("not an X11 window id: {}", h.window))),
        RawWindowHandle::Xcb(h) => Ok(h.window.get()),
        _ => Err(PlatformError::Unsupported),
    }
}

/// Makes the window of `handle` override-redirect: GNOME then leaves it
/// exactly where vento puts it, above every window (full-screen ones too),
/// out of the Dock and the window switcher, and never gives it focus. X11
/// applies this when a window is mapped, so a window on screen is mapped
/// again.
pub(super) fn make_override_redirect(handle: RawWindowHandle) -> Result<(), PlatformError> {
    let window = window_id(handle)?;
    let failed = |e: &dyn std::fmt::Display| PlatformError::Failed(e.to_string());
    CONN.with(|cell| {
        let conn = cell
            .get_or_init(|| {
                RustConnection::connect(None)
                    .inspect_err(|e| log::warn!("no X11 connection: {e}"))
                    .ok()
                    .map(|(conn, _)| conn)
            })
            .as_ref()
            .ok_or(PlatformError::Unsupported)?;
        let attrs = ChangeWindowAttributesAux::new().override_redirect(1);
        conn.change_window_attributes(window, &attrs)
            .map_err(|e| failed(&e))?;
        let mapped = conn
            .get_window_attributes(window)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?
            .map_state
            != MapState::UNMAPPED;
        if mapped {
            conn.unmap_window(window).map_err(|e| failed(&e))?;
            conn.map_window(window).map_err(|e| failed(&e))?;
        }
        conn.flush().map_err(|e| failed(&e))?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use raw_window_handle::{WaylandWindowHandle, XcbWindowHandle, XlibWindowHandle};

    use super::*;

    #[test]
    fn x11_handles_give_their_window_id() {
        assert_eq!(
            window_id(XlibWindowHandle::new(0x0c00006).into()).unwrap(),
            0x0c00006
        );
        let xcb = XcbWindowHandle::new(NonZeroU32::new(42).unwrap());
        assert_eq!(window_id(xcb.into()).unwrap(), 42);
    }

    #[test]
    fn wayland_windows_are_unsupported() {
        let surface = std::ptr::NonNull::<std::ffi::c_void>::dangling();
        assert!(matches!(
            window_id(WaylandWindowHandle::new(surface).into()),
            Err(PlatformError::Unsupported)
        ));
    }
}
