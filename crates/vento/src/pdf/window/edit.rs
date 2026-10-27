//! Editing a PDF window's page list: delete, drag to reorder, rotate, add
//! PDFs (the open panel or files dropped on the window), in-window undo
//! and redo, and the "pages removed" banner. Nothing here writes a file:
//! every edit changes only the window's [`PageList`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, warn};
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Image, Model, Timer, TimerMode};
use vento_core::geometry::Point;
use vento_core::i18n::{fill, tr};
use vento_core::pdf::{PageList, PageRef, PdfError, Selection, Source, open_sources};
use vento_platform::SystemPdf;

use super::{BANNER_TIME, DROP_BATCH, PdfView, SCROLL_TICK, live_view, spawn_worker};
use crate::exec::pdf_open_toast_text;
use crate::pdf::logic::{
    DragAction, DragEvent, DragPhase, auto_scroll_step, cell_origin, drag_step, dragged_pages,
    follow_selection, insert_point, insertion_bar, max_scroll, removed_text, turned_pages,
};
use crate::pdf::thumbs;

impl PdfView {
    /// Whether the page list can be edited now: not while a page is shown
    /// large or pages are being dragged.
    pub(super) fn can_edit(&self) -> bool {
        self.large.get().is_none() && self.drag.borrow().is_idle()
    }

    /// Runs `edit` on the page list and the selection, then shows the
    /// result: pages it turned show their old thumbnail turned at once,
    /// every other page with a cached thumbnail shows it again without a
    /// new drawing, and the title says "edited" while the list differs
    /// from how it opened. A banner showing goes: this is the next edit.
    pub(super) fn commit_edit(&self, edit: impl FnOnce(&mut PageList, &mut Selection)) {
        self.hide_banner();
        let before = self.pages.borrow().pages().to_vec();
        {
            let mut pages = self.pages.borrow_mut();
            let mut selection = self.selection.borrow_mut();
            edit(&mut pages, &mut selection);
        }
        self.turn_thumbnails(&before);
        self.pages_changed();
    }

    /// Caches the thumbnails of the pages turned since `before`, made by
    /// turning the image their row still shows from then.
    pub(super) fn turn_thumbnails(&self, before: &[PageRef]) {
        let turned = turned_pages(before, self.pages.borrow().pages());
        let mut thumbs = self.thumbs.borrow_mut();
        for (i, page, quarter_turns) in turned {
            let Some(row) = self.rows.row_data(i) else {
                continue;
            };
            if !row.rendered {
                continue;
            }
            if row.broken {
                thumbs.seed(page, None);
            } else if let Some(buffer) = row.image.to_rgba8() {
                let image = Image::from_rgba8(thumbs::turned(&buffer, quarter_turns));
                thumbs.seed(page, Some(image));
            }
        }
    }

    /// Deletes the selected pages and says so in the banner, with Undo.
    pub(super) fn delete_selected(&self) {
        if !self.can_edit() {
            return;
        }
        let at = self.selection.borrow().indices();
        if at.is_empty() {
            return;
        }
        self.commit_edit(|pages, selection| {
            pages.delete(&at);
            selection.clear();
        });
        self.show_banner(&removed_text(at.len()));
    }

    /// Turns the selected pages a quarter turn; they stay selected.
    pub(super) fn rotate_selected(&self, clockwise: bool) {
        if !self.can_edit() {
            return;
        }
        let at = self.selection.borrow().indices();
        if at.is_empty() {
            return;
        }
        self.commit_edit(|pages, _| pages.rotate(&at, clockwise));
    }

    /// Steps back one edit: ⌘Z, and Undo in the banner.
    pub(super) fn undo(&self) {
        self.step(PageList::undo);
    }

    /// Re-applies the last edit undone: ⌘⇧Z.
    pub(super) fn redo(&self) {
        self.step(PageList::redo);
    }

