//! One PDF Pages window: the pages of the PDFs from one drop as a grid of
//! thumbnails, with selection, the keyboard, the large view of a page, and
//! the page list's edits: delete, drag to reorder, rotate, add PDFs (the
//! open panel or files dropped on the window) and in-window undo, and
//! writing: Save as new PDF, Extract, Split, To images, and the question
//! about unsaved edits on closing.
//!
//! The view owns its Slint window, its page list, its thumbnail thread and
//! its image cache; the controller drops the view when the window closes,
//! which frees all of them.
//!
//! For the editing and writing tasks:
//! - the page list is [`PdfView::pages`] (`RefCell<PageList>`); after
//!   changing it call [`PdfView::pages_changed`], which rebuilds the Slint
//!   rows, starts a new thumbnail layout (bumping the generation, so older
//!   results are dropped) and retitles the window;
//! - every close goes through [`PdfView::request_close`] (or
//!   [`PdfView::close_asking`] when quitting);
//! - keys arrive in [`PdfView::key`];
//! - whether there is anything to save is [`PdfView::has_pages`].

mod edit;
mod write;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, warn};
use slint::platform::Key;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalPosition, LogicalSize, Model, ModelRc,
    SharedString, Timer, VecModel,
};
use vento_core::geometry::{Point, screen_for};
use vento_core::i18n::{self, fill, tr};
use vento_core::pdf::{
    GridMetrics, ImageKind, ImageOptions, PageList, PageRef, PdfError, Selection, Source,
};
use vento_platform::Platform;

use super::logic::{
    Arrow, DragEvent, footer_text, moved_focus, source_letter, sources_in_use, tint_of, title_text,
};
use super::thumbs::{self, LARGE, Rendered, ThumbState, ThumbWorker};
use crate::appearance;
use crate::exec::Executor;
use crate::settings_hub::SettingsHub;
use crate::settings_window::centred_on;
use crate::toast::Toaster;
use crate::ui::{PdfPage, PdfSourceTag, PdfWindow, Theme};
use edit::Drag;
use write::{BannerAction, Job, Popover, SplitChoice};

/// A grid cell, in logical points: the page box plus the number under it
/// (matches `PageCell` in `pdf.slint`: 6 pt inset, 20 pt label).
const CELL_W: f32 = 128.0;
const CELL_H: f32 = 184.0;
/// Space between cells, and between the grid's edge and the cells.
const GAP: f32 = 14.0;
const PAD: f32 = 16.0;
/// The longer side of a thumbnail, in points: the page box's height.
const THUMB_PT: f32 = 152.0;
/// Pages either side of the visible ones whose thumbnails are kept.
const MARGIN: usize = 24;
/// Thumbnails asked of the thread at a time.
const MAX_IN_FLIGHT: usize = 3;
/// The window's size when it opens (matches `pdf.slint`), and the room
/// kept around it on a smaller screen.
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 660.0;
const SCREEN_ROOM: f64 = 40.0;
/// How long the "pages removed" banner stays.
const BANNER_TIME: Duration = Duration::from_secs(5);
/// How often the grid scrolls while a drag is near its edge (a frame).
const SCROLL_TICK: Duration = Duration::from_millis(16);
/// winit delivers one `DroppedFile` per file; files arriving within this
/// long of each other are one drop.
const DROP_BATCH: Duration = Duration::from_millis(30);

