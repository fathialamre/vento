//! The wheel window and the controller that drives it from polled input.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use crate::ui::{Icons, Wheel, WheelCell, WheelLayout};
use log::{debug, warn};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, LogicalPosition, ModelRc, Timer, TimerMode, VecModel};
use vento_core::actions::ActionId;
use vento_core::geometry::{
    HEX_CORNER_RADIUS, HEX_RADIUS, Layout, PETAL_DEAD_ZONE, Point, Vec2, WheelGeometry,
    place_center, rounded_hex_path,
};
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

struct State {
    ticks: u32,
    settings: Settings,
    geometry: WheelGeometry,
    /// Wheel centre in global logical coordinates while the wheel is open.
    center: Point,
    highlighted: Option<usize>,
    /// Cursor offset from the centre at the last hover, to tell a drop on the
    /// centre from one outside every cell.
    last_offset: Vec2,
    dropped: Vec<PathBuf>,
}

/// Receives every input sample and whether a drag (of any type) is under way.
type SampleListener = Box<dyn Fn(&InputSample, bool)>;

/// Shows the wheel under the cursor while files are dragged and Ctrl is
/// pressed, and reports the action a file is dropped on.
pub struct WheelController {
    platform: Rc<dyn Platform>,
    window: Wheel,
    on_drop: Box<dyn Fn(ActionId, Vec<PathBuf>, Point)>,
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
        on_drop: Box<dyn Fn(ActionId, Vec<PathBuf>, Point)>,
    ) -> Rc<Self> {
        let window = Wheel::new().expect("cannot create the wheel window");
        let geometry = WheelGeometry::new(settings.layout, settings.actions.len());
        let machine = TriggerMachine::new(settings.trigger);
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            window,
            on_drop,
            machine: RefCell::new(machine),
            state: RefCell::new(State {
                ticks: 0,
                settings,
                geometry,
                center: Point { x: 0.0, y: 0.0 },
                highlighted: None,
                last_offset: Vec2 { x: 0.0, y: 0.0 },
                dropped: Vec::new(),
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
                x: extent + cell.center.x,
                y: extent + cell.center.y,
                r: cell.radius,
                label: action.label().into(),
                icon: match action {
                    ActionId::Pocket => icons.get_pocket(),
                    ActionId::Copy => icons.get_copy(),
                    ActionId::Move => icons.get_move(),
                    ActionId::CopyPath => icons.get_copy_path(),
                    ActionId::Terminal => icons.get_terminal(),
                    // Placeholder until Copy to gets its own icon.
                    ActionId::CopyTo => icons.get_copy(),
                },
            })
            .collect();
        let (layout, center_r) = match state.settings.layout {
            Layout::Honeycomb => (WheelLayout::Honeycomb, HEX_RADIUS),
            Layout::Petals => (WheelLayout::Petals, PETAL_DEAD_ZONE),
        };
        self.window.set_size(2.0 * extent);
        self.window
            .set_hex_path(rounded_hex_path(HEX_CORNER_RADIUS / HEX_RADIUS).into());
        self.window.set_layout(layout);
        self.window.set_center_r(center_r);
        self.window.set_cells(ModelRc::new(VecModel::from(cells)));
        self.window.set_highlighted(-1);
        self.window.set_center_text("".into());
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
        let extent = {
            let mut state = self.state.borrow_mut();
            let extent = state.geometry.extent();
            state.center = place_center(cursor, extent, &screens);
            state.highlighted = None;
            state.last_offset = Vec2 { x: 0.0, y: 0.0 };
            state.dropped.clear();
            f64::from(extent)
        };
        let center = self.state.borrow().center;
        // `dismiss` already cleared the highlight with animation off.
        self.window.set_animate_hover(true);
        self.window.window().set_position(LogicalPosition::new(
            (center.x - extent) as f32,
            (center.y - extent) as f32,
        ));
        if let Err(e) = self.window.show() {
            warn!("cannot show the wheel: {e}");
        }
        debug!("wheel open in {:?}", started.elapsed());
    }

    /// Highlights the cell under the cursor, if any.
    fn hover(&self, cursor: Point) {
        let mut state = self.state.borrow_mut();
        let offset = Vec2 {
            x: (cursor.x - state.center.x) as f32,
            y: (cursor.y - state.center.y) as f32,
        };
        state.last_offset = offset;
        let hit = state
            .geometry
            .hit(offset)
            .filter(|&i| i < state.settings.actions.len());
        if state.highlighted == hit {
            return;
        }
        state.highlighted = hit;
        let label = hit.map_or("", |i| state.settings.actions[i].label());
        self.window.set_highlighted(hit.map_or(-1, |i| i as i32));
        self.window.set_center_text(label.into());
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
        let (paths, action, center, on_centre) = {
            let mut state = self.state.borrow_mut();
            let action = state
                .highlighted
                .and_then(|i| state.settings.actions.get(i).copied());
            let centre_r = match state.settings.layout {
                Layout::Honeycomb => HEX_RADIUS,
                Layout::Petals => PETAL_DEAD_ZONE,
            };
            let on_centre = state.last_offset.x.hypot(state.last_offset.y) < centre_r;
            (
                std::mem::take(&mut state.dropped),
                action,
                state.center,
                on_centre,
            )
        };
        self.dismiss();
        match action {
            Some(action) => (self.on_drop)(action, paths, center),
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
        }
        // Reset without animating, so the next open does not flash the old
        // highlight fading out.
        self.window.set_animate_hover(false);
        self.window.set_highlighted(-1);
        self.window.set_center_text("".into());
        self.hide_window();
        self.machine.borrow_mut().close();
    }

    fn hide_window(&self) {
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the wheel: {e}");
        }
    }
}