    /// Undoes or redoes one edit. The selected pages stay selected where
    /// they went, and pages that came back are selected.
    pub(super) fn step(&self, step: fn(&mut PageList) -> bool) {
        if !self.can_edit() {
            return;
        }
        self.commit_edit(|pages, selection| {
            let before = pages.pages().to_vec();
            if step(pages) {
                selection.set(follow_selection(
                    &before,
                    pages.pages(),
                    &selection.indices(),
                ));
            }
        });
    }

    /// Shows `text` in the banner over the grid, with Undo, for a while.
    pub(super) fn show_banner(&self, text: &str) {
        self.window.set_banner(text.into());
        self.window.set_banner_shown(true);
        let this = self.this.clone();
        self.banner_timer
            .start(TimerMode::SingleShot, BANNER_TIME, move || {
                if let Some(view) = this.upgrade() {
                    view.hide_banner();
                }
            });
    }

    pub(super) fn hide_banner(&self) {
        self.banner_timer.stop();
        self.window.set_banner_shown(false);
    }

    /// Where the drag stands.
    fn drag_phase(&self) -> DragPhase {
        match &*self.drag.borrow() {
            Drag::Idle => DragPhase::Idle,
            Drag::Moving(_) => DragPhase::Moving,
            Drag::Cancelled => DragPhase::Cancelled,
        }
    }

    /// A drag event on page `i` with the pointer at `at` in the grid area
    /// (not scrolled; only moves use it), carried out as
    /// [`drag_step`] rules: only the left button lifts pages, any release
    /// or cancel ends the drag, and Escape cancels it until the release.
    pub(super) fn drag_event(&self, i: i32, event: DragEvent, at: (f32, f32)) {
        if let (Drag::Moving(moving), DragEvent::Moved { .. }) =
            (&mut *self.drag.borrow_mut(), event)
        {
            moving.at = at;
        }
        let (phase, action) = drag_step(self.drag_phase(), event);
        match action {
            DragAction::Nothing => {
                if phase == DragPhase::Idle {
                    *self.drag.borrow_mut() = Drag::Idle;
                }
            }
            DragAction::Start => {
                if let DragEvent::Moved { dx, dy, .. } = event {
                    self.start_drag(i, (dx, dy), at);
                }
            }
            DragAction::Follow => self.drag_moved(),
            DragAction::Drop => self.drop_pages(),
            DragAction::End => {
                self.end_drag();
                if phase == DragPhase::Cancelled {
                    *self.drag.borrow_mut() = Drag::Cancelled;
                }
            }
        }
    }

    /// Lifts page `i` (or the selection, when the page is in it), pressed
    /// `from` (dx, dy) before the pointer reached `at`; the drag follows the
    /// pointer from then on. Not while the page list cannot be edited.
    fn start_drag(&self, i: i32, from: (f32, f32), at: (f32, f32)) {
        if !self.can_edit() {
            return;
        }
        let Ok(i) = usize::try_from(i) else { return };
        if i >= self.rows.row_count() {
            return;
        }
        let ((dx, dy), (x, y)) = (from, at);
        let pages = dragged_pages(&self.selection.borrow().indices(), i);
        if !self.selection.borrow().contains(i) {
            self.selection.borrow_mut().click(i);
            self.selection_changed();
        }
        self.set_focus(Some(i), false);
        // Where the press was in the pressed page's cell.
        let (cell_x, cell_y) = cell_origin(&self.metrics(), i);
        let scroll = -self.window.get_content_y();
        let grab = (
            x - dx - (cell_x + self.grid_left.get()),
            y - dy - (cell_y - scroll),
        );
        self.set_ghosts(&pages, true);
        if let Some(row) = self.rows.row_data(i) {
            self.window.set_lifted(row);
        }
        self.window
            .set_lifted_count(i32::try_from(pages.len()).unwrap_or(i32::MAX));
        *self.drag.borrow_mut() = Drag::Moving(Moving {
            pages,
            grab,
            at: (x, y),
            before: i,
        });
        self.window.set_dragging(true);
        let this = self.this.clone();
        self.scroll_timer
            .start(TimerMode::Repeated, SCROLL_TICK, move || {
                if let Some(view) = this.upgrade() {
                    view.auto_scroll();
                }
            });
        self.drag_moved();
    }