thread_local! {
    /// The open views by id, for thumbnails coming back from their thread.
    static VIEWS: RefCell<HashMap<u64, Weak<PdfView>>> = RefCell::new(HashMap::new());
    /// The id the next view gets.
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

/// The view with `id`, if it is still open.
fn live_view(id: u64) -> Option<Rc<PdfView>> {
    VIEWS.with(|views| views.borrow().get(&id).and_then(Weak::upgrade))
}

pub use super::logic::Answered;

/// Added files once read: the PDFs opened and the paths skipped, with why.
type Added = (Vec<Source>, Vec<(PathBuf, PdfError)>);

/// What a view needs from the app.
pub struct ViewCtx {
    pub platform: Rc<dyn Platform>,
    /// Says which added files could not be opened.
    pub toaster: Rc<Toaster>,
    /// Runs the write jobs.
    pub executor: Rc<Executor>,
    /// Keeps To images' format and quality.
    pub settings: Rc<SettingsHub>,
    /// Told the view's id once its window has closed; the controller then
    /// drops the view.
    pub closed: Rc<dyn Fn(u64)>,
}

/// One PDF Pages window.
pub struct PdfView {
    id: u64,
    window: PdfWindow,
    /// The opened files, in the order they were opened (added files go
    /// at the end, so a page's source index never changes). Their bytes are
    /// the snapshot every thumbnail and write works from.
    pub sources: RefCell<Arc<[Source]>>,
    /// The grid's pages, with in-window undo. Call
    /// [`pages_changed`](Self::pages_changed) after changing it.
    pub pages: RefCell<PageList>,
    pub selection: RefCell<Selection>,
    /// The page the arrow keys move from.
    focus: Cell<Option<usize>>,
    rows: Rc<VecModel<PdfPage>>,
    thumbs: RefCell<ThumbState<Image>>,
    /// Draws the thumbnails; started again when files are added.
    worker: RefCell<ThumbWorker>,
    metrics: Cell<GridMetrics>,
    /// The left edge of the columns in the grid area (they are centred).
    grid_left: Cell<f32>,
    /// The page shown large, and the id of its latest drawing.
    large: Cell<Option<usize>>,
    large_seq: Cell<u64>,
    /// The longer side, in pixels, the large page was last asked for.
    large_px: Cell<u32>,
    /// A drag of pages, and the timer that scrolls the grid during it.
    drag: RefCell<Drag>,
    scroll_timer: Timer,
    /// Hides the banner, and what its button does.
    banner_timer: Timer,
    banner_action: RefCell<BannerAction>,
    /// The write running, and the timer that shows its progress.
    job: RefCell<Option<Job>>,
    job_timer: Timer,
    /// Saved: the window closes when this fires, and nothing more can be
    /// done in it meanwhile.
    saved: Cell<bool>,
    close_timer: Timer,
    /// The popover open, the Split popover's choice and N, and To images'
    /// format and quality.
    popover: Cell<Popover>,
    split_choice: Cell<SplitChoice>,
    split_n: Cell<usize>,
    image_opts: Cell<ImageOptions>,
    /// Cut mode's cuts, while it is on (insertion indices, sorted).
    cuts: RefCell<Option<Vec<usize>>>,
    /// The closing question shows, and who waits for its answer (quitting):
    /// told `true` once the window has closed, `false` on Cancel.
    sheet: Cell<bool>,
    answer: RefCell<Option<Answered>>,
    /// Added files: how many reads are under way, and those read that
    /// wait for a write or a hold of the window to end.
    reading: Cell<usize>,
    waiting: RefCell<Vec<Added>>,
    /// Files dropped on the window, collected until the drop is whole.
    dropped: RefCell<Vec<PathBuf>>,
    drop_timer: Timer,
    closed: Cell<bool>,
    ctx: ViewCtx,
    this: Weak<Self>,
}

impl PdfView {
    /// Opens a window on `sources` (at least one, each with pages),
    /// centred on the screen holding `at`, and starts its thumbnails.
    pub fn new(sources: Vec<Source>, at: Point, ctx: ViewCtx) -> Rc<PdfView> {
        let id = NEXT_ID.with(|next| next.replace(next.get() + 1));
        let window = PdfWindow::new().expect("cannot create the PDF window");
        let pages: Vec<PageRef> = sources
            .iter()
            .enumerate()
            .flat_map(|(source, s)| {
                (0..s.page_count).map(move |page| PageRef {
                    source,
                    page,
                    turns: 0,
                })
            })
            .collect();
        let count = pages.len();
        let bytes: Vec<Arc<[u8]>> = sources.iter().map(|s| s.bytes.clone()).collect();
        let worker = spawn_worker(id, bytes);
        let rtl = i18n::current().rtl;
        let view = Rc::new_cyclic(|this| PdfView {
            id,
            window,
            sources: RefCell::new(Arc::from(sources)),
            pages: RefCell::new(PageList::new(pages)),
            selection: RefCell::new(Selection::default()),
            focus: Cell::new(None),
            rows: Rc::new(VecModel::default()),
            thumbs: RefCell::new(ThumbState::new(count, MARGIN, MAX_IN_FLIGHT)),
            worker: RefCell::new(worker),
            metrics: Cell::new(GridMetrics::fit(
                WIDTH as f32,
                CELL_W,
                CELL_H,
                GAP,
                PAD,
                rtl,
            )),
            grid_left: Cell::new(0.0),
            large: Cell::new(None),
            large_seq: Cell::new(0),
            large_px: Cell::new(0),
            drag: RefCell::new(Drag::Idle),
            scroll_timer: Timer::default(),
            banner_timer: Timer::default(),
            banner_action: RefCell::new(BannerAction::Undo),
            job: RefCell::new(None),
            job_timer: Timer::default(),
            saved: Cell::new(false),
            close_timer: Timer::default(),
            popover: Cell::new(Popover::None),
            split_choice: Cell::new(SplitChoice::EveryPage),
            split_n: Cell::new(2),
            image_opts: Cell::new(ImageOptions {
                kind: ImageKind::Png,
                dpi: 300,
            }),
            cuts: RefCell::new(None),
            sheet: Cell::new(false),
            answer: RefCell::new(None),
            reading: Cell::new(0),
            waiting: RefCell::new(Vec::new()),
            dropped: RefCell::new(Vec::new()),
            drop_timer: Timer::default(),
            closed: Cell::new(false),
            ctx,
            this: this.clone(),
        });
        VIEWS.with(|views| views.borrow_mut().insert(id, Rc::downgrade(&view)));
        view.set_up();
        view.show(at);
        view
    }

    /// The view's id, as `ViewCtx::closed` reports it.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Whether the page list has changes not saved yet.
    pub fn is_edited(&self) -> bool {
        self.pages.borrow().is_edited()
    }

    /// Whether the grid has any page: Save as new PDF is dimmed without.
    pub fn has_pages(&self) -> bool {
        !self.pages.borrow().is_empty()
    }

    /// Whether the window has closed.
    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    /// Fills the window and connects it.
    fn set_up(&self) {
        let window = &self.window;
        window.set_cell_width(CELL_W);
        window.set_cell_height(CELL_H);
        window.set_gap(GAP);
        window.set_pad(PAD);
        window.set_pages(ModelRc::from(self.rows.clone()));
        self.connect();
        appearance::follow(window, appearance::Look::Full);
        appearance::follow_language(self.this.clone(), |view| view.retext());
        self.pages_changed();
    }

    /// The sources the window names (title, legend, letters and tints):
    /// those the pages use, or every source when no page is left. A file
    /// whose pages were all deleted, or whose adding was undone, stays
    /// opened for undo and redo but is not named.
    fn shown_sources(&self) -> Vec<usize> {
        let count = self.sources.borrow().len();
        let used = sources_in_use(self.pages.borrow().pages(), count);
        if used.is_empty() {
            (0..count).collect()
        } else {
            used
        }
    }

    /// Names the shown files in the toolbar when there are several.
    fn set_legend(&self, shown: &[usize]) {
        let legend: Vec<PdfSourceTag> = if shown.len() > 1 {
            let sources = self.sources.borrow();
            shown
                .iter()
                .filter_map(|&i| {
                    Some(PdfSourceTag {
                        letter: source_letter(i).into(),
                        name: file_name(sources.get(i)?).into(),
                        tint: tint_of(i, shown.len()),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        self.window.set_legend(ModelRc::new(VecModel::from(legend)));
    }

    fn connect(&self) {
        let window = &self.window;
        let this = self.this.clone();
        let with = move |f: &dyn Fn(&PdfView)| {
            if let Some(view) = this.upgrade() {
                f(&view);
            }
        };
        {
            let with = with.clone();
            window.on_grid_changed(move || with(&|v| v.layout_grid()));
        }
        {
            let with = with.clone();
            window.on_page_clicked(move |i, command, shift| {
                with(&|v| v.page_clicked(i, command, shift));
            });
        }
        {
            let with = with.clone();
            window.on_page_opened(move |i| with(&|v| v.open_large(i)));
        }
        {
            let with = with.clone();
            window.on_background_clicked(move || with(&|v| v.background_clicked()));
        }
        {
            let with = with.clone();
            window.on_large_closed(move || with(&|v| v.close_large()));
        }
        {
            let with = with.clone();
            window.on_select_all(move || with(&|v| v.select_all()));
        }
        {
            let with = with.clone();
            window.on_page_moved(move |i, left, dx, dy, x, y| {
                with(&|v| v.drag_event(i, DragEvent::Moved { left, dx, dy }, (x, y)));
            });
        }
        {
            let with = with.clone();
            window.on_page_released(move |i, left| {
                with(&|v| v.drag_event(i, DragEvent::Up { left }, (0.0, 0.0)));
            });
        }
        {
            let with = with.clone();
            window.on_page_cancelled(move |i| {
                with(&|v| v.drag_event(i, DragEvent::Cancel, (0.0, 0.0)));
            });
        }
        // The toolbar's page tools also act from the large view: it closes
        // first, as the edit may move its page.
        {
            let with = with.clone();
            window.on_delete_pages(move || {
                with(&|v| {
                    v.close_large();
                    v.delete_selected();
                });
            });
        }
        {
            let with = with.clone();
            window.on_rotate(move |clockwise| {
                with(&|v| {
                    v.close_large();
                    v.rotate_selected(clockwise);
                });
            });
        }
        {
            let with = with.clone();
            window.on_add_pdf(move || {
                with(&|v| {
                    v.close_large();
                    v.add_pdf();
                });
            });
        }
        {
            let with = with.clone();
            window.on_banner_clicked(move || with(&|v| v.banner_clicked()));
        }
        {
            let with = with.clone();
            window.on_save(move || with(&|v| v.save()));
        }
        {
            let with = with.clone();
            window.on_extract(move || with(&|v| v.extract()));
        }
        {
            let with = with.clone();
            window.on_split(move || with(&|v| v.open_split()));
        }
        {
            let with = with.clone();
            window.on_to_images(move || with(&|v| v.open_images()));
        }
        {
            let with = with.clone();
            window.on_cancel_job(move || with(&|v| v.cancel_job()));
        }
        {
            let with = with.clone();
            window.on_split_choose(move |choice| with(&|v| v.split_choose(choice)));
        }
        {
            let with = with.clone();
            window.on_split_step(move |delta| with(&|v| v.split_step(delta)));
        }
        {
            let with = with.clone();
            window.on_split_confirmed(move || with(&|v| v.split_confirmed()));
        }
        {
            let with = with.clone();
            window.on_images_format(move |jpeg| with(&|v| v.images_format(jpeg)));
        }
        {
            let with = with.clone();
            window.on_images_quality(move |print| with(&|v| v.images_quality(print)));
        }
        {
            let with = with.clone();
            window.on_images_confirmed(move || with(&|v| v.images_confirmed()));
        }
        {
            let with = with.clone();
            window.on_popover_cancel(move || with(&|v| v.close_popover()));
        }
        {
            let with = with.clone();
            window.on_cut_clicked(move |x, y| with(&|v| v.cut_clicked(x, y)));
        }
        {
            let with = with.clone();
            window.on_cut_confirmed(move || with(&|v| v.cut_confirmed()));
        }
        {
            let with = with.clone();
            window.on_cut_cancel(move || with(&|v| v.leave_cut_mode()));
        }
        {
            let with = with.clone();
            window.on_sheet_save(move || with(&|v| v.sheet_save()));
        }
        {
            let with = with.clone();
            window.on_sheet_discard(move || with(&|v| v.sheet_discard()));
        }
        {
            let with = with.clone();
            window.on_sheet_cancel(move || with(&|v| v.sheet_cancel()));
        }
        let this = self.this.clone();
        window.on_key(move |text, command, shift| {
            this.upgrade()
                .is_some_and(|view| view.key(&text, command, shift))
        });
        // The red button and ⌘W from the system.
        let this = self.this.clone();
        window.window().on_close_requested(move || {
            if let Some(view) = this.upgrade() {
                view.request_close();
            }
            CloseRequestResponse::KeepWindowShown
        });
        // Files dropped on the window, one event per file.
        let this = self.this.clone();
        window.window().on_winit_window_event(move |_, event| {
            if let WindowEvent::DroppedFile(path) = event
                && let Some(view) = this.upgrade()
            {
                view.file_dropped(path.clone());
            }
            EventResult::Propagate
        });
    }

    /// Sizes and centres the window on the screen holding `at`, brings
    /// vento forward and shows it.
    fn show(&self, at: Point) {
        let screens = self.ctx.platform.screens();
        let (w, h) = match screen_for(at, &screens) {
            Some(screen) => {
                let w = WIDTH.min(screen.w - SCREEN_ROOM).max(1.0);
                let h = HEIGHT.min(screen.h - SCREEN_ROOM).max(1.0);
                let (x, y) = centred_on(screen, w, h);
                self.window
                    .window()
                    .set_position(LogicalPosition::new(x as f32, y as f32));
                (w, h)
            }
            None => (WIDTH, HEIGHT),
        };
        self.window
            .window()
            .set_size(LogicalSize::new(w as f32, h as f32));
        // vento runs as an accessory app: bring it forward so the window
        // comes to the front and takes the keyboard.
        self.ctx.platform.activate_app();
        if let Err(e) = self.window.show() {
            warn!("cannot show the PDF window: {e}");
            self.close_now();
            return;
        }
        // The native window exists from the end of this turn of the event
        // loop: then it can take the focus and the title bar's look.
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            let Some(view) = this.upgrade() else { return };
            let theme = appearance::title_bar_theme(
                appearance::current(),
                view.window.global::<Theme>().get_dark(),
            );
            view.window.window().with_winit_window(|w| {
                w.set_theme(theme);
                w.focus_window();
            });
            view.layout_grid();
        });
        debug!("PDF window {} open", self.id);
    }

    /// Asks to close the window: every way of closing it (the red button,
    /// ⌘W, Escape with nothing selected) comes here. With unsaved edits it
    /// asks first: "Save your changes as a new PDF?" (Save / Discard /
    /// Cancel); without, it closes at once.
    pub fn request_close(&self) {
        self.ask_close(None);
    }

    /// Like [`request_close`](Self::request_close), for quitting: brings
    /// the window forward with its question when it has unsaved edits, then
    /// tells `answered` `true` once it has closed, or `false` on Cancel.
    pub fn close_asking(&self, answered: Answered) {
        self.ask_close(Some(answered));
    }

    /// Hides the window and tells the controller, which drops the view on
    /// the next turn of the event loop; whoever waits for the window to
    /// close is told. A write still running goes on and toasts its result.
    pub fn close_now(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.scroll_timer.stop();
        self.banner_timer.stop();
        self.drop_timer.stop();
        self.job_timer.stop();
        self.close_timer.stop();
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the PDF window: {e}");
        }
        (self.ctx.closed)(self.id);
        debug!("PDF window {} closed", self.id);
        let answered = self.answer.borrow_mut().take();
        if let Some(answered) = answered {
            answered(true);
        }
    }

    /// Takes a change to [`pages`](Self::pages): rebuilds the grid's rows,
    /// starts a new thumbnail layout (results asked before it are dropped;
    /// cached images come back at once), keeps the selection and focus
    /// within the list, and retitles the window. A drag under way ends
    /// without moving anything.
    pub fn pages_changed(&self) {
        self.end_drag();
        // Cuts are positions in the list that was.
        self.leave_cut_mode();
        let pages = self.pages.borrow().pages().to_vec();
        let shown = self.shown_sources();
        self.set_legend(&shown);
        let several = shown.len();
        let rows: Vec<PdfPage> = pages
            .iter()
            .enumerate()
            .map(|(i, page)| blank_row(i, *page, several))
            .collect();
        self.rows.set_vec(rows);
        self.thumbs.borrow_mut().relayout(&pages);
        {
            let mut selection = self.selection.borrow_mut();
            let kept: Vec<usize> = selection
                .indices()
                .into_iter()
                .filter(|&i| i < pages.len())
                .collect();
            if kept.len() != selection.indices().len() {
                selection.set(kept);
            }
        }
        if let Some(focus) = self.focus.get()
            && focus >= pages.len()
        {
            self.set_focus(pages.len().checked_sub(1), self.window.get_show_focus());
        }
        if self.large.get().is_some_and(|i| i >= pages.len()) {
            self.close_large();
        }
        self.window.set_has_pages(!pages.is_empty());
        self.update_locks();
        self.selection_changed();
        self.retext();
        self.pump();
    }

    /// Writes the window's text again: on open, after an edit and after a
    /// language change (which may also change the reading direction).
    fn retext(&self) {
        let names: Vec<String> = {
            let sources = self.sources.borrow();
            self.shown_sources()
                .into_iter()
                .filter_map(|i| sources.get(i).map(file_name))
                .collect()
        };
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (names, meta) = title_text(&names, self.pages.borrow().len(), self.is_edited());
        let heading = fill(
            &tr("{names} — {meta}"),
            &[("names", &names), ("meta", &meta)],
        );
        self.window.set_heading(heading.into());
        self.refresh_footer();
        self.refresh_cuts();
        match self.popover.get() {
            Popover::Split => self.refresh_split(),
            Popover::Images => self.refresh_images(),
            Popover::None => {}
        }
        let rtl = i18n::current().rtl;
        if self.metrics.get().rtl != rtl {
            self.layout_grid();
        }
    }

    /// Fits the columns to the grid area's width, then asks for the
    /// thumbnails now on screen. Runs on every resize and scroll.
    fn layout_grid(&self) {
        let width = self.window.get_grid_width();
        if width <= 0.0 {
            return;
        }
        let metrics = GridMetrics::fit(width, CELL_W, CELL_H, GAP, PAD, i18n::current().rtl);
        let left = ((width - metrics.content_width()) / 2.0).max(0.0).floor();
        self.metrics.set(metrics);
        self.grid_left.set(left);
        self.window.set_columns(metrics.columns as i32);
        self.window.set_grid_left(left);
        self.refresh_cuts();
        self.pump();
        if self.large.get().is_some() && self.large_wanted_px() > self.large_px.get() * 5 / 4 {
            self.request_large();
        }
    }

    /// A point in the grid area (window points, scroll included) in the
    /// grid's own content space, as `GridMetrics::index_at` and
    /// `insertion_index` take it: the columns are centred in the area.
    pub fn grid_point(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.grid_left.get(), y)
    }

    /// The current grid layout.
    pub fn metrics(&self) -> GridMetrics {
        self.metrics.get()
    }

    /// The pages on screen now.
    fn visible(&self) -> Range<usize> {
        let scroll = -self.window.get_content_y();
        let height = self.window.get_grid_height();
        self.metrics
            .get()
            .rows_visible(scroll, height, self.rows.row_count())
    }

    /// Shows cached thumbnails, frees far ones and asks for the rest.
    fn pump(&self) {
        let visible = self.visible();
        let plan = {
            let pages = self.pages.borrow();
            self.thumbs.borrow_mut().update(pages.pages(), visible)
        };
        self.apply(plan);
    }

    /// Carries out a thumbnail plan on the rows and the worker.
    fn apply(&self, plan: thumbs::Plan<Image>) {
        for i in plan.clear {
            if let Some(mut row) = self.rows.row_data(i) {
                row.image = Image::default();
                row.rendered = false;
                row.broken = false;
                self.rows.set_row_data(i, row);
            }
        }
        for (i, image) in plan.fill {
            if let Some(mut row) = self.rows.row_data(i) {
                row.broken = image.is_none();
                row.image = image.unwrap_or_default();
                row.rendered = true;
                self.rows.set_row_data(i, row);
            }
        }
        let max_px = self.pixels(THUMB_PT);
        for (ticket, page) in plan.send {
            self.worker.borrow().request(ticket, page, max_px);
        }
    }

    /// A drawing back from the thumbnail thread.
    fn thumb_arrived(&self, ticket: u64, page: PageRef, drawn: Rendered) {
        if ticket & LARGE != 0 {
            self.large_arrived(ticket, drawn);
            return;
        }
        let image = drawn.map(Image::from_rgba8);
        let plan = {
            let pages = self.pages.borrow();
            self.thumbs
                .borrow_mut()
                .arrived(ticket, page, image, pages.pages())
        };
        self.apply(plan);
    }

    /// `points` in the window's pixels.
    fn pixels(&self, points: f32) -> u32 {
        (points * self.window.window().scale_factor())
            .ceil()
            .max(1.0) as u32
    }

    fn page_clicked(&self, i: i32, command: bool, shift: bool) {
        // The release that ends a drag is not a click.
        if !self.drag.borrow().is_idle() {
            return;
        }
        let Ok(i) = usize::try_from(i) else { return };
        if i >= self.rows.row_count() {
            return;
        }
        {
            let mut selection = self.selection.borrow_mut();
            if shift {
                selection.extend_to(i);
            } else if command {
                selection.toggle(i);
            } else {
                selection.click(i);
            }
        }
        self.set_focus(Some(i), false);
        self.selection_changed();
    }

    fn background_clicked(&self) {
        self.selection.borrow_mut().clear();
        self.selection_changed();
    }

    fn select_all(&self) {
        let len = self.rows.row_count();
        self.selection.borrow_mut().select_all(len);
        self.selection_changed();
    }

    /// Marks the selected rows and words the footer.
    fn selection_changed(&self) {
        let selection = self.selection.borrow();
        for i in 0..self.rows.row_count() {
            if let Some(mut row) = self.rows.row_data(i) {
                let selected = selection.contains(i);
                if row.selected != selected {
                    row.selected = selected;
                    self.rows.set_row_data(i, row);
                }
            }
        }
        self.window.set_has_selection(!selection.is_empty());
        drop(selection);
        self.refresh_footer();
    }

    fn set_focus(&self, focus: Option<usize>, shown: bool) {
        self.focus.set(focus);
        self.window
            .set_focus_index(focus.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1));
        self.window.set_show_focus(shown && focus.is_some());
    }

    /// A key pressed in the window, with ⌘ (`command`) and Shift. Returns
    /// whether it was used. While a page is shown large only ⌘W and Escape
    /// act; while pages are dragged only Escape (which cancels the drag).
    pub fn key(&self, text: &str, command: bool, shift: bool) -> bool {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return false;
        };
        if command && matches!(c, 'w' | 'W') {
            self.request_close();
            return true;
        }
        let escape = c == char::from(Key::Escape);
        if let Some(used) = self.held_key(escape, c == char::from(Key::Return)) {
            return used;
        }
        if command && matches!(c, 's' | 'S') {
            if self.drag.borrow().is_idle() {
                self.save();
            }
            return true;
        }
        if escape {
            if !self.drag.borrow().is_idle() {
                self.drag_event(-1, DragEvent::Escape, (0.0, 0.0));
            } else if self.large.get().is_some() {
                self.close_large();
            } else if !self.selection.borrow().is_empty() {
                self.background_clicked();
            } else {
                self.request_close();
            }
            return true;
        }
        if self.large.get().is_some() || !self.drag.borrow().is_idle() {
            return false;
        }
        // Delete and Backspace (the Mac's delete key), with or without ⌘.
        if c == char::from(Key::Delete) || c == char::from(Key::Backspace) {
            self.delete_selected();
            return true;
        }
        if command {
            return match c {
                'a' | 'A' => {
                    self.select_all();
                    true
                }
                'z' | 'Z' if shift => {
                    self.redo();
                    true
                }
                'z' | 'Z' => {
                    self.undo();
                    true
                }
                '[' => {
                    self.rotate_selected(false);
                    true
                }
                ']' => {
                    self.rotate_selected(true);
                    true
                }
                _ => false,
            };
        }
        let arrow = match c {
            _ if c == char::from(Key::LeftArrow) => Some(Arrow::Left),
            _ if c == char::from(Key::RightArrow) => Some(Arrow::Right),
            _ if c == char::from(Key::UpArrow) => Some(Arrow::Up),
            _ if c == char::from(Key::DownArrow) => Some(Arrow::Down),
            _ => None,
        };
        if let Some(arrow) = arrow {
            let metrics = self.metrics.get();
            let focus = moved_focus(
                self.focus.get(),
                arrow,
                metrics.columns,
                self.rows.row_count(),
                metrics.rtl,
            );
            self.set_focus(focus, true);
            if let Some(i) = focus {
                self.scroll_to(i);
            }
            return true;
        }
        if c == ' ' {
            if let Some(i) = self.focus.get() {
                self.selection.borrow_mut().toggle(i);
                self.set_focus(Some(i), true);
                self.selection_changed();
            }
            return true;
        }
        false
    }

    /// Scrolls the grid just enough to show page `i`.
    fn scroll_to(&self, i: usize) {
        let metrics = self.metrics.get();
        let row = (i / metrics.columns.max(1)) as f32;
        let top = metrics.pad + row * (metrics.cell_h + metrics.gap);
        let bottom = top + metrics.cell_h;
        let scroll = -self.window.get_content_y();
        let height = self.window.get_grid_height();
        let target = if top - metrics.pad < scroll {
            (top - metrics.pad).max(0.0)
        } else if bottom + metrics.pad > scroll + height {
            bottom + metrics.pad - height
        } else {
            return;
        };
        self.window.set_content_y(-target);
        self.pump();
    }

    /// Shows page `i` large over the grid: its thumbnail at once, then a
    /// drawing for the window's size.
    fn open_large(&self, i: i32) {
        if !self.drag.borrow().is_idle() {
            return;
        }
        let Ok(i) = usize::try_from(i) else { return };
        let Some(row) = self.rows.row_data(i) else {
            return;
        };
        self.large.set(Some(i));
        self.window.set_large_image(row.image);
        self.window.set_large_open(true);
        self.request_large();
    }

    /// The longer side, in pixels, the large page needs now.
    fn large_wanted_px(&self) -> u32 {
        let side = self
            .window
            .get_grid_width()
            .max(self.window.get_grid_height());
        self.pixels(side)
    }

    /// Asks for the large page drawn for the window's size now.
    fn request_large(&self) {
        let Some(i) = self.large.get() else { return };
        let Some(page) = self.pages.borrow().pages().get(i).copied() else {
            return;
        };
        let seq = self.large_seq.get().wrapping_add(1) & !LARGE;
        self.large_seq.set(seq);
        let px = self.large_wanted_px();
        self.large_px.set(px);
        self.worker.borrow().request(LARGE | seq, page, px);
    }

    fn large_arrived(&self, ticket: u64, drawn: Rendered) {
        if self.large.get().is_none() || ticket != LARGE | self.large_seq.get() {
            return;
        }
        if let Some(pixels) = drawn {
            self.window.set_large_image(Image::from_rgba8(pixels));
        }
    }

    /// Back to the grid; the large drawing is freed.
    fn close_large(&self) {
        self.large.set(None);
        self.large_px.set(0);
        self.window.set_large_open(false);
        self.window.set_large_image(Image::default());
    }
}

