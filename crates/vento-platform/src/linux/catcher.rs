//! The Pocket island's drop target: a transparent X11 window of vento's
//! own, placed over the island during drags, that speaks XDND. GNOME hands
//! drags from Wayland apps to X11 windows as XDND too, so it hears them all.
//! A thread reads its events; what it finds waits for the main thread,
//! which passes it on from [`LinuxCatcher::pump`] on every input sample.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gio::prelude::*;
use vento_core::geometry::Rect;
use x11rb::NONE;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ColormapAlloc, ConfigureWindowAux, ConnectionExt,
    CreateWindowAux, EventMask, PropMode, Property, StackMode, VisualClass, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::{CatcherEvent, DropCatcher, Dropped};

/// The XDND version spoken.
const XDND_VERSION: u32 = 5;
/// How long the dragging app has to hand over one type of the drop.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3);
/// How long the catcher stays after it is told to hide. The Pocket hides it
/// when the extension sees the drag end, which can be before GNOME sends
/// the drop; hidden at once, the drop would turn into a leave.
const HIDE_GRACE: Duration = Duration::from_millis(300);
/// Text types, best first.
const TEXT_TYPES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];

/// The files and the first web link of a `text/uri-list`. Comments and
/// blank lines are skipped; `file:` URIs that are not local are dropped.
fn parse_uri_list(bytes: &[u8]) -> (Vec<PathBuf>, Option<String>) {
    let mut files = Vec::new();
    let mut link = None;
    for line in String::from_utf8_lossy(bytes).lines() {
        let uri = line.trim().trim_end_matches('\0');
        if uri.is_empty() || uri.starts_with('#') {
            continue;
        }
        if uri.starts_with("file:") {
            files.extend(gio::File::for_uri(uri).path());
        } else if link.is_none() {
            link = Some(uri.to_owned());
        }
    }
    (files, link)
}

/// Dropped text as a string; `None` when empty.
fn text_of(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches('\0');
    (!text.is_empty()).then(|| text.to_owned())
}

/// The link of a `text/x-moz-url`: UTF-16, the URL then its title.
fn moz_url(bytes: &[u8]) -> Option<String> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[lo, hi]| u16::from_le_bytes([lo, hi]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    let url = text.lines().next()?.trim().trim_end_matches('\0');
    (!url.is_empty() && !url.starts_with("file:")).then(|| url.to_owned())
}

/// Atoms the catcher uses.
struct Atoms {
    aware: u32,
    selection: u32,
    type_list: u32,
    enter: u32,
    position: u32,
    status: u32,
    leave: u32,
    drop: u32,
    finished: u32,
    copy: u32,
    incr: u32,
    /// Where dropped data is put on the catcher's window.
    property: u32,
    uri_list: u32,
    moz_url: u32,
    png: u32,
    tiff: u32,
    text: [u32; 4],
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
            selection: atom("XdndSelection")?,
            type_list: atom("XdndTypeList")?,
            enter: atom("XdndEnter")?,
            position: atom("XdndPosition")?,
            status: atom("XdndStatus")?,
            leave: atom("XdndLeave")?,
            drop: atom("XdndDrop")?,
            finished: atom("XdndFinished")?,
            copy: atom("XdndActionCopy")?,
            incr: atom("INCR")?,
            property: atom("VENTO_DROP")?,
            uri_list: atom("text/uri-list")?,
            moz_url: atom("text/x-moz-url")?,
            png: atom("image/png")?,
            tiff: atom("image/tiff")?,
            text: [
                atom(TEXT_TYPES[0])?,
                atom(TEXT_TYPES[1])?,
                atom(TEXT_TYPES[2])?,
                atom(TEXT_TYPES[3])?,
            ],
        })
    }

    /// Whether a drag offering `types` has something the Pocket takes.
    fn takes(&self, types: &[u32]) -> bool {
        let known = [self.uri_list, self.moz_url, self.png, self.tiff];
        types
            .iter()
            .any(|t| known.contains(t) || self.text.contains(t))
    }
}

/// The drag over the catcher.
struct Offer {
    source: u32,
    types: Vec<u32>,
    /// Whether `Entered` was reported.
    entered: bool,
}

