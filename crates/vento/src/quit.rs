//! Quitting vento from the tray menu, the Quick panel or the app menu (⌘Q
//! in a vento window, through `Platform::on_quit_request`): with PDF windows
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

/// Quits vento to finish an update, after the same questions as [`quit`].
/// A Cancel calls `on_cancel` and leaves everything as it was. Once every
/// window agreed, `swap` moves the new copy in and starts the watcher: an
/// `Err` is handed to `on_fail` and vento keeps running; `Ok` shuts the
/// executor down and quits, so the watcher can start the new copy.
// Only the macOS update controller calls it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn quit_for_update(
    exec: &Rc<Executor>,
    pdf: &Rc<PdfController>,
    swap: Box<dyn FnOnce() -> Result<(), String>>,
    on_cancel: Box<dyn FnOnce()>,
    on_fail: Box<dyn FnOnce(String)>,
) {
    let (exec, asked) = (exec.clone(), pdf.clone());
    quit_for_update_with(
        pdf.edited_windows(),
        move |on_done| asked.close_all(on_done),
        swap,
        move || {
            exec.shutdown();
            if let Err(e) = slint::quit_event_loop() {
                warn!("cannot quit: {e}");
            }
        },
        on_cancel,
        on_fail,
    );
}

/// The decision behind [`quit_for_update`]: the question as in
/// [`quit_with`], then `swap`, then `finish` only when the swap worked.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn quit_for_update_with(
    edited: usize,
    ask_all: impl FnOnce(Box<dyn FnOnce(bool)>),
    swap: Box<dyn FnOnce() -> Result<(), String>>,
    finish: impl FnOnce() + 'static,
    on_cancel: Box<dyn FnOnce()>,
    on_fail: Box<dyn FnOnce(String)>,
) {
    let go = move || match swap() {
        Ok(()) => finish(),
        Err(reason) => on_fail(reason),
    };
    if edited == 0 {
        go();
        return;
    }
    ask_all(Box::new(move |ok| {
        if ok {
            go();
        } else {
            on_cancel();
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

    /// What a `quit_for_update_with` run did, in order.
    fn run_update(
        edited: usize,
        answer: Option<bool>,
        swap_result: Result<(), String>,
    ) -> Vec<String> {
        let log: Rc<RefCell<Vec<String>>> = Rc::default();
        let l = |s: &str| {
            let (log, s) = (log.clone(), s.to_string());
            move || log.borrow_mut().push(s)
        };
        let (swap_log, fail_log, ask_log) = (log.clone(), log.clone(), log.clone());
        let finish = l("finish");
        let cancel = l("cancel");
        quit_for_update_with(
            edited,
            move |on_done| {
                ask_log.borrow_mut().push("ask".into());
                if let Some(ok) = answer {
                    on_done(ok);
                }
            },
            Box::new(move || {
                swap_log.borrow_mut().push("swap".into());
                swap_result
            }),
            finish,
            Box::new(cancel),
            Box::new(move |r| fail_log.borrow_mut().push(format!("fail {r}"))),
        );
        log.take()
    }

    #[test]
    fn update_quit_cancelled_runs_neither_swap_nor_finish() {
        assert_eq!(run_update(1, Some(false), Ok(())), ["ask", "cancel"]);
    }

    #[test]
    fn update_quit_waits_for_the_answers() {
        assert_eq!(run_update(2, None, Ok(())), ["ask"]);
    }

    #[test]
    fn swap_failure_keeps_running_and_reports() {
        assert_eq!(
            run_update(0, None, Err("busy".into())),
            ["swap", "fail busy"]
        );
        assert_eq!(
            run_update(1, Some(true), Err("busy".into())),
            ["ask", "swap", "fail busy"]
        );
    }

    #[test]
    fn swap_ok_finishes() {
        assert_eq!(run_update(0, None, Ok(())), ["swap", "finish"]);
        assert_eq!(run_update(1, Some(true), Ok(())), ["ask", "swap", "finish"]);
    }
}
