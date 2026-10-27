//! One PDF Pages window: the pages of the PDFs from one drop as a grid of
//! thumbnails, with selection, the keyboard and the large view of a page.
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
//! - every close goes through [`PdfView::request_close`];
//! - keys arrive in [`PdfView::key`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, warn};
use slint::platform::Key;
use slint::winit_030::WinitWindowAccessor;
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalPosition, LogicalSize, Model, ModelRc,
    SharedString, Timer, VecModel,
};
use vento_core::geometry::{Point, screen_for};
use vento_core::i18n::{self, fill, tr, trn};
use vento_core::pdf::{GridMetrics, PageList, PageRef, Selection, Source, page_ranges};
use vento_platform::Platform;

use super::thumbs::{self, LARGE, Rendered, ThumbState, ThumbWorker};
use crate::appearance;
use crate::exec::Executor;
use crate::settings_window::centred_on;
use crate::toast::Toaster;
use crate::ui::{PdfPage, PdfSourceTag, PdfWindow, Theme};

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

/// What a view needs from the app.
pub struct ViewCtx {
    pub platform: Rc<dyn Platform>,
    #[allow(dead_code)] // The page tools' toasts use it (Tasks 9-10).
    pub toaster: Rc<Toaster>,
    /// Runs the write jobs (Task 10).
    #[allow(dead_code)] // Saving and the page tools use it (Tasks 9-10).
    pub executor: Rc<Executor>,
    /// Told the view's id once its window has closed; the controller then
    /// drops the view.
    pub closed: Rc<dyn Fn(u64)>,
}

/// One PDF Pages window.
pub struct PdfView {
    id: u64,
    window: PdfWindow,
    /// The opened files, in the order their pages first appear. Their
    /// bytes are the snapshot every thumbnail and write works from.
    #[allow(dead_code)] // Saving and Add PDF use them (Tasks 9-10).
    pub sources: RefCell<Arc<[Source]>>,
    /// The grid's pages, with in-window undo. Call
    /// [`pages_changed`](Self::pages_changed) after changing it.
    pub pages: RefCell<PageList>,
    pub selection: RefCell<Selection>,
    /// The page the arrow keys move from.
    focus: Cell<Option<usize>>,
    rows: Rc<VecModel<PdfPage>>,
    thumbs: RefCell<ThumbState<Image>>,
    worker: ThumbWorker,
    metrics: Cell<GridMetrics>,
    /// The left edge of the columns in the grid area (they are centred).
    grid_left: Cell<f32>,
    /// The page shown large, and the id of its latest drawing.
    large: Cell<Option<usize>>,
    large_seq: Cell<u64>,
    /// The longer side, in pixels, the large page was last asked for.
    large_px: Cell<u32>,
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
        let worker = thumbs::spawn(bytes, move |ticket, page, drawn| {
            deliver(id, ticket, page, drawn)
        });
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
            worker,
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
        let legend: Vec<PdfSourceTag> = {
            let sources = self.sources.borrow();
            if sources.len() > 1 {
                sources
                    .iter()
                    .enumerate()
                    .map(|(i, s)| PdfSourceTag {
                        letter: source_letter(i).into(),
                        name: file_name(s).into(),
                        tint: tint_of(i, sources.len()),
                    })
                    .collect()
            } else {
                Vec::new()
            }
        };
        window.set_legend(ModelRc::new(VecModel::from(legend)));
        self.connect();
        appearance::follow(window, appearance::Look::Full);
        appearance::follow_language(self.this.clone(), |view| view.retext());
        self.pages_changed();
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
    /// ⌘W, Escape with nothing selected, quitting) comes here. Closes at
    /// once for now; asking about unsaved edits belongs here (Task 10).
    pub fn request_close(&self) {
        self.close_now();
    }

