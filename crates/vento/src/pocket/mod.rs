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
//! and the island opens as a drag nears it or on the global shortcut.
//!
//! With the clipboard history on, the expanded island has a second view, the
//! Clipboard ([`View`]); with the screenshots on, a third, the Screenshots
//! (`shots_view`). With two views or more the bar under them has a tab for
//! each.
//!
//! The Pocket view shows its items grouped by kind (`kinds`) or as a list
//! (`list_view`): it opens on the look the settings say ([`PocketView`]),
//! and its bar switches between the two.

mod add;
mod clip;
mod clip_thumb;
mod clip_view;
mod geometry;
mod kinds;
mod list_view;
mod menu;
pub(crate) mod out;
mod owned;
mod shots;
mod shots_view;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{debug, info, warn};
use raw_window_handle::HasWindowHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::winit_030::WinitWindowAccessor;
use slint::{
    ComponentHandle, Image, LogicalPosition, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode,
};
use vento_core::clip_history::ClipEntry;
use vento_core::geometry::{Point, Rect};
use vento_core::i18n::errors::io_reason;
use vento_core::i18n::{fill, tr, with_language};
use vento_core::pocket::{ItemSource, Pocket, PocketItem, pocket_dir, pocket_path};
use vento_core::pocket_zone::{IslandState, Presence};
use vento_core::settings::{PocketView, Settings};
use vento_core::trigger::InputSample;
use vento_platform::{
    CatcherEvent, DropCatcher, Dropped, IslandHome, Platform, PlatformError, RgbaImage,
};

use crate::appearance;
use crate::toast::Toaster;
use crate::ui::PocketIsland;

#[cfg(target_os = "linux")]
use crate::drag;
use add::{AddPlan, Entry, Failure, Print, SavedPrints, plan_new, saved_content, store};
use clip::ClipController;
use clip_view::{ClipView, view_index};
use geometry::{Frames, frames, hits, hover_event, interactive_rect, island_inputs, window_frame};
use kinds::Kinds;
use list_view::ListView;
use menu::{ItemMenu, MenuAction, apps_source, menu_targets, opens_menu};
use out::{
    Press, PressLatch, drag_end_events, drag_ids, drop_point, placeholder_icon, preview_png,
    removes_after_drop,
};
use owned::{PendingTrash, trash_kept, trash_owned};
pub use shots::Access;
use shots::ShotsController;
use shots_view::{ShotCard, ShotsView};

/// Icon size of a tile or a row, in points.
const ICON_PX: u32 = 40;
/// Where the window waits, out of sight, while it is pre-warmed.
const PARKED: LogicalPosition = LogicalPosition {
    x: -5000.0,
    y: -5000.0,
};
/// How long the parked window stays up, so the native window exists.
const WARM_DELAY: Duration = Duration::from_millis(200);
/// How long the window stays as tall as before after the island shrinks, so
/// the shrinking is seen: a little longer than the island's animation.
const SHRINK_DELAY: Duration = Duration::from_millis(280);
/// How long the island in the corner by the clock stays open after a drop
/// on its drop box, to show what came in.
const DROP_CONFIRM: Duration = Duration::from_millis(1500);
/// How long the island offers Undo after a clear or an x.
const UNDO_LIFETIME: Duration = Duration::from_secs(5);
/// How often the Undo ring is redrawn while it counts down in sight.
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

/// What a right-click menu action that failed was doing, for its toast.
#[derive(Debug, Clone, Copy)]
enum MenuFailure {
    Open,
    Copy,
}

/// Shows a launch that failed after Open With returned (`error`, from any
/// thread) in the Pocket's toast, on the main thread.
fn toast_late_open_error(error: PlatformError) {
    let delivered = slint::invoke_from_event_loop(move || {
        if let Some(c) = MAIN_POCKET.with(|slot| slot.borrow().upgrade()) {
            let reason = error.to_string();
            let text = fill(&tr("Couldn't open: {reason}"), &[("reason", &reason)]);
            c.toast.show(&text, None, c.toast_at());
        }
    });
    if let Err(e) = delivered {
        warn!("cannot report a failed open: {e}");
    }
}

/// What the expanded island shows. The last one used is kept in memory only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The Pocket's items.
    Pocket,
    /// The clipboard history.
    Clipboard,
    /// The recent screenshots.
    Screenshots,
}

/// The view the island shows after a request for `asked`, when it shows
/// `current` (`None` while not held open): it opens on `asked`, switches to
/// it, or closes when it already shows it.
fn next_open(current: Option<View>, asked: View) -> Option<View> {
    match current {
        Some(v) if v == asked => None,
        _ => Some(asked),
    }
}

/// The view an island opened by resting on it shows, when it showed `last`
/// before: the view of the Undo on offer (`undo_view`), where its ring is;
/// otherwise `last`, unless it has nothing to show (an empty Pocket, a
/// history off or empty: `clip_has` false) while there are screenshots
/// (`shots_has`), which the wings then show.
fn hover_view(
    pocket_empty: bool,
    clip_has: bool,
    shots_has: bool,
    undo_view: Option<View>,
    last: View,
) -> View {
    if let Some(v) = undo_view {
        return v;
    }
    let empty = match last {
        View::Pocket => pocket_empty,
        View::Clipboard => !clip_has,
        View::Screenshots => false,
    };
    if empty && shots_has {
        View::Screenshots
    } else {
        last
    }
}

/// What the island's Undo puts back.
enum Undo {
    /// The items of this trash job, taken out of the Pocket.
    Pocket(u64),
    /// Entries taken out of the clipboard history.
    Clip(Vec<ClipEntry>),
    /// A screenshot taken out of the grid, its file not in the Trash yet.
    Shot(PathBuf),
}

impl Undo {
    /// The view whose ring offers this Undo.
    fn view(&self) -> View {
        match self {
            Undo::Pocket(_) => View::Pocket,
            Undo::Clip(_) => View::Clipboard,
            Undo::Shot(_) => View::Screenshots,
        }
    }

    /// The window's number for this kind of Undo: only the view it belongs
    /// to (0 the Pocket, 1 the Clipboard, 3 the Screenshots) shows its ring.
    fn kind(&self) -> i32 {
        match self {
            Undo::Pocket(_) => 0,
            Undo::Clip(_) => 1,
            Undo::Shot(_) => 3,
        }
    }
}

