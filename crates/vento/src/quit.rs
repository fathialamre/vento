//! Quitting vento from the tray menu or the Quick panel: with PDF windows
//! that have unsaved page-list edits open, each asks its question first and
//! a Cancel keeps vento running.

use std::rc::Rc;

use log::warn;

use crate::exec::Executor;
use crate::pdf::PdfController;

/// Quits vento, after asking about every PDF window with unsaved edits.
/// Without any it quits at once. A job still running would leave its partial
/// output behind, so the executor is shut down first.
pub fn quit(exec: &Rc<Executor>, pdf: &Rc<PdfController>) {
    let (exec, asked) = (exec.clone(), pdf.clone());
    quit_with(
        pdf.edited_windows(),
        move |on_done| asked.close_all(on_done),
        move || {
            exec.shutdown();
            if let Err(e) = slint::quit_event_loop() {
                warn!("cannot quit: {e}");
            }
        },
    );
}

/// The decision behind [`quit`]: with `edited` windows to ask about,
/// `ask_all` is called with the continuation and `finish` runs only if it is
/// told `true`; with none, `finish` runs at once.
fn quit_with(
    edited: usize,
    ask_all: impl FnOnce(Box<dyn FnOnce(bool)>),
    finish: impl FnOnce() + 'static,
) {
    if edited == 0 {
        finish();
        return;
    }
    ask_all(Box::new(move |ok| {
        if ok {
            finish();
        }
    }));
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// The continuation `quit_with` hands to the question.
    type Continuation = Box<dyn FnOnce(bool)>;

    /// Runs `quit_with` and returns whether the question was asked, the
    /// answer callback (if asked) and whether it finished.
    fn run(edited: usize) -> (bool, Option<Continuation>, Rc<Cell<bool>>) {
        let asked = Rc::new(RefCell::new(None));
        let finished = Rc::new(Cell::new(false));
        let (slot, done) = (asked.clone(), finished.clone());
        quit_with(
            edited,
            move |on_done| *slot.borrow_mut() = Some(on_done),
            move || done.set(true),
        );
        let on_done = asked.borrow_mut().take();
        (on_done.is_some(), on_done, finished)
    }

    #[test]
    fn quits_at_once_without_edited_windows() {
        let (asked, _, finished) = run(0);
        assert!(!asked);
        assert!(finished.get());
    }

    #[test]
    fn asks_first_and_quits_only_when_every_window_agreed() {
        let (asked, on_done, finished) = run(2);
        assert!(asked);
        assert!(!finished.get(), "not before the answers");
        on_done.expect("asked")(true);
        assert!(finished.get());
    }

    #[test]
    fn a_cancel_keeps_vento_running() {
        let (asked, on_done, finished) = run(1);
        assert!(asked);
        on_done.expect("asked")(false);
        assert!(!finished.get());
    }
}
