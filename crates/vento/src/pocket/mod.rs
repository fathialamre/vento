//! The Pocket island: a black window that grows out of the notch (or sits as
//! a pill under the menu bar) and shows what the Pocket holds.
//!
//! The window always has the expanded size and sits at the top centre of the
//! island's screen; the collapsed shape is drawn inside it. Presence is
//! decided by [`IslandState`] from the wheel's input samples, so the island
//! adds no polling of its own.
//!
//! Items come in from the wheel's Pocket action ([`PocketController::add`]),
//! by a drop on the expanded island (through the platform's drop catcher),
//! and the island opens on a shake during a drag or on the global shortcut.

mod add;
mod geometry;
mod out;
mod owned;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{debug, info, warn};
use raw_window_handle::HasWindowHandle;
use slint::platform::WindowEvent;
use slint::winit_030::WinitWindowAccessor;
use slint::{
    ComponentHandle, Image, LogicalPosition, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer,
    TimerMode, VecModel,
};
use vento_core::geometry::{Point, Rect};
use vento_core::pocket::{ItemSource, Pocket, PocketItem, pocket_dir, pocket_path};
use vento_core::pocket_zone::{IslandState, Presence, ShakeDetector};
use vento_core::settings::Settings;
use vento_core::trigger::InputSample;
use vento_platform::{CatcherEvent, DropCatcher, Dropped, IslandHome, Platform, RgbaImage};

use crate::toast::Toaster;
use crate::ui::{Icons, PocketCell, PocketIsland};

use add::{AddPlan, Entry, plan_new, store};
use geometry::{Frames, MAX_CELLS, frames, hits, hover_event, island_inputs};
use out::{
    ALL, PressLatch, drag_end_events, drag_ids, drop_point, placeholder_icon, preview_png,
    removes_after_drop,
};
use owned::{PendingTrash, trash_kept, trash_owned};

/// Icon size of a cell, in points.
const ICON_PX: u32 = 40;
/// Where the window waits, out of sight, while it is pre-warmed.
const PARKED: LogicalPosition = LogicalPosition {
    x: -5000.0,
    y: -5000.0,
};
/// How long the parked window stays up, so the native window exists.
const WARM_DELAY: Duration = Duration::from_millis(200);
/// How long the island offers Undo after a clear or an x.
const UNDO_LIFETIME: Duration = Duration::from_secs(5);
/// How often the Undo ring is redrawn while it counts down.
const UNDO_TICK: Duration = Duration::from_millis(50);
/// How long after a clear or an x vento's own files go to the Trash: once
/// Undo is no longer offered.
const UNDO_WINDOW: Duration = UNDO_LIFETIME.saturating_add(Duration::from_millis(500));
/// How long after a drop removed items vento's own files go to the Trash,
/// so the app they were dropped on can read them first, even slowly
/// (ruling P16).
const DROP_GRACE: Duration = Duration::from_secs(60);

thread_local! {
    /// The controller on the main thread, for copy jobs finishing on a
    /// worker thread.
    static MAIN_POCKET: RefCell<Weak<PocketController>> = const { RefCell::new(Weak::new()) };
}

/// Cells for the first [`MAX_CELLS`] items; `icon` gets each item and
/// whether its file is missing.
fn build_cells(
    items: &[PocketItem],
    missing: &[u64],
    selected: &[u64],
    mut icon: impl FnMut(&PocketItem, bool) -> Image,
) -> Vec<PocketCell> {
    items
        .iter()
        .take(MAX_CELLS)
        .map(|item| {
            let gone = missing.contains(&item.id);
            PocketCell {
                name: display_name(&item.path).into(),
                icon: icon(item, gone),
                missing: gone,
                selected: selected.contains(&item.id),
            }
        })
        .collect()
}

pub(super) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// A Slint image of `icon`, or `None` if its buffer doesn't match its size.
fn to_image(icon: &RgbaImage) -> Option<Image> {
    let len = icon.width as usize * icon.height as usize * 4;
    if len == 0 || icon.pixels.len() != len {
        return None;
    }
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&icon.pixels, icon.width, icon.height);
    Some(Image::from_rgba8(buffer))
}

