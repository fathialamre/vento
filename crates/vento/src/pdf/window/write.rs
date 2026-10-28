//! Writing from a PDF window: Save as new PDF, Extract, Split (its popover
//! and cut mode), To images (its popover, with the format and quality kept
//! in the settings), the write's progress in the footer with Cancel, and the
//! question about unsaved edits on closing.
//!
//! Every write is one job on vento's file worker through
//! [`Executor::pdf_job`](crate::exec::Executor::pdf_job), which toasts the
//! result with Undo. While it runs the window's tools are dimmed and its
//! page list cannot be edited.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, warn};
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, ModelRc, TimerMode, VecModel};
use vento_core::archive::Progress;
use vento_core::i18n::{fill, tr, trn};
use vento_core::pdf::{ImageKind, ImageOptions, PageRef, PdfJob, PdfOutcome, SplitMode};

use super::{Answered, PdfView, file_name};
use crate::exec::{PdfDone, pdf_toast_text};
use crate::pdf::logic::{
    closes_after_save, cut_at, cut_footer, cut_marker, cut_summary, every_n_text,
    files_text, format_key, image_options, images_texts, job_fraction, pages_text, replace_answer,
    sheet_save_allowed, split_heading, split_n_range, toggle_cut,
};
use crate::ui::PdfCutMark;

/// How long a write runs before the footer shows its progress and Cancel.
const PROGRESS_DELAY: Duration = Duration::from_millis(500);
/// How often the footer's progress is read while a write runs.
const PROGRESS_TICK: Duration = Duration::from_millis(100);
/// How long a save that closes the window waits before closing it.
const SAVED_TIME: Duration = Duration::from_millis(1500);
/// How long the footer says what a write made (paused while the pointer is
/// over it), and how often its bar shortens.
const NOTE_TIME: Duration = Duration::from_secs(6);
const NOTE_TICK: Duration = Duration::from_millis(50);

/// A popover under a toolbar button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Popover {
    None,
    Split,
    Images,
}

/// The Split popover's choices, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SplitChoice {
    EveryPage,
    EveryN,
    Cuts,
}

impl SplitChoice {
    fn index(self) -> i32 {
        match self {
            SplitChoice::EveryPage => 0,
            SplitChoice::EveryN => 1,
            SplitChoice::Cuts => 2,
        }
    }

    fn from_index(i: i32) -> Option<SplitChoice> {
        match i {
            0 => Some(SplitChoice::EveryPage),
            1 => Some(SplitChoice::EveryN),
            2 => Some(SplitChoice::Cuts),
            _ => None,
        }
    }
}

/// What a write made, said in the footer for a while: what Show in Finder
/// reveals, and the executor's serial for its Undo.
pub(super) struct Note {
    reveal: PathBuf,
    undo: Option<u64>,
}

/// A write of this window that is running.
pub(super) struct Job {
    progress: Arc<Progress>,
    started: Instant,
    /// The closing question's own Save, which Discard and Cancel stop.
    closing: bool,
}

/// What happens once a write has ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    /// Save as new PDF: the window says "Saved …" and closes.
    Save,
    /// Save from the closing question: the window closes at once.
    SaveAndClose,
    /// Extract, Split and To images: the window stays as it is; `cut` is a
    /// split from cut mode, which then ends.
    Write { cut: bool },
}

impl PdfView {
    /// Whether a write of this window runs, or it is closing after a save.
    pub(super) fn is_busy(&self) -> bool {
        self.job.borrow().is_some() || self.saved.get()
    }

    /// Whether cut mode is on.
    pub(super) fn in_cut_mode(&self) -> bool {
        self.cuts.borrow().is_some()
    }

    /// Whether a popover, cut mode or the closing question holds the
    /// window: the page list cannot be edited meanwhile.
    pub(super) fn is_held(&self) -> bool {
        self.popover.get() != Popover::None || self.in_cut_mode() || self.sheet.get()
    }

    /// Whether a write can start from the toolbar or the footer now.
    fn can_write(&self) -> bool {
        self.has_pages() && !self.is_busy() && !self.is_held() && self.drag.borrow().is_idle()
    }

