//! Vento's own X11 windows (it runs under XWayland on GNOME).

use std::cell::OnceCell;
use std::time::{Duration, Instant};

use raw_window_handle::RawWindowHandle;
use vento_core::geometry::Rect;
use x11rb::connection::Connection;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClipOrdering, ConnectionExt, CreateGCAux, ImageFormat,
    MapState, Rectangle,
};
use x11rb::rust_connection::RustConnection;

use crate::PlatformError;

/// How long GNOME may take to let go of a window turning override-redirect.
const RELEASE_TIMEOUT: Duration = Duration::from_millis(500);

/// When to make a window draw again after its input shape changed; see
/// [`recommit_later`].
const RECOMMIT_AFTER: [Duration; 2] = [Duration::from_millis(60), Duration::from_millis(250)];

thread_local! {
    /// A connection of our own to the X server the windows live on.
    static CONN: OnceCell<Option<RustConnection>> = const { OnceCell::new() };
    /// The atoms [`front_window`] reads, interned on first use.
    static FRONT_ATOMS: OnceCell<Option<FrontAtoms>> = const { OnceCell::new() };
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
/// applies this when a window is mapped, so the window is mapped again, once
/// GNOME has let go of it: callers have just shown it, or are about to.
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
        let tree = conn
            .query_tree(window)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?;
        // In a frame of GNOME's: GNOME manages it.
        let framed = tree.parent != tree.root;
        if mapped || framed {
            conn.unmap_window(window).map_err(|e| failed(&e))?;
            conn.flush().map_err(|e| failed(&e))?;
            // GNOME takes a window it manages out of its frame once it sees
            // it unmapped; mapped again before that, it ends up unmapped.
            let deadline = Instant::now() + RELEASE_TIMEOUT;
            while framed && Instant::now() < deadline {
                let parent = conn
                    .query_tree(window)
                    .map_err(|e| failed(&e))?
                    .reply()
                    .map_err(|e| failed(&e))?
                    .parent;
                if parent == tree.root {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // Also when it looked unmapped: shown just before, it may wait on a
        // map request GNOME now ignores, as for any override-redirect window.
        conn.map_window(window).map_err(|e| failed(&e))?;
        conn.flush().map_err(|e| failed(&e))?;
        Ok(())
    })
}

/// The window types that are not an app's own window: a panel and the
/// desktop.
const SHELL_TYPES: [&str; 2] = ["_NET_WM_WINDOW_TYPE_DOCK", "_NET_WM_WINDOW_TYPE_DESKTOP"];

/// Whether a window of the `_NET_WM_WINDOW_TYPE`s `types` is an app's own
/// window; one with none set is a normal window.
pub(super) fn counts_as_front(types: &[&str]) -> bool {
    !types.iter().any(|t| SHELL_TYPES.contains(t))
}

/// What [`front_window`] asks the X server about.
#[derive(Debug, Clone, Copy)]
struct FrontAtoms {
    active: u32,
    wm_type: u32,
    dock: u32,
    desktop: u32,
    pid: u32,
    /// The window manager's frame around a window.
    frame_extents: u32,
    /// The shadow a client-side decorated (GTK) window draws inside its
    /// own window.
    gtk_frame_extents: u32,
}

impl FrontAtoms {
    /// All at once, in one round trip.
    fn intern(conn: &RustConnection) -> Result<Self, PlatformError> {
        let names = [
            "_NET_ACTIVE_WINDOW",
            "_NET_WM_WINDOW_TYPE",
            SHELL_TYPES[0],
            SHELL_TYPES[1],
            "_NET_WM_PID",
            "_NET_FRAME_EXTENTS",
            "_GTK_FRAME_EXTENTS",
        ];
        let mut cookies = Vec::with_capacity(names.len());
        for name in names {
            cookies.push(
                conn.intern_atom(false, name.as_bytes())
                    .map_err(|e| failed(&e))?,
            );
        }
        let mut atoms = [0; 7];
        for (atom, cookie) in atoms.iter_mut().zip(cookies) {
            *atom = cookie.reply().map_err(|e| failed(&e))?.atom;
        }
        let [
            active,
            wm_type,
            dock,
            desktop,
            pid,
            frame_extents,
            gtk_frame_extents,
        ] = atoms;
        Ok(Self {
            active,
            wm_type,
            dock,
            desktop,
            pid,
            frame_extents,
            gtk_frame_extents,
        })
    }

    /// The name of the window type `atom`, as far as [`counts_as_front`]
    /// tells them apart.
    fn type_name(&self, atom: u32) -> &'static str {
        if atom == self.dock {
            SHELL_TYPES[0]
        } else if atom == self.desktop {
            SHELL_TYPES[1]
        } else {
            ""
        }
    }
}

