//! The wheel window and the controller that drives it from polled input.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::appearance;
use crate::pocket::to_image;
use crate::ui::{Icons, RingCell, Wheel, WheelCell, WheelLayout};
use log::{debug, warn};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{
    ComponentHandle, Image, LogicalPosition, LogicalSize, Model, ModelRc, Timer, TimerMode,
    VecModel,
};
use vento_core::actions::ActionId;
use vento_core::archive::looks_like_archives;
use vento_core::convert::{ImageFormat, ring_formats};
use vento_core::destinations::{
    CellLabel, Destination, LABEL_CHARS, PETAL_LABEL_CHARS, RecentDestinations, cell_labels,
    display_path, fit_path, folder_exists, ring_destinations,
};
use vento_core::geometry::{
    self, HEX_CORNER_RADIUS, HEX_RADIUS, Layout, PETAL_DEAD_ZONE, Point, Rect, RingGeometry, Vec2,
    WheelGeometry, place_center, rounded_hex_path, screen_for,
};
use vento_core::i18n::tr;
use vento_core::pdf::looks_like_pdf;
use vento_core::ring::{Hover, RingEvent, RingTracker};
use vento_core::settings::Settings;
use vento_core::trigger::{InputSample, TriggerEvent, TriggerMachine};
use vento_platform::{Platform, RgbaImage};

/// Where the wheel window waits, out of sight, while it is pre-warmed.
const PARKED: LogicalPosition = LogicalPosition {
    x: -5000.0,
    y: -5000.0,
};
/// How long after a release a drop may still arrive before it counts as cancelled.
const RELEASE_GRACE: Duration = Duration::from_millis(250);
/// winit delivers one `DroppedFile` per file; collect them for this long.
const DROP_BATCH: Duration = Duration::from_millis(30);
/// Tick on which the parked window is shown, so the native window and renderer exist.
const WARM_SHOW_TICK: u32 = 1;
/// Tick on which the parked window is hidden again and the drop handler registered.
const WARM_HIDE_TICK: u32 = 3;
/// Size of the folder icons fetched for the ring, in pixels.
const FOLDER_ICON_PX: u32 = 40;
/// Size of the destination label under the ring (as drawn in `wheel.slint`).
const DEST_LABEL_W: f32 = 280.0;
const DEST_LABEL_H: f32 = 44.0;
/// The most characters of a folder's path the destination label shows (its
/// 10 px line is about 228 px wide); longer paths lose leading folders.
const DEST_PATH_CHARS: usize = 36;
/// Space between the lowest cell and the destination label.
const DEST_LABEL_GAP: f32 = 24.0;
/// Room kept between the destination label and the screen or window edge.
const DEST_LABEL_MARGIN: f32 = 4.0;

/// What the wheel window shows and the cursor has done since it opened.
struct State {
    ticks: u32,
    settings: Settings,
    geometry: WheelGeometry,
    /// Wheel centre in global logical coordinates while the wheel is open.
    center: Point,
    /// The screen work area the wheel opened on.
    screen: Option<Rect>,
    highlighted: Option<usize>,
    /// Cursor offset from the centre at the last hover, to tell a drop on the
    /// centre from one outside every cell.
    last_offset: Vec2,
    dropped: Vec<PathBuf>,
    /// Opens and closes the destination ring from the hover.
    tracker: RingTracker,
    /// The destination ring, while it is open.
    ring: Option<OpenRing>,
    /// The ring destination under the cursor.
    ring_hot: Option<usize>,
    /// Whether the files being dragged all look like archives, so the
    /// Archive cell reads "Extract" rather than "Compress".
    extracting: bool,
    /// Whether none of the files being dragged looks like a PDF, so the PDF
    /// cell is drawn dimmed (a drop on it still goes to the PDF window,
    /// which says there is nothing to open).
    pdf_dimmed: bool,
}

/// The destination ring around one action.
struct OpenRing {
    /// Index of the action the ring opened from.
    source: usize,
    geometry: RingGeometry,
    /// What each ring cell stands for, in cell order.
    items: RingItems,
}

/// What a ring holds: folders for Move and Copy to, image formats for Convert.
enum RingItems {
    Folders(Vec<Destination>),
    Formats(Vec<ImageFormat>),
}

impl RingItems {
    fn len(&self) -> usize {
        match self {
            RingItems::Folders(dests) => dests.len(),
            RingItems::Formats(formats) => formats.len(),
        }
    }

    fn truncate(&mut self, len: usize) {
        match self {
            RingItems::Folders(dests) => dests.truncate(len),
            RingItems::Formats(formats) => formats.truncate(len),
        }
    }
}

impl State {
    /// What the cursor at `offset` from the centre is over.
    fn classify(&self, offset: Vec2) -> Hover {
        let ring = self.ring.as_ref().map(|r| (r.source, &r.geometry));
        classify(
            &self.geometry,
            self.settings.layout,
            self.settings.actions.len(),
            ring,
            offset,
        )
    }
}

/// What the cursor at `offset` from the centre is over, on a wheel laid out
/// as `wheel` (`layout`) with `actions` actions and, while one is open, the
/// ring `(source, ring)`. Ring cells come first, as they may sit where wheel
/// actions are drawn.
///
/// A petal's hit area is its whole sector, out to `PETAL_MAX_REACH`.
/// While a ring is open, the other petals count only inside their drawn
/// circle, so a straight path from the source to a side ring petal, which
/// crosses a neighbour's sector, does not close the ring.
fn classify(
    wheel: &WheelGeometry,
    layout: Layout,
    actions: usize,
    ring: Option<(usize, &RingGeometry)>,
    offset: Vec2,
) -> Hover {
    if let Some(d) = ring.and_then(|(_, r)| r.hit(offset)) {
        return Hover::Destination(d);
    }
    let on_drawn_petal = |i: usize| {
        let cell = &wheel.cells()[i];
        (offset.x - cell.center.x).hypot(offset.y - cell.center.y) <= cell.radius
    };
    let counts = |i: usize| match (layout, ring) {
        (Layout::Petals, Some((source, _))) => i == source || on_drawn_petal(i),
        _ => true,
    };
    if let Some(i) = wheel.hit(offset).filter(|&i| i < actions && counts(i)) {
        return Hover::Action(i);
    }
    let distance = offset.x.hypot(offset.y);
    if distance < centre_radius(layout) {
        return Hover::Centre;
    }
    match ring {
        Some((_, ring)) if distance <= ring.extent() => Hover::Gap,
        _ => Hover::Outside,
    }
}