    /// Dims what cannot be used now: the tools while a write runs, Save
    /// without pages or while the window is held.
    pub(super) fn update_locks(&self) {
        let busy = self.is_busy();
        let pages = self.has_pages();
        self.window.set_busy(busy);
        self.window
            .set_can_save(pages && !busy && !self.in_cut_mode() && !self.sheet.get());
        self.window
            .set_sheet_can_save(sheet_save_allowed(pages, busy));
    }

    /// The footer's text: cut mode's pieces, or the selection.
    pub(super) fn refresh_footer(&self) {
        let text = match &*self.cuts.borrow() {
            Some(cuts) => cut_footer(&cut_summary(self.pages.borrow().len(), cuts).0),
            None => super::footer_text(&self.selection.borrow().indices()),
        };
        self.window.set_footer(text.into());
    }

    /// The name of the file the first page comes from, which outputs are
    /// named after.
    fn anchor_name(&self) -> String {
        let sources = self.sources.borrow();
        self.pages
            .borrow()
            .pages()
            .first()
            .and_then(|page| sources.get(page.source))
            .map(file_name)
            .unwrap_or_default()
    }

    // Save and Extract.

    /// Save as new PDF (the footer's button, ⌘S): the whole grid as one new
    /// file next to the first page's file.
    pub(super) fn save(&self) {
        if !self.can_write() {
            return;
        }
        self.close_large();
        self.start_job(PdfJob::Save, After::Save);
    }

    /// Extract: the selected pages, in grid order, as one new file.
    pub(super) fn extract(&self) {
        if !self.can_write() {
            return;
        }
        let at = self.selection.borrow().indices();
        if at.is_empty() {
            return;
        }
        self.close_large();
        self.start_job(PdfJob::Extract(at), After::Write { cut: false });
    }

    // The popovers.

    /// Split…: opens its popover.
    pub(super) fn open_split(&self) {
        if !self.can_write() {
            return;
        }
        self.close_large();
        self.popover.set(Popover::Split);
        self.refresh_split();
        self.window.set_split_open(true);
    }

    /// Words the Split popover for the page count and N now.
    pub(super) fn refresh_split(&self) {
        let count = self.pages.borrow().len();
        let (low, high) = split_n_range(count);
        let n = self.split_n.get().clamp(low, high);
        self.split_n.set(n);
        let window = &self.window;
        window.set_split_heading(split_heading(&self.anchor_name()).into());
        window.set_split_pages(trn("{n} page", "{n} pages", count as u64).into());
        window.set_split_every_page_files(files_text(count).into());
        window.set_split_every_n(every_n_text(n).into());
        window.set_split_every_n_files(files_text(count.div_ceil(n)).into());
        window.set_split_can_less(n > low);
        window.set_split_can_more(n < high);
        let choice = self.split_choice.get();
        window.set_split_choice(choice.index());
        let confirm = if choice == SplitChoice::Cuts {
            tr("Place cuts")
        } else {
            tr("Split")
        };
        window.set_split_confirm(confirm.into());
    }

    /// A choice picked in the Split popover.
    pub(super) fn split_choose(&self, choice: i32) {
        if let Some(choice) = SplitChoice::from_index(choice) {
            self.split_choice.set(choice);
            self.refresh_split();
        }
    }

    /// N one less (`delta` -1) or more (+1); picks "Every N pages".
    pub(super) fn split_step(&self, delta: i32) {
        let n = self.split_n.get().saturating_add_signed(delta as isize);
        self.split_n.set(n);
        self.split_choice.set(SplitChoice::EveryN);
        self.refresh_split();
    }

    /// The Split popover's orange button: splits, or starts cut mode.
    pub(super) fn split_confirmed(&self) {
        if self.popover.get() != Popover::Split {
            return;
        }
        self.close_popover();
        match self.split_choice.get() {
            SplitChoice::EveryPage => {
                self.start_job(
                    PdfJob::Split(SplitMode::EveryPage),
                    After::Write { cut: false },
                );
            }
            SplitChoice::EveryN => {
                let mode = SplitMode::Every(self.split_n.get());
                self.start_job(PdfJob::Split(mode), After::Write { cut: false });
            }
            SplitChoice::Cuts => self.enter_cut_mode(),
        }
    }

