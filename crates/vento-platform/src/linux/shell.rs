//! Talking to Vento's GNOME Shell extension (`gnome-shell/` in the repo),
//! which sees what a Wayland app cannot: the global pointer, drags in
//! other apps, the clipboard and the screens.
//!
//! A worker thread makes the D-Bus calls, so the main thread never waits
//! on GNOME Shell: each [`Shell::state`] hands back the latest answer and
//! asks for a fresh one. The extension sends no signals; every change shows
//! as a counter in its state.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gio::glib;
use gio::prelude::*;
use vento_core::geometry::{Point, Rect};
use vento_core::trigger::{InputSample, Modifiers};

const BUS_NAME: &str = "org.vento.Shell1";
const OBJECT_PATH: &str = "/org/vento/Shell1";
const INTERFACE: &str = "org.vento.Shell1";

/// The extension API this build speaks.
const API_VERSION: u32 = 2;

/// How long a call may take before its answer is given up.
const CALL_TIMEOUT_MS: i32 = 250;

/// How long reading one clipboard type may take: a picture can be large.
const READ_TIMEOUT_MS: i32 = 2000;

/// How long calls may keep failing before the last state is dropped. A
/// busy GNOME Shell misses a call now and then; reporting no state then
/// would read as the button let go and close the wheel mid-drag.
const GIVE_UP_AFTER: Duration = Duration::from_secs(2);

/// Clutter modifier mask bits, as `global.get_pointer()` reports them.
mod mask {
    pub const SHIFT: u32 = 1 << 0;
    pub const CONTROL: u32 = 1 << 2;
    pub const MOD1: u32 = 1 << 3;
    pub const MOD4: u32 = 1 << 6;
    pub const BUTTON1: u32 = 1 << 8;
    pub const SUPER: u32 = 1 << 26;
}

/// One answer to `State()`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct ShellState {
    pub x: i32,
    pub y: i32,
    pub mods: u32,
    pub drag_serial: u32,
    pub drag_active: bool,
    pub drag_has_files: bool,
    pub drag_cancelled: bool,
    pub clip_serial: u32,
    pub monitors_serial: u32,
}

/// One screen: its frame and its work area (without the top bar and the
/// dock), logical global coordinates, and its scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Monitor {
    pub frame: Rect,
    pub work: Rect,
    pub scale: f64,
}

/// `State()`'s `(iiuubbbuu)`, in [`ShellState`]'s field order.
type StateWire = (i32, i32, u32, u32, bool, bool, bool, u32, u32);

/// One monitor of `Monitors()`: frame x y w h, work area x y w h, scale.
type MonitorWire = (i32, i32, i32, i32, i32, i32, i32, i32, f64);

fn parse_state(reply: &glib::Variant) -> Option<ShellState> {
    let (x, y, mods, drag_serial, drag_active, drag_has_files, drag_cancelled, clip, monitors): StateWire =
        reply.try_child_value(0)?.get()?;
    Some(ShellState {
        x,
        y,
        mods,
        drag_serial,
        drag_active,
        drag_has_files,
        drag_cancelled,
        clip_serial: clip,
        monitors_serial: monitors,
    })
}

/// `IslandSlot()`'s `(biiii)`: the room kept for the island in the top bar,
/// if it is there.
fn parse_slot(reply: &glib::Variant) -> Option<Rect> {
    let (shown, x, y, w, h): (bool, i32, i32, i32, i32) = reply.try_child_value(0)?.get()?;
    (shown && w > 0 && h > 0).then(|| Rect {
        x: f64::from(x),
        y: f64::from(y),
        w: f64::from(w),
        h: f64::from(h),
    })
}

fn parse_monitors(reply: &glib::Variant) -> Option<Vec<Monitor>> {
    let list: Vec<MonitorWire> = reply.try_child_value(0)?.get()?;
    let rect = |x: i32, y: i32, w: i32, h: i32| Rect {
        x: f64::from(x),
        y: f64::from(y),
        w: f64::from(w),
        h: f64::from(h),
    };
    Some(
        list.into_iter()
            .map(|(fx, fy, fw, fh, wx, wy, ww, wh, scale)| Monitor {
                frame: rect(fx, fy, fw, fh),
                work: rect(wx, wy, ww, wh),
                scale: if scale > 0.0 { scale } else { 1.0 },
            })
            .collect(),
    )
}