/// Starts the thumbnail thread of view `id` on its sources' bytes.
fn spawn_worker(id: u64, bytes: Vec<Arc<[u8]>>) -> ThumbWorker {
    thumbs::spawn(bytes, move |ticket, page, drawn| {
        deliver(id, ticket, page, drawn)
    })
}

impl Drop for PdfView {
    fn drop(&mut self) {
        let id = self.id;
        VIEWS.with(|views| views.borrow_mut().remove(&id));
    }
}

/// Hands a drawing from view `id`'s thumbnail thread to the view on the
/// main thread. Called on that thread.
fn deliver(id: u64, ticket: u64, page: PageRef, drawn: Rendered) {
    let result = slint::invoke_from_event_loop(move || {
        if let Some(view) = live_view(id) {
            view.thumb_arrived(ticket, page, drawn);
        }
    });
    if let Err(e) = result {
        warn!("cannot hand a PDF thumbnail to the window: {e}");
    }
}

/// A row with no picture yet for `page` at grid position `i`, in a window
/// of `sources` files (letters and tints only with several).
fn blank_row(i: usize, page: PageRef, sources: usize) -> PdfPage {
    PdfPage {
        image: Image::default(),
        number: SharedString::from((i + 1).to_string()),
        letter: if sources > 1 {
            source_letter(page.source).into()
        } else {
            SharedString::new()
        },
        tint: tint_of(page.source, sources),
        selected: false,
        ghost: false,
        rendered: false,
        broken: false,
    }
}

/// The name of a source's file.
fn file_name(source: &Source) -> String {
    source.path.file_name().map_or_else(
        || source.path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}