/// The icon `action` shows on the wheel and in the Settings window.
pub fn action_icon(icons: &Icons<'_>, action: ActionId) -> Image {
    match action {
        ActionId::Pocket => icons.get_pocket(),
        ActionId::Copy => icons.get_copy(),
        ActionId::Move => icons.get_move(),
        ActionId::CopyPath => icons.get_copy_path(),
        ActionId::Terminal => icons.get_terminal(),
        ActionId::CopyTo => icons.get_copy_to(),
        ActionId::Archive => icons.get_archive(),
        ActionId::Info => icons.get_info(),
        ActionId::Convert => icons.get_convert(),
        ActionId::Pdf => icons.get_pdf(),
    }
}

/// What the wheel calls `action`: the Archive cell names what a drop will
/// do with the files being dragged.
fn action_text(action: ActionId, extracting: bool) -> String {
    match action {
        ActionId::Archive if extracting => tr("Extract"),
        ActionId::Archive => tr("Compress"),
        other => other.label(),
    }
}

/// Whether the PDF cell is dimmed for `paths`: none of them is named like a
/// PDF. A name check only, so it fits the wheel's opening budget.
fn pdf_cell_dimmed(paths: &[PathBuf]) -> bool {
    !paths.iter().any(|path| looks_like_pdf(path))
}

/// Whether the cell of `action` is drawn dimmed, given `pdf_dimmed`.
fn cell_dimmed(action: ActionId, pdf_dimmed: bool) -> bool {
    pdf_dimmed && action == ActionId::Pdf
}

/// The icon `action` shows on the wheel, matching [`action_text`].
fn wheel_icon(icons: &Icons<'_>, action: ActionId, extracting: bool) -> Image {
    match action {
        ActionId::Archive if extracting => icons.get_extract(),
        other => action_icon(icons, other),
    }
}

/// Radius of the wheel's centre, inside which a drop does nothing.
fn centre_radius(layout: Layout) -> f32 {
    match layout {
        Layout::Honeycomb => HEX_RADIUS,
        Layout::Petals => PETAL_DEAD_ZONE,
    }
}

/// A tracker that opens the ring of the actions in `settings` that take one,
/// after the hold delay.
fn ring_tracker(settings: &Settings) -> RingTracker {
    let actions = settings.actions.clone();
    RingTracker::new(
        Duration::from_millis(u64::from(settings.move_hold_ms)),
        move |i| actions.get(i).is_some_and(|a| a.takes_ring()),
    )
}

/// What a drop on the wheel asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct WheelDrop {
    pub action: ActionId,
    pub paths: Vec<PathBuf>,
    /// Wheel centre, in global logical coordinates.
    pub at: Point,
    /// The ring folder dropped on; `None` for a drop on the action itself.
    pub dest: Option<PathBuf>,
    /// The ring format dropped on; `Some` only for a drop on a Convert ring cell.
    pub format: Option<ImageFormat>,
}

/// The verb in front of the ring cell under the cursor.
fn ring_verb(action: ActionId) -> String {
    match action {
        ActionId::CopyTo => tr("Copy to"),
        ActionId::Convert => tr("Convert to"),
        _ => tr("Move to"),
    }
}

/// The text on the ring cell of `format`: its name and extension.
fn format_label(format: ImageFormat) -> CellLabel {
    CellLabel {
        line1: format.name().to_string(),
        line2: Some(format!(".{}", format.extension())),
        line2_dim: true,
    }
}

/// The ring's folders as last checked, and their icons.
struct RingFolders {
    /// The recent then pinned folders that existed at the last check;
    /// `None` until a check has reported.
    dests: Option<Vec<Destination>>,
    /// Finder icons of those folders, by path, drawn at `scale`.
    icons: HashMap<PathBuf, Image>,
    scale: f64,
}

/// What the folder worker is asked to check.
struct FolderCheck {
    recent: Vec<PathBuf>,
    pinned: Vec<PathBuf>,
    /// Pixels per point to draw icons at.
    scale: f64,
    /// Folders whose icons the main thread already has at `scale`.
    have_icons: HashSet<PathBuf>,
}

/// What the folder worker found.
struct FolderReport {
    dests: Vec<Destination>,
    scale: f64,
    /// Icons of the folders in `dests` that were not in `have_icons`; `None`
    /// where the system has none.
    icons: Vec<(PathBuf, Option<RgbaImage>)>,
}

/// Checks which ring folders exist and draws their icons on a thread of its
/// own, so a drag never waits on the file system (a sleeping disk, a gone
/// network volume) or on icon rendering. Reports are collected on the poll
/// tick. The thread ends when this is dropped.
struct FolderWorker {
    checks: Sender<FolderCheck>,
    reports: Receiver<FolderReport>,
}

impl FolderWorker {
    /// `None` if the thread cannot be started.
    fn spawn() -> Option<Self> {
        let (checks, inbox) = mpsc::channel::<FolderCheck>();
        let (outbox, reports) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("vento-ring-folders".into())
            .spawn(move || {
                while let Ok(mut check) = inbox.recv() {
                    // Only the newest check matters.
                    while let Ok(newer) = inbox.try_recv() {
                        check = newer;
                    }
                    if outbox.send(check_folders(check)).is_err() {
                        break;
                    }
                }
            });
        match spawned {
            Ok(_) => Some(Self { checks, reports }),
            Err(e) => {
                warn!("cannot start the ring folder worker: {e}");
                None
            }
        }
    }
}

/// Runs `check`: which folders exist, and the icons not yet drawn. Blocks on
/// the file system; any thread.
fn check_folders(check: FolderCheck) -> FolderReport {
    let dests = ring_destinations(&check.recent, &check.pinned, folder_exists);
    let icons = dests
        .iter()
        .filter(|d| !check.have_icons.contains(&d.path))
        .map(|d| {
            let icon = vento_platform::file_icon_at(&d.path, FOLDER_ICON_PX, check.scale);
            (d.path.clone(), icon)
        })
        .collect();
    FolderReport {
        dests,
        scale: check.scale,
        icons,
    }
}

/// Whether `settings` has an action whose ring shows folders.
fn has_folder_ring(settings: &Settings) -> bool {
    settings
        .actions
        .iter()
        .any(|a| matches!(a, ActionId::Move | ActionId::CopyTo))
}

