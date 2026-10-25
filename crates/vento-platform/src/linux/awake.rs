//! Keeps the computer awake through GNOME's session manager. GNOME drops an
//! inhibitor when the connection that asked for it closes, so one never
//! outlives vento.

use std::cell::Cell;

use gio::glib;
use gio::prelude::*;

use super::APP_ID;
use crate::{AwakeHold, PlatformError};

const BUS_NAME: &str = "org.gnome.SessionManager";
const OBJECT_PATH: &str = "/org/gnome/SessionManager";
const INTERFACE: &str = "org.gnome.SessionManager";
/// `Inhibit`'s flags: no suspend while idle, and no idle at all (no
/// dimming, blanking or lock).
const INHIBIT_SUSPEND: u32 = 4;
const INHIBIT_IDLE: u32 = 8;
/// Why, as GNOME shows it (in the power menu's "Some applications are
/// busy" and `gnome-session-inhibit --list`).
const REASON: &str = "Keep Awake";
const TIMEOUT_MS: i32 = 5_000;

/// The inhibitor vento holds, if any.
#[derive(Debug, Default)]
pub(super) struct Inhibitor {
    cookie: Cell<Option<u32>>,
}

impl Inhibitor {
    /// Holds `hold`, or releases with `None`. A new hold is taken before the
    /// old one goes, so the computer is never left without one in between.
    pub(super) fn set(&self, hold: Option<AwakeHold>) -> Result<(), PlatformError> {
        let bus = super::session_bus().map_err(failed)?;
        let new = match hold {
            Some(hold) => Some(inhibit(&bus, hold)?),
            None => None,
        };
        if let Some(old) = self.cookie.replace(new) {
            uninhibit(&bus, old);
        }
        Ok(())
    }
}

fn inhibit(bus: &gio::DBusConnection, hold: AwakeHold) -> Result<u32, PlatformError> {
    let flags = INHIBIT_SUSPEND | if hold.screen { INHIBIT_IDLE } else { 0 };
    let reply = bus
        .call_sync(
            Some(BUS_NAME),
            OBJECT_PATH,
            INTERFACE,
            "Inhibit",
            Some(&(APP_ID, 0u32, REASON, flags).to_variant()),
            Some(glib::VariantTy::new("(u)").expect("valid type")),
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
            gio::Cancellable::NONE,
        )
        .map_err(failed)?;
    let (cookie,): (u32,) = reply
        .get()
        .ok_or_else(|| PlatformError::Failed("Inhibit gave no cookie".into()))?;
    Ok(cookie)
}

fn uninhibit(bus: &gio::DBusConnection, cookie: u32) {
    if let Err(e) = bus.call_sync(
        Some(BUS_NAME),
        OBJECT_PATH,
        INTERFACE,
        "Uninhibit",
        Some(&(cookie,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        TIMEOUT_MS,
        gio::Cancellable::NONE,
    ) {
        log::warn!("cannot release the Keep Awake inhibitor {cookie}: {e}");
    }
}

fn failed(e: glib::Error) -> PlatformError {
    PlatformError::Failed(format!("Keep Awake: {e}"))
}