/// Shows the Pocket as an island at the top of the screen: hidden while
/// empty, collapsed while it holds items, expanded on hover or as a drag
/// approaches.
pub struct PocketController {
    platform: Rc<dyn Platform>,
    settings: Rc<RefCell<Settings>>,
    toast: Rc<Toaster>,
    window: PocketIsland,
    path: PathBuf,
    pocket: RefCell<Pocket>,
    island: RefCell<IslandState>,
    home: Cell<Option<IslandHome>>,
    /// Finder icons by item id, converted once.
    icons: RefCell<HashMap<u64, Image>>,
    cells: Rc<VecModel<PocketCell>>,
    /// Kept open by the shortcut until toggled again.
    pinned: Cell<bool>,
    /// Kept open until then, to confirm an add.
    pin_until: Cell<Option<Instant>>,
    /// Kept open by a shake until the drag ends.
    shaken: Cell<bool>,
    shake: RefCell<ShakeDetector>,
    /// The shake detector has been fed since the last mouse-up.
    shake_fed: Cell<bool>,
    /// The native drop target over the expanded island, once created.
    catcher: RefCell<Option<Rc<dyn DropCatcher>>>,
    /// Where the catcher is shown, or `None` while hidden.
    catcher_rect: Cell<Option<Rect>>,
    /// Files being copied into `kept/` by a running job, so a second drop
    /// of the same file is not copied again.
    copying: RefCell<Vec<PathBuf>>,
    /// The window has been pre-warmed; samples before that are ignored.
    warm: Cell<bool>,
    overlay_ready: Cell<bool>,
    /// `dragging` at the last sample, to spot the start of a drag.
    was_dragging: Cell<bool>,
    /// Whether the window takes the pointer (otherwise it ignores it).
    hittest: Cell<bool>,
    /// Ids of the selected items, toggled by clicking their cells.
    selected: RefCell<Vec<u64>>,
    /// Presses on the cells and on "Drag all".
    latch: PressLatch,
    /// Items taken out whose files wait to go to the Trash.
    pending: PendingTrash,
    /// The Undo on offer: its trash job, and when the offer ends.
    undo: Cell<Option<(u64, Instant)>>,
    /// Counts the offered Undo down.
    undo_timer: Timer,
    /// The cells a clear took, shown faded while its Undo is offered.
    ghosts: Rc<VecModel<PocketCell>>,
    /// vento is paused: the island rests and takes nothing.
    paused: Cell<bool>,
    /// The cursor at the last sample with the mouse up, and whether it was
    /// over the expanded window then, to replay hover moves.
    last_cursor: Cell<Option<Point>>,
    pointer_in: Cell<bool>,
    this: Weak<Self>,
}