/// The frame of the active window as the user sees it, in X11 pixels of
/// the root window, and the root window's width; `None` when no window is
/// active, or it is vento's own, a panel or the desktop. Two round trips.
pub(super) fn front_window() -> Option<(Rect, u16)> {
    let atoms = FRONT_ATOMS.with(|cell| {
        *cell.get_or_init(|| {
            with_conn(FrontAtoms::intern)
                .inspect_err(|e| log::debug!("no atoms for the front window: {e}"))
                .ok()
        })
    })?;
    with_conn(|conn| Ok(front_window_on(conn, &atoms)))
        .ok()
        .flatten()
}

fn front_window_on(conn: &RustConnection, atoms: &FrontAtoms) -> Option<(Rect, u16)> {
    let root = conn.setup().roots.first()?;
    let window = conn
        .get_property(false, root.root, atoms.active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()
        .filter(|&w| w != 0)?;
    // Everything asked at once, then the answers read.
    let types = conn
        .get_property(false, window, atoms.wm_type, AtomEnum::ATOM, 0, 32)
        .ok()?;
    let pid = conn
        .get_property(false, window, atoms.pid, AtomEnum::CARDINAL, 0, 1)
        .ok()?;
    let frame = conn
        .get_property(false, window, atoms.frame_extents, AtomEnum::CARDINAL, 0, 4)
        .ok()?;
    let shadow = conn
        .get_property(
            false,
            window,
            atoms.gtk_frame_extents,
            AtomEnum::CARDINAL,
            0,
            4,
        )
        .ok()?;
    let size = conn.get_geometry(window).ok()?;
    let origin = conn.translate_coordinates(window, root.root, 0, 0).ok()?;

    let types = types.reply().ok()?;
    let normal = types
        .value32()
        .is_none_or(|mut t| t.all(|t| counts_as_front(&[atoms.type_name(t)])));
    let pid = pid.reply().ok()?.value32().and_then(|mut v| v.next());
    let extents = |reply: x11rb::protocol::xproto::GetPropertyReply| {
        let mut v = reply.value32()?;
        let mut next = || v.next().map(f64::from);
        // Left, right, top, bottom.
        Some([next()?, next()?, next()?, next()?])
    };
    let frame = frame.reply().ok().and_then(extents);
    let shadow = shadow.reply().ok().and_then(extents);
    let size = size.reply().ok()?;
    let origin = origin.reply().ok()?;
    if !normal || pid == Some(std::process::id()) {
        return None;
    }
    let mut r = Rect {
        x: f64::from(origin.dst_x),
        y: f64::from(origin.dst_y),
        w: f64::from(size.width),
        h: f64::from(size.height),
    };
    if let Some([left, right, top, bottom]) = frame {
        r = Rect {
            x: r.x - left,
            y: r.y - top,
            w: r.w + left + right,
            h: r.h + top + bottom,
        };
    }
    if let Some([left, right, top, bottom]) = shadow {
        r = Rect {
            x: r.x + left,
            y: r.y + top,
            w: (r.w - left - right).max(0.0),
            h: (r.h - top - bottom).max(0.0),
        };
    }
    Some((r, root.width_in_pixels))
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

/// Sets the input shape of the window of `handle` to `rects`, or to
/// nothing when there are none.
pub(super) fn set_input_rects(
    handle: RawWindowHandle,
    rects: &[Rect],
) -> Result<(), PlatformError> {
    let window = window_id(handle)?;
    let rects: Vec<Rectangle> = rects.iter().map(pixel_rect).collect();
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
    fn docks_and_desktops_are_not_front_windows() {
        assert!(!counts_as_front(&["_NET_WM_WINDOW_TYPE_DOCK"]));
        assert!(!counts_as_front(&["_NET_WM_WINDOW_TYPE_DESKTOP"]));
        assert!(counts_as_front(&["_NET_WM_WINDOW_TYPE_NORMAL"]));
        assert!(counts_as_front(&[]));
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