    /// The pages To images writes: the selection, or every page (empty).
    fn image_positions(&self) -> Vec<usize> {
        self.selection.borrow().indices()
    }

    /// To images: opens its popover with the format and quality last used.
    pub(super) fn open_images(&self) {
        if !self.can_write() {
            return;
        }
        self.close_large();
        let settings = self.ctx.settings.current();
        self.image_opts.set(image_options(
            &settings.pdf_image_format,
            settings.pdf_image_dpi,
        ));
        self.popover.set(Popover::Images);
        self.refresh_images();
        self.window.set_images_open(true);
    }

    /// Words the To images popover for the pages and choices now.
    pub(super) fn refresh_images(&self) {
        let positions = self.image_positions();
        let n = if positions.is_empty() {
            self.pages.borrow().len()
        } else {
            positions.len()
        };
        let (heading, confirm) = images_texts(n);
        let taken: Vec<usize> = if positions.is_empty() {
            (0..n).collect()
        } else {
            positions
        };
        let opts = self.image_opts.get();
        let window = &self.window;
        window.set_images_heading(heading.into());
        window.set_images_pages(pages_text(&taken).into());
        window.set_images_confirm(confirm.into());
        window.set_images_jpeg(opts.kind == ImageKind::Jpeg);
        window.set_images_print(opts.dpi == 300);
    }

    /// JPEG (`true`) or PNG picked; kept in the settings for next time.
    pub(super) fn images_format(&self, jpeg: bool) {
        let opts = self.image_opts.get();
        let kind = if jpeg {
            ImageKind::Jpeg
        } else {
            ImageKind::Png
        };
        self.set_image_opts(ImageOptions { kind, ..opts });
    }

    /// Print (`true`, 300 DPI) or Screen (150 DPI) picked; kept in the
    /// settings for next time.
    pub(super) fn images_quality(&self, print: bool) {
        let opts = self.image_opts.get();
        let dpi = if print { 300 } else { 150 };
        self.set_image_opts(ImageOptions { dpi, ..opts });
    }

    fn set_image_opts(&self, opts: ImageOptions) {
        if opts == self.image_opts.get() {
            return;
        }
        self.image_opts.set(opts);
        self.refresh_images();
        // A change that cannot be saved still holds for this window.
        let saved = self.ctx.settings.update(|s| {
            s.pdf_image_format = format_key(opts.kind).to_owned();
            s.pdf_image_dpi = opts.dpi;
        });
        if let Err(e) = saved {
            warn!("cannot save the To images choice: {e}");
        }
    }

    /// The To images popover's orange button.
    pub(super) fn images_confirmed(&self) {
        if self.popover.get() != Popover::Images {
            return;
        }
        self.close_popover();
        let job = PdfJob::Images {
            positions: self.image_positions(),
            opts: self.image_opts.get(),
        };
        self.start_job(job, After::Write { cut: false });
    }

    /// Closes the popover open, if any.
    pub(super) fn close_popover(&self) {
        self.popover.set(Popover::None);
        self.window.set_split_open(false);
        self.window.set_images_open(false);
        self.add_waiting_soon();
    }

    /// The orange button of the popover open (Return).
    fn confirm_popover(&self) {
        match self.popover.get() {
            Popover::Split => self.split_confirmed(),
            Popover::Images => self.images_confirmed(),
            Popover::None => {}
        }
    }

    // Cut mode.

    /// Cut mode: the selection is hidden and a click in the gap between
    /// two pages places or removes a cut.
    fn enter_cut_mode(&self) {
        self.close_large();
        *self.cuts.borrow_mut() = Some(Vec::new());
        self.window.set_cut_mode(true);
        self.refresh_cuts();
        self.update_locks();
    }

    /// Leaves cut mode; its cuts are forgotten.
    pub(super) fn leave_cut_mode(&self) {
        if self.cuts.borrow_mut().take().is_none() {
            return;
        }
        self.window.set_cut_mode(false);
        self.window
            .set_cut_marks(ModelRc::new(VecModel::<PdfCutMark>::default()));
        self.refresh_footer();
        self.update_locks();
        self.add_waiting_soon();
    }

