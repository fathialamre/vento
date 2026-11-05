//! Turns Vento's GNOME Shell extension on for the user. A package installs
//! it for everyone under /usr/share but cannot turn it on for anyone; vento
//! does that the first time it runs, and says so when GNOME needs a log out
//! before the extension runs.

use std::time::{Duration, Instant};

use gio::glib;
use gio::prelude::*;
use vento_core::i18n::tr;

use super::shortcuts::APP_ID;

/// The extension's uuid, its folder's name.
const UUID: &str = "vento@fathialamre.github.io";

/// How long GNOME may take to start the extension once it is turned on.
const START_WAIT: Duration = Duration::from_secs(3);
const START_POLL: Duration = Duration::from_millis(250);

/// What stands between vento and its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// GNOME has not loaded it (it came after the login) or runs an older
    /// version of it: a log out fixes both.
    LogOut,
    /// The user turned every extension off.
    ExtensionsOff,
}

/// Checks on the extension in the background, turns it on if it is off, and
/// tells the user in a notification what is left for them to do.
pub(super) fn set_up() {
    let spawned = std::thread::Builder::new()
        .name("vento-extension".into())
        .spawn(|| {
            if let Some(missing) = check()
                && let Err(e) = notify(missing)
            {
                log::warn!("cannot show the extension notification: {e}");
            }
        });
    if let Err(e) = spawned {
        log::warn!("cannot check the GNOME Shell extension: {e}");
    }
}

/// What is missing, if anything, after turning the extension on.
fn check() -> Option<Missing> {
    let bus = super::session_bus().ok()?;
    match super::shell::extension_version(&bus) {
        Some(v) if v == super::shell::API_VERSION => return None,
        Some(v) => {
            log::warn!("GNOME runs version {v} of the extension; a log out loads the new one");
            return Some(Missing::LogOut);
        }
        None => {}
    }
    let Some(settings) = shell_settings() else {
        log::warn!("no GNOME Shell settings: Vento's extension cannot be turned on");
        return None;
    };
    if settings.boolean("disable-user-extensions") {
        log::warn!("GNOME extensions are turned off; Vento needs its own");
        return Some(Missing::ExtensionsOff);
    }
    if turn_on(&settings) {
        log::info!("turned on Vento's GNOME Shell extension");
        gio::Settings::sync();
    }
    // GNOME starts it at once if the extension was there at login.
    let start = Instant::now();
    while start.elapsed() < START_WAIT {
        if super::shell::extension_version(&bus).is_some() {
            return None;
        }
        std::thread::sleep(START_POLL);
    }
    log::warn!("Vento's GNOME Shell extension is not running: log out and back in once");
    Some(Missing::LogOut)
}

/// GNOME Shell's settings, or `None` without GNOME Shell.
fn shell_settings() -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.shell", true)?;
    Some(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        None,
    ))
}

/// Adds the extension to the enabled list and takes it off the disabled
/// one, which wins over it. Whether anything changed.
fn turn_on(settings: &gio::Settings) -> bool {
    let (enabled, changed_enabled) = with_uuid(strings(settings.strv("enabled-extensions")), true);
    let (disabled, changed_disabled) =
        with_uuid(strings(settings.strv("disabled-extensions")), false);
    let write = |key: &str, list: Vec<String>| {
        let list: Vec<&str> = list.iter().map(String::as_str).collect();
        if let Err(e) = settings.set_strv(key, list) {
            log::warn!("cannot write {key}: {e}");
        }
    };
    if changed_enabled {
        write("enabled-extensions", enabled);
    }
    if changed_disabled {
        write("disabled-extensions", disabled);
    }
    changed_enabled || changed_disabled
}

fn strings(list: glib::StrV) -> Vec<String> {
    list.iter().map(|s| s.as_str().to_owned()).collect()
}

/// `list` with the uuid in it (`present`) or out of it, and whether that
/// changed it.
fn with_uuid(mut list: Vec<String>, present: bool) -> (Vec<String>, bool) {
    let had = list.iter().any(|u| u == UUID);
    if present && !had {
        list.push(UUID.to_owned());
    } else if !present {
        list.retain(|u| u != UUID);
    }
    (list, had != present)
}

/// The notification's title and text for `missing`, in the interface
/// language.
fn message(missing: Missing) -> (String, String) {
    match missing {
        Missing::LogOut => (
            tr("Log out once to finish setting up Vento"),
            tr(
                "GNOME loads Vento's extension when you log in. Until then Vento cannot see your drags.",
            ),
        ),
        Missing::ExtensionsOff => (
            tr("Turn on GNOME extensions for Vento"),
            tr(
                "Vento works through its GNOME Shell extension, and extensions are turned off. Turn them on in the Extensions app.",
            ),
        ),
    }
}

/// Shows `missing` as a desktop notification, which GNOME keeps in its list
/// until it is read.
fn notify(missing: Missing) -> Result<(), glib::Error> {
    let bus = super::session_bus()?;
    let (summary, body) = message(missing);
    let hints = glib::VariantDict::new(None);
    hints.insert("desktop-entry", APP_ID);
    hints.insert("urgency", 2u8);
    let args = glib::Variant::tuple_from_iter([
        "Vento".to_variant(),
        0u32.to_variant(),
        APP_ID.to_variant(),
        summary.to_variant(),
        body.to_variant(),
        Vec::<String>::new().to_variant(),
        hints.end(),
        (-1i32).to_variant(),
    ]);
    bus.call_sync(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "Notify",
        Some(&args),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_uuid_goes_in_once() {
        let (list, changed) = with_uuid(strv(&["a@b"]), true);
        assert_eq!(list, ["a@b", UUID]);
        assert!(changed);
        let (list, changed) = with_uuid(strv(&[UUID]), true);
        assert_eq!(list, [UUID]);
        assert!(!changed);
    }

    #[test]
    fn the_uuid_comes_off_the_disabled_list() {
        let (list, changed) = with_uuid(strv(&[UUID, "a@b"]), false);
        assert_eq!(list, ["a@b"]);
        assert!(changed);
        let (_, changed) = with_uuid(strv(&["a@b"]), false);
        assert!(!changed);
    }
}
