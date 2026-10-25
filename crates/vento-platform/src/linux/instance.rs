//! One Vento per session. The first claims the app's name on the session
//! bus; a later launch (from the app grid, or a second start at login) asks
//! it to open its settings and goes. The request arrives on a thread of its
//! own and waits for the main thread, which hands it on from
//! [`asked_to_open`] on every input sample.

use std::sync::atomic::{AtomicBool, Ordering};

use gio::glib;
use gio::prelude::*;

use super::APP_ID;

const OBJECT_PATH: &str = "/io/github/fathialamre/Vento";
const INTERFACE: &str = "io.github.fathialamre.Vento1";
const INTROSPECTION: &str = r#"<node>
  <interface name="io.github.fathialamre.Vento1">
    <method name="Open"/>
  </interface>
</node>"#;

/// `RequestName`'s `DBUS_NAME_FLAG_DO_NOT_QUEUE`.
const DO_NOT_QUEUE: u32 = 4;
/// `RequestName` replies: the name is ours.
const PRIMARY_OWNER: u32 = 1;
const ALREADY_OWNER: u32 = 4;

/// Set when another launch asked this Vento to open.
static OPEN_ASKED: AtomicBool = AtomicBool::new(false);

/// Whether this is the only Vento: true after claiming the app's name (or
/// without a session bus, where there is nothing to ask), false after
/// asking the Vento that has it to open its settings.
pub fn claim_instance() -> bool {
    let bus = match super::session_bus() {
        Ok(bus) => bus,
        Err(e) => {
            log::warn!("no session bus to keep vento to one copy: {e}");
            return true;
        }
    };
    let reply = bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "RequestName",
        Some(&(APP_ID, DO_NOT_QUEUE).to_variant()),
        Some(glib::VariantTy::new("(u)").expect("a valid type")),
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    );
    match reply.ok().and_then(|r| r.get::<(u32,)>()) {
        Some((PRIMARY_OWNER | ALREADY_OWNER,)) => {
            serve(bus);
            true
        }
        Some(_) => {
            ask_to_open(&bus);
            false
        }
        None => {
            log::warn!("cannot claim {APP_ID} on the session bus; going on");
            true
        }
    }
}

/// Asks the running Vento to open its settings.
fn ask_to_open(bus: &gio::DBusConnection) {
    let result = bus.call_sync(
        Some(APP_ID),
        OBJECT_PATH,
        INTERFACE,
        "Open",
        None,
        None,
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    );
    match result {
        Ok(_) => log::info!("vento is already running: asked it to open its settings"),
        Err(e) => log::warn!("vento is already running, and did not answer: {e}"),
    }
}

/// Answers `Open` on a thread with a main loop of its own.
fn serve(bus: gio::DBusConnection) {
    let spawned = std::thread::Builder::new()
        .name("vento-instance".into())
        .spawn(move || {
            let context = glib::MainContext::new();
            let registered = context.with_thread_default(|| {
                let info = gio::DBusNodeInfo::for_xml(INTROSPECTION)
                    .ok()
                    .and_then(|node| node.lookup_interface(INTERFACE))?;
                bus.register_object(OBJECT_PATH, &info)
                    .method_call(|_, _, _, _, method, _, invocation| {
                        if method == "Open" {
                            OPEN_ASKED.store(true, Ordering::SeqCst);
                        }
                        invocation.return_value(None);
                    })
                    .build()
                    .inspect_err(|e| log::warn!("cannot answer other launches: {e}"))
                    .ok()
            });
            if let Ok(Some(_registration)) = registered {
                glib::MainLoop::new(Some(&context), false).run();
            }
        });
    if let Err(e) = spawned {
        log::warn!("cannot answer other launches: {e}");
    }
}

/// Whether another launch asked this Vento to open since the last call.
pub(super) fn asked_to_open() -> bool {
    OPEN_ASKED.swap(false, Ordering::SeqCst)
}