    /// Hides the window and tells the controller, which drops the view on
    /// the next turn of the event loop.
    pub fn close_now(&self) {
        if self.closed.replace(true) {
            return;
        }
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the PDF window: {e}");
        }
        (self.ctx.closed)(self.id);
        debug!("PDF window {} closed", self.id);
    }

    /// Takes a change to [`pages`](Self::pages): rebuilds the grid's rows,
    /// starts a new thumbnail layout (results asked before it are dropped;
    /// cached images come back at once), keeps the selection and focus
    /// within the list, and retitles the window.
    pub fn pages_changed(&self) {
        let pages = self.pages.borrow().pages().to_vec();
        let several = self.sources.borrow().len();
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
            self.focus
                .set(pages.len().checked_sub(1).filter(|_| !pages.is_empty()));
        }
        if self.large.get().is_some_and(|i| i >= pages.len()) {
            self.close_large();
        }
        self.window.set_has_pages(!pages.is_empty());
        self.selection_changed();
        self.retext();
        self.pump();
    }

    /// Writes the window's text again: on open, after an edit and after a
    /// language change (which may also change the reading direction).
    fn retext(&self) {
        let names: Vec<String> = self.sources.borrow().iter().map(file_name).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (names, meta) = title_text(&names, self.pages.borrow().len(), self.is_edited());
        let heading = fill(
            &tr("{names} — {meta}"),
            &[("names", &names), ("meta", &meta)],
        );
        self.window.set_heading(heading.into());
        let selected = self.selection.borrow().indices();
        self.window.set_footer(footer_text(&selected).into());
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
        self.pump();
        if self.large.get().is_some() && self.large_wanted_px() > self.large_px.get() * 5 / 4 {
            self.request_large();
        }
    }

    /// A point in the grid area (window points, scroll included) in the
    /// grid's own content space, as `GridMetrics::index_at` and
    /// `insertion_index` take it: the columns are centred in the area.
    #[allow(dead_code)] // Dragging pages uses it (Task 9).
    pub fn grid_point(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.grid_left.get(), y)
    }

    /// The current grid layout.
    #[allow(dead_code)] // Dragging pages uses it (Task 9).
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
            self.worker.request(ticket, page, max_px);
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
        self.window
            .set_footer(footer_text(&selection.indices()).into());
    }

    fn set_focus(&self, focus: Option<usize>, shown: bool) {
        self.focus.set(focus);
        self.window
            .set_focus_index(focus.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1));
        self.window.set_show_focus(shown && focus.is_some());
    }

    /// A key pressed in the window, with ⌘ (`command`) and Shift. Returns
    /// whether it was used.
    pub fn key(&self, text: &str, command: bool, shift: bool) -> bool {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return false;
        };
        // Shift matters to the editing keys (⇧⌘Z, Task 9).
        let _ = shift;
        if c == char::from(Key::Escape) {
            if self.large.get().is_some() {
                self.close_large();
            } else if !self.selection.borrow().is_empty() {
                self.background_clicked();
            } else {
                self.request_close();
            }
            return true;
        }
        if command {
            return match c {
                'a' | 'A' => {
                    self.select_all();
                    true
                }
                'w' | 'W' => {
                    self.request_close();
                    true
                }
                _ => false,
            };
        }
        if self.large.get().is_some() {
            return false;
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
        self.worker.request(LARGE | seq, page, px);
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

/// The tint of source `i` in a window of `sources` files: 0 (none) with a
/// single file, else its place from 1.
fn tint_of(i: usize, sources: usize) -> i32 {
    if sources > 1 {
        i32::try_from(i % 1000 + 1).unwrap_or(1)
    } else {
        0
    }
}

/// The name of a source's file.
fn file_name(source: &Source) -> String {
    source.path.file_name().map_or_else(
        || source.path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Arrow keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// The window title's two parts: the files' names (`report.pdf`,
/// `report.pdf + scan.pdf`, `report.pdf + 2 more`) and the page count, with
/// "edited" once the page list changed.
pub fn title_text(names: &[&str], pages: usize, edited: bool) -> (String, String) {
    let names_text = match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, second] => fill(
            &tr("{first} + {second}"),
            &[("first", first), ("second", second)],
        ),
        [first, rest @ ..] => fill(
            &trn("{name} + {n} more", "{name} + {n} more", rest.len() as u64),
            &[("name", first)],
        ),
    };
    let count = trn("{n} page", "{n} pages", pages as u64);
    let meta = if edited {
        fill(&tr("{pages} · edited"), &[("pages", &count)])
    } else {
        count
    };
    (names_text, meta)
}

/// The footer's text for the selected positions (zero-based, sorted).
pub fn footer_text(selected: &[usize]) -> String {
    if selected.is_empty() {
        return tr("Nothing selected");
    }
    fill(
        &trn(
            "{n} selected · page {pages}",
            "{n} selected · pages {pages}",
            selected.len() as u64,
        ),
        &[("pages", &page_ranges(selected))],
    )
}

/// The letter of source `i`: A to Z, then A2 to Z2, and so on.
pub fn source_letter(i: usize) -> String {
    let letter = char::from(b'A' + (i % 26) as u8);
    match i / 26 {
        0 => letter.to_string(),
        round => format!("{letter}{}", round + 1),
    }
}

/// Where an arrow key moves the focus in a grid of `count` pages in
/// `columns`: Left and Right step through reading order (mirrored right to
/// left), Up and Down a row; the focus stops at the edges. Without a focus
/// any arrow focuses the first page.
pub fn moved_focus(
    focus: Option<usize>,
    arrow: Arrow,
    columns: usize,
    count: usize,
    rtl: bool,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let Some(focus) = focus else {
        return Some(0);
    };
    let focus = focus.min(count - 1);
    let columns = columns.max(1);
    let forward = match (arrow, rtl) {
        (Arrow::Right, false) | (Arrow::Left, true) => Some(true),
        (Arrow::Left, false) | (Arrow::Right, true) => Some(false),
        _ => None,
    };
    let moved = match (forward, arrow) {
        (Some(true), _) => (focus + 1).min(count - 1),
        (Some(false), _) => focus.saturating_sub(1),
        (None, Arrow::Down) if focus / columns < (count - 1) / columns => {
            (focus + columns).min(count - 1)
        }
        (None, Arrow::Up) if focus >= columns => focus - columns,
        _ => focus,
    };
    Some(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::i18n::with_language;

    #[test]
    fn titles() {
        assert_eq!(title_text(&["report.pdf"], 8, false).0, "report.pdf");
        assert_eq!(
            title_text(&["report.pdf", "scan.pdf"], 8, false).0,
            "report.pdf + scan.pdf"
        );
        assert_eq!(
            title_text(&["a.pdf", "b.pdf", "c.pdf"], 9, true).0,
            "a.pdf + 2 more"
        );
        assert_eq!(title_text(&["a.pdf"], 5, true).1, "5 pages · edited");
    }

    #[test]
    fn title_counts_one_page_and_unedited() {
        assert_eq!(title_text(&["a.pdf"], 1, false).1, "1 page");
        assert_eq!(title_text(&["a.pdf"], 8, false).1, "8 pages");
    }

    #[test]
    fn footers() {
        assert_eq!(footer_text(&[]), "Nothing selected");
        assert_eq!(footer_text(&[1, 2, 6]), "3 selected · pages 2–3, 7");
    }

    #[test]
    fn footer_names_one_page_singular() {
        assert_eq!(footer_text(&[4]), "1 selected · page 5");
    }

    #[test]
    fn window_texts_in_arabic() {
        with_language("ar", || {
            assert_ne!(footer_text(&[]), "Nothing selected");
            assert_ne!(title_text(&["a.pdf"], 5, true).1, "5 pages · edited");
            assert!(
                title_text(&["a.pdf", "b.pdf", "c.pdf"], 9, false)
                    .0
                    .contains("a.pdf")
            );
        });
    }

    #[test]
    fn letters_name_sources() {
        assert_eq!(source_letter(0), "A");
        assert_eq!(source_letter(1), "B");
        assert_eq!(source_letter(25), "Z");
        assert_eq!(source_letter(26), "A2");
        assert_eq!(source_letter(53), "B3");
    }

    #[test]
    fn arrows_move_focus_in_reading_order() {
        // 10 pages, 4 columns.
        assert_eq!(moved_focus(None, Arrow::Right, 4, 10, false), Some(0));
        assert_eq!(moved_focus(Some(0), Arrow::Right, 4, 10, false), Some(1));
        assert_eq!(moved_focus(Some(1), Arrow::Left, 4, 10, false), Some(0));
        assert_eq!(moved_focus(Some(0), Arrow::Left, 4, 10, false), Some(0));
        assert_eq!(moved_focus(Some(9), Arrow::Right, 4, 10, false), Some(9));
        assert_eq!(moved_focus(Some(1), Arrow::Down, 4, 10, false), Some(5));
        assert_eq!(moved_focus(Some(6), Arrow::Down, 4, 10, false), Some(9));
        assert_eq!(moved_focus(Some(9), Arrow::Down, 4, 10, false), Some(9));
        assert_eq!(moved_focus(Some(5), Arrow::Up, 4, 10, false), Some(1));
        assert_eq!(moved_focus(Some(2), Arrow::Up, 4, 10, false), Some(2));
        assert_eq!(moved_focus(Some(3), Arrow::Right, 4, 0, false), None);
    }

    #[test]
    fn arrows_mirror_right_to_left() {
        assert_eq!(moved_focus(Some(0), Arrow::Left, 4, 10, true), Some(1));
        assert_eq!(moved_focus(Some(1), Arrow::Right, 4, 10, true), Some(0));
        assert_eq!(moved_focus(Some(1), Arrow::Down, 4, 10, true), Some(5));
    }

    #[test]
    fn focus_past_the_end_is_clamped() {
        // After pages were removed the focus may point past the end: it
        // moves from the last page.
        assert_eq!(moved_focus(Some(12), Arrow::Left, 4, 10, false), Some(8));
        assert_eq!(moved_focus(Some(12), Arrow::Down, 4, 10, false), Some(9));
    }

    #[test]
    fn single_files_have_no_tint() {
        assert_eq!(tint_of(0, 1), 0);
        assert_eq!(tint_of(0, 2), 1);
        assert_eq!(tint_of(1, 2), 2);
    }
}
