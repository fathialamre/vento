//! Global shortcuts on Linux, through the desktop portal's GlobalShortcuts
//! (`global-hotkey` only works on X11 and cannot see keys pressed in
//! Wayland apps). The first time a shortcut is bound, GNOME asks the user to
//! allow it under its description.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::warn;
use vento_core::settings::ShortcutError;
use vento_platform::GlobalShortcut;

thread_local! {
    /// What each bound shortcut does; lives on the main thread, where the
    /// presses are handed to.
    static ACTIONS: RefCell<HashMap<u64, Rc<dyn Fn()>>> = RefCell::new(HashMap::new());
}

/// Keeps the shortcut bound; dropping it unbinds the shortcut and forgets
/// its action.
pub struct HotkeyHandle {
    id: u64,
    _shortcut: GlobalShortcut,
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        ACTIONS.with(|actions| actions.borrow_mut().remove(&self.id));
    }
}

/// Binds `spec` as a global shortcut named `description`, calling
/// `on_press` on the main thread each time it is pressed. The binding
/// finishes in the background; one GNOME refuses is logged.
///
/// # Errors
/// If the spec is invalid.
pub fn install_hotkey(
    spec: &str,
    description: &str,
    on_press: Box<dyn Fn()>,
) -> Result<HotkeyHandle, ShortcutError> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    // Presses arrive on the portal's thread: hand only the (Send) id to the
    // main thread and act there.
    let shortcut = vento_platform::bind_global_shortcut(
        spec,
        description,
        Box::new(move || {
            if let Err(e) = slint::invoke_from_event_loop(move || {
                let action = ACTIONS.with(|actions| actions.borrow().get(&id).cloned());
                if let Some(action) = action {
                    action();
                }
            }) {
                warn!("cannot deliver a shortcut press: {e}");
            }
        }),
    )?;
    ACTIONS.with(|actions| actions.borrow_mut().insert(id, Rc::from(on_press)));
    Ok(HotkeyHandle {
        id,
        _shortcut: shortcut,
    })
}
