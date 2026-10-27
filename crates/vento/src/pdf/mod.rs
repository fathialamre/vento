//! The PDF action: dropping PDFs on the wheel's PDF cell opens their pages
//! in a window of their own, one window per drop.
//!
//! Opening reads the files on a short-lived worker thread (no file is read
//! on the main thread), then the window opens with the PDFs that could be
//! read and a toast names the rest. The controller keeps the open views
//! and drops each one, with its window, thumbnail thread and images, once
//! its window has closed: nothing of a closed window stays in memory.

mod thumbs;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

use log::{debug, warn};
use slint::Timer;
use vento_core::geometry::Point;
use vento_core::i18n::{fill, tr};
use vento_core::pdf::{PdfError, Source, open_sources};
use vento_platform::{Platform, SystemPdf};

use crate::exec::{Executor, pdf_open_toast_text};
use crate::toast::Toaster;
pub use window::PdfView;
use window::ViewCtx;

thread_local! {
    /// The controller, for files read on the opening thread. Set by
    /// [`PdfController::new`] on the main thread.
    static MAIN_PDF: RefCell<Weak<PdfController>> = const { RefCell::new(Weak::new()) };
}

/// Opens PDF windows and keeps them while they are open.
pub struct PdfController {
    platform: Rc<dyn Platform>,
    toaster: Rc<Toaster>,
    executor: Rc<Executor>,
    /// The open windows, in the order they opened.
    views: RefCell<Vec<Rc<PdfView>>>,
    /// Whether another vento window is open (it keeps the focus when the
    /// last PDF window closes).
    other_window_open: RefCell<Option<Box<dyn Fn() -> bool>>>,
    this: Weak<Self>,
}

impl PdfController {
    /// No windows yet.
    pub fn new(
        platform: Rc<dyn Platform>,
        toaster: Rc<Toaster>,
        executor: Rc<Executor>,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            toaster,
            executor,
            views: RefCell::new(Vec::new()),
            other_window_open: RefCell::new(None),
            this: this.clone(),
        });
        MAIN_PDF.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        controller
    }

    /// Tells the controller how to ask whether another vento window is
    /// open.
    pub fn set_other_window_open(&self, f: Box<dyn Fn() -> bool>) {
        *self.other_window_open.borrow_mut() = Some(f);
    }

    /// Opens a window for the PDFs among `paths`, dropped at `at` (global
    /// logical points). The files are read on a worker thread; the window
    /// opens when they have been, or a toast says why none could be.
    pub fn open(&self, paths: Vec<PathBuf>, at: Point) {
        let spawned = std::thread::Builder::new()
            .name("vento-pdf-open".into())
            .spawn(move || {
                let (sources, skipped) = open_sources(&paths, &SystemPdf);
                let result = slint::invoke_from_event_loop(move || {
                    if let Some(controller) = MAIN_PDF.with(|slot| slot.borrow().upgrade()) {
                        controller.opened(sources, skipped, at);
                    }
                });
                if let Err(e) = result {
                    warn!("cannot hand the opened PDFs to the window: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start opening the PDFs: {e}");
            let text = fill(
                &tr("Couldn't open the PDF: {reason}"),
                &[("reason", &e.to_string())],
            );
            self.toaster.show(&text, None, at);
        }
    }

    /// The files read, on the main thread: a toast for the ones skipped,
    /// and a window for the rest unless there are none.
    fn opened(&self, sources: Vec<Source>, skipped: Vec<(PathBuf, PdfError)>, at: Point) {
        for (path, error) in &skipped {
            debug!("PDF skipped: {}: {error:?}", path.display());
        }
        let text = pdf_open_toast_text(sources.len(), &skipped)
            .or_else(|| sources.is_empty().then(|| tr("No PDFs to open")));
        if let Some(text) = text {
            self.toaster.show(&text, None, at);
        }
        if sources.is_empty() {
            return;
        }
        let this = self.this.clone();
        let ctx = ViewCtx {
            platform: self.platform.clone(),
            toaster: self.toaster.clone(),
            executor: self.executor.clone(),
            closed: Rc::new(move |id| {
                if let Some(controller) = this.upgrade() {
                    controller.closed(id);
                }
            }),
        };
        let view = PdfView::new(sources, at, ctx);
        if !view.is_closed() {
            self.views.borrow_mut().push(view);
        }
    }

    /// Drops view `id` once its window has closed: on the next turn of the
    /// event loop, never inside one of the window's own callbacks. When it
    /// was the last vento window, the focus goes back to the app that had
    /// it.
    fn closed(&self, id: u64) {
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            let Some(controller) = this.upgrade() else {
                return;
            };
            let gone = {
                let mut views = controller.views.borrow_mut();
                views
                    .iter()
                    .position(|view| view.id() == id)
                    .map(|i| views.remove(i))
            };
            // Dropped after the borrow ends: dropping runs Slint code.
            drop(gone);
            let other = controller
                .other_window_open
                .borrow()
                .as_ref()
                .is_some_and(|open| open());
            if !controller.has_windows() && !other {
                controller.platform.deactivate_app();
            }
        });
    }

    /// Whether any PDF window is open.
    pub fn has_windows(&self) -> bool {
        self.views.borrow().iter().any(|view| !view.is_closed())
    }

    /// How many open windows have page-list edits not saved yet.
    #[allow(dead_code)] // Quitting asks about these (Task 11).
    pub fn edited_windows(&self) -> usize {
        self.views
            .borrow()
            .iter()
            .filter(|view| !view.is_closed() && view.is_edited())
            .count()
    }

    /// Closes every window, then calls `on_done` with whether quitting may
    /// go on. Every window closes through its `request_close` path; until
    /// that can ask about unsaved edits (Task 10), all close and quitting
    /// goes on.
    #[allow(dead_code)] // Quitting calls it (Task 11).
    pub fn close_all(&self, on_done: Box<dyn FnOnce(bool)>) {
        let views: Vec<Rc<PdfView>> = self.views.borrow().clone();
        for view in views {
            view.request_close();
        }
        on_done(true);
    }
}