impl PocketController {
    /// Loads the Pocket and creates the (hidden) island window. Call
    /// [`start`](Self::start) once the event loop is about to run.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    pub fn new(
        platform: Rc<dyn Platform>,
        settings: Rc<RefCell<Settings>>,
        toast: Rc<Toaster>,
    ) -> Rc<Self> {
        let path = pocket_path();
        let pocket = Pocket::load(&path);
        info!("pocket loaded: {} items", pocket.items().len());
        let window = PocketIsland::new().expect("cannot create the pocket window");
        let cells = Rc::new(VecModel::default());
        window.set_cells(ModelRc::from(cells.clone()));
        let ghosts = Rc::new(VecModel::default());
        window.set_ghosts(ModelRc::from(ghosts.clone()));
        let home = platform.pocket_home();
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            settings,
            toast,
            window,
            path,
            pocket: RefCell::new(pocket),
            island: RefCell::new(IslandState::new()),
            home: Cell::new(home),
            icons: RefCell::new(HashMap::new()),
            cells,
            pinned: Cell::new(false),
            pin_until: Cell::new(None),
            shaken: Cell::new(false),
            shake: RefCell::new(ShakeDetector::new()),
            shake_fed: Cell::new(false),
            catcher: RefCell::new(None),
            catcher_rect: Cell::new(None),
            copying: RefCell::new(Vec::new()),
            warm: Cell::new(false),
            overlay_ready: Cell::new(false),
            was_dragging: Cell::new(false),
            hittest: Cell::new(true),
            selected: RefCell::new(Vec::new()),
            latch: PressLatch::default(),
            pending: PendingTrash::default(),
            undo: Cell::new(None),
            undo_timer: Timer::default(),
            ghosts,
            paused: Cell::new(false),
            last_cursor: Cell::new(None),
            pointer_in: Cell::new(false),
            this: this.clone(),
        });
        if let Some(home) = home {
            controller.apply_home(&home);
        }
        controller.connect();
        MAIN_POCKET.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        controller
    }

    /// Routes the island's presses, moves and buttons to the controller.
    fn connect(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        let c = with.clone();
        w.on_item_pressed(move |i| {
            if let (Some(c), Ok(i)) = (c(), u32::try_from(i)) {
                c.latch.press(i);
            }
        });
        let c = with.clone();
        w.on_item_moved(move |_, dx, dy| {
            if let Some(c) = c() {
                c.moved(dx, dy);
            }
        });
        let c = with.clone();
        w.on_item_released(move |_| {
            if let Some(c) = c() {
                c.released();
            }
        });
        let c = with.clone();
        w.on_all_pressed(move || {
            if let Some(c) = c() {
                c.latch.press(ALL);
            }
        });
        let c = with.clone();
        w.on_all_moved(move |dx, dy| {
            if let Some(c) = c() {
                c.moved(dx, dy);
            }
        });
        let c = with.clone();
        w.on_all_released(move || {
            if let Some(c) = c() {
                c.released();
            }
        });
        let c = with.clone();
        w.on_remove(move |i| {
            if let (Some(c), Ok(i)) = (c(), usize::try_from(i)) {
                c.remove_at(i);
            }
        });
        let c = with.clone();
        w.on_undo(move || {
            if let Some(c) = c() {
                c.undo();
            }
        });
        w.on_clear(move || {
            if let Some(c) = with() {
                c.clear();
            }
        });
    }

    /// Id of the item shown in cell `index`.
    fn id_at(&self, index: usize) -> Option<u64> {
        self.pocket.borrow().items().get(index).map(|i| i.id)
    }

    /// A press moved: starts its one drag once it has moved far enough.
    fn moved(&self, dx: f32, dy: f32) {
        let Some(target) = self.latch.moved(dx, dy) else {
            return;
        };
        let pressed = if target == ALL {
            None
        } else {
            match self.id_at(target as usize) {
                Some(id) => Some(id),
                None => {
                    self.latch.ended();
                    return;
                }
            }
        };
        let ids = {
            let pocket = self.pocket.borrow();
            let all: Vec<u64> = pocket.items().iter().map(|i| i.id).collect();
            drag_ids(pressed, &all, &self.selected.borrow(), &pocket.missing())
        };
        self.start_drag_out(ids);
    }

    /// A press ended: a click on a cell toggles its selection.
    fn released(&self) {
        match self.latch.release() {
            Some(target) if target != ALL => self.toggle_selected(target as usize),
            _ => {}
        }
    }

    fn toggle_selected(&self, index: usize) {
        let Some(id) = self.id_at(index) else {
            return;
        };
        let on = {
            let mut selected = self.selected.borrow_mut();
            match selected.iter().position(|&s| s == id) {
                Some(i) => {
                    selected.swap_remove(i);
                    false
                }
                None => {
                    selected.push(id);
                    true
                }
            }
        };
        if let Some(mut cell) = self.cells.row_data(index) {
            cell.selected = on;
            self.cells.set_row_data(index, cell);
        }
    }

    /// Starts one drag session with the files of `ids`, the first one's
    /// icon as its image. With nothing left to drag (every item asked for is
    /// missing), says so instead. The press latch is already marked running.
    fn start_drag_out(&self, ids: Vec<u64>) {
        let paths: Vec<PathBuf> = {
            let pocket = self.pocket.borrow();
            ids.iter()
                .filter_map(|&id| pocket.get(id))
                .map(|i| i.path.clone())
                .collect()
        };
        let Some(first) = paths.first() else {
            self.latch.ended();
            info!("drag out: nothing left to drag");
            self.toast
                .show("Those items are no longer there", None, self.toast_at());
            return;
        };
        let icon = self
            .platform
            .file_icon(first, ICON_PX)
            .unwrap_or_else(|| placeholder_icon(ICON_PX));
        let Some(preview) = preview_png(&icon, ICON_PX)
            .or_else(|| preview_png(&placeholder_icon(ICON_PX), ICON_PX))
        else {
            self.latch.ended();
            warn!("cannot make a drag preview");
            return;
        };
        info!("drag out start: {} file(s)", paths.len());
        let result = drag::start_drag(
            &self.window.window().window_handle(),
            drag::DragItem::Files(paths),
            drag::Image::Raw(preview),
            move |result, pos| {
                // Called on the main thread, but the callback must be Send:
                // reach the controller through the event loop.
                let ids = ids.clone();
                let at = drop_point(&pos);
                let delivered = slint::invoke_from_event_loop(move || {
                    if let Some(c) = MAIN_POCKET.with(|slot| slot.borrow().upgrade()) {
                        c.drag_ended(&ids, result, at);
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report the end of a drag out: {e}");
                }
            },
            drag::Options::default(),
        );
        if let Err(e) = result {
            warn!("cannot start the drag out: {e}");
            self.latch.ended();
        }
    }

    /// A drag session ended: releases the press Slint still holds, and
    /// removes what the drag took if the setting says so and it was dropped
    /// anywhere but back on the island.
    fn drag_ended(&self, ids: &[u64], result: drag::DragResult, at: Point) {
        self.latch.ended();
        info!("drag out end: {result:?} at ({}, {})", at.x, at.y);
        self.release_grab(at);
        let remove = self.settings.borrow().pocket_remove_after_drop;
        let island = self.island_frames().map(|f| f.expanded);
        if removes_after_drop(result, remove, at, island) {
            // Forgotten now; vento's own files stay a while, so the app
            // they were dropped on can still read them (ruling P16).
            self.remove_items(ids, DROP_GRACE, false);
        }
    }

    /// Tells the window the press that started a drag out is over, at
    /// `at`: the drag session took the mouse, so Slint never saw its
    /// release and would keep the pressed cell grabbed.
    fn release_grab(&self, at: Point) {
        let Some(f) = self.island_frames() else {
            return;
        };
        let over = hits(&f, self.island.borrow().presence(), at);
        for event in drag_end_events(&f.expanded, at, over) {
            self.dispatch(event);
        }
        self.pointer_in.set(over);
    }

    fn dispatch(&self, event: WindowEvent) {
        if let Err(e) = self.window.window().dispatch_event_with_result(event) {
            warn!("cannot pass a pointer event to the pocket: {e}");
        }
    }

    /// The cell's x button: the item goes at once, with Undo; vento's own
    /// file goes to the Trash once Undo is no longer offered.
    fn remove_at(&self, index: usize) {
        let Some(id) = self.id_at(index) else {
            return;
        };
        self.remove_items(&[id], UNDO_WINDOW, true);
    }

    /// Takes `ids` out of the Pocket; vento's own files among them go to
    /// the Trash after `trash_after`. With `undo`, the island offers Undo.
    fn remove_items(&self, ids: &[u64], trash_after: Duration, undo: bool) {
        let removed = self.pocket.borrow_mut().remove(ids);
        if removed.is_empty() {
            return;
        }
        debug!("pocket removed {} item(s)", removed.len());
        self.save();
        self.refresh();
        self.trash_later(removed, trash_after, undo.then(Vec::new));
    }

    /// Empties the Pocket. The island's Undo puts the items back; vento's
    /// own files go to the Trash only once Undo is no longer offered.
    fn clear(&self) {
        let ghosts = self
            .cells
            .iter()
            .map(|c| PocketCell {
                selected: false,
                ..c
            })
            .collect();
        let items = self.pocket.borrow_mut().clear();
        if items.is_empty() {
            return;
        }
        debug!("pocket cleared {} item(s)", items.len());
        self.save();
        self.refresh();
        self.trash_later(items, UNDO_WINDOW, Some(ghosts));
    }

    /// Puts removed items back, oldest first. An item whose file came back
    /// in another way meanwhile is already there and stays as it is.
    fn restore(&self, items: Vec<PocketItem>) {
        let count = items.len();
        {
            let mut pocket = self.pocket.borrow_mut();
            for item in items {
                pocket.add_with_origin(item.path, item.source, item.origin, item.added_unix);
            }
        }
        debug!("pocket restored {count} item(s)");
        self.save();
        self.refresh();
    }

    /// Holds `items` (just taken out) for `after`, then moves vento's own
    /// files among them to the Trash. With `undo`, the island offers Undo,
    /// which puts the items back instead, showing those cells faded.
    fn trash_later(&self, items: Vec<PocketItem>, after: Duration, undo: Option<Vec<PocketCell>>) {
        let job = self.pending.add(items);
        if let Some(ghosts) = undo {
            self.offer_undo(job, ghosts);
        }
        let this = self.this.clone();
        Timer::single_shot(after, move || {
            if let Some(c) = this.upgrade()
                && let Some(items) = c.pending.take(job)
            {
                c.trash_unheld(items);
            }
        });
    }

    /// Offers Undo for trash job `job` in the island for [`UNDO_LIFETIME`],
    /// replacing any Undo already offered (whose files still go on their
    /// own timer). The island stays up meanwhile, even with nothing in it.
    fn offer_undo(&self, job: u64, ghosts: Vec<PocketCell>) {
        self.undo.set(Some((job, Instant::now() + UNDO_LIFETIME)));
        self.ghosts.set_vec(ghosts);
        self.window.set_undo_active(true);
        self.tick_undo();
        let this = self.this.clone();
        self.undo_timer
            .start(TimerMode::Repeated, UNDO_TICK, move || {
                if let Some(c) = this.upgrade() {
                    c.tick_undo();
                }
            });
        self.fit_panel();
    }

    /// Redraws the Undo countdown, or withdraws the offer once it is up.
    fn tick_undo(&self) {
        let Some((_, until)) = self.undo.get() else {
            return;
        };
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            self.end_undo();
            return;
        }
        self.window
            .set_undo_left(left.as_secs_f32() / UNDO_LIFETIME.as_secs_f32());
        self.window
            .set_undo_secs(i32::try_from(left.as_millis().div_ceil(1000)).unwrap_or(0));
    }

    /// The island's Undo: puts back what the offered job took.
    fn undo(&self) {
        let Some((job, _)) = self.undo.get() else {
            return;
        };
        self.end_undo();
        if let Some(items) = self.pending.take(job) {
            self.restore(items);
        }
    }

    /// Withdraws the offered Undo.
    fn end_undo(&self) {
        if self.undo.take().is_none() {
            return;
        }
        self.undo_timer.stop();
        self.window.set_undo_active(false);
        self.ghosts.set_vec(Vec::new());
        self.fit_panel();
    }

    /// Moves every file still waiting for its Undo window or drop grace to
    /// the Trash now. Call when vento quits.
    pub fn flush_pending(&self) {
        let items = self.pending.take_all();
        if !items.is_empty() {
            info!(
                "pocket: {} pending item(s) to the Trash at quit",
                items.len()
            );
        }
        self.trash_unheld(items);
    }

    /// Moves vento's own files of `items` to the Trash, except any the
    /// Pocket holds again by now.
    fn trash_unheld(&self, items: Vec<PocketItem>) {
        let dir = pocket_dir();
        let trash = |p: &Path| self.platform.trash(p);
        for item in items {
            if !self.pocket.borrow().contains(&item.path) {
                trash_owned(&item, &dir, &trash);
            }
        }
    }

    /// While vento is paused the island rests (collapsed with items, hidden
    /// without), takes no pointer and no drops; once unpaused, the next
    /// sample decides again.
    pub fn set_paused(&self, paused: bool) {
        self.paused.set(paused);
        if !paused || !self.warm.get() {
            return;
        }
        let after = self.settle_now();
        debug!("pocket paused: {after:?}");
    }

    /// Drops the island to its resting presence at once — no collapse delay,
    /// no waiting for the next poll tick — and releases the pointer. Used by
    /// pause and by closing the island with the shortcut.
    fn settle_now(&self) -> Presence {
        let has_items = self.has_content();
        let before = self.island.borrow().presence();
        let after = self.island.borrow_mut().settle(has_items);
        self.pin_until.set(None);
        self.shaken.set(false);
        if after != before {
            self.present(before, after);
        }
        self.set_hittest(false);
        self.place_catcher(None);
        if self.pointer_in.replace(false) {
            self.dispatch(WindowEvent::PointerExited);
        }
        after
    }

    /// Pre-warms the window off-screen and lifts it above the menu bar.
    pub fn start(self: &Rc<Self>) {
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            let Some(controller) = this.upgrade() else {
                return;
            };
            controller.window.window().set_position(PARKED);
            if let Err(e) = controller.window.show() {
                warn!("cannot pre-warm the pocket window: {e}");
            }
            let this = controller.this.clone();
            Timer::single_shot(WARM_DELAY, move || {
                if let Some(controller) = this.upgrade() {
                    controller.finish_warm();
                }
            });
        });
    }

    fn finish_warm(&self) {
        self.make_overlay();
        self.install_catcher();
        self.set_hittest(false);
        self.hide_window();
        self.warm.set(true);
        debug!("pocket pre-warmed");
    }

    /// Floats the window above the menu bar on every Space. Needs the
    /// native window, which exists once the window has been shown.
    fn make_overlay(&self) {
        let result = match self.window.window().window_handle().window_handle() {
            Ok(handle) => self
                .platform
                .make_overlay(handle.as_raw())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => {
                self.overlay_ready.set(true);
                info!("pocket overlay ready");
            }
            Err(e) => warn!("cannot float the pocket above the menu bar: {e}"),
        }
        let result = match self.window.window().window_handle().window_handle() {
            Ok(handle) => self
                .platform
                .make_non_key(handle.as_raw())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => info!("pocket window never takes key focus"),
            Err(e) => warn!("cannot keep the pocket from taking key focus: {e}"),
        }
    }

    /// Creates the drop catcher and routes its events to
    /// [`on_catcher`](Self::on_catcher) on a later turn of the event loop, so
    /// a hide inside the controller that makes the catcher report `Exited`
    /// never re-enters it.
    fn install_catcher(&self) {
        let Some(catcher) = self.platform.drop_catcher() else {
            warn!("no drop catcher: drops on the island are not available");
            return;
        };
        let this = self.this.clone();
        catcher.on_event(Box::new(move |event| {
            let this = this.clone();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(controller) = this.upgrade() {
                    controller.on_catcher(event);
                }
            });
        }));
        *self.catcher.borrow_mut() = Some(catcher);
    }

    /// Acts on what the drop catcher reports: the "Drop to add" hint while a
    /// drag is over the island, and the drop itself.
    fn on_catcher(&self, e: CatcherEvent) {
        debug!("pocket catcher {e:?}");
        match e {
            // Events are delivered late: an entry seen before the catcher was
            // hidden must not leave the hint on.
            CatcherEvent::Entered => self
                .window
                .set_hint(self.catcher_rect.get().is_some() && !self.latch.running()),
            CatcherEvent::Exited => self.window.set_hint(false),
            CatcherEvent::Dropped(d) => {
                self.window.set_hint(false);
                self.add_dropped(d);
            }
        }
    }

    /// Shows the catcher over `rect`, or hides it for `None`.
    fn place_catcher(&self, rect: Option<Rect>) {
        if self.catcher_rect.get() == rect {
            return;
        }
        self.catcher_rect.set(rect);
        // Clone it out: hiding may make it report `Exited` right away.
        let Some(catcher) = self.catcher.borrow().clone() else {
            return;
        };
        match rect {
            Some(rect) => {
                debug!("pocket catcher shown at {rect:?}");
                catcher.show(rect);
            }
            None => {
                debug!("pocket catcher hidden");
                catcher.hide();
                self.window.set_hint(false);
            }
        }
    }

    /// Feeds the shake detector while dragging; a shake keeps the island
    /// open until the drag ends. The detector re-arms on mouse-up.
    fn track_shake(&self, sample: &InputSample, dragging: bool, now: Instant) {
        if dragging {
            self.shake_fed.set(true);
            if self.shake.borrow_mut().feed(sample.cursor.x, now) {
                debug!("pocket shake");
                self.shaken.set(true);
            }
        } else {
            self.shaken.set(false);
        }
        if !sample.left_down && self.shake_fed.replace(false) {
            self.shake.borrow_mut().reset();
        }
    }

    /// Updates the island from one input sample; `dragging` is whether a drag
    /// of any type is under way. Called on every poll tick of the wheel.
    pub fn observe(&self, sample: &InputSample, dragging: bool) {
        if !self.warm.get() || self.paused.get() {
            return;
        }
        let now = Instant::now();
        self.track_shake(sample, dragging, now);
        let screens_changed = self.platform.screens_changed();
        if screens_changed || dragging && !self.was_dragging.get() {
            // The screens changed, or a drag may be headed for the island:
            // make sure it is where the screens are now, even when the
            // Pocket is empty.
            self.rehome();
        }
        self.was_dragging.set(dragging);
        let Some(home) = self.home.get() else {
            return;
        };
        let f = self.frames_of(&home);
        let before = self.island.borrow().presence();
        let has_items = self.has_content();
        let pinned = self.pinned_open(now);
        let inputs = island_inputs(&f, has_items, dragging, sample.cursor, pinned);
        let after = self.island.borrow_mut().update(&inputs, now);
        let changed = after != before;
        if changed {
            self.rehome();
            self.present(before, after);
        }
        let f = self.island_frames().unwrap_or(f);
        let inside = hits(&f, after, sample.cursor);
        if changed || inside != self.hittest.get() {
            self.set_hittest(inside);
        }
        // Only an expanded island takes drops, only during a drag, and never
        // the Pocket's own drag out.
        let catches = dragging && after == Presence::Expanded && !self.latch.running();
        self.place_catcher(catches.then_some(f.expanded));
        if !sample.left_down {
            self.replay_hover(&f, after, sample.cursor);
        }
    }

    /// Passes hover moves over the expanded island to the window, which
    /// gets none from AppKit while vento isn't the active app. A press is
    /// left alone: its moves always arrive.
    fn replay_hover(&self, f: &Frames, presence: Presence, cursor: Point) {
        let window = (presence == Presence::Expanded).then_some(&f.expanded);
        let last = self.last_cursor.replace(Some(cursor));
        let (event, inside) = hover_event(window, cursor, last, self.pointer_in.get());
        self.pointer_in.set(inside);
        if let Some(event) = event {
            self.dispatch(event);
        }
    }

    /// Re-reads where the island lives and moves the window if that changed.
    fn rehome(&self) {
        let Some(home) = self.platform.pocket_home() else {
            return;
        };
        if self.home.get() != Some(home) {
            debug!("pocket home is now {home:?}");
            self.home.set(Some(home));
            self.apply_home(&home);
        }
    }

    /// Lets the window take the pointer only over the drawn shape.
    fn set_hittest(&self, on: bool) {
        self.hittest.set(on);
        let result = self
            .window
            .window()
            .with_winit_window(|w| w.set_cursor_hittest(on));
        match result {
            Some(Ok(())) => debug!("pocket hittest {on}"),
            Some(Err(e)) => warn!("cannot set the pocket hit-testing: {e}"),
            None => warn!("the pocket has no native window for hit-testing"),
        }
    }

    fn pinned_open(&self, now: Instant) -> bool {
        if self.pin_until.get().is_some_and(|until| now >= until) {
            self.pin_until.set(None);
        }
        self.pinned.get() || self.shaken.get() || self.pin_until.get().is_some()
    }

    /// Shows, hides, expands or collapses the window for a presence change.
    fn present(&self, before: Presence, after: Presence) {
        debug!("pocket {before:?} -> {after:?}");
        if before == Presence::Hidden
            && let Some(home) = self.home.get()
        {
            // The window waited parked while hidden.
            self.apply_home(&home);
        }
        if after == Presence::Expanded || before == Presence::Hidden {
            self.refresh();
        }
        self.window.set_expanded(after == Presence::Expanded);
        if after == Presence::Hidden {
            self.hide_window();
        } else if before == Presence::Hidden {
            if !self.overlay_ready.get() {
                self.make_overlay();
            }
            // Clicks fall through until the pointer is over the shape.
            self.set_hittest(false);
            if let Err(e) = self.window.show() {
                warn!("cannot show the pocket: {e}");
            }
            let w = self.window.window();
            debug!(
                "pocket shown at {:?}, {:?}, scale {}, key {:?}",
                w.position(),
                w.size(),
                w.scale_factor(),
                w.with_winit_window(|w| w.has_focus())
            );
        }
    }

    /// Sizes the window for `home` and places it at the top centre.
    fn apply_home(&self, home: &IslandHome) {
        let f = self.frames_of(home);
        self.window.set_panel_w(f.expanded.w as f32);
        self.window.set_notch_style(f.notch_style);
        self.window.set_notch_w(f.notch_w as f32);
        self.window.set_top_h(f.top_h as f32);
        self.window.window().set_position(LogicalPosition::new(
            f.expanded.x as f32,
            f.expanded.y as f32,
        ));
    }

    /// Adds `paths` (as references, or as kept copies when the Pocket keeps
    /// copies), saves, and keeps the island open for `pin_open_for` to show
    /// them. Paths already in the Pocket are skipped.
    pub fn add(&self, paths: Vec<PathBuf>, pin_open_for: Duration) {
        self.add_dropped(Dropped::Files(paths));
        self.pin_until.set(Some(Instant::now() + pin_open_for));
    }

    /// Adds what was dropped on the island. Files already in the Pocket are
    /// skipped silently; anything that fails is reported in a toast. Kept
    /// copies are made on a worker thread and added when done; each drop
    /// runs its own job, so drops keep working meanwhile.
    fn add_dropped(&self, d: Dropped) {
        let keep_copies = self.settings.borrow().pocket_keep_copies;
        let plan = {
            let pocket = self.pocket.borrow();
            let copying = self.copying.borrow();
            plan_new(&d, keep_copies, |p| {
                pocket.contains(p) || copying.iter().any(|c| c == p)
            })
        };
        match plan {
            AddPlan::Copies(paths) if paths.is_empty() => {}
            AddPlan::Copies(paths) => self.spawn_copies(paths),
            plan => {
                let (entries, errors) = store(plan, &pocket_dir());
                self.finish_add(entries, &errors);
            }
        }
    }

    /// Copies `paths` into `kept/` on a worker thread, then adds the copies
    /// on the main thread.
    fn spawn_copies(&self, paths: Vec<PathBuf>) {
        self.copying.borrow_mut().extend(paths.iter().cloned());
        let (job, done) = (paths.clone(), paths.clone());
        let spawned = std::thread::Builder::new()
            .name("vento-pocket-copy".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    store(AddPlan::Copies(job), &pocket_dir())
                }));
                let delivered = slint::invoke_from_event_loop(move || {
                    let Some(controller) = MAIN_POCKET.with(|slot| slot.borrow().upgrade()) else {
                        return;
                    };
                    controller.end_copies(&done);
                    match result {
                        Ok((entries, errors)) => controller.finish_add(entries, &errors),
                        Err(_) => controller.finish_add(Vec::new(), &["internal error".into()]),
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report finished pocket copies: {e}");
                }
            });
        if let Err(e) = spawned {
            self.end_copies(&paths);
            self.finish_add(Vec::new(), &[format!("cannot start copying: {e}")]);
        }
    }

    /// Forgets `paths` as being copied.
    fn end_copies(&self, paths: &[PathBuf]) {
        let mut copying = self.copying.borrow_mut();
        for p in paths {
            if let Some(i) = copying.iter().position(|c| c == p) {
                copying.swap_remove(i);
            }
        }
    }

    /// Adds stored files to the Pocket, saves and refreshes, and toasts the
    /// first error. A kept copy of a file the Pocket now holds anyway (it
    /// came in another way while copying) is removed: vento made it.
    fn finish_add(&self, entries: Vec<Entry>, errors: &[String]) {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut added = 0;
        let mut unused = Vec::new();
        {
            let mut pocket = self.pocket.borrow_mut();
            for (path, source, origin) in entries {
                let kept = source == ItemSource::Kept;
                if pocket
                    .add_with_origin(path.clone(), source, origin, now_unix)
                    .is_some()
                {
                    added += 1;
                } else if kept && !pocket.contains(&path) {
                    unused.push(path);
                }
            }
        }
        let dir = pocket_dir();
        let trash = |p: &Path| self.platform.trash(p);
        for path in unused {
            trash_kept(&path, &dir, &trash);
        }
        debug!("pocket added {added} item(s)");
        if added > 0 {
            self.save();
            self.refresh();
        }
        if let Some(reason) = errors.first() {
            warn!("cannot add to the pocket: {}", errors.join("; "));
            self.toast.show(
                &format!("Couldn't add to Pocket: {reason}"),
                None,
                self.toast_at(),
            );
        }
    }

    /// Keeps the island open, or lets it close again.
    pub fn toggle_pinned(&self) {
        let pinned = !self.pinned.get();
        self.pinned.set(pinned);
        debug!("pocket pinned {pinned}");
        // Closing with the shortcut is a request to close now: skip the
        // hover collapse delay that would otherwise hold the island open.
        if !pinned && self.warm.get() && !self.paused.get() {
            self.settle_now();
        }
    }

    /// Re-reads which files are missing, loads icons not yet cached, and
    /// pushes the cells and the count into the window.
    pub fn refresh(&self) {
        let pocket = self.pocket.borrow();
        let missing = pocket.missing();
        let mut icons = self.icons.borrow_mut();
        icons.retain(|id, _| pocket.get(*id).is_some());
        let mut selected = self.selected.borrow_mut();
        selected.retain(|&id| pocket.get(id).is_some());
        let fallback = self.window.global::<Icons>().get_file();
        let cells = build_cells(pocket.items(), &missing, &selected, |item, gone| {
            if gone {
                return fallback.clone();
            }
            if let Some(icon) = icons.get(&item.id) {
                return icon.clone();
            }
            match self
                .platform
                .file_icon(&item.path, ICON_PX)
                .as_ref()
                .and_then(to_image)
            {
                Some(icon) => {
                    icons.insert(item.id, icon.clone());
                    icon
                }
                None => fallback.clone(),
            }
        });
        self.cells.set_vec(cells);
        let count = pocket.items().len();
        self.window
            .set_count(i32::try_from(count).unwrap_or(i32::MAX));
        drop((icons, selected, pocket));
        self.fit_panel();
    }

    /// Widens or narrows the panel to what it shows (ruling P18).
    fn fit_panel(&self) {
        if let Some(home) = self.home.get()
            && self.window.get_panel_w() != frames(&home, self.shown_count()).expanded.w as f32
        {
            self.apply_home(&home);
        }
    }

    /// Cells the expanded island shows: the items, or a clear's ghosts.
    fn shown_count(&self) -> usize {
        let items = self.pocket.borrow().items().len();
        items.max(self.ghosts.row_count())
    }

    /// The island has something to show: items, or an Undo on offer.
    fn has_content(&self) -> bool {
        !self.pocket.borrow().is_empty() || self.undo.get().is_some()
    }

    fn save(&self) {
        if let Err(e) = self.pocket.borrow().save(&self.path) {
            warn!("cannot save the pocket to {}: {e}", self.path.display());
            self.toast.show(
                &format!("Couldn't save the Pocket: {e}"),
                None,
                self.toast_at(),
            );
        }
    }

    /// Below the middle of the expanded island.
    fn toast_at(&self) -> Point {
        self.island_frames().map_or(Point { x: 0.0, y: 0.0 }, |f| {
            let e = f.expanded;
            Point {
                x: e.x + e.w / 2.0,
                y: e.y + e.h,
            }
        })
    }

    /// The island's frames for `home`, sized for the cells it shows now.
    fn frames_of(&self, home: &IslandHome) -> Frames {
        frames(home, self.shown_count())
    }

    fn island_frames(&self) -> Option<Frames> {
        self.home.get().map(|home| self.frames_of(&home))
    }

    fn hide_window(&self) {
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the pocket: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use vento_core::pocket::{ItemSource, PocketItem};

    fn item(id: u64, path: &str) -> PocketItem {
        PocketItem {
            id,
            path: PathBuf::from(path),
            source: ItemSource::Reference,
            added_unix: 0,
            origin: None,
        }
    }

    #[test]
    fn cells_cap_at_eight_with_overflow_count() {
        let items: Vec<PocketItem> = (0..11)
            .map(|i| item(i, &format!("/tmp/vento-test/file-{i}.txt")))
            .collect();
        let cells = build_cells(&items, &[2], &[1], |_, _| slint::Image::default());
        assert_eq!(cells.len(), 8);
        // The island shows "+{count - cells}".
        assert_eq!(items.len() - cells.len(), 3);
        assert_eq!(cells[0].name, "file-0.txt");
        assert_eq!(cells[7].name, "file-7.txt");
        assert!(cells[2].missing);
        assert!(!cells[1].missing);
        assert!(cells[1].selected);
        assert_eq!(cells.iter().filter(|c| c.selected).count(), 1);
    }

    #[test]
    fn icon_buffers_must_match_their_size() {
        let icon = |width, height, len| RgbaImage {
            width,
            height,
            pixels: vec![255; len],
        };
        assert!(to_image(&icon(2, 2, 16)).is_some());
        assert!(to_image(&icon(2, 2, 15)).is_none());
        assert!(to_image(&icon(0, 0, 0)).is_none());
    }
}