/// The [`InputSample`] for `state`. Escape counts as held when GNOME
/// cancelled the drag (Escape) while the button is still down: the press
/// itself never reaches Vento.
pub(super) fn to_sample(state: &ShellState) -> InputSample {
    let m = state.mods;
    let left_down = m & mask::BUTTON1 != 0;
    InputSample {
        left_down,
        modifiers: Modifiers {
            ctrl: m & mask::CONTROL != 0,
            alt: m & mask::MOD1 != 0,
            shift: m & mask::SHIFT != 0,
            meta: m & (mask::SUPER | mask::MOD4) != 0,
        },
        escape_down: state.drag_cancelled && left_down,
        drag_change_count: i64::from(state.drag_serial),
        drag_has_files: state.drag_has_files,
        clip_change_count: i64::from(state.clip_serial),
        cursor: Point {
            x: f64::from(state.x),
            y: f64::from(state.y),
        },
    }
}

/// `state` with its cancel kept only while the press that made the drag
/// goes on. The extension remembers a cancelled drag until the next one
/// starts, so without this every later click would read as Escape.
/// `released_after` is the drag serial the button was last seen up at.
fn settle_cancel(mut state: ShellState, released_after: &mut Option<u32>) -> ShellState {
    if state.mods & mask::BUTTON1 == 0 {
        *released_after = Some(state.drag_serial);
    }
    state.drag_cancelled &= *released_after != Some(state.drag_serial);
    state
}

/// Local paths of `file:` URIs; others are dropped.
pub(super) fn local_paths(uris: &[String]) -> Vec<PathBuf> {
    uris.iter()
        .filter(|u| u.starts_with("file:"))
        .filter_map(|u| gio::File::for_uri(u).path())
        .collect()
}

/// Whether the left button is down, and where the pointer is, asked of
/// the extension directly; `None` when it is not answering. Blocks for one
/// call; any thread.
pub(super) fn pointer_now() -> Option<(bool, Point)> {
    let bus = super::session_bus().ok()?;
    let state = parse_state(&call(&bus, "State", None).ok()?)?;
    let sample = to_sample(&state);
    Some((sample.left_down, sample.cursor))
}

/// The monitor whose frame holds `p`.
/// X11 pixels per logical pixel, given both widths of the screens. GNOME
/// that scales X11 apps natively has XWayland draw at one whole scale for
/// all screens (2 at 133 %); otherwise X11 and logical pixels are the same.
fn scale_between(x11_width: f64, logical_width: f64) -> f64 {
    if logical_width > 0.0 && x11_width > 0.0 {
        (x11_width / logical_width).round().max(1.0)
    } else {
        1.0
    }
}

/// [`scale_between`] for an X11 screen `x11_width` pixels wide and the
/// screens GNOME has now (asked directly; 1 if the extension is silent).
pub(super) fn x11_scale(x11_width: u16) -> f64 {
    let logical = super::session_bus()
        .ok()
        .and_then(|bus| call(&bus, "Monitors", None).ok())
        .and_then(|r| parse_monitors(&r))
        .map(|monitors| {
            monitors
                .iter()
                .map(|m| m.frame.x + m.frame.w)
                .fold(0.0, f64::max)
        })
        .unwrap_or(0.0);
    scale_between(f64::from(x11_width), logical)
}

pub(super) fn monitor_at(monitors: &[Monitor], p: Point) -> Option<&Monitor> {
    monitors.iter().find(|m| {
        let f = m.frame;
        p.x >= f.x && p.x < f.x + f.w && p.y >= f.y && p.y < f.y + f.h
    })
}

/// The state to report after a failed call: none once the extension is
/// `gone` (its bus name has no owner) or has failed for [`GIVE_UP_AFTER`],
/// else the `previous` one.
fn after_failure(
    previous: Option<ShellState>,
    gone: bool,
    failing_for: Duration,
) -> Option<ShellState> {
    if gone || failing_for >= GIVE_UP_AFTER {
        None
    } else {
        previous
    }
}

/// What the worker last learned.
#[derive(Debug, Default)]
struct Latest {
    state: Option<ShellState>,
    /// When the calls started failing, while they fail.
    failing_since: Option<Instant>,
    /// For [`settle_cancel`].
    released_after: Option<u32>,
    monitors: Option<Vec<Monitor>>,
    /// The `monitors_serial` the monitors above were read at.
    monitors_serial: Option<u32>,
    /// The room in the top bar for the Pocket island, read with the monitors.
    island_slot: Option<Rect>,
}

