//! Vento's own X11 windows (it runs under XWayland on GNOME).

use std::cell::OnceCell;
use std::time::Duration;

use raw_window_handle::RawWindowHandle;
use vento_core::geometry::Rect;
use x11rb::connection::Connection;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ClipOrdering, ConnectionExt, CreateGCAux, ImageFormat, MapState,
    Rectangle,
};
use x11rb::rust_connection::RustConnection;

use crate::PlatformError;

/// When to make a window draw again after its input shape changed; see
/// [`recommit_later`].
const RECOMMIT_AFTER: [Duration; 2] = [Duration::from_millis(60), Duration::from_millis(250)];

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

/// Runs `f` on our connection; `Unsupported` without an X server.
fn with_conn<T>(
    f: impl FnOnce(&RustConnection) -> Result<T, PlatformError>,
) -> Result<T, PlatformError> {
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
        f(conn)
    })
}

fn failed(e: &dyn std::fmt::Display) -> PlatformError {
    PlatformError::Failed(e.to_string())
}

/// Makes the window of `handle` override-redirect: GNOME then leaves it
/// exactly where vento puts it, above every window (full-screen ones too),
/// out of the Dock and the window switcher, and never gives it focus. X11
/// applies this when a window is mapped, so a window on screen is mapped
/// again.
pub(super) fn make_override_redirect(handle: RawWindowHandle) -> Result<(), PlatformError> {
    let window = window_id(handle)?;
    with_conn(|conn| {
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

/// The X11 rectangle covering `rect`, rounded out to whole pixels.
fn pixel_rect(rect: &Rect) -> Rectangle {
    let x = rect.x.floor();
    let y = rect.y.floor();
    Rectangle {
        x: x as i16,
        y: y as i16,
        width: ((rect.x + rect.w).ceil() - x).max(0.0) as u16,
        height: ((rect.y + rect.h).ceil() - y).max(0.0) as u16,
    }
}

/// Sets the input shape of the window of `handle` to `rect`, or to nothing.
pub(super) fn set_input_rect(
    handle: RawWindowHandle,
    rect: Option<Rect>,
) -> Result<(), PlatformError> {
    let window = window_id(handle)?;
    let rects: Vec<Rectangle> = rect.iter().map(pixel_rect).collect();
    with_conn(|conn| {
        conn.shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            ClipOrdering::UNSORTED,
            window,
            0,
            0,
            &rects,
        )
        .map_err(|e| failed(&e))?;
        conn.flush().map_err(|e| failed(&e))?;
        Ok(())
    })?;
    recommit_later(window);
    Ok(())
}

/// GNOME applies an XWayland window's input shape only when the window
/// next draws, and the frame drawn along with a change can reach it before
/// the shape does: the window then keeps its old shape, and presses on the
/// new part fall through to what is below. So the window draws again a
/// little later, its top-left pixel written back unchanged.
fn recommit_later(window: u32) {
    let spawned = std::thread::Builder::new()
        .name("vento-input-shape".into())
        .spawn(move || {
            let conn = match RustConnection::connect(None) {
                Ok((conn, _)) => conn,
                Err(e) => return log::debug!("no X11 connection to redraw with: {e}"),
            };
            for wait in RECOMMIT_AFTER {
                std::thread::sleep(wait);
                if let Err(e) = rewrite_pixel(&conn, window) {
                    // Gone or hidden meanwhile: nothing left to redraw.
                    return log::debug!("cannot redraw window {window:#x}: {e}");
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("cannot start the input shape redraw: {e}");
    }
}

/// Writes the top-left pixel of `window` back as it is.
fn rewrite_pixel(conn: &RustConnection, window: u32) -> Result<(), Box<dyn std::error::Error>> {
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, 1, 1, !0)?
        .reply()?;
    let gc = conn.generate_id()?;
    conn.create_gc(gc, window, &CreateGCAux::new())?;
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        window,
        gc,
        1,
        1,
        0,
        0,
        0,
        image.depth,
        &image.data,
    )?;
    conn.free_gc(gc)?;
    conn.flush()?;
    Ok(())
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

    #[test]
    fn input_rects_round_out_to_whole_pixels() {
        let r = pixel_rect(&Rect {
            x: 1.5,
            y: 0.2,
            w: 10.0,
            h: 3.6,
        });
        assert_eq!((r.x, r.y, r.width, r.height), (1, 0, 11, 4));
    }
}
