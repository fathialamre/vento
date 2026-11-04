//! What GNOME says about animations and notification banners, read in
//! process through gio. A missing schema or key is false.

use gio::prelude::*;

/// Whether animations are turned off.
pub(super) fn reduce_motion() -> bool {
    setting_bool("org.gnome.desktop.interface", "enable-animations") == Some(false)
}

/// Whether notification banners are turned off (Do Not Disturb).
pub(super) fn quiet_time() -> bool {
    setting_bool("org.gnome.desktop.notifications", "show-banners") == Some(false)
}

/// A boolean setting, `None` if the schema or the key is not installed
/// (asking gio for either would abort).
fn setting_bool(schema: &str, key: &str) -> Option<bool> {
    let found = gio::SettingsSchemaSource::default()?.lookup(schema, true)?;
    if !found.has_key(key) {
        return None;
    }
    Some(gio::Settings::new(schema).boolean(key))
}