    /// A click at (`x`, `y`) in the grid's content in cut mode.
    pub(super) fn cut_clicked(&self, x: f32, y: f32) {
        let count = self.pages.borrow().len();
        let (gx, gy) = self.grid_point(x, y);
        let Some(cut) = cut_at(&self.metrics(), gx, gy, count) else {
            return;
        };
        match &mut *self.cuts.borrow_mut() {
            Some(cuts) => toggle_cut(cuts, cut),
            None => return,
        }
        self.refresh_cuts();
    }

    /// Draws the cuts and words the footer and its button for them; again
    /// after the grid reflows.
    pub(super) fn refresh_cuts(&self) {
        let Some(cuts) = self.cuts.borrow().clone() else {
            return;
        };
        let metrics = self.metrics();
        let count = self.pages.borrow().len();
        let left = self.grid_left.get();
        let marks: Vec<PdfCutMark> = cuts
            .iter()
            .map(|&cut| {
                let (x, y) = cut_marker(&metrics, cut, count);
                PdfCutMark { x: x + left, y }
            })
            .collect();
        self.window
            .set_cut_marks(ModelRc::new(VecModel::from(marks)));
        let (_, button) = cut_summary(count, &cuts);
        self.window.set_cut_button(button.into());
        self.refresh_footer();
    }

    /// Cut mode's orange button: splits at the cuts (none is one piece).
    pub(super) fn cut_confirmed(&self) {
        if self.is_busy() {
            return;
        }
        let Some(cuts) = self.cuts.borrow().clone() else {
            return;
        };
        self.start_job(
            PdfJob::Split(SplitMode::AtCuts(cuts)),
            After::Write { cut: true },
        );
    }

    // Running a write.

    /// Starts `job` over the grid as it stands; `after` says what follows.
    /// False when nothing started: another write runs here or elsewhere in
    /// vento (that one has already said "Busy").
    fn start_job(&self, job: PdfJob, after: After) -> bool {
        if self.is_busy() {
            return false;
        }
        let sources = self.sources.borrow().clone();
        let pages = self.pages.borrow().pages().to_vec();
        if pages.is_empty() {
            return false;
        }
        // A new write: what the last one made is no longer news.
        self.hide_note();
        let progress = Progress::new();
        *self.job.borrow_mut() = Some(Job {
            progress: progress.clone(),
            started: Instant::now(),
            closing: after == After::SaveAndClose,
        });
        self.update_locks();
        let this = self.this.clone();
        self.job_timer
            .start(TimerMode::Repeated, PROGRESS_TICK, move || {
                if let Some(view) = this.upgrade() {
                    view.job_tick();
                }
            });
        let this = self.this.clone();
        let written = pages.clone();
        let done: PdfDone = Box::new(move |outcome| {
            this.upgrade()
                .is_some_and(|view| view.job_done(outcome, after, &written))
        });
        debug!("PDF window {} writes {job:?}", self.id);
        // No borrow is held here: `done` may run before this returns.
        let started = self
            .ctx
            .executor
            .pdf_job(job, sources, pages, progress, self.centre(), done);
        if !started {
            self.job_over();
        }
        started
    }

    /// Shows the write's progress in the footer once it has run a while.
    fn job_tick(&self) {
        let (snapshot, elapsed) = match &*self.job.borrow() {
            Some(job) => (job.progress.snapshot(), job.started.elapsed()),
            None => return,
        };
        if elapsed < PROGRESS_DELAY {
            return;
        }
        let fraction = job_fraction(snapshot.done, snapshot.total);
        self.window.set_progress_measured(fraction.is_some());
        self.window.set_progress(fraction.unwrap_or(0.0));
        let text = if snapshot.current.is_empty() {
            tr("Writing…")
        } else {
            fill(&tr("Writing {name}…"), &[("name", &snapshot.current)])
        };
        self.window.set_progress_text(text.into());
        self.window.set_progress_shown(true);
    }

    /// Cancel beside the footer's progress: the write stops as soon as it
    /// can and leaves nothing behind.
    pub(super) fn cancel_job(&self) {
        if let Some(job) = &*self.job.borrow() {
            job.progress.cancel();
        }
    }