fn call(
    bus: &gio::DBusConnection,
    method: &str,
    args: Option<&glib::Variant>,
) -> Result<glib::Variant, glib::Error> {
    call_within(bus, method, args, CALL_TIMEOUT_MS)
}

fn call_within(
    bus: &gio::DBusConnection,
    method: &str,
    args: Option<&glib::Variant>,
    timeout_ms: i32,
) -> Result<glib::Variant, glib::Error> {
    bus.call_sync(
        Some(BUS_NAME),
        OBJECT_PATH,
        INTERFACE,
        method,
        args,
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        timeout_ms,
        gio::Cancellable::NONE,
    )
}

/// Asks for a fresh state, and fresh monitors when their serial moved, into
/// `latest`. `reported_missing` keeps a silent extension to one warning.
fn refresh(bus: &gio::DBusConnection, latest: &Mutex<Latest>, reported_missing: &mut bool) {
    let state = call(bus, "State", None)
        .map_err(|e| {
            let gone = e.matches(gio::DBusError::ServiceUnknown)
                || e.matches(gio::DBusError::NameHasNoOwner);
            (e.message().to_owned(), gone)
        })
        .and_then(|r| parse_state(&r).ok_or(("unexpected State reply".to_owned(), true)));
    let state = match state {
        Ok(s) => {
            *reported_missing = false;
            let mut l = latest.lock().unwrap();
            l.failing_since = None;
            Some(settle_cancel(s, &mut l.released_after))
        }
        Err((e, gone)) => {
            let mut l = latest.lock().unwrap();
            let since = *l.failing_since.get_or_insert_with(Instant::now);
            log::debug!("State call failed (gone: {gone}): {e}");
            let kept = after_failure(l.state, gone, since.elapsed());
            if kept.is_none() && !*reported_missing {
                log::warn!("Vento's GNOME Shell extension is not answering: {e}");
                *reported_missing = true;
            }
            l.state = kept;
            return;
        }
    };
    let stale = state.is_some_and(|s| {
        let l = latest.lock().unwrap();
        l.monitors.is_none() || l.monitors_serial != Some(s.monitors_serial)
    });
    let monitors = if stale {
        call(bus, "Monitors", None)
            .ok()
            .and_then(|r| parse_monitors(&r))
    } else {
        None
    };
    // Asked again with the monitors: the extension forgets it when GNOME
    // turns extensions off (the lock screen does), and starts over.
    let slot = stale.then(|| {
        let _ = call(bus, "ReserveIsland", Some(&(true,).to_variant()));
        call(bus, "IslandSlot", None)
            .ok()
            .and_then(|r| parse_slot(&r))
    });
    let mut l = latest.lock().unwrap();
    l.state = state;
    if let (Some(m), Some(s)) = (monitors, state) {
        l.monitors = Some(m);
        l.monitors_serial = Some(s.monitors_serial);
        l.island_slot = slot.flatten();
    }
}

/// Answers each wake-up with a [`refresh`]. Ends when the [`Shell`] is
/// dropped.
fn worker(
    bus: gio::DBusConnection,
    wake: Receiver<()>,
    latest: Arc<Mutex<Latest>>,
    mut reported_missing: bool,
) {
    while wake.recv().is_ok() {
        // Answer every request queued meanwhile with this one call.
        while wake.try_recv().is_ok() {}
        refresh(&bus, &latest, &mut reported_missing);
    }
}

/// The link to the extension.
#[derive(Debug)]
pub(super) struct Shell {
    bus: Option<gio::DBusConnection>,
    wake: Option<Sender<()>>,
    latest: Arc<Mutex<Latest>>,
}

impl Shell {
    pub fn connect() -> Self {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let bus = super::session_bus()
            .inspect_err(|e| log::warn!("no session bus: {e}"))
            .ok();
        let wake = bus.clone().map(|bus| {
            if let Ok(v) = call(&bus, "Version", None)
                && let Some(version) = v.try_child_value(0).and_then(|v| v.get::<u32>())
                && version != API_VERSION
            {
                log::warn!("the GNOME Shell extension speaks API {version}, Vento {API_VERSION}");
            }
            // Once here, so the screens and a first state are known before
            // the app asks; the worker answers from then on.
            let mut reported_missing = false;
            refresh(&bus, &latest, &mut reported_missing);
            let (tx, rx) = mpsc::channel();
            let latest = latest.clone();
            std::thread::Builder::new()
                .name("vento-shell".into())
                .spawn(move || worker(bus, rx, latest, reported_missing))
                .expect("spawn the shell worker");
            tx
        });
        Self { bus, wake, latest }
    }

