//! Linux stand-in for global shortcuts. `global-hotkey` only works on X11
//! and cannot see keys pressed in Wayland apps; GNOME's GlobalShortcuts
//! portal will take its place. Until then every shortcut is refused with
//! the reason, which the caller shows.

use vento_core::settings::parse_shortcut;

/// Holds nothing: no shortcut is ever registered.
pub struct HotkeyHandle;

/// Refuses `spec`, saying why: an invalid spec as on macOS, any other
/// because Linux has no global shortcuts yet.
///
/// # Errors
/// Always.
pub fn install_hotkey(spec: &str, _on_press: Box<dyn Fn()>) -> Result<HotkeyHandle, String> {
    if parse_shortcut(spec).is_none() {
        return Err(format!("invalid shortcut \"{spec}\""));
    }
    Err("global shortcuts are not available on Linux yet".into())
}
