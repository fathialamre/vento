//! Dragging files out of vento's windows to other apps: an XDND drag from
//! X11 (vento runs under XWayland), which GNOME carries on to Wayland apps
//! such as Files.
//!
//! The press that starts a drag keeps its own grab, held by the pressed
//! window's connection (winit's): letting go of it mid-press makes GNOME hand
//! the held button to the window under the pointer, which then draws a
//! selection. So the drag reads the pointer and the button from vento's
//! GNOME Shell extension instead of X11 events, and a connection of its own,
//! on a thread, owns the drag's data and talks XDND to the window under the
//! pointer.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gio::prelude::*;
use vento_core::geometry::Point;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ClipOrdering, ColormapAlloc, ConfigureWindowAux, ConnectionExt,
    CreateGCAux, CreateWindowAux, EventMask, GrabMode, ImageFormat, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, StackMode, VisualClass,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use crate::RgbaImage;

/// The XDND version spoken.
const XDND_VERSION: u32 = 5;
/// How long a dropped-on window has to say it is done.
const FINISH_TIMEOUT: Duration = Duration::from_secs(3);
/// `XK_Escape`.
const ESCAPE_KEYSYM: u32 = 0xff1b;
/// How often the pointer is read from the extension.
const POINTER_SAMPLE: Duration = Duration::from_millis(16);
/// How long the extension may stay silent before the drag gives up.
const POINTER_SILENCE: Duration = Duration::from_secs(2);
/// Where the drag's picture sits from the pointer, so it is never the window
/// under the pointer.
const PICTURE_OFFSET: i16 = 16;

/// How a drag ended, and where (logical global coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragEnd {
    pub dropped: bool,
    pub at: Point,
}

/// `pixels` (RGBA, straight) as X11's 32-bit ARGB in memory order on a
/// little-endian machine (B, G, R, A), premultiplied as the compositor wants.
fn premultiplied_bgra(pixels: &[u8]) -> Vec<u8> {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let p = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
            [p(b), p(g), p(r), a]
        })
        .collect()
}

/// The `text/uri-list` of `paths`.
fn uri_list(paths: &[PathBuf]) -> Vec<u8> {
    let mut list = String::new();
    for path in paths {
        list.push_str(&gio::File::for_path(path).uri());
        list.push_str("\r\n");
    }
    list.into_bytes()
}

/// XdndPosition's packed root coordinates.
fn packed(x: i16, y: i16) -> u32 {
    (u32::from(x as u16) << 16) | u32::from(y as u16)
}

/// Atoms the drag uses.
struct Atoms {
    aware: u32,
    proxy: u32,
    selection: u32,
    enter: u32,
    position: u32,
    status: u32,
    leave: u32,
    drop: u32,
    finished: u32,
    copy: u32,
    uri_list: u32,
    targets: u32,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self, String> {
        let atom = |name: &str| -> Result<u32, String> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?
                .atom)
        };
        Ok(Self {
            aware: atom("XdndAware")?,
            proxy: atom("XdndProxy")?,
            selection: atom("XdndSelection")?,
            enter: atom("XdndEnter")?,
            position: atom("XdndPosition")?,
            status: atom("XdndStatus")?,
            leave: atom("XdndLeave")?,
            drop: atom("XdndDrop")?,
            finished: atom("XdndFinished")?,
            copy: atom("XdndActionCopy")?,
            uri_list: atom("text/uri-list")?,
            targets: atom("TARGETS")?,
        })
    }
}

/// The picture that follows the pointer: XDND draws none, and GNOME draws
/// none for a drag from X11.
struct Picture {
    window: u32,
    gc: u32,
    width: u16,
    height: u16,
    /// Premultiplied BGRA.
    pixels: Vec<u8>,
}