    /// The latest state, or `None` while the extension isn't answering.
    /// Asks the worker for a fresh one either way.
    pub fn state(&self) -> Option<ShellState> {
        if let Some(wake) = &self.wake {
            let _ = wake.send(());
        }
        self.latest.lock().unwrap().state
    }

    /// The screens, primary first; empty until the extension has answered.
    pub fn monitors(&self) -> Vec<Monitor> {
        self.latest
            .lock()
            .unwrap()
            .monitors
            .clone()
            .unwrap_or_default()
    }

    /// The room the extension keeps for the Pocket island in the top bar.
    pub fn island_slot(&self) -> Option<Rect> {
        self.latest.lock().unwrap().island_slot
    }

    /// The serial the current monitors were read at.
    pub fn monitors_serial(&self) -> Option<u32> {
        self.latest.lock().unwrap().monitors_serial
    }

    /// The clipboard's serial now, asked for directly (not the worker's
    /// latest). Blocks for one call.
    pub fn clip_serial_now(&self) -> Option<u32> {
        let bus = self.bus.as_ref()?;
        call(bus, "State", None)
            .ok()
            .and_then(|r| parse_state(&r))
            .map(|s| s.clip_serial)
    }

    /// The clipboard's MIME types (and X11 targets). Blocks for one call.
    pub fn clipboard_types(&self) -> Vec<String> {
        let Some(bus) = &self.bus else {
            return Vec::new();
        };
        call(bus, "ClipboardTypes", None)
            .ok()
            .and_then(|r| r.try_child_value(0)?.get::<Vec<String>>())
            .unwrap_or_default()
    }

    /// The clipboard's data in `mime`; `None` if it can't be read. Blocks
    /// for one call.
    pub fn read_clipboard(&self, mime: &str) -> Option<Vec<u8>> {
        let bus = self.bus.as_ref()?;
        let args = (mime,).to_variant();
        let data = call_within(bus, "ReadClipboard", Some(&args), READ_TIMEOUT_MS)
            .inspect_err(|e| log::debug!("reading {mime} failed: {e}"))
            .ok()?
            .try_child_value(0)?
            .get::<Vec<u8>>()?;
        // The extension answers an unreadable type with no bytes.
        (!data.is_empty()).then_some(data)
    }

    /// The files of the drag in progress. Blocks for one call.
    pub fn drag_uris(&self) -> Vec<String> {
        let Some(bus) = &self.bus else {
            return Vec::new();
        };
        call(bus, "DragUris", None)
            .ok()
            .and_then(|r| r.try_child_value(0)?.get::<Vec<String>>())
            .unwrap_or_default()
    }

    /// Calls `Activate` or `Deactivate` for this process.
    pub fn focus(&self, method: &str) -> bool {
        let Some(bus) = &self.bus else {
            return false;
        };
        let pid = (std::process::id(),).to_variant();
        call(bus, method, Some(&pid))
            .ok()
            .and_then(|r| r.try_child_value(0)?.get::<bool>())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_reply(s: StateWire) -> glib::Variant {
        glib::Variant::tuple_from_iter([s.to_variant()])
    }

    #[test]
    fn state_replies_are_read_field_by_field() {
        let reply = state_reply((812, 440, 0x104, 3, true, true, false, 9, 2));
        assert_eq!(
            parse_state(&reply),
            Some(ShellState {
                x: 812,
                y: 440,
                mods: 0x104,
                drag_serial: 3,
                drag_active: true,
                drag_has_files: true,
                drag_cancelled: false,
                clip_serial: 9,
                monitors_serial: 2,
            })
        );
        assert_eq!(
            parse_state(&glib::Variant::tuple_from_iter([1u32.to_variant()])),
            None
        );
    }

    #[test]
    fn monitor_replies_become_rects_and_a_scale() {
        let list = vec![
            (0, 0, 2016, 1112, 67, 32, 1949, 1080, 1.0f64),
            (2016, 0, 100, 100, 2016, 0, 100, 100, 0.0),
        ];
        let reply = glib::Variant::tuple_from_iter([list.to_variant()]);
        let m = parse_monitors(&reply).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[0].work,
            Rect {
                x: 67.0,
                y: 32.0,
                w: 1949.0,
                h: 1080.0
            }
        );
        assert_eq!(m[0].frame.w, 2016.0);
        assert_eq!(m[1].scale, 1.0, "a nonsense scale is taken as 1");
    }