/// Receives every input sample and whether a drag (of any type) is under way.
type SampleListener = Box<dyn Fn(&InputSample, bool)>;

/// Shows the wheel under the cursor while files are dragged and Ctrl is
/// pressed, and reports the action a file is dropped on.
pub struct WheelController {
    platform: Rc<dyn Platform>,
    window: Wheel,
    on_drop: Box<dyn Fn(WheelDrop)>,
    /// Folders recently moved or copied to; written by the executor.
    recent: Rc<RefCell<RecentDestinations>>,
    /// The home folder, shown as `~` in destination paths.
    home: Option<PathBuf>,
    /// The ring's folders and their icons, as the worker last reported.
    folders: RefCell<RingFolders>,
    /// Checks the ring's folders off the main thread; `None` if it could not
    /// start, and the checks then run on the main thread.
    folder_worker: Option<FolderWorker>,
    machine: RefCell<TriggerMachine>,
    state: RefCell<State>,
    paused: Cell<bool>,
    poll: Timer,
    grace: Timer,
    batch: Timer,
    sample_listener: RefCell<Option<SampleListener>>,
    this: Weak<Self>,
}

impl WheelController {
    /// Creates the (hidden) wheel window. Call [`start`](Self::start) to begin
    /// polling.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    pub fn new(
        platform: Rc<dyn Platform>,
        settings: Settings,
        recent: Rc<RefCell<RecentDestinations>>,
        on_drop: Box<dyn Fn(WheelDrop)>,
    ) -> Rc<Self> {
        let window = Wheel::new().expect("cannot create the wheel window");
        // The wheel and its ring never mirror (a gesture around the
        // pointer), but its `Theme` knows the direction like every window's.
        appearance::follow(&window, appearance::Look::Direction);
        let geometry = WheelGeometry::new(settings.layout, settings.actions.len());
        let machine = TriggerMachine::new(settings.trigger).with_shake(settings.shake_to_open);
        let tracker = ring_tracker(&settings);
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            window,
            on_drop,
            recent,
            home: std::env::var_os("HOME").map(PathBuf::from),
            folders: RefCell::new(RingFolders {
                dests: None,
                icons: HashMap::new(),
                scale: 1.0,
            }),
            folder_worker: FolderWorker::spawn(),
            machine: RefCell::new(machine),
            state: RefCell::new(State {
                ticks: 0,
                settings,
                geometry,
                center: Point { x: 0.0, y: 0.0 },
                screen: None,
                highlighted: None,
                last_offset: Vec2 { x: 0.0, y: 0.0 },
                dropped: Vec::new(),
                tracker,
                ring: None,
                ring_hot: None,
                extracting: false,
                pdf_dimmed: false,
            }),
            paused: Cell::new(false),
            poll: Timer::default(),
            grace: Timer::default(),
            batch: Timer::default(),
            sample_listener: RefCell::new(None),
            this: this.clone(),
        });
        controller.render_geometry();
        controller
    }

    /// Pre-warms the window on the first ticks, then polls for the trigger.
    pub fn start(self: &Rc<Self>) {
        let this = self.this.clone();
        let interval = self.machine.borrow().poll_interval();
        self.poll.start(TimerMode::Repeated, interval, move || {
            if let Some(controller) = this.upgrade() {
                controller.tick();
                controller.rearm();
            }
        });
    }

    /// While paused, input is not sampled and the poll stops (once the
    /// window is pre-warmed). An open wheel is dismissed. Unpausing starts
    /// from a fresh trigger state, so nothing seen before the pause can open
    /// the wheel.
    pub fn set_paused(&self, paused: bool) {
        self.paused.set(paused);
        if paused {
            if self.machine.borrow().is_open() {
                self.dismiss();
            }
            if self.warmed_up() {
                self.poll.stop();
            }
        } else {
            self.reset_trigger();
            if !self.poll.running() {
                // Not running: this only sets the interval `restart` uses.
                self.poll
                    .set_interval(self.machine.borrow().poll_interval());
                self.poll.restart();
            }
        }
    }

    /// Whether the window has been pre-warmed (and the drop handler set).
    fn warmed_up(&self) -> bool {
        self.state.borrow().ticks >= WARM_HIDE_TICK
    }

    /// Hands every input sample to `f`, with whether a drag of any type is
    /// under way, on the poll tick that took it (also while the wheel is
    /// open). Nothing is sampled, so nothing is passed on, while paused.
    pub fn set_sample_listener(&self, f: SampleListener) {
        *self.sample_listener.borrow_mut() = Some(f);
    }

    fn reset_trigger(&self) {
        let (trigger, shake) = {
            let state = self.state.borrow();
            (state.settings.trigger, state.settings.shake_to_open)
        };
        *self.machine.borrow_mut() = TriggerMachine::new(trigger).with_shake(shake);
    }

    /// Replaces the settings; the layout and the number of actions may change.
    pub fn apply_settings(&self, settings: Settings) {
        if self.machine.borrow().is_open() {
            self.dismiss();
        }
        {
            let mut state = self.state.borrow_mut();
            state.geometry = WheelGeometry::new(settings.layout, settings.actions.len());
            state.tracker = ring_tracker(&settings);
            state.settings = settings;
        }
        self.reset_trigger();
        self.render_geometry();
        // Pinned folders may have changed; the warm-up runs the first check.
        if self.warmed_up() {
            self.check_folders(None);
        }
    }

    /// Pushes the current geometry and labels into the Slint window.
    fn render_geometry(&self) {
        let state = self.state.borrow();
        let extent = state.geometry.extent();
        let cells = self.cells(&state);
        let (layout, center_r) = match state.settings.layout {
            Layout::Honeycomb => (WheelLayout::Honeycomb, HEX_RADIUS),
            Layout::Petals => (WheelLayout::Petals, PETAL_DEAD_ZONE),
        };
        self.set_window_side(2.0 * extent);
        self.window
            .set_hex_path(rounded_hex_path(HEX_CORNER_RADIUS / HEX_RADIUS).into());
        self.window.set_layout(layout);
        self.window.set_center_r(center_r);
        self.window.set_cells(ModelRc::new(VecModel::from(cells)));
        self.window.set_highlighted(-1);
        self.window.set_center_text("".into());
    }

    /// The wheel's cells for `state`, with their labels and icons.
    fn cells(&self, state: &State) -> Vec<WheelCell> {
        let icons = self.window.global::<Icons>();
        state
            .geometry
            .cells()
            .iter()
            .zip(&state.settings.actions)
            .map(|(cell, action)| WheelCell {
                x: cell.center.x,
                y: cell.center.y,
                r: cell.radius,
                covered: false,
                dimmed: cell_dimmed(*action, state.pdf_dimmed),
                label: action_text(*action, state.extracting).into(),
                icon: wheel_icon(&icons, *action, state.extracting),
            })
            .collect()
    }

    /// Labels the Archive cell and dims the PDF cell after the files being
    /// dragged, reading their paths from the drag pasteboard once (names
    /// only, no file I/O). The cells are rebuilt only when either changed.
    fn label_cells(&self) {
        let changed = {
            let mut state = self.state.borrow_mut();
            let actions = &state.settings.actions;
            let (archive, pdf) = (
                actions.contains(&ActionId::Archive),
                actions.contains(&ActionId::Pdf),
            );
            if !archive && !pdf {
                return;
            }
            let paths = self.platform.drag_paths();
            let mut changed = false;
            if archive {
                let extracting = looks_like_archives(&paths);
                changed |= state.extracting != extracting;
                state.extracting = extracting;
            }
            if pdf {
                let dimmed = pdf_cell_dimmed(&paths);
                changed |= state.pdf_dimmed != dimmed;
                state.pdf_dimmed = dimmed;
            }
            changed
        };
        if changed {
            let cells = self.cells(&self.state.borrow());
            self.window.set_cells(ModelRc::new(VecModel::from(cells)));
        }
    }

    /// Resizes the (square) window to `side`, at once, so a move that
    /// follows lands where it should.
    fn set_window_side(&self, side: f32) {
        self.window.set_size(side);
        self.window.window().set_size(LogicalSize::new(side, side));
    }

    /// Moves the window so the wheel centre sits at `state.center`.
    fn centre_window(&self, half: f32) {
        let center = self.state.borrow().center;
        let half = f64::from(half);
        self.window.window().set_position(LogicalPosition::new(
            (center.x - half) as f32,
            (center.y - half) as f32,
        ));
    }

    /// After a tick: moves the poll timer to the interval the trigger asks
    /// for now, if that changed, or stops it while paused.
    fn rearm(&self) {
        if self.paused.get() && self.warmed_up() {
            self.poll.stop();
            return;
        }
        let interval = self.machine.borrow().poll_interval();
        if self.poll.interval() != interval {
            self.poll.set_interval(interval);
        }
    }

    fn tick(&self) {
        let started = Instant::now();
        let ticks = {
            let mut state = self.state.borrow_mut();
            state.ticks = state.ticks.saturating_add(1);
            state.ticks
        };
        match ticks {
            WARM_SHOW_TICK => {
                self.platform.prepare_app();
                self.window.window().set_position(PARKED);
                if let Err(e) = self.window.show() {
                    warn!("cannot pre-warm the wheel window: {e}");
                }
            }
            WARM_HIDE_TICK => {
                // On Linux the window exists by now: make it override-
                // redirect, or GNOME places it itself and gives it focus.
                // macOS places the wheel where it is put as it is.
                #[cfg(target_os = "linux")]
                self.make_overlay();
                self.hide_window();
                self.register_drop_handler();
                debug!("wheel pre-warmed");
                self.check_folders(None);
            }
            t if t < WARM_HIDE_TICK => {}
            _ if self.paused.get() => {}
            _ => {
                self.collect_folder_reports();
                let sample = self.platform.sample_input();
                let (event, dragging) = {
                    let mut machine = self.machine.borrow_mut();
                    let event = machine.feed(&sample);
                    (event, machine.drag_in_progress())
                };
                self.handle(event, started);
                // After the release (the grace period and the drop batch),
                // the ring neither opens nor closes.
                if dragging && self.machine.borrow().is_open() {
                    self.ring_tick(started);
                }
                if let Some(listener) = &*self.sample_listener.borrow() {
                    listener(&sample, dragging);
                }
            }
        }
    }

    fn handle(&self, event: TriggerEvent, started: Instant) {
        match event {
            TriggerEvent::None => {}
            TriggerEvent::Open(cursor) => self.open(cursor, started),
            TriggerEvent::Move(cursor) => self.hover(cursor),
            TriggerEvent::Cancel => {
                self.dismiss();
                debug!("cancelled (esc)");
            }
            TriggerEvent::Release => {
                let this = self.this.clone();
                self.grace
                    .start(TimerMode::SingleShot, RELEASE_GRACE, move || {
                        if let Some(controller) = this.upgrade() {
                            controller.grace_expired();
                        }
                    });
            }
        }
    }

    fn open(&self, cursor: Point, started: Instant) {
        // The ring opens a hold later at the earliest: check its folders now.
        self.check_folders(Some(cursor));
        let screens = self.platform.screens();
        let extent = {
            let mut state = self.state.borrow_mut();
            let extent = state.geometry.extent();
            state.center = place_center(cursor, extent, &screens);
            state.screen = screen_for(cursor, &screens).copied();
            state.highlighted = None;
            state.last_offset = Vec2 { x: 0.0, y: 0.0 };
            state.dropped.clear();
            extent
        };
        self.label_cells();
        // `dismiss` already cleared the highlight with animation off.
        self.window.set_animate_hover(true);
        self.centre_window(extent);
        if let Err(e) = self.window.show() {
            warn!("cannot show the wheel: {e}");
        }
        debug!("wheel open in {:?}", started.elapsed());
    }

    /// Highlights the cell under the cursor, if any.
    fn hover(&self, cursor: Point) {
        let hover = {
            let mut state = self.state.borrow_mut();
            let offset = Vec2 {
                x: (cursor.x - state.center.x) as f32,
                y: (cursor.y - state.center.y) as f32,
            };
            state.last_offset = offset;
            state.classify(offset)
        };
        match hover {
            Hover::Action(i) => self.set_hot(Some(i), None),
            Hover::Destination(d) => self.set_hot(None, Some(d)),
            Hover::Centre | Hover::Gap | Hover::Outside => self.set_hot(None, None),
        }
    }

    /// Highlights the action `action` or the ring destination `dest` (at
    /// most one of them), and labels it.
    fn set_hot(&self, action: Option<usize>, dest: Option<usize>) {
        let mut state = self.state.borrow_mut();
        if state.highlighted != action {
            state.highlighted = action;
            let label = action.map_or_else(String::new, |i| {
                action_text(state.settings.actions[i], state.extracting)
            });
            self.window.set_highlighted(action.map_or(-1, |i| i as i32));
            self.window.set_center_text(label.into());
        }
        if state.ring_hot == dest {
            return;
        }
        state.ring_hot = dest;
        self.window.set_ring_hot(dest.map_or(-1, |d| d as i32));
        let Some((ring, d)) = state.ring.as_ref().zip(dest) else {
            return;
        };
        self.window
            .set_dest_verb(ring_verb(state.settings.actions[ring.source]).into());
        match &ring.items {
            RingItems::Folders(dests) => {
                let path = &dests[d].path;
                self.window.set_dest_name(folder_name(path).into());
                let shown = fit_path(&display_path(path, self.home.as_deref()), DEST_PATH_CHARS);
                self.window.set_dest_path(shown.into());
                self.window.set_dest_icon(self.folder_icon(path));
                self.window.set_dest_icon_tint(false);
            }
            RingItems::Formats(formats) => {
                self.window.set_dest_name(formats[d].name().into());
                self.window.set_dest_path("".into());
                self.window
                    .set_dest_icon(self.window.global::<Icons>().get_photo());
                self.window.set_dest_icon_tint(true);
            }
        }
    }

    /// Feeds the hover to the ring tracker, and opens or closes the ring.
    fn ring_tick(&self, now: Instant) {
        let event = {
            let mut state = self.state.borrow_mut();
            let hover = state.classify(state.last_offset);
            state.tracker.feed(hover, now)
        };
        match event {
            RingEvent::None => {}
            RingEvent::Open(source) => self.open_ring(source),
            RingEvent::Close => self.close_ring(),
        }
    }

    /// The ring's folders as last checked: recent ones, then pinned ones,
    /// leaving out those that did not exist. Empty before the first check.
    fn destinations(&self) -> Vec<Destination> {
        self.folders.borrow().dests.clone().unwrap_or_default()
    }

    /// Asks for a fresh check of the ring's folders, their icons drawn for
    /// the screen at `at` (the primary screen when `None`). Nothing to do
    /// when no action shows folders.
    fn check_folders(&self, at: Option<Point>) {
        let (recent, pinned) = {
            let state = self.state.borrow();
            if !has_folder_ring(&state.settings) {
                return;
            }
            let recent = self.recent.borrow().items().to_vec();
            (recent, state.settings.pinned_folders.clone())
        };
        let scale = self
            .platform
            .scale_factor_at(at.unwrap_or(Point { x: 0.0, y: 0.0 }));
        let have_icons = {
            let folders = self.folders.borrow();
            if folders.scale == scale {
                folders.icons.keys().cloned().collect()
            } else {
                HashSet::new()
            }
        };
        let check = FolderCheck {
            recent,
            pinned,
            scale,
            have_icons,
        };
        let check = match &self.folder_worker {
            Some(worker) => match worker.checks.send(check) {
                Ok(()) => return,
                Err(mpsc::SendError(check)) => {
                    warn!("the ring folder worker is gone; checking here");
                    check
                }
            },
            None => check,
        };
        self.take_folder_report(check_folders(check));
    }

    /// Takes in what the folder worker has reported since the last tick.
    fn collect_folder_reports(&self) {
        let Some(worker) = &self.folder_worker else {
            return;
        };
        while let Ok(report) = worker.reports.try_recv() {
            self.take_folder_report(report);
        }
    }

    /// Keeps the folders and icons a check found, and brings the ring up to
    /// date with them.
    fn take_folder_report(&self, report: FolderReport) {
        let changed = {
            let mut folders = self.folders.borrow_mut();
            if folders.scale != report.scale {
                // Asked for with no icons in hand: the report has them all.
                folders.icons.clear();
                folders.scale = report.scale;
            }
            for (path, icon) in report.icons {
                let icon = icon.as_ref().and_then(to_image).unwrap_or_default();
                folders.icons.insert(path, icon);
            }
            folders
                .icons
                .retain(|path, _| report.dests.iter().any(|d| d.path == *path));
            let changed = folders.dests.as_ref() != Some(&report.dests);
            folders.dests = Some(report.dests);
            changed
        };
        self.refresh_folder_ring(changed);
    }

    /// After a folder check: during a drag, the folder ring the cursor holds
    /// open is rebuilt when the folders `changed` (the check was slower than
    /// the hold) or opened when it had nothing to show; otherwise an open
    /// folder ring only gets its icons.
    fn refresh_folder_ring(&self, changed: bool) {
        let dragging = {
            let machine = self.machine.borrow();
            machine.is_open() && machine.drag_in_progress()
        };
        let reopen = {
            let state = self.state.borrow();
            let shown = matches!(
                state.ring,
                Some(OpenRing {
                    items: RingItems::Folders(_),
                    ..
                })
            );
            state.tracker.open_for().filter(|&source| {
                dragging
                    && (changed || !shown)
                    && state
                        .settings
                        .actions
                        .get(source)
                        .is_some_and(|a| matches!(a, ActionId::Move | ActionId::CopyTo))
            })
        };
        if let Some(source) = reopen {
            self.close_ring();
            self.open_ring(source);
            return;
        }
        let state = self.state.borrow();
        let Some(OpenRing {
            items: RingItems::Folders(dests),
            ..
        }) = &state.ring
        else {
            return;
        };
        let cells = self.window.get_ring_cells();
        for (i, dest) in dests.iter().enumerate() {
            if let Some(mut cell) = cells.row_data(i) {
                cell.icon = self.folder_icon(&dest.path);
                cells.set_row_data(i, cell);
            }
        }
        if let Some(dest) = state.ring_hot.and_then(|d| dests.get(d)) {
            self.window.set_dest_icon(self.folder_icon(&dest.path));
        }
    }

    /// Opens the ring around the action at `source`: formats for Convert,
    /// folders otherwise. With nothing to show, or no room for it, the wheel
    /// stays as it is (a drop on a folder action still opens the folder
    /// picker).
    fn open_ring(&self, source: usize) {
        let is_convert = self.state.borrow().settings.actions[source] == ActionId::Convert;
        let mut items = if is_convert {
            RingItems::Formats(ring_formats(
                &self.platform.drag_paths(),
                &vento_platform::convert_targets(),
            ))
        } else {
            RingItems::Folders(self.destinations())
        };
        if items.len() == 0 {
            debug!("nothing for the ring");
            return;
        }
        let (ring, label, half) = {
            let state = self.state.borrow();
            // With no screen known, nothing limits the ring.
            let bounds = state.screen.map_or(
                Rect {
                    x: -1e6,
                    y: -1e6,
                    w: 2e6,
                    h: 2e6,
                },
                |s| Rect {
                    x: s.x - state.center.x,
                    y: s.y - state.center.y,
                    w: s.w,
                    h: s.h,
                },
            );
            let ring = state.geometry.ring(source, items.len(), bounds);
            let actions = state
                .geometry
                .cells()
                .iter()
                .take(state.settings.actions.len());
            let centre = geometry::Cell {
                center: Vec2 { x: 0.0, y: 0.0 },
                radius: centre_radius(state.settings.layout),
            };
            let shown = actions.chain(ring.cells()).chain([&centre]);
            let label = label_origin(shown, &bounds);
            let half = window_half(ring.extent(), label);
            (ring, label, half)
        };
        if ring.cells().is_empty() {
            debug!("no room for the ring");
            return;
        }
        items.truncate(ring.cells().len());
        let label_chars = match self.state.borrow().settings.layout {
            Layout::Honeycomb => LABEL_CHARS,
            Layout::Petals => PETAL_LABEL_CHARS,
        };
        // Each cell's icon, label and pin mark.
        let contents: Vec<(Image, CellLabel, bool)> = match &items {
            RingItems::Folders(dests) => dests
                .iter()
                .zip(cell_labels(dests, label_chars))
                .map(|(dest, label)| (self.folder_icon(&dest.path), label, dest.pinned))
                .collect(),
            RingItems::Formats(formats) => {
                let photo = self.window.global::<Icons>().get_photo();
                formats
                    .iter()
                    .map(|&f| (photo.clone(), format_label(f), false))
                    .collect()
            }
        };
        let cells: Vec<RingCell> = ring
            .cells()
            .iter()
            .zip(contents)
            .map(|(cell, (icon, label, pinned))| RingCell {
                x: cell.center.x,
                y: cell.center.y,
                r: cell.radius,
                icon,
                line1: label.line1.into(),
                line2: label.line2.unwrap_or_default().into(),
                line2_dim: label.line2_dim,
                pinned,
                tint: matches!(items, RingItems::Formats(_)),
            })
            .collect();
        self.set_covered(Some(&ring));
        self.window
            .set_ring_cells(ModelRc::new(VecModel::from(cells)));
        self.window.set_ring_source(source as i32);
        self.window.set_dest_x(label.x);
        self.window.set_dest_y(label.y);
        self.set_window_side(2.0 * half);
        self.centre_window(half);
        debug!("ring open with {} item(s)", items.len());
        self.state.borrow_mut().ring = Some(OpenRing {
            source,
            geometry: ring,
            items,
        });
    }

    /// Closes the destination ring, if open, and gives the wheel its own size
    /// back.
    fn close_ring(&self) {
        let extent = {
            let mut state = self.state.borrow_mut();
            state.ring_hot = None;
            if state.ring.take().is_none() {
                return;
            }
            state.geometry.extent()
        };
        self.set_covered(None);
        self.window.set_ring_hot(-1);
        self.window.set_ring_source(-1);
        self.window.set_ring_cells(ModelRc::default());
        self.window.set_dest_icon(Image::default());
        self.set_window_side(2.0 * extent);
        self.centre_window(extent);
    }

    /// Marks the wheel cells a cell of `ring` sits on, or none.
    fn set_covered(&self, ring: Option<&RingGeometry>) {
        let cells = self.window.get_cells();
        for i in 0..cells.row_count() {
            let Some(mut cell) = cells.row_data(i) else {
                continue;
            };
            let covered = ring.is_some_and(|ring| {
                ring.cells()
                    .iter()
                    .any(|r| (r.center.x - cell.x).hypot(r.center.y - cell.y) < 1.0)
            });
            if cell.covered != covered {
                cell.covered = covered;
                cells.set_row_data(i, cell);
            }
        }
    }

    /// The Finder icon of `folder` as the folder worker drew it; empty while
    /// it has not yet, or if the system has none.
    fn folder_icon(&self, folder: &Path) -> Image {
        self.folders
            .borrow()
            .icons
            .get(folder)
            .cloned()
            .unwrap_or_default()
    }

    /// The button was released over nothing the wheel received: pass through.
    fn grace_expired(&self) {
        if self.batch.running() || !self.state.borrow().dropped.is_empty() {
            return;
        }
        self.dismiss();
        debug!("cancelled");
    }

    fn register_drop_handler(&self) {
        let this = self.this.clone();
        self.window.window().on_winit_window_event(move |_, event| {
            if let WindowEvent::DroppedFile(path) = event
                && let Some(controller) = this.upgrade()
            {
                controller.file_dropped(path.clone());
            }
            EventResult::Propagate
        });
    }

    fn file_dropped(&self, path: PathBuf) {
        if !self.machine.borrow().is_open() {
            return;
        }
        self.state.borrow_mut().dropped.push(path);
        if !self.batch.running() {
            let this = self.this.clone();
            self.batch
                .start(TimerMode::SingleShot, DROP_BATCH, move || {
                    if let Some(controller) = this.upgrade() {
                        controller.finish_drop();
                    }
                });
        }
    }

    fn finish_drop(&self) {
        let (paths, target, center, on_centre) = {
            let mut state = self.state.borrow_mut();
            let on_ring = state
                .ring
                .as_ref()
                .zip(state.ring_hot)
                .and_then(|(ring, d)| {
                    let action = state.settings.actions.get(ring.source).copied()?;
                    Some(match &ring.items {
                        RingItems::Folders(dests) => {
                            (action, Some(dests.get(d)?.path.clone()), None)
                        }
                        RingItems::Formats(formats) => (action, None, Some(*formats.get(d)?)),
                    })
                });
            let target = on_ring.or_else(|| {
                state
                    .highlighted
                    .and_then(|i| state.settings.actions.get(i).copied())
                    .map(|action| (action, None, None))
            });
            let centre_r = centre_radius(state.settings.layout);
            let on_centre = state.last_offset.x.hypot(state.last_offset.y) < centre_r;
            (
                std::mem::take(&mut state.dropped),
                target,
                state.center,
                on_centre,
            )
        };
        self.dismiss();
        match target {
            Some((action, dest, format)) => (self.on_drop)(WheelDrop {
                action,
                paths,
                at: center,
                dest,
                format,
            }),
            None if on_centre => debug!("dropped on centre"),
            None => debug!("dropped outside cells"),
        }
    }

    /// Hides the wheel, clears its highlight and returns the trigger machine
    /// to idle.
    fn dismiss(&self) {
        self.grace.stop();
        self.batch.stop();
        {
            let mut state = self.state.borrow_mut();
            state.dropped.clear();
            state.highlighted = None;
            state.tracker.reset();
        }
        // Reset without animating, so the next open does not flash the old
        // highlight fading out.
        self.window.set_animate_hover(false);
        self.window.set_highlighted(-1);
        self.window.set_center_text("".into());
        self.hide_window();
        self.close_ring();
        self.machine.borrow_mut().close();
    }

    fn hide_window(&self) {
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the wheel: {e}");
        }
    }

    /// Keeps the wheel above every window, out of focus, exactly where it
    /// is placed. Needs the native window, which exists once it was shown.
    #[cfg(target_os = "linux")]
    fn make_overlay(&self) {
        use raw_window_handle::HasWindowHandle;

        let result = match self.window.window().window_handle().window_handle() {
            Ok(handle) => self
                .platform
                .make_overlay(handle.as_raw())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => debug!("wheel overlay ready"),
            Err(e) => warn!("cannot make the wheel an overlay: {e}"),
        }
    }
}