    /// The write has ended: the footer and the tools are back.
    fn job_over(&self) {
        self.job.borrow_mut().take();
        self.job_timer.stop();
        self.window.set_progress_shown(false);
        self.update_locks();
        self.add_waiting_soon();
    }

    /// How a write of the pages `written` ended. A save marks exactly what
    /// it wrote as saved; pages added while it ran keep the window open and
    /// edited. Returns whether the footer says what was made (the window
    /// stays open); otherwise the executor toasts it.
    fn job_done(&self, outcome: &PdfOutcome, after: After, written: &[PageRef]) -> bool {
        self.job_over();
        let made = (outcome.failed.is_none() && !outcome.cancelled)
            .then(|| outcome.made.first().map(|m| m.output.clone()))
            .flatten();
        debug!(
            "PDF window {} write ended: made {made:?}, cancelled {}",
            self.id, outcome.cancelled
        );
        let Some(made) = made else {
            // Failed or cancelled: the window stays as it was, the closing
            // question included.
            return false;
        };
        if self.closed.get() {
            return false;
        }
        match after {
            After::Save => {
                self.pages.borrow_mut().mark_saved_as(written);
                self.retext();
                if !self.closes_after_save(written) {
                    self.show_note(&pdf_toast_text(outcome), made);
                    return true;
                }
                self.saved.set(true);
                self.update_locks();
                let this = self.this.clone();
                self.close_timer
                    .start(TimerMode::SingleShot, SAVED_TIME, move || {
                        if let Some(view) = this.upgrade() {
                            view.close_now();
                        }
                    });
                false
            }
            After::SaveAndClose if !self.sheet.get() => {
                // Cancel came too late to stop the write: the file is made,
                // so the window is saved, but it stays open as asked.
                self.pages.borrow_mut().mark_saved_as(written);
                self.retext();
                self.show_note(&pdf_toast_text(outcome), made);
                true
            }
            After::SaveAndClose => {
                self.pages.borrow_mut().mark_saved_as(written);
                self.sheet.set(false);
                self.window.set_sheet_open(false);
                if self.closes_after_save(written) {
                    self.close_now();
                    return false;
                }
                // Pages came in while it wrote and are not in the file: the
                // window stays, edited, and whoever asked it to close (only
                // quitting asks) hears that it did not.
                self.update_locks();
                self.retext();
                self.show_note(&pdf_toast_text(outcome), made);
                self.add_waiting_soon();
                let answered = self.answer.borrow_mut().take();
                if let Some(answered) = answered {
                    answered(false);
                }
                true
            }
            After::Write { cut } => {
                if cut {
                    self.leave_cut_mode();
                }
                self.show_note(&pdf_toast_text(outcome), made);
                true
            }
        }
    }

    /// Whether a save that wrote `written` may close the window now.
    fn closes_after_save(&self, written: &[PageRef]) -> bool {
        closes_after_save(written, self.pages.borrow().pages(), self.is_adding())
    }

    /// Says `text` in the footer for a while, with Show in Finder for
    /// `made` and Undo for the write the executor just recorded.
    fn show_note(&self, text: &str, made: PathBuf) {
        let undo = self.ctx.executor.last_serial();
        *self.note.borrow_mut() = Some(Note { reveal: made, undo });
        let window = &self.window;
        window.set_note_text(text.into());
        window.set_note_can_undo(undo.is_some());
        window.set_note_left(1.0);
        window.set_note_shown(true);
        let this = self.this.clone();
        self.note_timer
            .start(TimerMode::Repeated, NOTE_TICK, move || {
                if let Some(view) = this.upgrade() {
                    view.note_tick();
                }
            });
    }

    /// Shortens the footer's bar, unless the pointer rests on the footer;
    /// at its end the footer is back.
    fn note_tick(&self) {
        if self.window.get_note_hovered() {
            return;
        }
        let left = self.window.get_note_left()
            - NOTE_TICK.as_secs_f32() / NOTE_TIME.as_secs_f32();
        if left <= 0.0 {
            self.hide_note();
        } else {
            self.window.set_note_left(left);
        }
    }

    pub(super) fn hide_note(&self) {
        self.note_timer.stop();
        self.note.borrow_mut().take();
        self.window.set_note_shown(false);
    }