/// Reads the catcher window's XDND messages and drops.
struct Target {
    conn: Arc<RustConnection>,
    window: u32,
    atoms: Atoms,
    events: Arc<Mutex<VecDeque<CatcherEvent>>>,
    offer: Option<Offer>,
}

impl Target {
    fn report(&self, event: CatcherEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push_back(event);
        }
    }

    fn send(&self, to: u32, kind: u32, data: [u32; 5]) {
        let event = ClientMessageEvent::new(32, to, kind, data);
        let _ = self.conn.send_event(false, to, EventMask::NO_EVENT, event);
        let _ = self.conn.flush();
    }

    /// The types a drag from `source` offers, given its XdndEnter `data`.
    fn offered(&self, source: u32, data: [u32; 5]) -> Vec<u32> {
        if data[1] & 1 == 0 {
            return data[2..].iter().copied().filter(|&t| t != NONE).collect();
        }
        self.conn
            .get_property(false, source, self.atoms.type_list, AtomEnum::ATOM, 0, 1024)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|p| p.value32().map(Iterator::collect))
            .unwrap_or_default()
    }

    /// The name of `atom`, for the log.
    fn name(&self, atom: u32) -> String {
        self.conn
            .get_atom_name(atom)
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or_else(
                || atom.to_string(),
                |r| String::from_utf8_lossy(&r.name).into_owned(),
            )
    }

    fn serve(mut self) {
        loop {
            let event = match self.conn.wait_for_event() {
                Ok(event) => event,
                Err(e) => return log::warn!("drop catcher: X11 connection lost: {e}"),
            };
            if let Event::ClientMessage(m) = event {
                self.message(m.type_, m.data.as_data32());
            }
        }
    }

    fn message(&mut self, kind: u32, data: [u32; 5]) {
        let a = &self.atoms;
        if kind == a.enter {
            let types = self.offered(data[0], data);
            if log::log_enabled!(log::Level::Debug) {
                let names: Vec<String> = types.iter().map(|&t| self.name(t)).collect();
                log::debug!(
                    "drop catcher: enter from {:#x}: {}",
                    data[0],
                    names.join(", ")
                );
            }
            self.offer = Some(Offer {
                source: data[0],
                types,
                entered: false,
            });
        } else if kind == a.position {
            let Some(offer) = self.offer.as_mut() else {
                return;
            };
            let first = !offer.entered;
            offer.entered = true;
            let (source, takes) = (offer.source, a.takes(&offer.types));
            if first {
                log::debug!(
                    "drop catcher: source asks for {}, taken: {takes}",
                    self.name(data[4])
                );
                self.report(CatcherEvent::Entered);
            }
            // Bit 1: keep sending positions; the catcher is one rectangle.
            let flags = u32::from(takes) | 2;
            let action = if takes { a.copy } else { NONE };
            self.send(source, a.status, [self.window, flags, 0, 0, action]);
        } else if kind == a.leave {
            log::debug!("drop catcher: leave");
            if self.offer.take().is_some_and(|o| o.entered) {
                self.report(CatcherEvent::Exited);
            }
        } else if kind == a.drop {
            let Some(offer) = self.offer.take() else {
                return;
            };
            let dropped = self.fetch(&offer.types, data[2]);
            log::debug!("drop catcher: dropped {:?}", dropped.as_ref().map(kind_of));
            let done = dropped.is_some();
            let action = if done { self.atoms.copy } else { NONE };
            self.send(
                offer.source,
                self.atoms.finished,
                [self.window, u32::from(done), action, 0, 0],
            );
            self.report(match dropped {
                Some(d) => CatcherEvent::Dropped(d),
                None => CatcherEvent::Exited,
            });
        }
    }

    /// What was dropped, read in the Pocket's order (ruling P5): files, an
    /// image (PNG before TIFF), a web link, text.
    fn fetch(&self, types: &[u32], time: u32) -> Option<Dropped> {
        let a = &self.atoms;
        let offers = |t: u32| types.contains(&t);
        let mut link = None;
        if offers(a.uri_list)
            && let Some(bytes) = self.convert(a.uri_list, time)
        {
            let (files, url) = parse_uri_list(&bytes);
            if !files.is_empty() {
                return Some(Dropped::Files(files));
            }
            link = url;
        }
        for (atom, ext) in [(a.png, "png"), (a.tiff, "tiff")] {
            if offers(atom)
                && let Some(bytes) = self.convert(atom, time).filter(|b| !b.is_empty())
            {
                return Some(Dropped::Image { bytes, ext });
            }
        }
        if link.is_none() && offers(a.moz_url) {
            link = self.convert(a.moz_url, time).and_then(|b| moz_url(&b));
        }
        if let Some(url) = link {
            return Some(Dropped::Url(url));
        }
        a.text
            .iter()
            .filter(|&&t| offers(t))
            .find_map(|&t| self.convert(t, time).and_then(|b| text_of(&b)))
            .map(Dropped::Text)
    }

    /// The drop as `target`, `None` if the dragging app refused it or took
    /// too long.
    fn convert(&self, target: u32, time: u32) -> Option<Vec<u8>> {
        let a = &self.atoms;
        let conn = &self.conn;
        conn.convert_selection(self.window, a.selection, target, a.property, time)
            .ok()?;
        conn.flush().ok()?;
        let deadline = Instant::now() + TRANSFER_TIMEOUT;
        let notify = self.wait(deadline, |e| match e {
            Event::SelectionNotify(n) if n.requestor == self.window => Some(n.property),
            _ => None,
        })?;
        if notify == NONE {
            return None;
        }
        let reply = conn
            .get_property(
                true,
                self.window,
                a.property,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )
            .ok()?
            .reply()
            .ok()?;
        if reply.type_ != a.incr {
            return Some(reply.value);
        }
        // Large data comes in pieces, each announced by a new value.
        let mut data = Vec::new();
        loop {
            let deadline = Instant::now() + TRANSFER_TIMEOUT;
            self.wait(deadline, |e| match e {
                Event::PropertyNotify(p)
                    if p.window == self.window
                        && p.atom == a.property
                        && p.state == Property::NEW_VALUE =>
                {
                    Some(())
                }
                _ => None,
            })?;
            let piece = conn
                .get_property(
                    true,
                    self.window,
                    a.property,
                    AtomEnum::ANY,
                    0,
                    u32::MAX / 4,
                )
                .ok()?
                .reply()
                .ok()?;
            if piece.value.is_empty() {
                return Some(data);
            }
            data.extend_from_slice(&piece.value);
        }
    }

    /// The first event `f` picks before `deadline`; others are dropped.
    fn wait<T>(&self, deadline: Instant, f: impl Fn(&Event) -> Option<T>) -> Option<T> {
        while Instant::now() < deadline {
            match self.conn.poll_for_event() {
                Ok(Some(event)) => {
                    if let Some(found) = f(&event) {
                        return Some(found);
                    }
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => return None,
            }
        }
        log::debug!("drop catcher: the dragging app took too long");
        None
    }
}