    #[test]
    fn samples_carry_buttons_modifiers_and_counters() {
        let s = to_sample(&ShellState {
            x: 5,
            y: 6,
            mods: mask::BUTTON1 | mask::CONTROL | mask::MOD1 | mask::SHIFT,
            drag_serial: 4,
            drag_has_files: true,
            clip_serial: 11,
            ..Default::default()
        });
        assert!(s.left_down && s.drag_has_files && !s.escape_down);
        assert_eq!(
            s.modifiers,
            Modifiers {
                ctrl: true,
                alt: true,
                shift: true,
                meta: false
            }
        );
        assert_eq!((s.drag_change_count, s.clip_change_count), (4, 11));
        assert_eq!(s.cursor, Point { x: 5.0, y: 6.0 });
        assert!(
            to_sample(&ShellState {
                mods: mask::SUPER,
                ..Default::default()
            })
            .modifiers
            .meta
        );
        assert!(
            to_sample(&ShellState {
                mods: mask::MOD4,
                ..Default::default()
            })
            .modifiers
            .meta
        );
    }

    #[test]
    fn a_cancelled_drag_reads_as_escape_only_while_the_button_is_down() {
        let held = ShellState {
            mods: mask::BUTTON1,
            drag_cancelled: true,
            ..Default::default()
        };
        assert!(to_sample(&held).escape_down);
        let released = ShellState {
            drag_cancelled: true,
            ..Default::default()
        };
        assert!(!to_sample(&released).escape_down);
    }

    #[test]
    fn a_missed_call_keeps_the_last_state_for_a_while() {
        let held = Some(ShellState {
            mods: mask::BUTTON1,
            ..Default::default()
        });
        let brief = Duration::from_millis(300);
        assert_eq!(
            after_failure(held, false, brief),
            held,
            "the wheel keeps its drag"
        );
        assert_eq!(after_failure(held, false, GIVE_UP_AFTER), None);
        assert_eq!(
            after_failure(held, true, Duration::ZERO),
            None,
            "no extension, no state"
        );
    }

    #[test]
    fn a_cancel_lasts_only_while_its_press_goes_on() {
        let mut released = None;
        let at = |mods, serial, cancelled| ShellState {
            mods,
            drag_serial: serial,
            drag_cancelled: cancelled,
            ..Default::default()
        };
        let idle = settle_cancel(at(0, 4, false), &mut released);
        assert!(!idle.drag_cancelled);
        // Drag 5 starts and is cancelled while the button stays down.
        let held = settle_cancel(at(mask::BUTTON1, 5, true), &mut released);
        assert!(held.drag_cancelled, "Escape while the drag's press goes on");
        let up = settle_cancel(at(0, 5, true), &mut released);
        assert!(!up.drag_cancelled);
        let click = settle_cancel(at(mask::BUTTON1, 5, true), &mut released);
        assert!(!click.drag_cancelled, "a later click is no Escape");
        let next = settle_cancel(at(mask::BUTTON1, 6, true), &mut released);
        assert!(next.drag_cancelled, "the next drag's cancel counts again");
    }

    #[test]
    fn x11_scale_is_a_whole_number_at_least_one() {
        // 133 %: XWayland at 2.
        assert_eq!(scale_between(3072.0, 1536.0), 2.0);
        // No native scaling: the same pixels.
        assert_eq!(scale_between(2048.0, 2048.0), 1.0);
        assert_eq!(scale_between(2048.0, 0.0), 1.0);
        assert_eq!(scale_between(1000.0, 2048.0), 1.0);
    }

    #[test]
    fn only_local_uris_become_paths() {
        let uris = [
            "file:///home/me/My%20File.txt".to_owned(),
            "https://example.com/x".to_owned(),
            "trash:///x".to_owned(),
        ];
        assert_eq!(local_paths(&uris), [PathBuf::from("/home/me/My File.txt")]);
    }

    #[test]
    fn points_find_the_monitor_holding_them() {
        let m = |x: f64, scale| Monitor {
            frame: Rect {
                x,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            work: Rect {
                x,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            scale,
        };
        let ms = [m(0.0, 1.0), m(100.0, 2.0)];
        assert_eq!(
            monitor_at(&ms, Point { x: 150.0, y: 5.0 }).unwrap().scale,
            2.0
        );
        assert_eq!(
            monitor_at(&ms, Point { x: 99.9, y: 5.0 }).unwrap().scale,
            1.0
        );
        assert!(monitor_at(&ms, Point { x: 200.0, y: 5.0 }).is_none());
    }
}