/// The name shown for `folder`: its last component, or the whole path for a
/// root.
fn folder_name(folder: &Path) -> String {
    folder.file_name().map_or_else(
        || folder.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Top-left of the destination label, relative to the wheel centre:
/// [`DEST_LABEL_GAP`] below the lowest of `cells`, centred under the wheel
/// and kept on the screen (`bounds`, in centre coordinates). With no room
/// below, it goes the same gap above the highest cell.
fn label_origin<'a>(cells: impl Iterator<Item = &'a geometry::Cell>, bounds: &Rect) -> Vec2 {
    let (top, bottom) = cells.fold((0.0f32, 0.0f32), |(top, bottom), c| {
        (
            top.min(c.center.y - c.radius),
            bottom.max(c.center.y + c.radius),
        )
    });
    let (left, right) = (bounds.x as f32, (bounds.x + bounds.w) as f32);
    let x = (-DEST_LABEL_W / 2.0)
        .min(right - DEST_LABEL_MARGIN - DEST_LABEL_W)
        .max(left + DEST_LABEL_MARGIN);
    let below = bottom + DEST_LABEL_GAP;
    let above = top - DEST_LABEL_GAP - DEST_LABEL_H;
    let fits_below = below + DEST_LABEL_H + DEST_LABEL_MARGIN <= (bounds.y + bounds.h) as f32;
    let fits_above = above - DEST_LABEL_MARGIN >= bounds.y as f32;
    let y = if fits_below || !fits_above {
        below
    } else {
        above
    };
    Vec2 { x, y }
}

/// Half the side of the window that holds a ring reaching `ring_extent` and
/// the destination label at `label`.
fn window_half(ring_extent: f32, label: Vec2) -> f32 {
    [
        label.x,
        label.x + DEST_LABEL_W,
        label.y,
        label.y + DEST_LABEL_H,
    ]
    .into_iter()
    .map(|v| v.abs() + DEST_LABEL_MARGIN)
    .fold(ring_extent, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_cell_label() {
        let label = format_label(ImageFormat::Jpeg);
        assert_eq!(label.line1, "JPEG");
        assert_eq!(label.line2.as_deref(), Some(".jpg"));
        assert!(label.line2_dim);
    }

    #[test]
    fn folder_check_skips_missing_folders_and_icons_in_hand() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, gone) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("gone"),
        );
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        // Run where the worker does: off the main thread.
        let report = std::thread::spawn({
            let (a, b, gone) = (a.clone(), b.clone(), gone.clone());
            move || {
                check_folders(FolderCheck {
                    recent: vec![a.clone(), gone],
                    pinned: vec![b],
                    scale: 2.0,
                    have_icons: HashSet::from([a]),
                })
            }
        })
        .join()
        .unwrap();
        let paths: Vec<_> = report.dests.iter().map(|d| (&d.path, d.pinned)).collect();
        assert_eq!(paths, [(&a, false), (&b, true)]);
        assert_eq!(report.scale, 2.0);
        let drawn: Vec<_> = report.icons.iter().map(|(p, _)| p).collect();
        assert_eq!(drawn, [&b], "only the icon not in hand is drawn");
        #[cfg(target_os = "macos")]
        {
            let icon = report.icons[0].1.as_ref().expect("a folder icon");
            assert_eq!(
                (icon.width, icon.height),
                (2 * FOLDER_ICON_PX, 2 * FOLDER_ICON_PX)
            );
        }
    }

    #[test]
    fn ring_verb_per_action() {
        assert_eq!(ring_verb(ActionId::Move), "Move to");
        assert_eq!(ring_verb(ActionId::CopyTo), "Copy to");
        assert_eq!(ring_verb(ActionId::Convert), "Convert to");
    }

    #[test]
    fn cells_dim_pdf_without_pdfs() {
        let png = [PathBuf::from("/d/a.png")];
        let pdf = [PathBuf::from("/d/a.png"), PathBuf::from("/d/b.PDF")];
        assert!(pdf_cell_dimmed(&png));
        assert!(pdf_cell_dimmed(&[]));
        assert!(!pdf_cell_dimmed(&pdf));
        let actions = [ActionId::Copy, ActionId::Pdf];
        let dim = |dimmed| actions.map(|a| cell_dimmed(a, dimmed));
        assert_eq!(dim(true), [false, true], "only the PDF cell dims");
        assert_eq!(dim(false), [false, false]);
    }

    #[test]
    fn archive_cell_says_compress_or_extract() {
        assert_eq!(action_text(ActionId::Archive, false), "Compress");
        assert_eq!(action_text(ActionId::Archive, true), "Extract");
        assert_eq!(action_text(ActionId::Move, true), "Move");
        assert_eq!(action_text(ActionId::CopyPath, false), "Copy path");
        assert_eq!(action_text(ActionId::Info, false), "Info");
    }

    #[test]
    fn wheel_text_reads_in_arabic() {
        vento_core::i18n::with_language("ar", || {
            assert_eq!(ring_verb(ActionId::Move), "نقل إلى");
            assert_eq!(ring_verb(ActionId::CopyTo), "نسخ إلى");
            assert_eq!(ring_verb(ActionId::Convert), "تحويل إلى");
            assert_eq!(action_text(ActionId::Archive, false), "ضغط");
            assert_eq!(action_text(ActionId::Archive, true), "فك الضغط");
            assert_eq!(action_text(ActionId::Move, true), "نقل");
            assert_eq!(action_text(ActionId::CopyPath, false), "نسخ المسار");
        });
    }

    fn cell(x: f32, y: f32, radius: f32) -> geometry::Cell {
        geometry::Cell {
            center: Vec2 { x, y },
            radius,
        }
    }

    fn screen(x: f64, y: f64) -> Rect {
        Rect {
            x,
            y,
            w: 1440.0,
            h: 900.0,
        }
    }

    /// Points along the straight segment from `a` to `b`, both included.
    fn segment(a: Vec2, b: Vec2, steps: usize) -> impl Iterator<Item = Vec2> {
        (0..=steps).map(move |n| {
            let t = n as f32 / steps as f32;
            Vec2 {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
            }
        })
    }

    #[test]
    fn petals_straight_path_to_a_side_ring_petal_keeps_the_ring() {
        let wheel = WheelGeometry::new(Layout::Petals, 6);
        let everywhere = Rect {
            x: -1e6,
            y: -1e6,
            w: 2e6,
            h: 2e6,
        };
        let ring = wheel.ring(0, 5, everywhere);
        assert_eq!(ring.cells().len(), 5);
        let from = wheel.cells()[0].center;
        // Cells alternate right and left: k = 0, +1, -1, +2, -2.
        for target in [3, 4] {
            let to = ring.cells()[target].center;
            let mut tracker = RingTracker::new(Duration::ZERO, |i| i == 0);
            let t0 = Instant::now();
            assert_eq!(tracker.feed(Hover::Action(0), t0), RingEvent::Open(0));
            for at in segment(from, to, 400) {
                let hover = classify(&wheel, Layout::Petals, 6, Some((0, &ring)), at);
                assert!(
                    matches!(hover, Hover::Action(0) | Hover::Destination(_) | Hover::Gap),
                    "{hover:?} at {at:?} towards ring cell {target}"
                );
                assert_eq!(tracker.feed(hover, t0), RingEvent::None, "at {at:?}");
            }
            assert_eq!(
                classify(&wheel, Layout::Petals, 6, Some((0, &ring)), to),
                Hover::Destination(target)
            );
        }
    }

    #[test]
    fn petals_with_a_ring_still_reach_a_neighbour_inside_its_petal() {
        let wheel = WheelGeometry::new(Layout::Petals, 6);
        let everywhere = Rect {
            x: -1e6,
            y: -1e6,
            w: 2e6,
            h: 2e6,
        };
        let ring = wheel.ring(0, 5, everywhere);
        let open = Some((0, &ring));
        let neighbour = wheel.cells()[1];
        assert_eq!(
            classify(&wheel, Layout::Petals, 6, open, neighbour.center),
            Hover::Action(1)
        );
        // In petal 1's sector but outside its circle: a gap while the ring is
        // open, the action while it is not.
        let beside = Vec2 {
            x: neighbour.center.x,
            y: neighbour.center.y - neighbour.radius - 2.0,
        };
        assert_eq!(
            classify(&wheel, Layout::Petals, 6, None, beside),
            Hover::Action(1)
        );
        assert_eq!(
            classify(&wheel, Layout::Petals, 6, open, beside),
            Hover::Gap
        );
        // The source petal keeps its whole sector.
        let source = wheel.cells()[0].center;
        let off_source = Vec2 {
            x: source.x + 45.0,
            y: source.y,
        };
        assert_eq!(
            classify(&wheel, Layout::Petals, 6, open, off_source),
            Hover::Action(0)
        );
    }

    #[test]
    fn label_sits_under_the_lowest_cell() {
        let cells = [cell(0.0, -70.0, 38.0), cell(0.0, 140.0, 38.0)];
        let at = label_origin(cells.iter(), &screen(-720.0, -450.0));
        assert_eq!(
            at,
            Vec2 {
                x: -140.0,
                y: 140.0 + 38.0 + 24.0
            }
        );
    }

    #[test]
    fn label_stays_on_screen_at_the_left_edge() {
        // The wheel centre 110 px from the screen's left edge.
        let cells = [cell(0.0, 70.0, 38.0)];
        let at = label_origin(cells.iter(), &screen(-110.0, -450.0));
        assert_eq!(at.x, -110.0 + DEST_LABEL_MARGIN);
        let at = label_origin(cells.iter(), &screen(110.0 - 1440.0, -450.0));
        assert_eq!(at.x + DEST_LABEL_W, 110.0 - DEST_LABEL_MARGIN);
    }

    #[test]
    fn label_goes_above_with_no_room_below() {
        let cells = [cell(0.0, -70.0, 38.0), cell(0.0, 70.0, 38.0)];
        // The screen ends 130 px below the centre.
        let at = label_origin(cells.iter(), &screen(-720.0, 130.0 - 900.0));
        assert_eq!(at.y, -70.0 - 38.0 - 24.0 - DEST_LABEL_H);
    }

    #[test]
    fn window_holds_the_ring_and_the_label() {
        // The ring reaches farther than the label.
        assert_eq!(
            window_half(
                300.0,
                Vec2 {
                    x: -140.0,
                    y: 100.0
                }
            ),
            300.0
        );
        // The label reaches farther than the ring.
        let half = window_half(
            150.0,
            Vec2 {
                x: -140.0,
                y: 200.0,
            },
        );
        assert_eq!(half, 200.0 + DEST_LABEL_H + DEST_LABEL_MARGIN);
        // A label pushed right by a screen edge.
        let half = window_half(150.0, Vec2 { x: -20.0, y: 0.0 });
        assert_eq!(half, -20.0 + DEST_LABEL_W + DEST_LABEL_MARGIN);
    }
}