    /// The footer's Show in Finder.
    pub(super) fn note_reveal(&self) {
        let reveal = self.note.borrow().as_ref().map(|n| n.reveal.clone());
        if let Some(path) = reveal {
            self.ctx.platform.reveal_in_finder(&[path]);
        }
    }

    /// The footer's Undo: undoes the write if nothing has been done (or
    /// undone) since, and says no more about it either way.
    pub(super) fn note_undo(&self) {
        let serial = self.note.borrow().as_ref().and_then(|n| n.undo);
        self.hide_note();
        if let Some(serial) = serial {
            self.ctx.executor.undo_if_last(serial);
        }
    }

    /// The banner's Undo (after pages were deleted).
    pub(super) fn banner_clicked(&self) {
        self.undo();
    }

    // Closing.

    /// Asks to close: with unsaved edits the closing question shows first.
    pub(super) fn ask_close(&self, answered: Option<Answered>) {
        if self.closed.get() {
            if let Some(answered) = answered {
                answered(true);
            }
            return;
        }
        if !self.is_edited() || self.saved.get() {
            // `close_now` tells `answered`.
            if let Some(answered) = answered {
                replace_answer(&self.answer, answered);
            }
            self.close_now();
            return;
        }
        if let Some(answered) = answered {
            // An earlier asker (only quitting asks) gives way to this one.
            replace_answer(&self.answer, answered);
        }
        if !self.sheet.get() {
            self.close_popover();
            self.leave_cut_mode();
            self.close_large();
            self.end_drag();
            self.sheet.set(true);
            self.window.set_sheet_open(true);
            self.update_locks();
        }
        self.bring_forward();
    }

    /// Brings vento and this window to the front with the keyboard.
    fn bring_forward(&self) {
        self.ctx.platform.activate_app();
        self.window.window().with_winit_window(|w| w.focus_window());
    }

    /// The closing question's Save: closes once the new file is written; a
    /// failed or cancelled write keeps the window and the question.
    pub(super) fn sheet_save(&self) {
        if !self.sheet.get() || !sheet_save_allowed(self.has_pages(), self.is_busy()) {
            return;
        }
        self.start_job(PdfJob::Save, After::SaveAndClose);
    }

    /// The closing question's Discard: closes without writing. A write that
    /// runs for another reason (Extract, Split…) keeps going on its own; the
    /// sheet's own Save is stopped.
    pub(super) fn sheet_discard(&self) {
        if !self.sheet.get() {
            return;
        }
        self.stop_closing_save();
        self.sheet.set(false);
        self.window.set_sheet_open(false);
        self.close_now();
    }

    /// The closing question's Cancel: the window stays open. It works while a
    /// write runs; the sheet's own Save is stopped, any other write goes on.
    pub(super) fn sheet_cancel(&self) {
        if !self.sheet.get() {
            return;
        }
        self.stop_closing_save();
        self.sheet.set(false);
        self.window.set_sheet_open(false);
        self.update_locks();
        self.add_waiting_soon();
        let answered = self.answer.borrow_mut().take();
        if let Some(answered) = answered {
            answered(false);
        }
    }

    /// Stops the write that the closing question's Save started, if it runs.
    fn stop_closing_save(&self) {
        if let Some(job) = &*self.job.borrow()
            && job.closing
        {
            job.progress.cancel();
        }
    }

    /// Keys while the closing question, a popover or cut mode holds the
    /// window: Escape backs out, Return does the orange button. `None` when
    /// none holds it; otherwise whether the key was used.
    pub(super) fn held_key(&self, escape: bool, enter: bool) -> Option<bool> {
        if self.sheet.get() {
            if escape {
                self.sheet_cancel();
            } else if enter {
                self.sheet_save();
            }
            return Some(escape || enter);
        }
        if self.popover.get() != Popover::None {
            if escape {
                self.close_popover();
            } else if enter {
                self.confirm_popover();
            }
            return Some(escape || enter);
        }
        if self.in_cut_mode() {
            if escape {
                self.leave_cut_mode();
            } else if enter {
                self.cut_confirmed();
            }
            return Some(escape || enter);
        }
        None
    }
}
