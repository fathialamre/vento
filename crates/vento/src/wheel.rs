//! The wheel window and the controller that drives it from polled input.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

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
use vento_core::destinations::{
    Destination, RecentDestinations, cell_labels, display_path, folder_exists, ring_destinations,
};
use vento_core::geometry::{
    self, HEX_CORNER_RADIUS, HEX_RADIUS, Layout, PETAL_DEAD_ZONE, Point, Rect, RingGeometry, Vec2,
    WheelGeometry, place_center, rounded_hex_path, screen_for,
};
use vento_core::ring::{Hover, RingEvent, RingTracker};
use vento_core::settings::Settings;
use vento_core::trigger::{InputSample, TriggerEvent, TriggerMachine};
use vento_platform::Platform;

/// Where the wheel window waits, out of sight, while it is pre-warmed.
const PARKED: LogicalPosition = LogicalPosition {
    x: -5000.0,
    y: -5000.0,
};
/// How long after a release a drop may still arrive before it counts as cancelled.
const RELEASE_GRACE: Duration = Duration::from_millis(250);
/// winit delivers one `DroppedFile` per file; collect them for this long.
const DROP_BATCH: Duration = Duration::from_millis(30);
/// Polling rate while paused (slower than idle polling; input is not sampled).
const PAUSED_INTERVAL: Duration = Duration::from_millis(500);
/// Tick on which the parked window is shown, so the native window and renderer exist.
const WARM_SHOW_TICK: u32 = 1;
/// Tick on which the parked window is hidden again and the drop handler registered.
const WARM_HIDE_TICK: u32 = 3;
/// Size of the folder icons fetched for the ring, in pixels.
const FOLDER_ICON_PX: u32 = 40;
/// Size of the destination label under the ring (as drawn in `wheel.slint`).
const DEST_LABEL_W: f32 = 280.0;
const DEST_LABEL_H: f32 = 44.0;
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
}

/// The destination ring around one action.
struct OpenRing {
    /// Index of the action the ring opened from.
    source: usize,
    geometry: RingGeometry,
    /// The folder on each ring cell, in cell order.
    dests: Vec<Destination>,
}

impl State {
    /// What the cursor at `offset` from the centre is over; ring cells come
    /// first, as they may sit where wheel actions are drawn.
    fn classify(&self, offset: Vec2) -> Hover {
        if let Some(d) = self.ring.as_ref().and_then(|r| r.geometry.hit(offset)) {
            return Hover::Destination(d);
        }
        if let Some(i) = self
            .geometry
            .hit(offset)
            .filter(|&i| i < self.settings.actions.len())
        {
            return Hover::Action(i);
        }
        let distance = offset.x.hypot(offset.y);
        if distance < centre_radius(self.settings.layout) {
            return Hover::Centre;
        }
        match &self.ring {
            Some(ring) if distance <= ring.geometry.extent() => Hover::Gap,
            _ => Hover::Outside,
        }
    }
}