pub(super) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// A Slint image of `icon`, or `None` if its buffer doesn't match its size.
pub(crate) fn to_image(icon: &RgbaImage) -> Option<Image> {
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
    /// The Pocket view's groups and tiles.
    kinds: Kinds,
    /// Kept open by the shortcut until toggled again.
    pinned: Cell<bool>,
    /// Kept open until then, to confirm an add.
    pin_until: Cell<Option<Instant>>,
    /// The native drop target over the expanded island, once created.
    catcher: RefCell<Option<Rc<dyn DropCatcher>>>,
    /// Where the catcher is shown, or `None` while hidden.
    catcher_rect: Cell<Option<Rect>>,
    /// Files being copied into `kept/` by a running job, so a second drop
    /// of the same file is not copied again.
    copying: RefCell<Vec<PathBuf>>,
    /// What the saved files hold, so a drop of the same content is spotted
    /// without reading them all again.
    saved_prints: RefCell<SavedPrints>,
    /// The window has been pre-warmed; samples before that are ignored.
    warm: Cell<bool>,
    overlay_ready: Cell<bool>,
    /// `dragging` at the last sample, to spot the start of a drag.
    was_dragging: Cell<bool>,
    /// The island in the corner by the clock shows only its drop box: a
    /// drag from another app is under way.
    drop_box: Cell<bool>,
    /// Whether another window of vento's now fills the island's corner by
    /// the clock; see [`set_corner_taken`](Self::set_corner_taken).
    corner_taken: RefCell<Option<Box<dyn Fn() -> bool>>>,
    /// The island gave way to that window at the last sample.
    yielded: Cell<bool>,
    /// Resting on the collapsed shape doesn't open the island until the
    /// pointer leaves it, after giving way.
    hover_blocked: Cell<bool>,
    /// Whether the window takes the pointer (otherwise it ignores it).
    hittest: Cell<bool>,
    /// Whether the platform sets where the window takes the pointer from
    /// its shape; until it says it can't.
    input_by_shape: Cell<bool>,
    /// The input shape last set, in physical window pixels; `None` before
    /// the first.
    input_shape: Cell<Option<Option<Rect>>>,
    /// Ids of the selected items, toggled by clicking their tiles or rows.
    selected: RefCell<Vec<u64>>,
    /// Presses on the tiles, the rows, "Drag N" and "Drag all".
    latch: PressLatch,
    /// Items taken out whose files wait to go to the Trash.
    pending: PendingTrash,
    /// The Undo on offer, and when the offer ends.
    undo: RefCell<Option<(Undo, Instant)>>,
    /// Counts the offered Undo down.
    undo_timer: Timer,
    /// `undo_timer` repeats to redraw the ring in sight; otherwise it only
    /// waits for the offer to end.
    undo_ticking: Cell<bool>,
    /// vento is paused: the island rests and takes nothing.
    paused: Cell<bool>,
    /// The cursor at the last sample with the mouse up, and whether it was
    /// over the expanded window then, to replay hover moves.
    last_cursor: Cell<Option<Point>>,
    pointer_in: Cell<bool>,
    /// The clipboard history.
    clip: Rc<ClipController>,
    /// The screenshots in the screenshot folder.
    shots: Rc<ShotsController>,
    /// Tells the Settings window the access to the screenshot folder may
    /// have changed.
    on_shots_changed: RefCell<Option<Box<dyn Fn()>>>,
    /// The view shown expanded (or last shown).
    view: Cell<View>,
    /// The Clipboard view's rows.
    clip_view: ClipView,
    /// The Screenshots view's days and tiles.
    shots_view: ShotsView,
    /// The card of a screenshot just saved, while it is up.
    card: ShotCard,
    /// The drag out running (while the latch says so) is the card's: it
    /// leaves the view the island will open on as it is.
    card_drag: Cell<bool>,
    /// The Pocket view's look in the settings, which it opens on.
    pocket_view: Cell<PocketView>,
    /// The Pocket list, shown by the bar's look toggle or a group.
    list: ListView,
    /// Brings the window down to the island's height once it has shrunk.
    shrink_timer: Timer,
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
        // The island keeps its own dark look; it takes the direction.
        appearance::follow(&window, appearance::Look::Direction);
        let kinds = Kinds::new(&window);
        let clip_view = ClipView::new(&window);
        let shots_view = ShotsView::new(&window);
        let pocket_view = settings.borrow().pocket_view;
        let list = ListView::new(&window, pocket_view);
        let home = platform.pocket_home();
        let (clip_platform, clip_settings) = (platform.clone(), settings.clone());
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
            kinds,
            pinned: Cell::new(false),
            pin_until: Cell::new(None),
            catcher: RefCell::new(None),
            catcher_rect: Cell::new(None),
            copying: RefCell::new(Vec::new()),
            saved_prints: RefCell::new(SavedPrints::default()),
            warm: Cell::new(false),
            overlay_ready: Cell::new(false),
            was_dragging: Cell::new(false),
            drop_box: Cell::new(false),
            corner_taken: RefCell::new(None),
            yielded: Cell::new(false),
            hover_blocked: Cell::new(false),
            hittest: Cell::new(true),
            input_by_shape: Cell::new(true),
            input_shape: Cell::new(None),
            selected: RefCell::new(Vec::new()),
            latch: PressLatch::default(),
            pending: PendingTrash::default(),
            undo: RefCell::new(None),
            undo_timer: Timer::default(),
            undo_ticking: Cell::new(false),
            paused: Cell::new(false),
            last_cursor: Cell::new(None),
            pointer_in: Cell::new(false),
            clip: Self::clip_history(clip_platform, &clip_settings, this.clone()),
            shots: Self::screenshots(&clip_settings, this.clone()),
            on_shots_changed: RefCell::new(None),
            view: Cell::new(View::Pocket),
            clip_view,
            shots_view,
            card: ShotCard::default(),
            card_drag: Cell::new(false),
            pocket_view: Cell::new(pocket_view),
            list,
            shrink_timer: Timer::default(),
            this: this.clone(),
        });
        if let Some(home) = home {
            controller.apply_home(&home);
        }
        controller.connect();
        controller.connect_clip();
        controller.connect_list();
        controller.connect_kinds();
        controller.connect_shots();
        MAIN_POCKET.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        controller
    }

    /// The clipboard history, refreshing the island through `this` on every
    /// change to it.
    fn clip_history(
        platform: Rc<dyn Platform>,
        settings: &RefCell<Settings>,
        this: Weak<Self>,
    ) -> Rc<ClipController> {
        ClipController::new(
            platform,
            settings,
            Box::new(move || {
                if let Some(c) = this.upgrade() {
                    c.refresh();
                }
            }),
        )
    }

    /// The screenshots, refreshing the island through `this` when a listing
    /// changes them, and showing the card for a new one.
    fn screenshots(settings: &RefCell<Settings>, this: Weak<Self>) -> Rc<ShotsController> {
        let on_new = this.clone();
        ShotsController::new(
            settings,
            Box::new(move || {
                if let Some(c) = this.upgrade() {
                    c.refresh();
                    c.shots_changed();
                }
            }),
            Box::new(move |path| {
                if let Some(c) = on_new.upgrade() {
                    c.show_card(path);
                }
            }),
        )
    }

    /// Routes the island's presses, moves and buttons to the controller.
    fn connect(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        let c = with.clone();
        w.on_item_pressed(move |i| {
            if let (Some(c), Ok(i)) = (c(), u32::try_from(i)) {
                c.latch.press(Press::Cell(i));
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
        w.on_item_context(move |i, x, y, left| {
            let Some(c) = c() else {
                return false;
            };
            let ctrl = left && c.platform.sample_input().modifiers.ctrl;
            if !opens_menu(left, ctrl) {
                return false;
            }
            // The item under the press now, whatever the cells show later.
            let Some(id) = usize::try_from(i).ok().and_then(|i| c.id_at(i)) else {
                return true;
            };
            let at = Point {
                x: f64::from(x),
                y: f64::from(y),
            };
            let button = if left {
                PointerEventButton::Left
            } else {
                PointerEventButton::Right
            };
            // On a later turn of the event loop: the menu runs its own loop
            // until it closes, and the press it ends must be passed back to
            // the window, which can't take events while it handles this one.
            let this = c.this.clone();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(c) = this.upgrade() {
                    c.show_menu(id, at, button);
                }
            });
            true
        });
        let c = with.clone();
        w.on_all_pressed(move || {
            if let Some(c) = c() {
                c.latch.press(Press::All);
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
        let pressed = match target {
            // Under the list's kind, "Drag N" takes that kind.
            Press::All => match self.list_kind() {
                Some(kind) => {
                    self.drag_kind(kind);
                    return;
                }
                None => None,
            },
            Press::Kind(kind) => {
                self.drag_kind(kind);
                return;
            }
            Press::Shot(i) => {
                self.card_drag.set(false);
                match self.shots_view.path_at(i32::try_from(i).unwrap_or(-1)) {
                    Some(path) => self.start_shot_drag(path),
                    None => self.latch.ended(),
                }
                return;
            }
            Press::Card => {
                match self.card.path() {
                    Some(path) => {
                        self.card_drag.set(true);
                        self.start_shot_drag(path);
                    }
                    None => self.latch.ended(),
                }
                return;
            }
            Press::Cell(i) => match self.id_at(i as usize) {
                Some(id) => Some(id),
                None => {
                    self.latch.ended();
                    return;
                }
            },
        };
        // A row's press, while the list is shown, drags only what it lists.
        let ids = match pressed {
            Some(id) if self.list_shown() => self.list_drag_ids(id),
            _ => {
                let pocket = self.pocket.borrow();
                let all: Vec<u64> = pocket.items().iter().map(|i| i.id).collect();
                drag_ids(pressed, &all, &self.selected.borrow(), &pocket.missing())
            }
        };
        self.start_drag_out(ids);
    }

    /// A press ended: a click on a tile or a row toggles its selection; on
    /// a screenshot's tile, it opens its peek; on the card's picture, the
    /// Screenshots view with it peeked.
    fn released(&self) {
        match self.latch.release() {
            Some(Press::Cell(i)) => self.toggle_selected(i as usize),
            Some(Press::Shot(i)) => self.shot_clicked(i),
            Some(Press::Card) => self.card_clicked(),
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
        self.kinds.set_selected(index, on);
        self.list.set_selected(id, on);
    }

    /// The right-click menu on a tile, at window-local `at`: acts on the
    /// selection when the tile is part of a bigger one, otherwise on the
    /// tile's item. While it is up (it is modal), nothing else runs, so the
    /// island stays as it is; afterwards it settles as usual.
    fn show_menu(&self, clicked: u64, at: Point, button: PointerEventButton) {
        if self.paused.get()
            || self.island.borrow().presence() != Presence::Expanded
            || self.latch.running()
            || self.pocket.borrow().get(clicked).is_none()
        {
            debug!("pocket menu not shown: the island changed since the press");
            return;
        }
        let (targets, paths, source) = {
            let pocket = self.pocket.borrow();
            let all: Vec<u64> = pocket.items().iter().map(|i| i.id).collect();
            let targets = menu_targets(clicked, &all, &self.selected.borrow());
            let missing = pocket.missing();
            let path_of = |id: &u64| {
                (!missing.contains(id))
                    .then(|| pocket.get(*id).map(|i| i.path.clone()))
                    .flatten()
            };
            let paths: Vec<PathBuf> = targets.iter().filter_map(path_of).collect();
            let source = apps_source(clicked, &targets, &missing).and_then(|id| path_of(&id));
            (targets, paths, source)
        };
        // Asked only now: nothing is done while idle.
        let apps = source
            .map(|p| self.platform.apps_for(&p))
            .unwrap_or_default();
        let menu = ItemMenu::build(targets.len(), paths.len(), &apps);
        let handle = match self.window.window().window_handle().window_handle() {
            Ok(handle) => handle.as_raw(),
            Err(e) => {
                warn!("cannot show the pocket menu: {e}");
                return;
            }
        };
        debug!(
            "pocket menu on {} item(s), {} there, {} app(s)",
            targets.len(),
            paths.len(),
            apps.len()
        );
        let chosen = self.platform.context_menu(handle, at, &menu.entries);
        // The menu took the mouse: Slint never saw the button go up, and
        // the cell would keep the pointer grabbed.
        self.dispatch(WindowEvent::PointerReleased {
            position: LogicalPosition::new(at.x as f32, at.y as f32),
            button,
        });
        let Some(action) = chosen.and_then(|id| menu.action(id)) else {
            return;
        };
        info!("pocket menu: {action:?} on {} item(s)", targets.len());
        self.run_menu_action(action, &targets, &paths);
    }

    /// Does what the menu's `action` says, to `targets` (all of them, for
    /// Remove) or to `paths` (the files of those still there).
    fn run_menu_action(&self, action: MenuAction, targets: &[u64], paths: &[PathBuf]) {
        let result = match action {
            MenuAction::Open => self
                .platform
                .open_paths(paths)
                .map_err(|e| (MenuFailure::Open, e)),
            MenuAction::OpenWith(app) => self
                .platform
                .open_with(paths, &app, Box::new(toast_late_open_error))
                .map_err(|e| (MenuFailure::Open, e)),
            MenuAction::Reveal => {
                self.platform.reveal_in_finder(paths);
                Ok(())
            }
            MenuAction::Copy => self
                .platform
                .copy_files(paths)
                .map_err(|e| (MenuFailure::Copy, e)),
            // As the cell's x does, for one item or many.
            MenuAction::Remove => {
                self.remove_items(targets, UNDO_WINDOW, true);
                Ok(())
            }
        };
        if let Err((failure, e)) = result {
            warn!(
                "pocket menu: {failure:?} failed: {}",
                with_language("en", || e.to_string())
            );
            let template = match failure {
                MenuFailure::Open => tr("Couldn't open: {reason}"),
                MenuFailure::Copy => tr("Couldn't copy: {reason}"),
            };
            let text = fill(&template, &[("reason", &e.to_string())]);
            self.toast.show(&text, None, self.toast_at());
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
            self.toast.show(
                &tr("Those items are no longer there"),
                None,
                self.toast_at(),
            );
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
        for event in drag_end_events(&self.window_frame(&f), at, over) {
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
        self.trash_later(removed, trash_after, undo);
    }

    /// Empties the Pocket. The island's Undo puts the items back; vento's
    /// own files go to the Trash only once Undo is no longer offered.
    fn clear(&self) {
        let items = self.pocket.borrow_mut().clear();
        if items.is_empty() {
            return;
        }
        debug!("pocket cleared {} item(s)", items.len());
        self.save();
        self.refresh();
        self.trash_later(items, UNDO_WINDOW, true);
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
    /// which puts the items back instead.
    fn trash_later(&self, items: Vec<PocketItem>, after: Duration, undo: bool) {
        let job = self.pending.add(items);
        if undo {
            self.offer_undo(Undo::Pocket(job));
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

    /// Offers `undo` in the island for [`UNDO_LIFETIME`]. It replaces any
    /// Undo already offered: a Pocket job's files and a screenshot still go
    /// on their own timer, history entries are let go now. The island stays
    /// up meanwhile, even with nothing in it.
    fn offer_undo(&self, undo: Undo) {
        self.window.set_undo_kind(undo.kind());
        let replaced = self
            .undo
            .replace(Some((undo, Instant::now() + UNDO_LIFETIME)));
        if let Some((Undo::Clip(entries), _)) = replaced {
            self.clip.discard(entries);
        }
        self.window.set_undo_active(true);
        self.run_undo_timer();
        self.fit_panel();
    }

    /// Counts the offered Undo down, or withdraws the offer once it is up.
    /// Its ring is redrawn every [`UNDO_TICK`] while in sight; otherwise
    /// nothing runs until the offer ends (collapsed, the island shows no
    /// time). Call again whenever the ring may come into or go out of sight.
    fn run_undo_timer(&self) {
        let Some((kind, until)) = self
            .undo
            .borrow()
            .as_ref()
            .map(|(undo, until)| (undo.kind(), *until))
        else {
            return;
        };
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // A Pocket job's files and a screenshot go on their own timer.
            if let Some(Undo::Clip(entries)) = self.end_undo() {
                self.clip.discard(entries);
            }
            return;
        }
        let this = self.this.clone();
        let tick = move || {
            if let Some(c) = this.upgrade() {
                c.run_undo_timer();
            }
        };
        if self.expanded() && !self.card.is_up() && view_index(self.shown_view()) == kind {
            self.window
                .set_undo_left(left.as_secs_f32() / UNDO_LIFETIME.as_secs_f32());
            self.window
                .set_undo_secs(i32::try_from(left.as_millis().div_ceil(1000)).unwrap_or(0));
            if !self.undo_ticking.replace(true) {
                self.undo_timer.start(TimerMode::Repeated, UNDO_TICK, tick);
            }
        } else {
            self.undo_ticking.set(false);
            self.undo_timer.start(TimerMode::SingleShot, left, tick);
        }
    }

    /// The island's Undo: puts back what the offered Undo took.
    fn undo(&self) {
        match self.end_undo() {
            Some(Undo::Pocket(job)) => {
                if let Some(items) = self.pending.take(job) {
                    self.restore(items);
                }
            }
            Some(Undo::Clip(entries)) => self.clip.restore(entries),
            Some(Undo::Shot(path)) => self.undo_shot_trash(&path),
            None => {}
        }
    }

    /// Withdraws the offered Undo and returns it.
    fn end_undo(&self) -> Option<Undo> {
        let (undo, _) = self.undo.take()?;
        self.undo_timer.stop();
        self.undo_ticking.set(false);
        self.window.set_undo_active(false);
        self.fit_panel();
        Some(undo)
    }

    /// Moves every file still waiting for its Undo window or drop grace to
    /// the Trash now. Call when vento quits.
    pub fn flush_pending(&self) {
        // The history's entries an offered Undo holds are among those the
        // history still holds for an Undo, which this deletes.
        self.clip.flush();
        let items = self.pending.take_all();
        if !items.is_empty() {
            info!(
                "pocket: {} pending item(s) to the Trash at quit",
                items.len()
            );
        }
        self.trash_unheld(items);
        let shots = self.shots_view.take_pending();
        if !shots.is_empty() {
            info!("{} screenshot(s) to the Trash at quit", shots.len());
        }
        for path in shots {
            self.trash_shot(&path);
        }
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
        self.clip.set_paused(paused);
        if !paused || !self.warm.get() {
            return;
        }
        self.close_card();
        let after = self.settle_now();
        debug!("pocket paused: {after:?}");
    }

    /// Tells the island in the corner by the clock whether another window of
    /// vento's fills that corner now (the Quick panel, which opens above the
    /// tray icon): while `taken` says so, the island rests, and a rest on the
    /// taskbar opens it again only once the pointer has left the taskbar
    /// under it. An island on top ignores it.
    pub fn set_corner_taken(&self, taken: Box<dyn Fn() -> bool>) {
        *self.corner_taken.borrow_mut() = Some(taken);
    }

    /// Drops the island to its resting presence at once — no collapse delay,
    /// no waiting for the next poll tick — and releases the pointer. Used by
    /// pause and by closing the island with the shortcut.
    fn settle_now(&self) -> Presence {
        let has_items = self.has_content();
        let before = self.island.borrow().presence();
        let after = self.island.borrow_mut().settle(has_items);
        self.pin_until.set(None);
        if after != before {
            self.present(before, after);
        }
        if !self.shape_input(after) {
            self.set_hittest(false);
        }
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
        if !self.shape_input(Presence::Hidden) {
            self.set_hittest(false);
        }
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
                // By the clock, the drop box gives way to the whole island
                // for a moment, to show what came in.
                if self.island_frames().is_some_and(|f| f.corner) {
                    self.pin_until.set(Some(Instant::now() + DROP_CONFIRM));
                }
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

    /// Updates the island from one input sample; `dragging` is whether a drag
    /// of any type is under way. Called on every poll tick of the wheel.
    pub fn observe(&self, sample: &InputSample, dragging: bool) {
        self.clip.observe(sample.clip_change_count);
        if !self.warm.get() || self.paused.get() {
            return;
        }
        let now = Instant::now();
        // A card that never got to show (the island could not open) goes
        // once its time is up.
        if self.card.is_up() && !self.expanded() && !self.card.holds_open(now) {
            self.close_card();
        }
        let screens_changed = self.platform.screens_changed();
        if screens_changed || dragging && !self.was_dragging.get() {
            // The screens changed, or a drag may be headed for the island:
            // make sure it is where the screens are now, even when the
            // Pocket is empty.
            self.rehome();
        }
        self.was_dragging.set(dragging);
        // A drag from elsewhere takes the island for a drop: the card goes.
        if dragging && !self.latch.running() {
            self.close_card();
        }
        let Some(home) = self.home.get() else {
            return;
        };
        let corner = home.tray_corner.is_some();
        // Another window of vento's in the corner (the Quick panel) has it
        // to itself: the island rests at once. A drag still brings the drop
        // box out.
        let taken = corner && !dragging && self.corner_taken.borrow().as_ref().is_some_and(|f| f());
        if self.yielded.replace(taken) != taken && taken {
            debug!("pocket gives way to the quick panel");
            self.settle_now();
            self.hover_blocked.set(true);
        }
        if taken {
            return;
        }
        self.show_drop_box(corner && dragging && !self.latch.running(), now);
        let f = self.frames_of(&home);
        let before = self.island.borrow().presence();
        let has_items = self.has_content();
        let pinned = self.pinned_open(now);
        // The Pocket's own drag out leaves the island by the clock open only
        // while the pointer is over it, as on top, not wherever it goes.
        let drag_in = dragging && !(corner && self.latch.running());
        // After giving way, a rest on the collapsed shape counts again only
        // once the pointer has left it: it may still be on the tray icon
        // that closed the panel.
        if self.hover_blocked.get() && !geometry::contains(&f.collapsed, sample.cursor) {
            self.hover_blocked.set(false);
        }
        let mut inputs = island_inputs(&f, has_items, drag_in, sample.cursor, pinned);
        if self.hover_blocked.get() {
            inputs.hover = geometry::NOWHERE;
        }
        let mut after = self.island.borrow_mut().update(&inputs, now);
        // The card goes as the island leaves it, and the island rests as
        // it would without it.
        if after != Presence::Expanded && self.card.is_up() {
            self.close_card();
            if !self.has_content() {
                after = self.island.borrow_mut().settle(false);
            }
        }
        self.card_hover(after, hits(&f, after, sample.cursor), now);
        // Opened by resting on it: with an empty Pocket, on the view the
        // wings show.
        if before != Presence::Expanded
            && after == Presence::Expanded
            && !dragging
            && !pinned
            && !self.card.is_up()
        {
            let last = self.shown_view();
            let undo_view = self.undo.borrow().as_ref().map(|(u, _)| u.view());
            let v = hover_view(
                self.pocket.borrow().is_empty(),
                self.clip_has(),
                self.shots_have(),
                undo_view,
                last,
            );
            if v != last {
                self.set_view(v);
            }
        }
        let changed = after != before;
        if changed {
            self.rehome();
            self.present(before, after);
        }
        // A drag over the island is headed for the Pocket; one elsewhere
        // leaves a pinned Clipboard alone, and a screenshot's own drag out
        // (from a tile or the card) leaves the view as it is.
        let card_drag = self.card_drag.get() && self.latch.running();
        let headed_in = match self.view.get() {
            View::Clipboard => !card_drag,
            View::Screenshots => !self.latch.running(),
            View::Pocket => false,
        };
        if dragging && hits(&f, after, sample.cursor) && headed_in {
            self.set_view(View::Pocket);
        }
        // One over the Pocket list (not the list's own drag out) goes back
        // to the kinds, which say what a drop does.
        if dragging && !self.latch.running() && hits(&f, after, sample.cursor) && self.list_shown()
        {
            self.close_pocket_list();
        }
        let f = self.island_frames().unwrap_or(f);
        self.window.set_bar_hover(
            home.slot.is_some()
                && after == Presence::Collapsed
                && geometry::contains(&f.collapsed, sample.cursor),
        );
        if !self.shape_input(after) {
            let inside = hits(&f, after, sample.cursor);
            if changed || inside != self.hittest.get() {
                self.set_hittest(inside);
            }
        }
        // Only an expanded island takes drops, only during a drag, and never
        // the Pocket's own drag out.
        let catches = dragging && after == Presence::Expanded && !self.latch.running();
        self.place_catcher(catches.then_some(f.expanded));
        if !sample.left_down {
            self.replay_hover(&f, after, sample.cursor);
        }
    }

    /// Turns the drop box of the island by the clock on or off. On, it shows
    /// the Pocket, only where to drop. Off with no drop to show (the drag
    /// ended elsewhere), the island goes back at once rather than growing
    /// into the whole island on its way out.
    fn show_drop_box(&self, on: bool, now: Instant) {
        if self.drop_box.replace(on) == on {
            return;
        }
        debug!("pocket drop box {on}");
        self.window.set_drop_box(on);
        if on {
            self.close_pocket_list();
            self.set_view(View::Pocket);
        } else if !self.pinned_open(now) {
            self.settle_now();
        }
        self.fit_panel();
    }

    /// Passes hover moves over the expanded island to the window, which
    /// gets none from AppKit while vento isn't the active app. A press is
    /// left alone: its moves always arrive.
    fn replay_hover(&self, f: &Frames, presence: Presence, cursor: Point) {
        let window = (presence == Presence::Expanded).then(|| self.window_frame(f));
        let last = self.last_cursor.replace(Some(cursor));
        let (event, inside) = hover_event(window.as_ref(), cursor, last, self.pointer_in.get());
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

    /// Lets the window take the pointer only over its shape for
    /// `presence`, wherever the pointer is. GNOME applies a new input shape
    /// only when the window next draws, so one switched on under a resting
    /// pointer would let presses fall through to the desktop. False where
    /// the platform can't; the window then follows the pointer through
    /// `set_hittest`.
    fn shape_input(&self, presence: Presence) -> bool {
        if !self.input_by_shape.get() {
            return false;
        }
        let rect = self.input_rect(presence);
        if self.input_shape.get() == Some(rect) {
            return true;
        }
        let result = match self.window.window().window_handle().window_handle() {
            Ok(handle) => self.platform.set_input_rect(handle.as_raw(), rect),
            Err(e) => Err(PlatformError::Failed(e.to_string())),
        };
        match result {
            Ok(()) => {
                debug!("pocket input shape {rect:?}");
                self.input_shape.set(Some(rect));
                true
            }
            Err(PlatformError::Unsupported) => {
                self.input_by_shape.set(false);
                false
            }
            Err(e) => {
                warn!("cannot set the pocket's input shape: {e}");
                false
            }
        }
    }

    /// The window's shape for `presence`, in physical pixels from its
    /// top-left corner; `None` when hidden or not placed yet.
    fn input_rect(&self, presence: Presence) -> Option<Rect> {
        let f = self.island_frames()?;
        let r = interactive_rect(&f, presence)?;
        let window = self.window_frame(&f);
        let scale = self.window.window().scale_factor() as f64;
        Some(Rect {
            x: (r.x - window.x) * scale,
            y: (r.y - window.y) * scale,
            w: r.w * scale,
            h: r.h * scale,
        })
    }

    fn pinned_open(&self, now: Instant) -> bool {
        if self.pin_until.get().is_some_and(|until| now >= until) {
            self.pin_until.set(None);
        }
        self.pinned.get() || self.pin_until.get().is_some() || self.card.holds_open(now)
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
        // The next opening shows the look the settings say, with no peek
        // open; a collapse during the Pocket's own drag out keeps what it
        // shows, so rows can be dragged out one after another.
        if after != Presence::Expanded && !self.latch.running() {
            self.reset_look();
        }
        self.window.set_expanded(after == Presence::Expanded);
        self.run_undo_timer();
        if after == Presence::Hidden {
            self.hide_window();
        } else if before == Presence::Hidden {
            if !self.overlay_ready.get() {
                self.make_overlay();
            }
            // Clicks fall through until the pointer is over the shape.
            if !self.shape_input(after) {
                self.set_hittest(false);
            }
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

    /// Sizes the window for `home` and places it at the top centre (or in
    /// the corner by the clock). The island grows or shrinks to its new
    /// height as it animates; a window that shrinks waits for it, so the
    /// shrinking is seen.
    fn apply_home(&self, home: &IslandHome) {
        let f = self.frames_of(home);
        let panel_h = (f.expanded.h - f.top_h) as f32;
        self.window.set_panel_w(f.expanded.w as f32);
        self.window.set_panel_h(panel_h);
        if panel_h >= self.window.get_window_h() || !self.expanded() {
            self.shrink_timer.stop();
            self.window.set_window_h(panel_h);
        } else {
            let this = self.this.clone();
            self.shrink_timer
                .start(TimerMode::SingleShot, SHRINK_DELAY, move || {
                    if let Some(c) = this.upgrade() {
                        c.window.set_window_h(c.window.get_panel_h());
                        if let Some(f) = c.island_frames() {
                            c.place_window(&f);
                        }
                    }
                });
        }
        self.window.set_corner_style(f.corner);
        self.window.set_notch_style(f.notch_style);
        self.window.set_notch_w(f.notch_w as f32);
        self.window.set_top_h(f.top_h as f32);
        self.place_window(&f);
    }

    /// Moves the window to where it goes for `f`, as tall as it is now.
    fn place_window(&self, f: &Frames) {
        let w = self.window_frame(f);
        self.window
            .window()
            .set_position(LogicalPosition::new(w.x as f32, w.y as f32));
    }

    /// The window's frame for `f`, as tall as it is now.
    fn window_frame(&self, f: &Frames) -> Rect {
        window_frame(f, f64::from(self.window.get_window_h()))
    }

    /// Adds `paths` (as references, or as kept copies when the Pocket keeps
    /// copies), saves, and keeps the island open for `pin_open_for` to show
    /// them. Paths already in the Pocket are skipped.
    pub fn add(&self, paths: Vec<PathBuf>, pin_open_for: Duration) {
        self.add_dropped(Dropped::Files(paths));
        self.pin_until.set(Some(Instant::now() + pin_open_for));
    }

    /// Adds what was dropped on the island. Files already in the Pocket, and
    /// text, links or images it already saved, are skipped silently;
    /// anything that fails is reported in a toast. Kept copies are made on a
    /// worker thread and added when done; each drop runs its own job, so
    /// drops keep working meanwhile.
    fn add_dropped(&self, d: Dropped) {
        let keep_copies = self.settings.borrow().pocket_keep_copies;
        let (plan, print, held) = {
            let pocket = self.pocket.borrow();
            let copying = self.copying.borrow();
            let plan = plan_new(&d, keep_copies, |p| {
                pocket.contains(p) || copying.iter().any(|c| c == p)
            });
            let saved = pocket
                .items()
                .iter()
                .filter(|i| i.source == ItemSource::Saved)
                .map(|i| i.path.as_path());
            let print = saved_content(&plan).map(|bytes| Print::of(&bytes));
            let held = print.is_some_and(|p| self.saved_prints.borrow_mut().holds(saved, p));
            (plan, print, held)
        };
        match plan {
            _ if held => debug!("pocket already holds this content"),
            AddPlan::Copies(paths) if paths.is_empty() => {}
            AddPlan::Copies(paths) => self.spawn_copies(paths),
            plan => {
                let (entries, errors) = store(plan, &pocket_dir());
                if let Some(print) = print {
                    let mut prints = self.saved_prints.borrow_mut();
                    for (path, _, _) in &entries {
                        prints.remember(path, print);
                    }
                }
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
                        Err(_) => controller
                            .finish_add(Vec::new(), &[Failure::new(|| tr("internal error"))]),
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report finished pocket copies: {e}");
                }
            });
        if let Err(e) = spawned {
            self.end_copies(&paths);
            let failure = Failure::new(|| {
                fill(
                    &tr("cannot start copying: {reason}"),
                    &[("reason", &io_reason(&e))],
                )
            });
            self.finish_add(Vec::new(), &[failure]);
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
    fn finish_add(&self, entries: Vec<Entry>, errors: &[Failure]) {
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
        if let Some(first) = errors.first() {
            let logged: Vec<&str> = errors.iter().map(|f| f.logged.as_str()).collect();
            warn!("cannot add to the pocket: {}", logged.join("; "));
            let text = fill(
                &tr("Couldn't add to Pocket: {reason}"),
                &[("reason", &first.shown)],
            );
            self.toast.show(&text, None, self.toast_at());
        }
    }

    /// The Pocket and Clipboard shortcuts: keeps the island open on view
    /// `v`; switches to it if the island is held open on another view;
    /// lets it close if it already shows `v`.
    pub fn open_view(&self, v: View) {
        self.close_card();
        let current = self.pinned.get().then(|| self.shown_view());
        let next = next_open(current, v);
        self.pinned.set(next.is_some());
        debug!("pocket pinned on {next:?}");
        match next {
            Some(v) => self.set_view(v),
            // Closing with the shortcut is a request to close now: skip the
            // hover collapse delay that would otherwise hold the island open.
            None if self.warm.get() && !self.paused.get() => {
                self.settle_now();
            }
            None => {}
        }
    }

    /// Follows the history setting of `s`. Switched on or off, the next
    /// sample only sets the baseline, so what is on the clipboard then is
    /// not recorded. Switched off, the island falls back to the Pocket view
    /// and an Undo of the history is withdrawn; the stored history stays.
    pub fn apply_clip_settings(&self, s: &Settings) {
        let on = s.clipboard_history;
        if on == self.clip.enabled() {
            return;
        }
        debug!("clipboard history {}", if on { "on" } else { "off" });
        self.clip.set_enabled(on);
        if !on {
            self.withdraw_clip_undo();
            self.set_view(View::Pocket);
        }
        self.refresh();
        // The view shown, and so the ring in sight, may have changed.
        self.run_undo_timer();
    }

    /// Follows the screenshots setting of `s`: switched on, the folder is
    /// listed at once, so the system asks for access right then; switched
    /// off, the watch and the list go and the island falls back to the
    /// Pocket view.
    pub fn apply_shot_settings(&self, s: &Settings) {
        if s.screenshots == (self.shots.access() != Access::Off) {
            return;
        }
        self.shots.apply(s);
        if !s.screenshots && self.view.get() == View::Screenshots {
            self.set_view(View::Pocket);
        }
        self.refresh();
        self.shots_changed();
        // The view shown, and so the ring in sight, may have changed.
        self.run_undo_timer();
    }

    /// Whether the screenshot folder is being read, and the folder (read
    /// from the system when the feature was last switched on or
    /// [`shots_touch`](Self::shots_touch)ed): for the Settings page.
    pub fn shots_access(&self) -> (Access, PathBuf) {
        (self.shots.access(), self.shots.folder())
    }

    /// The Settings page of the screenshots opened: reads the folder again,
    /// as opening the tab does, and refreshes the island with the result.
    pub fn shots_touch(&self) {
        self.shots.touch();
        self.refresh();
        self.shots_changed();
    }

    /// Has the Settings window call `f` whenever the access to the
    /// screenshot folder may have changed (a listing came back, the feature
    /// went on or off, the page was opened).
    pub fn set_on_shots_changed(&self, f: Box<dyn Fn()>) {
        *self.on_shots_changed.borrow_mut() = Some(f);
    }

    fn shots_changed(&self) {
        if let Some(f) = self.on_shots_changed.borrow().as_ref() {
            f();
        }
    }

    /// Follows the Pocket look of `s`: a change shows it at once.
    pub fn apply_pocket_settings(&self, s: &Settings) {
        let look = s.pocket_view;
        if self.pocket_view.replace(look) == look {
            return;
        }
        debug!("pocket view {look:?}");
        self.reset_look();
    }

    /// Writes the texts Rust pushes into the island again after a language
    /// change: the kind names of the groups, the filter chips of the list,
    /// the clipboard history's labels and ages and the screenshots' days and
    /// times. Only what is in sight is made, as for any refresh.
    pub fn apply_language(&self) {
        self.refresh_kinds();
        self.refresh_list();
        self.refresh_clip();
        self.refresh_shots();
    }

    /// Empties the clipboard history at once, with no Undo (the settings'
    /// "Clear history"). An Undo of the history on offer is withdrawn too,
    /// its entries let go with the rest, and a history file set aside as
    /// unusable is deleted.
    pub fn clear_clip_history(&self) {
        let gone = self.clip.clear();
        self.clip.discard(gone);
        self.withdraw_clip_undo();
        self.clip.forget_backup();
        self.refresh();
    }

    /// Withdraws the Undo on offer if it puts history entries back, and
    /// lets those entries go.
    fn withdraw_clip_undo(&self) {
        let is_clip = matches!(*self.undo.borrow(), Some((Undo::Clip(_), _)));
        if is_clip && let Some(Undo::Clip(entries)) = self.end_undo() {
            self.clip.discard(entries);
        }
    }

    /// How many items the Pocket holds.
    pub fn item_count(&self) -> usize {
        self.pocket.borrow().items().len()
    }

    /// Pushes the count into the window, and the clipboard history and the
    /// screenshots with it.
    /// The tiles are rebuilt only while the island is expanded (a hidden or
    /// collapsed island shows none), so a copy made meanwhile costs no look
    /// at every file; [`present`](Self::present) refreshes as it expands.
    pub fn refresh(&self) {
        self.refresh_kinds();
        self.refresh_list();
        let count = self.item_count();
        self.window
            .set_count(i32::try_from(count).unwrap_or(i32::MAX));
        self.refresh_clip();
        self.refresh_shots();
        self.fit_panel();
    }

    /// The icon a tile or a row shows for `item`: its Finder icon, converted
    /// once and kept in `icons`, or `fallback` while its file is missing
    /// (`gone`) or has no icon.
    fn item_icon(
        &self,
        icons: &mut HashMap<u64, Image>,
        item: &PocketItem,
        gone: bool,
        fallback: &Image,
    ) -> Image {
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
    }

    /// The island is expanded.
    fn expanded(&self) -> bool {
        self.island.borrow().presence() == Presence::Expanded
    }

    /// Sizes the panel for the view shown.
    fn fit_panel(&self) {
        let Some(home) = self.home.get() else {
            return;
        };
        let f = self.frames_of(&home);
        if self.window.get_panel_w() != f.expanded.w as f32
            || self.window.get_panel_h() != (f.expanded.h - f.top_h) as f32
        {
            self.apply_home(&home);
        }
    }

    /// The island has something to show: items, an Undo on offer, a
    /// clipboard history, screenshots in their view, or the card.
    fn has_content(&self) -> bool {
        !self.pocket.borrow().is_empty()
            || self.undo.borrow().is_some()
            || self.clip_has()
            || self.shots_have()
            || self.card.is_up()
    }

    fn save(&self) {
        if let Err(e) = self.pocket.borrow().save(&self.path) {
            warn!("cannot save the pocket to {}: {e}", self.path.display());
            let text = fill(
                &tr("Couldn't save the Pocket: {error}"),
                &[("error", &io_reason(&e))],
            );
            self.toast.show(&text, None, self.toast_at());
        }
    }

    /// Below the middle of the expanded island; above it by the clock,
    /// where the taskbar is below.
    fn toast_at(&self) -> Point {
        self.island_frames().map_or(Point { x: 0.0, y: 0.0 }, |f| {
            let e = f.expanded;
            let mid = e.x + e.w / 2.0;
            if f.corner {
                crate::toast::anchor_above(Point { x: mid, y: e.y })
            } else {
                Point {
                    x: mid,
                    y: e.y + e.h,
                }
            }
        })
    }

    /// The island's frames for `home`, sized for the view it shows now.
    fn frames_of(&self, home: &IslandHome) -> Frames {
        frames(home, self.panel())
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

    #[test]
    fn open_view_toggles_and_switches() {
        use View::{Clipboard, Pocket, Screenshots};
        assert_eq!(next_open(None, Clipboard), Some(Clipboard));
        assert_eq!(next_open(None, Pocket), Some(Pocket));
        // Asked for the view it shows: it closes.
        assert_eq!(next_open(Some(Clipboard), Clipboard), None);
        assert_eq!(next_open(Some(Pocket), Pocket), None);
        // Asked for the other view: it switches and stays open.
        assert_eq!(next_open(Some(Pocket), Clipboard), Some(Clipboard));
        assert_eq!(next_open(Some(Clipboard), Pocket), Some(Pocket));
        assert_eq!(next_open(Some(Clipboard), Screenshots), Some(Screenshots));
        assert_eq!(next_open(Some(Screenshots), Screenshots), None);
    }

    #[test]
    fn hover_opens_the_view_that_has_something() {
        use View::{Clipboard, Pocket, Screenshots};
        // Items in the Pocket: the last view, as before.
        assert_eq!(
            hover_view(false, true, true, None, Screenshots),
            Screenshots
        );
        assert_eq!(hover_view(false, false, true, None, Pocket), Pocket);
        assert_eq!(hover_view(false, true, true, None, Clipboard), Clipboard);
        // An empty Pocket with some history: the last view, unchanged.
        assert_eq!(hover_view(true, true, false, None, Pocket), Pocket);
        assert_eq!(hover_view(true, true, true, None, Pocket), Screenshots);
        assert_eq!(hover_view(true, true, true, None, Clipboard), Clipboard);
        // The last view has nothing and there are screenshots: those.
        assert_eq!(hover_view(true, false, true, None, Pocket), Screenshots);
        assert_eq!(hover_view(true, false, true, None, Clipboard), Screenshots);
        assert_eq!(hover_view(false, false, true, None, Clipboard), Screenshots);
        // Nothing anywhere: the last view.
        assert_eq!(hover_view(true, false, false, None, Clipboard), Clipboard);
        assert_eq!(hover_view(true, false, false, None, Pocket), Pocket);
    }

    #[test]
    fn hover_opens_where_the_undo_is() {
        use View::{Clipboard, Pocket, Screenshots};
        // The history was cleared (empty, its Undo on offer): the
        // Clipboard, though there are screenshots.
        let clip = Undo::Clip(Vec::new()).view();
        assert_eq!(clip, Clipboard);
        assert_eq!(hover_view(true, false, true, Some(clip), Pocket), Clipboard);
        // The last items were taken out: the Pocket, though there is history.
        let pocket = Undo::Pocket(1).view();
        assert_eq!(pocket, Pocket);
        assert_eq!(
            hover_view(true, true, true, Some(pocket), Clipboard),
            Pocket
        );
        // A screenshot was trashed: the Screenshots.
        let shot = Undo::Shot(PathBuf::from("/tmp/a.png")).view();
        assert_eq!(shot, Screenshots);
        assert_eq!(
            hover_view(false, true, false, Some(shot), Pocket),
            Screenshots
        );
    }

    #[test]
    fn each_undo_belongs_to_its_view() {
        // The window shows a ring in the view whose number is the kind.
        assert_eq!(Undo::Pocket(3).kind(), 0);
        assert_eq!(Undo::Clip(Vec::new()).kind(), 1);
        assert_eq!(
            Undo::Shot(PathBuf::from("/tmp/a.png")).kind(),
            view_index(View::Screenshots)
        );
        assert_eq!(view_index(View::Screenshots), 3);
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