    /// Marks the rows at `at` as the faint places dragged pages left.
    pub(super) fn set_ghosts(&self, at: &[usize], ghost: bool) {
        for &i in at {
            if let Some(mut row) = self.rows.row_data(i)
                && row.ghost != ghost
            {
                row.ghost = ghost;
                self.rows.set_row_data(i, row);
            }
        }
    }

    /// Moves the lifted page to the pointer and the orange bar to where
    /// the pages would land.
    pub(super) fn drag_moved(&self) {
        let metrics = self.metrics();
        let count = self.rows.row_count();
        let scroll = -self.window.get_content_y();
        let (lifted, bar) = {
            let mut drag = self.drag.borrow_mut();
            let Drag::Moving(moving) = &mut *drag else {
                return;
            };
            let (x, y) = moving.at;
            let (gx, gy) = self.grid_point(x, y + scroll);
            moving.before = metrics.insertion_index(gx, gy, count);
            (
                (x - moving.grab.0, y - moving.grab.1),
                insertion_bar(&metrics, moving.before, count, gy),
            )
        };
        self.window.set_lifted_x(lifted.0);
        self.window.set_lifted_y(lifted.1);
        self.window.set_bar_x(bar.0 + self.grid_left.get());
        self.window.set_bar_y(bar.1);
    }

    /// Scrolls the grid while the pointer is near its top or bottom edge
    /// during a drag; runs every frame of the drag.
    pub(super) fn auto_scroll(&self) {
        let at_y = match &*self.drag.borrow() {
            Drag::Moving(moving) => moving.at.1,
            _ => return,
        };
        let height = self.window.get_grid_height();
        let step = auto_scroll_step(at_y, height);
        if step == 0.0 {
            return;
        }
        let scroll = -self.window.get_content_y();
        let max = max_scroll(&self.metrics(), self.rows.row_count(), height);
        let next = (scroll + step).clamp(0.0, max);
        if next != scroll {
            self.window.set_content_y(-next);
            self.drag_moved();
        }
    }

    /// The left button went up during a drag: the pages drop where the
    /// orange bar stands and stay selected there.
    fn drop_pages(&self) {
        let drag = std::mem::replace(&mut *self.drag.borrow_mut(), Drag::Idle);
        let Drag::Moving(moving) = drag else { return };
        let mut moved = Vec::new();
        self.commit_edit(|pages, selection| {
            moved = pages.move_to(&moving.pages, moving.before);
            selection.set(moved.clone());
        });
        if let Some(&first) = moved.first() {
            self.set_focus(Some(first), false);
        }
    }

    /// Ends any drag without moving anything: the pages are no longer
    /// faint, and the lifted page and the bar go.
    pub(super) fn end_drag(&self) {
        let drag = std::mem::replace(&mut *self.drag.borrow_mut(), Drag::Idle);
        self.scroll_timer.stop();
        self.window.set_dragging(false);
        if let Drag::Moving(moving) = drag {
            self.set_ghosts(&moving.pages, false);
        }
    }