/// The icon `action` shows on the wheel and in the settings panel.
pub fn action_icon(icons: &Icons<'_>, action: ActionId) -> Image {
    match action {
        ActionId::Pocket => icons.get_pocket(),
        ActionId::Copy => icons.get_copy(),
        ActionId::Move => icons.get_move(),
        ActionId::CopyPath => icons.get_copy_path(),
        ActionId::Terminal => icons.get_terminal(),
        ActionId::CopyTo => icons.get_copy_to(),
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
    /// Finder icons of ring folders, by path.
    folder_icons: RefCell<HashMap<PathBuf, Image>>,
    machine: RefCell<TriggerMachine>,
    state: RefCell<State>,
    paused: Cell<bool>,
    poll: Timer,
    grace: Timer,
    batch: Timer,
    /// Fetches the ring's folder icons just after the wheel shows.
    icon_fetch: Timer,
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
        let geometry = WheelGeometry::new(settings.layout, settings.actions.len());
        let machine = TriggerMachine::new(settings.trigger);
        let tracker = ring_tracker(&settings);
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            window,
            on_drop,
            recent,
            home: std::env::var_os("HOME").map(PathBuf::from),
            folder_icons: RefCell::new(HashMap::new()),
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
            }),
            paused: Cell::new(false),
            poll: Timer::default(),
            grace: Timer::default(),
            batch: Timer::default(),
            icon_fetch: Timer::default(),
            sample_listener: RefCell::new(None),
            this: this.clone(),
        });
        controller.render_geometry();
        controller
    }

    /// Pre-warms the window on the first ticks, then polls for the trigger.
    pub fn start(self: &Rc<Self>) {
        self.schedule();
    }

    /// While paused, input is not sampled and polling slows to 500 ms. An open
    /// wheel is dismissed. Unpausing starts from a fresh trigger state, so
    /// nothing seen before the pause can open the wheel.
    pub fn set_paused(&self, paused: bool) {
        self.paused.set(paused);
        if paused {
            if self.machine.borrow().is_open() {
                self.dismiss();
            }
        } else {
            self.reset_trigger();
        }
    }

    /// Hands every input sample to `f`, with whether a drag of any type is
    /// under way, on the poll tick that took it (also while the wheel is
    /// open). Nothing is sampled, so nothing is passed on, while paused.
    pub fn set_sample_listener(&self, f: SampleListener) {
        *self.sample_listener.borrow_mut() = Some(f);
    }

    fn reset_trigger(&self) {
        let trigger = self.state.borrow().settings.trigger;
        *self.machine.borrow_mut() = TriggerMachine::new(trigger);
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
    }

    /// Pushes the current geometry and labels into the Slint window.
    fn render_geometry(&self) {
        let state = self.state.borrow();
        let extent = state.geometry.extent();
        let icons = self.window.global::<Icons>();
        let cells: Vec<WheelCell> = state
            .geometry
            .cells()
            .iter()
            .zip(&state.settings.actions)
            .map(|(cell, action)| WheelCell {
                x: cell.center.x,
                y: cell.center.y,
                r: cell.radius,
                covered: false,
                label: action.label().into(),
                icon: action_icon(&icons, *action),
            })
            .collect();
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

    /// Arms the single-shot poll timer for the next tick.
    fn schedule(&self) {
        let interval = if self.paused.get() {
            PAUSED_INTERVAL
        } else {
            self.machine.borrow().poll_interval()
        };
        let this = self.this.clone();
        self.poll.start(TimerMode::SingleShot, interval, move || {
            if let Some(controller) = this.upgrade() {
                controller.tick();
                controller.schedule();
            }
        });
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
                self.hide_window();
                self.register_drop_handler();
                debug!("wheel pre-warmed");
            }
            t if t < WARM_HIDE_TICK => {}
            _ if self.paused.get() => {}
            _ => {
                let sample = self.platform.sample_input();
                let (event, dragging) = {
                    let mut machine = self.machine.borrow_mut();
                    let event = machine.feed(&sample);
                    (event, machine.drag_in_progress())
                };
                self.handle(event, started);
                if self.machine.borrow().is_open() {
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
        let screens = self.platform.screens();
        let (extent, rings) = {
            let mut state = self.state.borrow_mut();
            let extent = state.geometry.extent();
            state.center = place_center(cursor, extent, &screens);
            state.screen = screen_for(cursor, &screens).copied();
            state.highlighted = None;
            state.last_offset = Vec2 { x: 0.0, y: 0.0 };
            state.dropped.clear();
            let rings = state.settings.actions.iter().any(|a| a.takes_ring());
            (extent, rings)
        };
        // `dismiss` already cleared the highlight with animation off.
        self.window.set_animate_hover(true);
        self.centre_window(extent);
        if let Err(e) = self.window.show() {
            warn!("cannot show the wheel: {e}");
        }
        debug!("wheel open in {:?}", started.elapsed());
        if rings {
            // Folder icons are slow to make: fetch them once the wheel is up.
            let this = self.this.clone();
            self.icon_fetch
                .start(TimerMode::SingleShot, Duration::ZERO, move || {
                    if let Some(controller) = this.upgrade() {
                        controller.fetch_folder_icons();
                    }
                });
        }
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
            let label = action.map_or("", |i| state.settings.actions[i].label());
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
        let path = &ring.dests[d].path;
        let verb = match state.settings.actions[ring.source] {
            ActionId::CopyTo => "Copy to",
            _ => "Move to",
        };
        self.window.set_dest_verb(verb.into());
        self.window.set_dest_name(folder_name(path).into());
        self.window
            .set_dest_path(display_path(path, self.home.as_deref()).into());
        self.window.set_dest_icon(self.folder_icon(path));
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

    /// The ring's folders as they are now: recent ones, then pinned ones,
    /// leaving out those that do not exist.
    fn destinations(&self) -> Vec<Destination> {
        let state = self.state.borrow();
        ring_destinations(
            self.recent.borrow().items(),
            &state.settings.pinned_folders,
            folder_exists,
        )
    }

    /// Opens the destination ring around the action at `source`. With no
    /// folder to show, or no room for one, the wheel stays as it is (a drop
    /// on the action still opens the folder picker).
    fn open_ring(&self, source: usize) {
        let mut dests = self.destinations();
        if dests.is_empty() {
            debug!("no destinations for the ring");
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
            let ring = state.geometry.ring(source, dests.len(), bounds);
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
        dests.truncate(ring.cells().len());
        let labels = cell_labels(&dests);
        let cells: Vec<RingCell> = ring
            .cells()
            .iter()
            .zip(&dests)
            .zip(labels)
            .map(|((cell, dest), label)| RingCell {
                x: cell.center.x,
                y: cell.center.y,
                r: cell.radius,
                icon: self.folder_icon(&dest.path),
                line1: label.line1.into(),
                line2: label.line2.unwrap_or_default().into(),
                line2_dim: label.line2_dim,
                pinned: dest.pinned,
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
        debug!("ring open with {} folder(s)", dests.len());
        self.state.borrow_mut().ring = Some(OpenRing {
            source,
            geometry: ring,
            dests,
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

    /// The Finder icon of `folder`, fetched once and then kept; empty if the
    /// system has none.
    fn folder_icon(&self, folder: &Path) -> Image {
        if let Some(icon) = self.folder_icons.borrow().get(folder) {
            return icon.clone();
        }
        let icon = self
            .platform
            .file_icon(folder, FOLDER_ICON_PX)
            .as_ref()
            .and_then(to_image)
            .unwrap_or_default();
        self.folder_icons
            .borrow_mut()
            .insert(folder.to_path_buf(), icon.clone());
        icon
    }

    /// Fetches the icons of the folders the ring would show now, and forgets
    /// those of folders it no longer shows.
    fn fetch_folder_icons(&self) {
        if !self.machine.borrow().is_open() {
            return;
        }
        let dests = self.destinations();
        self.folder_icons
            .borrow_mut()
            .retain(|path, _| dests.iter().any(|d| d.path == *path));
        for dest in &dests {
            self.folder_icon(&dest.path);
        }
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
                    Some((action, Some(ring.dests.get(d)?.path.clone())))
                });
            let target = on_ring.or_else(|| {
                state
                    .highlighted
                    .and_then(|i| state.settings.actions.get(i).copied())
                    .map(|action| (action, None))
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
            Some((action, dest)) => (self.on_drop)(WheelDrop {
                action,
                paths,
                at: center,
                dest,
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
        self.icon_fetch.stop();
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
