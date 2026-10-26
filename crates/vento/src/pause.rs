//! Whether vento is paused: the one state the tray's "Pause Vento" item and
//! the Quick panel's Pause both show and change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Called with the new state after every change.
type Listener = Box<dyn Fn(bool)>;

/// The paused state and who follows it.
#[derive(Default)]
pub struct PauseSwitch {
    paused: Cell<bool>,
    listeners: RefCell<Vec<Listener>>,
}

impl PauseSwitch {
    /// Running, with no listeners.
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn is_paused(&self) -> bool {
        self.paused.get()
    }

    /// Pauses or resumes vento and tells every listener; setting the state
    /// it already has does nothing.
    pub fn set(&self, paused: bool) {
        if self.paused.replace(paused) == paused {
            return;
        }
        for listener in self.listeners.borrow().iter() {
            listener(paused);
        }
    }

    /// Calls `f` with the new state after every change. A listener must not
    /// add listeners nor change the state.
    pub fn on_change(&self, f: Listener) {
        self.listeners.borrow_mut().push(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listener_hears_each_change_once() {
        let switch = PauseSwitch::new();
        let heard = Rc::new(RefCell::new(Vec::new()));
        for name in ["tray", "panel"] {
            let heard = heard.clone();
            switch.on_change(Box::new(move |p| heard.borrow_mut().push((name, p))));
        }
        switch.set(true);
        assert!(switch.is_paused());
        // The same state again is not a change.
        switch.set(true);
        switch.set(false);
        assert_eq!(
            *heard.borrow(),
            [
                ("tray", true),
                ("panel", true),
                ("tray", false),
                ("panel", false)
            ]
        );
    }

    #[test]
    fn starts_running() {
        assert!(!PauseSwitch::new().is_paused());
    }
}
