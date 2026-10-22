//! Global shortcuts, such as the ones that open the Pocket and the Clipboard
//! history: Carbon hot keys through `global-hotkey`, which need no
//! Accessibility or Input Monitoring permission. Any number can be registered
//! at once; each keeps its own action.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use log::{debug, warn};
use vento_core::settings::parse_shortcut;

thread_local! {
    /// What each registered hot key does, by hot key id; lives on the main
    /// thread, where the non-`Send` controllers are.
    static ON_PRESS: RefCell<HashMap<u32, Rc<dyn Fn()>>> = RefCell::new(HashMap::new());
}

/// Records what the hot key `id` does.
fn remember(id: u32, action: Rc<dyn Fn()>) {
    ON_PRESS.with(|map| map.borrow_mut().insert(id, action));
}

/// Drops the action of the hot key `id`.
fn forget(id: u32) {
    ON_PRESS.with(|map| map.borrow_mut().remove(&id));
}

/// The action of the hot key `id`, cloned out so it may replace hot keys.
fn action_for(id: u32) -> Option<Rc<dyn Fn()>> {
    ON_PRESS.with(|map| map.borrow().get(&id).cloned())
}

/// `global-hotkey` keeps one event handler for the whole process.
static HANDLER: Once = Once::new();

/// Keeps the hot key registered; dropping it forgets its action and
/// unregisters the key.
pub struct HotkeyHandle {
    id: u32,
    _manager: GlobalHotKeyManager,
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        // Runs before the manager field drops and unregisters the key.
        forget(self.id);
    }
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
/// thread each time it is pressed. Other shortcuts stay registered with their
/// own actions; to register the same key again, drop its earlier handle
/// first, so the key is free.
///
/// # Errors
/// If the spec is invalid or the key cannot be registered (for example,
/// another app holds it).
pub fn install_hotkey(spec: &str, on_press: Box<dyn Fn()>) -> Result<HotkeyHandle, String> {
    let hotkey = to_hotkey(spec)?;
    let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
    manager.register(hotkey).map_err(|e| e.to_string())?;
    let id = hotkey.id();
    remember(id, Rc::from(on_press));
    // Hot key events may arrive on any thread: forward only the (Send) id to
    // the main thread and act on it there.
    HANDLER.call_once(|| {
        GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
            if event.state() != HotKeyState::Pressed {
                return;
            }
            let id = event.id();
            if let Err(e) = slint::invoke_from_event_loop(move || {
                if let Some(action) = action_for(id) {
                    action();
                }
            }) {
                warn!("cannot deliver the shortcut: {e}");
            }
        }));
    });
    debug!("shortcut {spec} registered");
    Ok(HotkeyHandle {
        id,
        _manager: manager,
    })
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

    #[test]
    fn two_actions_are_kept_by_id() {
        let hits = Rc::new(RefCell::new(Vec::new()));
        let first = hits.clone();
        let second = hits.clone();
        remember(1, Rc::new(move || first.borrow_mut().push("first")));
        remember(2, Rc::new(move || second.borrow_mut().push("second")));

        action_for(1).expect("first is kept")();
        action_for(2).expect("second is kept")();
        assert_eq!(*hits.borrow(), ["first", "second"]);

        forget(1);
        assert!(action_for(1).is_none());
        assert!(action_for(2).is_some());
        forget(2);
        assert!(action_for(2).is_none());
    }
}