/// A short name for what was dropped, for the log.
fn kind_of(d: &Dropped) -> String {
    match d {
        Dropped::Files(files) => format!("{} file(s)", files.len()),
        Dropped::Text(t) => format!("text, {} bytes", t.len()),
        Dropped::Url(_) => "a link".into(),
        Dropped::Image { bytes, ext } => format!("{ext} image, {} bytes", bytes.len()),
    }
}

/// What the Pocket hears the catcher's events with.
type Handler = Box<dyn Fn(CatcherEvent)>;

/// See the module documentation.
pub(super) struct LinuxCatcher {
    conn: Arc<RustConnection>,
    window: u32,
    events: Arc<Mutex<VecDeque<CatcherEvent>>>,
    handler: RefCell<Option<Handler>>,
    /// Counts shows and hides, so a late hide never undoes a newer show.
    generation: Arc<AtomicU64>,
}

impl std::fmt::Debug for LinuxCatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxCatcher")
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

impl LinuxCatcher {
    /// Makes the window (hidden) and starts reading its events.
    pub fn start() -> Result<Self, String> {
        let failed = |e: &dyn std::fmt::Display| e.to_string();
        let (conn, screen) = RustConnection::connect(None).map_err(|e| failed(&e))?;
        let conn = Arc::new(conn);
        let root = conn.setup().roots[screen].root;
        // A window that draws (an input-only one has no surface for GNOME
        // to hand drags to), fully transparent.
        let visual = conn.setup().roots[screen]
            .allowed_depths
            .iter()
            .find(|d| d.depth == 32)
            .and_then(|d| {
                d.visuals
                    .iter()
                    .find(|v| v.class == VisualClass::TRUE_COLOR)
            })
            .ok_or("no 32-bit visual")?
            .visual_id;
        let colormap = conn.generate_id().map_err(|e| failed(&e))?;
        conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual)
            .map_err(|e| failed(&e))?;
        let window = conn.generate_id().map_err(|e| failed(&e))?;
        conn.create_window(
            32,
            window,
            root,
            -100,
            -100,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(0)
                .border_pixel(0)
                .colormap(colormap)
                .override_redirect(1)
                .event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| failed(&e))?;
        let atoms = Atoms::intern(&conn)?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms.aware,
            AtomEnum::ATOM,
            &[XDND_VERSION],
        )
        .map_err(|e| failed(&e))?;
        conn.flush().map_err(|e| failed(&e))?;
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let target = Target {
            conn: conn.clone(),
            window,
            atoms,
            events: events.clone(),
            offer: None,
        };
        std::thread::Builder::new()
            .name("vento-drop-catcher".into())
            .spawn(move || target.serve())
            .map_err(|e| failed(&e))?;
        Ok(Self {
            conn,
            window,
            events,
            handler: RefCell::new(None),
            generation: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Hands what the catcher heard to its handler. Main thread only.
    pub fn pump(&self) {
        let pending: Vec<CatcherEvent> = match self.events.lock() {
            Ok(mut events) => events.drain(..).collect(),
            Err(_) => return,
        };
        if let Some(handler) = self.handler.borrow().as_ref() {
            for event in pending {
                handler(event);
            }
        }
    }
}

impl DropCatcher for LinuxCatcher {
    /// `rect` is taken as X11 coordinates, which XWayland makes logical
    /// ones, as the rest of vento's X11 code does.
    fn show(&self, rect: Rect) {
        let aux = ConfigureWindowAux::new()
            .x(rect.x.round() as i32)
            .y(rect.y.round() as i32)
            .width(rect.w.round().max(1.0) as u32)
            .height(rect.h.round().max(1.0) as u32)
            .stack_mode(StackMode::ABOVE);
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.conn.configure_window(self.window, &aux);
        let _ = self.conn.map_window(self.window);
        let _ = self.conn.flush();
    }

    /// After [`HIDE_GRACE`], unless shown again meanwhile.
    fn hide(&self) {
        let hide = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (conn, window, generation) = (self.conn.clone(), self.window, self.generation.clone());
        let spawned = std::thread::Builder::new()
            .name("vento-catcher-hide".into())
            .spawn(move || {
                std::thread::sleep(HIDE_GRACE);
                if generation.load(Ordering::SeqCst) == hide {
                    let _ = conn.unmap_window(window);
                    let _ = conn.flush();
                }
            });
        if spawned.is_err() {
            let _ = self.conn.unmap_window(self.window);
            let _ = self.conn.flush();
        }
    }

    fn on_event(&self, f: Box<dyn Fn(CatcherEvent)>) {
        *self.handler.borrow_mut() = Some(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_give_files_and_the_first_link() {
        let list = b"# a comment\r\nfile:///home/me/a%20b.txt\r\n\r\nhttps://example.com/x\r\nhttps://example.com/y\r\n";
        let (files, link) = parse_uri_list(list);
        assert_eq!(files, vec![PathBuf::from("/home/me/a b.txt")]);
        assert_eq!(link.as_deref(), Some("https://example.com/x"));
    }

    #[test]
    fn links_only_give_no_files() {
        let (files, link) = parse_uri_list(b"https://example.com/\n");
        assert!(files.is_empty());
        assert_eq!(link.as_deref(), Some("https://example.com/"));
    }

    #[test]
    fn text_loses_its_trailing_nul() {
        assert_eq!(text_of(b"hello\0").as_deref(), Some("hello"));
        assert_eq!(text_of(b""), None);
        assert_eq!(text_of(b"\0"), None);
    }

    #[test]
    fn moz_urls_give_the_link_not_the_title() {
        let bytes: Vec<u8> = "https://example.com/\nExample"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(moz_url(&bytes).as_deref(), Some("https://example.com/"));
        let file: Vec<u8> = "file:///tmp/a"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(moz_url(&file), None);
    }
}
