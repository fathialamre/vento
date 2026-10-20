//! The global shortcut that opens the Pocket: a Carbon hot key through
//! `global-hotkey`, which needs no Accessibility or Input Monitoring
//! permission.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Once;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use log::{debug, warn};
use vento_core::settings::parse_shortcut;

/// The action for the registered hot key, by id.
type Pressed = (u32, Rc<dyn Fn()>);

thread_local! {
    /// What the current hot key does; lives on the main thread, where the
    /// non-`Send` controllers are.
    static ON_PRESS: RefCell<Option<Pressed>> = const { RefCell::new(None) };
}

/// `global-hotkey` keeps one event handler for the whole process.
static HANDLER: Once = Once::new();

/// Keeps the hot key registered; dropping it unregisters the key.
pub struct HotkeyHandle {
    _manager: GlobalHotKeyManager,
}

/// The hot key for a spec such as `"ctrl+alt+p"`; see [`parse_shortcut`].
fn to_hotkey(spec: &str) -> Result<HotKey, String> {
    let (m, key) = parse_shortcut(spec).ok_or_else(|| format!("invalid shortcut \"{spec}\""))?;
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::CONTROL, m.ctrl);
    mods.set(Modifiers::ALT, m.alt);
    mods.set(Modifiers::SHIFT, m.shift);
    mods.set(Modifiers::META, m.meta);
    let code = key_code(key).ok_or_else(|| format!("invalid shortcut \"{spec}\""))?;
    Ok(HotKey::new(Some(mods), code))
}

/// The key code for a lower-case ASCII letter or a digit.
fn key_code(key: char) -> Option<Code> {
    const LETTERS: [Code; 26] = [
        Code::KeyA,
        Code::KeyB,
        Code::KeyC,
        Code::KeyD,
        Code::KeyE,
        Code::KeyF,
        Code::KeyG,
        Code::KeyH,
        Code::KeyI,
        Code::KeyJ,
        Code::KeyK,
        Code::KeyL,
        Code::KeyM,
        Code::KeyN,
        Code::KeyO,
        Code::KeyP,
        Code::KeyQ,
        Code::KeyR,
        Code::KeyS,
        Code::KeyT,
        Code::KeyU,
        Code::KeyV,
        Code::KeyW,
        Code::KeyX,
        Code::KeyY,
        Code::KeyZ,
    ];
    const DIGITS: [Code; 10] = [
        Code::Digit0,
        Code::Digit1,
        Code::Digit2,
        Code::Digit3,
        Code::Digit4,
        Code::Digit5,
        Code::Digit6,
        Code::Digit7,
        Code::Digit8,
        Code::Digit9,
    ];
    match key {
        'a'..='z' => LETTERS.get(key as usize - 'a' as usize).copied(),
        '0'..='9' => DIGITS.get(key as usize - '0' as usize).copied(),
        _ => None,
    }
}

/// Registers `spec` as a global shortcut and calls `on_press` on the main
/// thread each time it is pressed. Replaces any earlier action; drop the
/// earlier handle first, so its key is free to register again.
///
/// # Errors
/// If the spec is invalid or the key cannot be registered (for example,
/// another app holds it).
pub fn install_hotkey(spec: &str, on_press: Box<dyn Fn()>) -> Result<HotkeyHandle, String> {
    let hotkey = to_hotkey(spec)?;
    let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
    manager.register(hotkey).map_err(|e| e.to_string())?;
    ON_PRESS.with(|slot| *slot.borrow_mut() = Some((hotkey.id(), Rc::from(on_press))));
    // Hot key events may arrive on any thread: forward only the (Send) id to
    // the main thread and act on it there.
    HANDLER.call_once(|| {
        GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
            if event.state() != HotKeyState::Pressed {
                return;
            }
            let id = event.id();
            if let Err(e) = slint::invoke_from_event_loop(move || {
                // Clone it out, so the action may replace the hot key.
                let action = ON_PRESS.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .filter(|(current, _)| *current == id)
                        .map(|(_, f)| f.clone())
                });
                if let Some(action) = action {
                    action();
                }
            }) {
                warn!("cannot deliver the shortcut: {e}");
            }
        }));
    });
    debug!("pocket shortcut {spec} registered");
    Ok(HotkeyHandle { _manager: manager })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_alt_p_maps_to_carbon_modifiers_and_key() {
        let hotkey = to_hotkey("ctrl+alt+p").unwrap();
        assert_eq!(hotkey.mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(hotkey.key, Code::KeyP);
    }

    #[test]
    fn cmd_shift_digit_maps() {
        let hotkey = to_hotkey("cmd+shift+7").unwrap();
        assert_eq!(hotkey.mods, Modifiers::META | Modifiers::SHIFT);
        assert_eq!(hotkey.key, Code::Digit7);
        assert_eq!(to_hotkey("alt+a").unwrap().key, Code::KeyA);
        assert_eq!(to_hotkey("alt+z").unwrap().key, Code::KeyZ);
        assert_eq!(to_hotkey("alt+0").unwrap().key, Code::Digit0);
    }

    #[test]
    fn bad_specs_say_why() {
        assert_eq!(to_hotkey("p").unwrap_err(), "invalid shortcut \"p\"");
        assert!(to_hotkey("ctrl+alt+").is_err());
        assert!(to_hotkey("hyper+p").is_err());
    }
}