impl Picture {
    /// A transparent override-redirect window showing `image`, or `None`
    /// without a 32-bit visual to draw it with.
    fn new(conn: &RustConnection, screen: usize, image: &RgbaImage) -> Option<Self> {
        let width = u16::try_from(image.width).ok()?;
        let height = u16::try_from(image.height).ok()?;
        if image.pixels.len() != usize::from(width) * usize::from(height) * 4 {
            return None;
        }
        let root = conn.setup().roots[screen].root;
        let visual = conn.setup().roots[screen]
            .allowed_depths
            .iter()
            .find(|d| d.depth == 32)?
            .visuals
            .iter()
            .find(|v| v.class == VisualClass::TRUE_COLOR)?
            .visual_id;
        let colormap = conn.generate_id().ok()?;
        conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual)
            .ok()?;
        let window = conn.generate_id().ok()?;
        conn.create_window(
            32,
            window,
            root,
            -1000,
            -1000,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(0)
                .border_pixel(0)
                .colormap(colormap)
                .override_redirect(1)
                .event_mask(EventMask::EXPOSURE),
        )
        .ok()?;
        // Never in the way of the pointer, nor of the drop target search.
        conn.shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            ClipOrdering::UNSORTED,
            window,
            0,
            0,
            &[],
        )
        .ok()?;
        let gc = conn.generate_id().ok()?;
        conn.create_gc(gc, window, &CreateGCAux::new()).ok()?;
        conn.map_window(window).ok()?;
        Some(Self {
            window,
            gc,
            width,
            height,
            pixels: premultiplied_bgra(&image.pixels),
        })
    }

    fn draw(&self, conn: &RustConnection) {
        let _ = conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.window,
            self.gc,
            self.width,
            self.height,
            0,
            0,
            0,
            32,
            &self.pixels,
        );
        let _ = conn.flush();
    }

    /// Puts it beside the pointer at (`x`, `y`).
    fn follow(&self, conn: &RustConnection, x: i16, y: i16) {
        let aux = ConfigureWindowAux::new()
            .x(i32::from(x) + i32::from(PICTURE_OFFSET))
            .y(i32::from(y) + i32::from(PICTURE_OFFSET))
            .stack_mode(StackMode::ABOVE);
        let _ = conn.configure_window(self.window, &aux);
        let _ = conn.flush();
    }
}

/// A drag in progress, on its thread.
struct Drag {
    conn: RustConnection,
    atoms: Atoms,
    root: u32,
    source: u32,
    data: Vec<u8>,
    /// The XDND window under the pointer, and where its messages go.
    target: Option<(u32, u32)>,
    accepted: bool,
    picture: Option<Picture>,
    /// The compositor's own XDND window, which stands for the Wayland
    /// window under the pointer; see [`compositor_drop_window`].
    compositor: Option<u32>,
}

impl Drag {
    fn send(&self, to: u32, kind: u32, data: [u32; 5]) {
        let event = ClientMessageEvent::new(32, to, kind, data);
        let _ = self.conn.send_event(false, to, EventMask::NO_EVENT, event);
        let _ = self.conn.flush();
    }

    /// The XDND-aware window under (`x`, `y`) and its proxy, if any.
    fn target_at(&self, x: i16, y: i16) -> Option<(u32, u32)> {
        let mut window = self.root;
        // Whether an X11 window that draws is under the pointer; only
        // input-only ones (GNOME has one over the whole screen) don't count.
        let mut drawn = false;
        // Top-level windows sit a few levels down (frames, decorations).
        for _ in 0..16 {
            let child = self
                .conn
                .translate_coordinates(self.root, window, x, y)
                .ok()?
                .reply()
                .ok()?
                .child;
            if child == NONE {
                // No X11 window here: a Wayland one, or the desktop.
                return (!drawn)
                    .then_some(self.compositor)
                    .flatten()
                    .map(|w| (w, w));
            }
            if self.picture.as_ref().is_some_and(|p| p.window == child) {
                // The pointer caught up with the picture: nothing changed.
                return self.target;
            }
            window = child;
            drawn |= self
                .conn
                .get_window_attributes(window)
                .ok()?
                .reply()
                .ok()?
                .class
                != WindowClass::INPUT_ONLY;
            let aware = self
                .conn
                .get_property(false, window, self.atoms.aware, AtomEnum::ATOM, 0, 1)
                .ok()?
                .reply()
                .ok()?;
            if aware.value_len > 0 {
                let proxy = self
                    .conn
                    .get_property(false, window, self.atoms.proxy, AtomEnum::WINDOW, 0, 1)
                    .ok()?
                    .reply()
                    .ok()?;
                let to = proxy.value32().and_then(|mut v| v.next()).unwrap_or(window);
                return Some((window, to));
            }
        }
        None
    }

    /// Follows the pointer to (`x`, `y`) at `time`.
    fn moved(&mut self, x: i16, y: i16, time: u32) {
        if let Some(picture) = &self.picture {
            picture.follow(&self.conn, x, y);
        }
        let now = self.target_at(x, y);
        if now.map(|t| t.0) != self.target.map(|t| t.0) {
            if let Some((_, to)) = self.target {
                self.send(to, self.atoms.leave, [self.source, 0, 0, 0, 0]);
            }
            if let Some((_, to)) = now {
                let a = &self.atoms;
                self.send(
                    to,
                    a.enter,
                    [self.source, XDND_VERSION << 24, a.uri_list, 0, 0],
                );
            }
            self.target = now;
            self.accepted = false;
        }
        if let Some((_, to)) = self.target {
            let a = &self.atoms;
            self.send(to, a.position, [self.source, 0, packed(x, y), time, a.copy]);
        }
    }