    /// ＋ Add PDF: the open panel, starting in the first file's folder.
    pub(super) fn add_pdf(&self) {
        let start = self
            .sources
            .borrow()
            .first()
            .and_then(|s| s.path.parent().map(Path::to_path_buf));
        let paths = vento_platform::pick_pdfs(start.as_deref());
        // The modal panel had the key focus; its focus changes may still
        // be on their way, so the window takes it back on the next turn.
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(view) = this.upgrade() {
                view.window.window().with_winit_window(|w| w.focus_window());
            }
        });
        if !paths.is_empty() {
            self.add_files(paths);
        }
    }

    /// A file dropped on the window: collected with the rest of its drop.
    pub(super) fn file_dropped(&self, path: PathBuf) {
        if self.closed.get() {
            return;
        }
        self.dropped.borrow_mut().push(path);
        if !self.drop_timer.running() {
            let this = self.this.clone();
            self.drop_timer
                .start(TimerMode::SingleShot, DROP_BATCH, move || {
                    if let Some(view) = this.upgrade() {
                        let paths = std::mem::take(&mut *view.dropped.borrow_mut());
                        view.add_files(paths);
                    }
                });
        }
    }

    /// Reads `paths` on a worker thread (never on the main thread), then
    /// adds the PDFs among them.
    pub(super) fn add_files(&self, paths: Vec<PathBuf>) {
        let id = self.id;
        let spawned = std::thread::Builder::new()
            .name("vento-pdf-open".into())
            .spawn(move || {
                let (sources, skipped) = open_sources(&paths, &SystemPdf);
                let result = slint::invoke_from_event_loop(move || {
                    if let Some(view) = live_view(id) {
                        view.files_opened(sources, skipped);
                    }
                });
                if let Err(e) = result {
                    warn!("cannot hand the added PDFs to the window: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start opening the added PDFs: {e}");
            let text = fill(
                &tr("Couldn't open the PDF: {reason}"),
                &[("reason", &e.to_string())],
            );
            self.ctx.toaster.show(&text, None, self.centre());
        }
    }

    /// Added files read: a toast names the ones skipped (not PDFs, locked,
    /// broken), and the pages of the rest go after the focused page, or at
    /// the end, selected.
    pub(super) fn files_opened(&self, opened: Vec<Source>, skipped: Vec<(PathBuf, PdfError)>) {
        if self.closed.get() {
            return;
        }
        for (path, error) in &skipped {
            debug!("added PDF skipped: {}: {error:?}", path.display());
        }
        if let Some(text) = pdf_open_toast_text(opened.len(), &skipped) {
            self.ctx.toaster.show(&text, None, self.centre());
        }
        if opened.is_empty() {
            return;
        }
        let first = self.sources.borrow().len();
        let added: Vec<PageRef> = opened
            .iter()
            .enumerate()
            .flat_map(|(k, s)| {
                (0..s.page_count).map(move |page| PageRef {
                    source: first + k,
                    page,
                    turns: 0,
                })
            })
            .collect();
        let sources: Arc<[Source]> = self
            .sources
            .borrow()
            .iter()
            .cloned()
            .chain(opened)
            .collect();
        let bytes = sources.iter().map(|s| s.bytes.clone()).collect();
        *self.sources.borrow_mut() = sources;
        // The old thread still answers what it was asked; those results are
        // for a layout that has gone by then and are dropped.
        *self.worker.borrow_mut() = spawn_worker(self.id, bytes);
        self.close_large();
        self.end_drag();
        let at = insert_point(self.focus.get(), self.pages.borrow().len());
        let n = added.len();
        self.commit_edit(|pages, selection| {
            pages.insert(added, at);
            selection.set((at..at + n).collect());
        });
        self.scroll_to(at);
    }

    /// The window's centre in global logical points, where its toasts go.
    pub(super) fn centre(&self) -> Point {
        let window = self.window.window();
        let scale = window.scale_factor();
        let position = window.position().to_logical(scale);
        let size = window.size().to_logical(scale);
        Point {
            x: f64::from(position.x + size.width / 2.0),
            y: f64::from(position.y + size.height / 2.0),
        }
    }
}

/// A drag of pages in the grid.
pub(super) enum Drag {
    Idle,
    Moving(Moving),
    /// Cancelled with Escape while the button is still down: its release
    /// does nothing, and is not a click.
    Cancelled,
}

impl Drag {
    pub(super) fn is_idle(&self) -> bool {
        matches!(self, Drag::Idle)
    }
}

/// Pages being dragged.
pub(super) struct Moving {
    /// Their positions, sorted.
    pages: Vec<usize>,
    /// Where the press was within the pressed page's cell.
    grab: (f32, f32),
    /// The pointer in the grid area (not scrolled).
    at: (f32, f32),
    /// Where they would land, `0..=len`.
    before: usize,
}
