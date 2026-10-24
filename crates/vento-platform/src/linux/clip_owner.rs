//! Putting things on the clipboard: vento owns the X11 `CLIPBOARD` from an
//! invisible window and answers every request for one of the types it
//! offers, from a thread of its own. GNOME passes the copy on to Wayland
//! apps, in all those types at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, SELECTION_NOTIFY_EVENT,
    SelectionNotifyEvent, SelectionRequestEvent, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use crate::PlatformError;

fn failed(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Failed(e.to_string())
}

/// What vento offers while it owns the clipboard: (target atom, data).
type Offers = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Debug)]
pub(super) struct ClipOwner {
    conn: Arc<RustConnection>,
    window: u32,
    clipboard: u32,
    offers: Offers,
    atoms: Mutex<HashMap<String, u32>>,
}

impl ClipOwner {
    /// Opens a connection, makes the window and starts answering requests.
    pub fn start() -> Result<Self, PlatformError> {
        let (conn, screen) = RustConnection::connect(None).map_err(failed)?;
        let conn = Arc::new(conn);
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().map_err(failed)?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
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
        .map_err(failed)?;
        let intern = |name: &str| -> Result<u32, PlatformError> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(failed)?
                .reply()
                .map_err(failed)?
                .atom)
        };
        let clipboard = intern("CLIPBOARD")?;
        let targets = intern("TARGETS")?;
        conn.flush().map_err(failed)?;
        let offers: Offers = Arc::default();
        {
            let (conn, offers) = (conn.clone(), offers.clone());
            std::thread::Builder::new()
                .name("vento-clipboard".into())
                .spawn(move || serve(&conn, clipboard, targets, &offers))
                .map_err(failed)?;
        }
        Ok(Self {
            conn,
            window,
            clipboard,
            offers,
            atoms: Mutex::default(),
        })
    }

    fn atom(&self, name: &str) -> Result<u32, PlatformError> {
        if let Some(&atom) = self.atoms.lock().unwrap().get(name) {
            return Ok(atom);
        }
        let atom = self
            .conn
            .intern_atom(false, name.as_bytes())
            .map_err(failed)?
            .reply()
            .map_err(failed)?
            .atom;
        self.atoms.lock().unwrap().insert(name.to_owned(), atom);
        Ok(atom)
    }

    /// Takes the clipboard, offering `offers` (type name, data). Types too
    /// big for one X11 request are left out.
    pub fn own(&self, offers: Vec<(String, Vec<u8>)>) -> Result<(), PlatformError> {
        let limit = self.conn.maximum_request_bytes().saturating_sub(64);
        let mut atoms = Vec::with_capacity(offers.len());
        for (name, data) in offers {
            if data.len() > limit {
                log::warn!(
                    "clipboard: {name} left out, {} bytes is too big",
                    data.len()
                );
                continue;
            }
            atoms.push((self.atom(&name)?, data));
        }
        *self.offers.lock().unwrap() = atoms;
        self.conn
            .set_selection_owner(self.window, self.clipboard, CURRENT_TIME)
            .map_err(failed)?;
        let owner = self
            .conn
            .get_selection_owner(self.clipboard)
            .map_err(failed)?
            .reply()
            .map_err(failed)?
            .owner;
        if owner == self.window {
            Ok(())
        } else {
            Err(PlatformError::Failed(
                "another app kept the clipboard".into(),
            ))
        }
    }
}

/// Answers requests for the clipboard until the connection breaks.
fn serve(conn: &RustConnection, clipboard: u32, targets: u32, offers: &Offers) {
    loop {
        let event = match conn.wait_for_event() {
            Ok(e) => e,
            Err(e) => {
                log::warn!("clipboard owner stopped: {e}");
                return;
            }
        };
        match event {
            Event::SelectionRequest(request) if request.selection == clipboard => {
                if let Err(e) = answer(conn, &request, targets, offers) {
                    log::warn!("clipboard request not answered: {e}");
                }
            }
            Event::SelectionClear(clear) if clear.selection == clipboard => {
                offers.lock().unwrap().clear();
            }
            _ => {}
        }
    }
}

/// Puts the asked-for type on the requestor's property, or refuses.
fn answer(
    conn: &RustConnection,
    request: &SelectionRequestEvent,
    targets: u32,
    offers: &Offers,
) -> Result<(), PlatformError> {
    // Old clients name no property: use the target.
    let property = if request.property == NONE {
        request.target
    } else {
        request.property
    };
    let given = {
        let offers = offers.lock().unwrap();
        if offers.is_empty() {
            false
        } else if request.target == targets {
            let mut list: Vec<u32> = offers.iter().map(|(atom, _)| *atom).collect();
            list.push(targets);
            conn.change_property32(
                PropMode::REPLACE,
                request.requestor,
                property,
                AtomEnum::ATOM,
                &list,
            )
            .map_err(failed)?;
            true
        } else if let Some((_, data)) = offers.iter().find(|(atom, _)| *atom == request.target) {
            conn.change_property8(
                PropMode::REPLACE,
                request.requestor,
                property,
                request.target,
                data,
            )
            .map_err(failed)?;
            true
        } else {
            false
        }
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
    conn.send_event(false, request.requestor, EventMask::NO_EVENT, notify)
        .map_err(failed)?;
    conn.flush().map_err(failed)?;
    Ok(())
}