    /// The button was let go at (`x`, `y`): drops on a target that accepts,
    /// otherwise ends the drag. `None` while a drop waits for its answer.
    fn released(&mut self, x: i16, y: i16, time: u32) -> Option<DragEnd> {
        log::debug!(
            "drag out: released at {x},{y} over {:?}, accepted {}",
            self.target.map(|t| t.0),
            self.accepted
        );
        self.ungrab(time);
        let at = Point {
            x: f64::from(x),
            y: f64::from(y),
        };
        match self.target {
            Some((_, to)) if self.accepted => {
                self.send(to, self.atoms.drop, [self.source, 0, time, 0, 0]);
                None
            }
            Some((_, to)) => {
                self.send(to, self.atoms.leave, [self.source, 0, 0, 0, 0]);
                Some(DragEnd { dropped: false, at })
            }
            None => Some(DragEnd { dropped: false, at }),
        }
    }

    /// Answers a request for the drag's data: its types, or its files.
    fn answer(&self, request: &SelectionRequestEvent) {
        let a = &self.atoms;
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let given = if request.target == a.targets {
            self.conn
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &[a.targets, a.uri_list],
                )
                .is_ok()
        } else if request.target == a.uri_list {
            self.conn
                .change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    a.uri_list,
                    &self.data,
                )
                .is_ok()
        } else {
            false
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if given { property } else { NONE },
        };
        let _ = self
            .conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify);
        let _ = self.conn.flush();
    }

    /// Runs the drag until it is dropped or cancelled.
    fn run(mut self) -> DragEnd {
        let at = |x: i16, y: i16| Point {
            x: f64::from(x),
            y: f64::from(y),
        };
        let (mut x, mut y) = match super::shell::pointer_now() {
            Some((_, p)) => (p.x as i16, p.y as i16),
            None => (0, 0),
        };
        self.moved(x, y, CURRENT_TIME);
        let mut dropped_since: Option<Instant> = None;
        let mut sampled = Instant::now();
        let mut silent_since: Option<Instant> = None;
        loop {
            if dropped_since.is_some_and(|t| t.elapsed() > FINISH_TIMEOUT) {
                log::warn!("drag out: the target never said it was done");
                return DragEnd {
                    dropped: true,
                    at: at(x, y),
                };
            }
            loop {
                let event = match self.conn.poll_for_event() {
                    Ok(Some(e)) => e,
                    Ok(None) => break,
                    Err(e) => {
                        log::warn!("drag out stopped: {e}");
                        return DragEnd {
                            dropped: false,
                            at: at(x, y),
                        };
                    }
                };
                if let Some(end) = self.handle(event, dropped_since.is_some(), at(x, y)) {
                    return end;
                }
            }
            if dropped_since.is_none() && sampled.elapsed() >= POINTER_SAMPLE {
                sampled = Instant::now();
                match super::shell::pointer_now() {
                    Some((true, p)) => {
                        silent_since = None;
                        if (p.x as i16, p.y as i16) != (x, y) {
                            (x, y) = (p.x as i16, p.y as i16);
                            self.moved(x, y, CURRENT_TIME);
                        }
                    }
                    Some((false, p)) => {
                        (x, y) = (p.x as i16, p.y as i16);
                        match self.released(x, y, CURRENT_TIME) {
                            Some(end) => return end,
                            None => dropped_since = Some(Instant::now()),
                        }
                    }
                    None => {
                        let since = *silent_since.get_or_insert_with(Instant::now);
                        if since.elapsed() > POINTER_SILENCE {
                            log::warn!("drag out: the GNOME Shell extension is not answering");
                            self.cancel();
                            return DragEnd {
                                dropped: false,
                                at: at(x, y),
                            };
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    /// Acts on one X11 event; `Some` when the drag is over. `dropping`: a
    /// drop was sent and waits for its answer.
    fn handle(&mut self, event: Event, dropping: bool, at: Point) -> Option<DragEnd> {
        match event {
            Event::KeyPress(e) if !dropping && self.is_escape(e.detail) => {
                self.cancel();
                return Some(DragEnd { dropped: false, at });
            }
            Event::ClientMessage(e) if e.type_ == self.atoms.status => {
                self.accepted = e.data.as_data32()[1] & 1 == 1;
            }
            Event::ClientMessage(e) if e.type_ == self.atoms.finished => {
                let accepted = e.data.as_data32()[1] & 1 == 1;
                return Some(DragEnd {
                    dropped: accepted,
                    at,
                });
            }
            Event::SelectionRequest(r) if r.selection == self.atoms.selection => {
                self.answer(&r);
            }
            Event::Expose(_) => {
                if let Some(picture) = &self.picture {
                    picture.draw(&self.conn);
                }
            }
            _ => {}
        }
        None
    }

    /// Leaves the target, if any, and lets go of the keyboard.
    fn cancel(&self) {
        self.ungrab(CURRENT_TIME);
        if let Some((_, to)) = self.target {
            self.send(to, self.atoms.leave, [self.source, 0, 0, 0, 0]);
        }
    }

    fn is_escape(&self, keycode: u8) -> bool {
        self.conn
            .get_keyboard_mapping(keycode, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|m| m.keysyms.first() == Some(&ESCAPE_KEYSYM))
    }

    fn ungrab(&self, time: u32) {
        let _ = self.conn.ungrab_keyboard(time);
        let _ = self.conn.flush();
    }
}

/// Sets up a drag of `data` on a connection of its own: a source window,
/// the drag's data, and the keyboard grabbed for Escape.
fn begin(data: Vec<u8>, image: Option<&RgbaImage>) -> Result<Drag, String> {
    let failed = |e: &dyn std::fmt::Display| e.to_string();
    let (conn, screen) = RustConnection::connect(None).map_err(|e| failed(&e))?;
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::intern(&conn)?;
    let source = conn.generate_id().map_err(|e| failed(&e))?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        source,
        root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        COPY_FROM_PARENT,
        &CreateWindowAux::new(),
    )
    .map_err(|e| failed(&e))?;
    conn.set_selection_owner(source, atoms.selection, CURRENT_TIME)
        .map_err(|e| failed(&e))?;
    // For Escape; a drag goes on without it.
    let _ = conn.grab_keyboard(false, root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC);
    conn.flush().map_err(|e| failed(&e))?;
    let picture = image.and_then(|image| Picture::new(&conn, screen, image));
    if let Some(picture) = &picture {
        picture.draw(&conn);
    }
    let compositor = compositor_drop_window(&conn, root, &atoms);
    log::debug!("drag out: compositor drop window {compositor:x?}");
    Ok(Drag {
        conn,
        atoms,
        root,
        source,
        data,
        target: None,
        accepted: false,
        picture,
        compositor,
    })
}

/// GNOME's XDND window for drops on Wayland windows: input-only,
/// override-redirect and XDND-aware. GNOME raises it over the Wayland window
/// under the pointer only when that window changes, never when a drag
/// starts over the window the last one ended on, so it is often not where a
/// search by stacking would find it. It takes the messages wherever it is
/// and passes them to the window under the pointer.
fn compositor_drop_window(conn: &RustConnection, root: u32, atoms: &Atoms) -> Option<u32> {
    let children = conn.query_tree(root).ok()?.reply().ok()?.children;
    children.into_iter().find(|&window| {
        let input_only = conn
            .get_window_attributes(window)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|a| a.class == WindowClass::INPUT_ONLY && a.override_redirect);
        input_only
            && conn
                .get_property(false, window, atoms.aware, AtomEnum::ATOM, 0, 1)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|p| p.value_len > 0)
    })
}

/// Drags `paths` out while the press that started it is held, with `image`
/// beside the pointer, and calls `on_end` (on another thread) with how and
/// where it ended.
///
/// # Errors
/// If the window is not an X11 one or the drag cannot start.
pub fn start_file_drag(
    paths: Vec<PathBuf>,
    image: Option<RgbaImage>,
    on_end: Box<dyn FnOnce(DragEnd) + Send>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("nothing to drag".into());
    }
    let data = uri_list(&paths);
    let drag = begin(data, image.as_ref())?;
    std::thread::Builder::new()
        .name("vento-drag-out".into())
        .spawn(move || on_end(drag.run()))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_go_as_a_uri_list() {
        let list = uri_list(&[
            PathBuf::from("/home/me/a b.txt"),
            PathBuf::from("/tmp/ح.png"),
        ]);
        assert_eq!(
            String::from_utf8(list).unwrap(),
            "file:///home/me/a%20b.txt\r\nfile:///tmp/%D8%AD.png\r\n"
        );
    }

    #[test]
    fn pictures_become_premultiplied_bgra() {
        let rgba = [255, 128, 0, 255, 255, 255, 255, 128, 10, 20, 30, 0];
        assert_eq!(
            premultiplied_bgra(&rgba),
            [0, 128, 255, 255, 128, 128, 128, 128, 0, 0, 0, 0]
        );
    }

    #[test]
    fn positions_pack_x_high_and_y_low() {
        assert_eq!(packed(885, 816), (885 << 16) | 816);
        assert_eq!(packed(0, 0), 0);
    }

    #[test]
    fn nothing_to_drag_is_an_error() {
        assert!(start_file_drag(Vec::new(), None, Box::new(|_| {})).is_err());
    }
}
