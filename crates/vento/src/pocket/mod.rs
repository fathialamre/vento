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
mod news;
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
    ComponentHandle, Image, LogicalPosition, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer,
    TimerMode, VecModel,
};
use vento_core::actions::ActionId;
use vento_core::clip_history::ClipEntry;
use vento_core::geometry::{Point, Rect};
use vento_core::i18n::errors::io_reason;
use vento_core::i18n::{fill, tr, with_language};
use vento_core::news::{Stage, Tone};
use vento_core::pocket::{ItemSource, Pocket, PocketItem, pocket_dir, pocket_path};
use vento_core::pocket_zone::{IslandState, Presence};
use vento_core::settings::{
    IslandMode, IslandPlace, IslandShape, IslandSpot, PocketView, Settings, ShimmerEvery,
    ShimmerStyle,
};
use vento_core::shimmer::{AWAY_AFTER, MomentConditions, moment_allowed, motion_reduced};
use vento_core::trigger::InputSample;
use vento_platform::{
    CatcherEvent, DropCatcher, Dropped, IslandHome, Platform, PlatformError, RgbaImage,
};

use crate::appearance;
use crate::exec::files_copied_notice;
use crate::notice::{ButtonLabel, JobNotice, Notice, NoticeSink};
use crate::toast::Toaster;
use crate::ui::{ActivityRow, Icons, PocketIsland};

#[cfg(target_os = "linux")]
use crate::drag;
use add::{AddPlan, Entry, Failure, Print, SavedPrints, plan_new, saved_content, store};
use clip::ClipController;
use clip_view::{ClipView, set_rows_in_place, view_index};
use geometry::{
    Frames, NewsRects, VirtualSpot, drag_spot, frames, hits, home_at, hover_event,
    interactive_rects, island_inputs, screen_key, virtual_frames, virtual_mid_x, window_frame,
};
use kinds::Kinds;
use list_view::ListView;
use menu::{ItemMenu, MenuAction, apps_source, menu_targets, opens_menu};
use news::{ActivityRowData, News, NewsView, today_start, with_details};
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
/// How often the virtual island asks whether the front window has a bar
/// under it: often enough to follow a window that is maximized or moved,
/// rarely enough to cost nothing on the wheel's samples.
pub(super) const FRONT_CHECK: Duration = Duration::from_millis(250);
/// How far, in px, the cursor moves from the press on the grip before the
/// island follows it.
const DRAG_SLOP: f64 = 3.0;
/// How often a held grip moves the island with the cursor, at most: once a
/// frame.
const DRAG_FRAME: Duration = Duration::from_millis(16);
/// How often the island asks the system whether it wants less motion: a
/// system call on macOS and Windows, so not on every sample.
const REDUCE_CHECK: Duration = Duration::from_secs(30);

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
            c.toast.notify(
                Notice::error(text).with_icon(ActionId::Pocket),
                c.toast_at(),
            );
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

/// Which view's icon the collapsed wings show (while no Undo is on offer,
/// which they show instead), when the island showed `last`: with items in
/// the Pocket, its icon and count. With an empty one, the icon of the view
/// resting on the island opens (see [`hover_view`]) when that view has
/// something; when it opens one with nothing (an empty Pocket or
/// Screenshots), the clipboard icon while there is some history, else the
/// photo icon while there are screenshots.
fn wing_view(pocket_empty: bool, clip_has: bool, shots_has: bool, last: View) -> View {
    if !pocket_empty {
        return View::Pocket;
    }
    match hover_view(true, clip_has, shots_has, None, last) {
        View::Clipboard if clip_has => View::Clipboard,
        View::Screenshots if shots_has => View::Screenshots,
        _ if clip_has => View::Clipboard,
        _ if shots_has => View::Screenshots,
        _ => View::Pocket,
    }
}

/// Whether the island is the virtual one: chosen in the settings where it
/// is offered.
pub(super) fn virtual_active(mode: IslandMode, home: &IslandHome, supported: bool) -> bool {
    mode == IslandMode::Virtual && virtual_offered(home, supported)
}

/// Whether the virtual island can be chosen at `home`: on a screen with no
/// notch (where the notch island stays), and where the platform can place
/// it and let clicks through it (`supported`).
pub(crate) fn virtual_offered(home: &IslandHome, supported: bool) -> bool {
    home.notch.is_none() && supported
}

/// Where the virtual island lies at `home` with the island's settings, or
/// `None` when the island is not the virtual one there (`supported` says the
/// platform can place it).
fn virtual_spot_of(
    mode: IslandMode,
    home: &IslandHome,
    supported: bool,
    place: IslandSpot,
    bar_under: bool,
    shape: IslandShape,
) -> Option<VirtualSpot> {
    virtual_active(mode, home, supported).then_some(VirtualSpot {
        place,
        bar_under,
        shape,
    })
}

/// The middle of the island dragged by its grip: where it was at the press
/// (`start_mid`), moved as far as the cursor has moved since (`press_x` to
/// `cursor_x`, global x).
fn drag_mid(start_mid: f64, press_x: f64, cursor_x: f64) -> f64 {
    start_mid + (cursor_x - press_x)
}

/// Whether the cursor pressed on the grip at `press_x` has moved far enough
/// (`cursor_x`, global x) for the island to follow it: at least
/// [`DRAG_SLOP`], so a click or a double-click doesn't move it.
fn drag_started(press_x: f64, cursor_x: f64) -> bool {
    (cursor_x - press_x).abs() >= DRAG_SLOP
}

/// Whether a drag of the grip under way (`dragging`) has ended: the left
/// button is up at a sample, whether or not the window saw the release.
fn drag_ended(dragging: bool, left_down: bool) -> bool {
    dragging && !left_down
}

/// The island's home: for the virtual island, the notch-free home of the
/// screen `key` names among `homes`; otherwise, or for a screen that is gone
/// (or has a notch), `fallback`, the primary one. The key counts only where
/// the virtual island is offered at `fallback` (`supported` says the
/// platform can place it): a Mac with a notch keeps its notch island, and
/// GNOME's top bar slot stays where it is.
fn home_for(
    mode: IslandMode,
    homes: &[IslandHome],
    key: Option<&str>,
    fallback: Option<IslandHome>,
    supported: bool,
) -> Option<IslandHome> {
    let virtual_here =
        mode == IslandMode::Virtual && fallback.is_some_and(|h| virtual_offered(&h, supported));
    key.filter(|_| virtual_here)
        .and_then(|key| geometry::home_by_key(homes, key))
        .or(fallback)
}

/// The key of the screen the island is on, as [`home_for`] finds its home
/// (`key` the screen in the settings, `fallback` the primary home): the
/// screen whose place the settings read and write. `None` with no home.
pub(crate) fn island_screen_key(
    mode: IslandMode,
    homes: &[IslandHome],
    key: Option<&str>,
    fallback: Option<IslandHome>,
    supported: bool,
) -> Option<String> {
    home_for(mode, homes, key, fallback, supported).map(|h| screen_key(&h.screen))
}

/// Whether the island is already saved in the middle of the screen whose
/// key is `key`: its spot there is the centre (`spot`) and the settings
/// have it on that screen (`stored`). A double-click of the grip then has
/// nothing to save.
fn centre_saved(spot: IslandSpot, stored: Option<&str>, key: &str) -> bool {
    spot == IslandSpot::Side(IslandPlace::Centre) && stored == Some(key)
}

/// Whether the drop box is wanted: something is dragged (and not by the
/// Pocket's own drag out) anywhere on the screen of the island in the
/// corner by the clock, or near the virtual circle (`in_zone`, the drag in
/// its zone), which then opens as the drop orb. The circle already open on
/// a view (`open_already`) stays open: the drag drops on it, as on the pill.
fn drop_box_wanted(
    corner: bool,
    circle_virtual: bool,
    in_zone: bool,
    open_already: bool,
    dragging: bool,
    latch_running: bool,
) -> bool {
    (corner || (circle_virtual && in_zone && !open_already)) && dragging && !latch_running
}

/// Whether the circle's resting pill rests as the round disc: it stretches
/// only to say something (paused with Resume, Keep Awake's time), and only
/// while the pointer is on it.
fn rest_round(paused: bool, awake: bool, hovered: bool) -> bool {
    !(hovered && (paused || awake))
}

/// Whether the news pill rests round next, given `last`. Off the circle it
/// never does. With news shown it follows [`news::circle_round`]; with none
/// it keeps `last`, so the pill fades out in the form it had.
fn news_round_next(last: bool, circle: bool, view: Option<&NewsView>, hovered: bool) -> bool {
    match (circle, view) {
        (false, _) => false,
        (true, Some(v)) => news::circle_round(v, hovered),
        (true, None) => last,
    }
}

/// How the window numbers a shimmer style: its place in
/// [`ShimmerStyle::ALL`], 0 for Off.
fn shimmer_index(s: ShimmerStyle) -> i32 {
    ShimmerStyle::ALL
        .iter()
        .position(|&x| x == s)
        .map_or(0, |i| i as i32)
}

/// Whether the system's Reduce motion is read again at `now`, given when it
/// was `last` read: never read yet, or [`REDUCE_CHECK`] ago.
fn reduce_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= REDUCE_CHECK)
}

/// Whether the island rests where a shimmer may play: it is ready, laid out
/// as the virtual island at `spot`, not open, and resting as the pill or the
/// circle, not as the lip or the half-moon over a window's bar. (News is
/// ruled on apart; the window draws the same rule with it, so a moment under
/// way ends when news comes or the island opens.)
fn virtual_resting(warm: bool, spot: Option<VirtualSpot>, expanded: bool) -> bool {
    warm && spot.is_some_and(|s| !s.bar_under) && !expanded
}

/// Whether the pointer moving again after `idle_before` without any starts
/// the wait for the next shimmer moment again: after [`AWAY_AFTER`] away,
/// the next one waits a whole period from the return.
fn rearm_on_return(idle_before: Duration) -> bool {
    idle_before >= AWAY_AFTER
}

/// Whether news that `was` shown and `now` is not starts the wait for the
/// next shimmer moment again, so none comes right after a job or a notice.
fn rearm_on_news_end(was: bool, now: bool) -> bool {
    was && !now
}

/// How long until the next shimmer moment with `style` and `every`: one
/// period, or `None` for Off, which plays none.
fn next_arm(style: ShimmerStyle, every: ShimmerEvery) -> Option<Duration> {
    (style != ShimmerStyle::Off).then(|| every.period())
}

/// The Pocket's count on the circle's badge: nothing for none, the number up
/// to 99, then "99+".
fn badge_text(count: usize) -> String {
    match count {
        0 => String::new(),
        1..=99 => count.to_string(),
        _ => "99+".into(),
    }
}

/// The window's `glass` for the settings' Glass level, in percent.
fn glass_alpha(percent: u32) -> f32 {
    percent as f32 / 100.0
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

/// Asks the executor for the serial of the operation Undo would undo now.
type LastSerial = Box<dyn Fn() -> Option<u64>>;

/// Learns whether the island is the virtual one.
type OnVirtual = Box<dyn Fn(bool)>;

/// Saves where the island was dragged: the key of its screen and its spot
/// there.
type OnPlaceDropped = Box<dyn Fn(Option<String>, IslandSpot)>;

/// The open virtual island dragged by its grip: the cursor's global x at
/// the press and the island's middle then, the home and spot it was dragged
/// from, and whether the cursor has moved far enough for the island to
/// follow ([`drag_started`]).
#[derive(Debug, Clone, Copy)]
struct GripDrag {
    press_x: f64,
    start_mid: f64,
    from: (IslandHome, IslandSpot),
    moving: bool,
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
    input_shape: RefCell<Option<Vec<Rect>>>,
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
    /// The island's mode in the settings, and the virtual island's spot on
    /// the top edge of its screen: the screen's own in the settings, or
    /// where it is dragged to.
    mode: Cell<IslandMode>,
    spot: Cell<IslandSpot>,
    /// The drag of the open virtual island's grip under way.
    drag: Cell<Option<GripDrag>>,
    /// Moves the dragged island to the cursor at the next frame, while the
    /// grip's moves come in.
    drag_frame: Timer,
    /// Whether the platform can read the cursor alone, for the dragged
    /// island to follow it at every frame; elsewhere it follows at the
    /// samples.
    follows_frames: bool,
    /// Every screen's home, as last read (on a rehome and as a drag starts).
    homes: RefCell<Vec<IslandHome>>,
    /// What saves the island's place once it is dropped.
    on_place_dropped: RefCell<Option<OnPlaceDropped>>,
    /// The island is saving its own place: the change that comes back from
    /// the settings leaves it open.
    saving_place: Cell<bool>,
    /// The virtual island's shape in the settings: the island, or the circle.
    shape: Cell<IslandShape>,
    /// What the window was last told of the circle: the news pill and the
    /// resting pill rest as the round disc.
    news_round: Cell<bool>,
    rest_round: Cell<bool>,
    /// Plays the next shimmer moment, one period after the island starts
    /// resting (and after every moment, played or not).
    shimmer_timer: Timer,
    /// When the cursor or a button last changed, for the shimmer to know
    /// whether anyone is at the computer.
    last_input: Cell<Instant>,
    /// The cursor and the left button at the last sample.
    last_seen: Cell<Option<(Point, bool)>>,
    /// When the system's Reduce motion was last read, and what it said.
    reduce_cache: Cell<(Option<Instant>, bool)>,
    /// The front window has a bar under the virtual island, which then
    /// rests as the lip; asked at most every [`FRONT_CHECK`].
    bar_under: Cell<bool>,
    /// When the front window was last asked about; `None` to ask at the
    /// next sample.
    front_checked: Cell<Option<Instant>>,
    /// Where the window was last laid out for the virtual island; `None`
    /// in the other modes.
    laid_out: Cell<Option<VirtualSpot>>,
    /// Whether the desktop was last told to keep room for the island in
    /// its top bar, which it does until told otherwise.
    slot_kept: Cell<bool>,
    /// The notices and jobs the virtual island took.
    news: RefCell<News>,
    /// The news last given to the window; `None` while it shows none.
    news_view: RefCell<Option<NewsView>>,
    /// What the paused pill's Resume runs: vento resumes.
    on_resume: RefCell<Option<Box<dyn Fn()>>>,
    /// What learns whether the island is the virtual one, when that changes.
    on_virtual: RefCell<Option<OnVirtual>>,
    /// The island is open on Activity, today's jobs and results, over the
    /// view.
    activity: Cell<bool>,
    /// The rows Activity shows, and the window's copy of them.
    activity_rows: RefCell<Vec<ActivityRowData>>,
    activity_model: Rc<VecModel<ActivityRow>>,
    /// The executor's undo serial now, for Activity's Undo.
    last_serial: RefCell<Option<LastSerial>>,
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
        let (pocket_view, mode, shape, glass) = {
            let s = settings.borrow();
            (s.pocket_view, s.island_mode, s.island_shape, s.island_glass)
        };
        window.set_glass(glass_alpha(glass));
        let list = ListView::new(&window, pocket_view);
        let activity_model = Rc::new(VecModel::default());
        window.set_activity_rows(ModelRc::from(activity_model.clone()));
        let homes = platform.island_homes();
        let follows_frames = platform.cursor().is_some();
        let (home, spot) = {
            let s = settings.borrow();
            let home = home_for(
                mode,
                &homes,
                s.island_screen.as_deref(),
                platform.pocket_home(),
                platform.virtual_island_supported(),
            );
            let key = home.map(|h| screen_key(&h.screen));
            (home, s.island_spot(key.as_deref()))
        };
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
            input_shape: RefCell::new(None),
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
            mode: Cell::new(mode),
            spot: Cell::new(spot),
            drag: Cell::new(None),
            drag_frame: Timer::default(),
            follows_frames,
            homes: RefCell::new(homes),
            on_place_dropped: RefCell::new(None),
            saving_place: Cell::new(false),
            shape: Cell::new(shape),
            news_round: Cell::new(false),
            rest_round: Cell::new(false),
            shimmer_timer: Timer::default(),
            last_input: Cell::new(Instant::now()),
            last_seen: Cell::new(None),
            reduce_cache: Cell::new((None, false)),
            bar_under: Cell::new(false),
            front_checked: Cell::new(None),
            laid_out: Cell::new(None),
            slot_kept: Cell::new(true),
            news: RefCell::new(News::default()),
            news_view: RefCell::new(None),
            on_resume: RefCell::new(None),
            on_virtual: RefCell::new(None),
            activity: Cell::new(false),
            activity_rows: RefCell::new(Vec::new()),
            activity_model,
            last_serial: RefCell::new(None),
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
        w.on_grip_pressed(move || {
            if let Some(c) = c() {
                c.grip_pressed();
            }
        });
        let c = with.clone();
        w.on_grip_moved(move || {
            if let Some(c) = c() {
                c.grip_moved();
            }
        });
        let c = with.clone();
        w.on_grip_released(move || {
            if let Some(c) = c() {
                c.grip_released();
            }
        });
        let c = with.clone();
        w.on_grip_centre(move || {
            if let Some(c) = c() {
                c.grip_centre();
            }
        });
        let c = with.clone();
        w.on_news_button_clicked(move || {
            if let Some(c) = c() {
                c.news_button_clicked();
            }
        });
        let c = with.clone();
        w.on_news_chip_clicked(move || {
            if let Some(c) = c() {
                c.open_activity();
            }
        });
        let c = with.clone();
        w.on_activity_button_clicked(move |i| {
            if let (Some(c), Ok(i)) = (c(), usize::try_from(i)) {
                c.activity_button_clicked(i);
            }
        });
        let c = with.clone();
        w.on_rest_button_clicked(move || {
            if let Some(c) = c() {
                c.resume_clicked();
            }
        });
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
                .map(|()| {
                    self.toast
                        .notify(files_copied_notice(paths.len()), self.toast_at());
                })
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
            self.toast.notify(
                Notice::error(text).with_icon(ActionId::Pocket),
                self.toast_at(),
            );
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
            self.toast.notify(
                Notice::info(tr("Those items are no longer there")).with_icon(ActionId::Pocket),
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
        let news = self.news_rects(&f);
        let resume = self.resume_rect(&f);
        let over = hits(
            &f,
            self.island.borrow().presence(),
            news.as_ref(),
            resume,
            at,
        );
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
    /// without), takes no pointer and no drops; the virtual island's pill is
    /// grey, and only its Resume takes clicks. Once unpaused, the next
    /// sample decides again.
    pub fn set_paused(&self, paused: bool) {
        self.paused.set(paused);
        self.clip.set_paused(paused);
        self.window.set_rest_paused(paused);
        if !paused || !self.warm.get() {
            return;
        }
        self.close_card();
        // Paused, the island shows no news (and takes none) until unpaused.
        self.show_news(None);
        let after = self.settle_now();
        debug!("pocket paused: {after:?}");
    }

    /// Sets how Activity asks the executor for its undo serial now (see
    /// `Executor::last_serial`).
    pub fn set_last_serial(&self, f: LastSerial) {
        *self.last_serial.borrow_mut() = Some(f);
    }

    /// Sets what the paused pill's Resume runs.
    pub fn set_on_resume(&self, f: Box<dyn Fn()>) {
        *self.on_resume.borrow_mut() = Some(f);
    }

    /// Tells `f` whether the island is the virtual one, now and whenever
    /// that changes: while paused, its pill's Resume needs the samples
    /// (macOS and Windows have no input shape, so the pointer is followed).
    pub fn set_on_virtual(&self, f: OnVirtual) {
        f(self.laid_out.get().is_some());
        *self.on_virtual.borrow_mut() = Some(f);
    }

    /// Sets what saves the virtual island's place once it is dropped after
    /// a drag of its grip, or put back in the middle: the key of its screen
    /// and its spot there.
    pub fn on_place_dropped(&self, f: OnPlaceDropped) {
        *self.on_place_dropped.borrow_mut() = Some(f);
    }

    /// Keep Awake runs with `status` (the time it has left, empty for no
    /// end; `None` while it does not run), and ends soon: the virtual
    /// island's resting pill shows a cup and the time. `left` is how much of
    /// a timed session is left, 1 down to 0, for the circle's ring; `None`
    /// for one with no length.
    pub fn set_awake(&self, status: Option<String>, ending_soon: bool, left: Option<f32>) {
        let w = &self.window;
        w.set_rest_awake(status.is_some());
        w.set_rest_awake_text(status.unwrap_or_default().into());
        w.set_rest_awake_soon(ending_soon);
        w.set_rest_awake_fraction(left.unwrap_or(-1.0));
    }

    /// Shows `n` in the virtual island; where it does not take it, as a
    /// toast under the island (none for a notice for the island only).
    pub fn notify(&self, n: Notice) {
        self.toast.notify(n, self.toast_at());
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
    /// pause, by closing the island with the shortcut and by a change of
    /// the island's settings. The virtual island rests in sight even with
    /// nothing in it.
    fn settle_now(&self) -> Presence {
        // A drag of the grip under way is called off: the island goes back
        // to where the settings have it.
        if self.drag.take().is_some() {
            debug!("pocket grip drag called off");
            self.drag_frame.stop();
            self.rehome();
        }
        let visible = self.has_content() || self.island_frames().is_some_and(|f| f.virtual_island);
        let before = self.island.borrow().presence();
        let after = self.island.borrow_mut().settle(visible);
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
        if let Some(copied) = self.clip.observe(sample.clip_change_count) {
            self.notify(copied);
        }
        self.note_input(sample);
        if !self.warm.get() {
            return;
        }
        if self.paused.get() {
            self.follow_resume(sample.cursor);
            return;
        }
        // The grip held moves the island with the cursor; the button up
        // drops it where the cursor is, whether or not the window saw the
        // release.
        if drag_ended(self.drag.get().is_some(), sample.left_down) {
            self.follow_drag(sample.cursor);
            self.end_drag();
            // The window may not have seen the button go up: the grip would
            // keep the pointer grabbed.
            if let Some(f) = self.island_frames() {
                let w = self.window_frame(&f);
                self.dispatch(WindowEvent::PointerReleased {
                    position: LogicalPosition::new(
                        (sample.cursor.x - w.x) as f32,
                        (sample.cursor.y - w.y) as f32,
                    ),
                    button: PointerEventButton::Left,
                });
            }
        } else {
            self.follow_drag(sample.cursor);
        }
        let now = Instant::now();
        if reduce_due(self.reduce_cache.get().0, now) {
            self.refresh_reduce_motion(now);
        }
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
        // A drag from elsewhere takes the island for a drop: the card and
        // Activity go.
        if dragging && !self.latch.running() {
            self.close_card();
            self.close_activity();
        }
        let Some(home) = self.home.get() else {
            return;
        };
        // The virtual island follows the front window's bar, and is laid out
        // again when it changes form or the mode takes effect.
        if self.virtual_spot(&home).is_some() {
            self.check_front(&home, now);
        }
        let spot = self.virtual_spot(&home);
        if spot != self.laid_out.get() {
            self.apply_home(&home);
        }
        // Only the virtual island shows the news. The pointer on the pill
        // holds a result, though only its button and chip take clicks; on
        // the open island it is on the panel, not the news.
        let hovered = geometry::news_hovered(
            self.island.borrow().presence(),
            self.news_rects(&self.frames_of(&home)).as_ref(),
            sample.cursor,
        );
        let view = self.news.borrow_mut().tick(now, hovered);
        self.show_news(view.filter(|_| spot.is_some()));
        self.show_news_round(&home, hovered);
        self.show_rest_round(
            &self.frames_of(&home),
            self.island.borrow().presence(),
            sample.cursor,
        );
        if self.activity.get() {
            self.refresh_activity();
        }
        // The virtual island lives on top even where the taskbar's clock
        // has a corner for the island.
        let corner = home.tray_corner.is_some() && spot.is_none();
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
        let circle = self.circle_virtual(&home);
        // Near the circle: in its zone, which holds the orb too.
        let near = geometry::contains(&self.frames_of(&home).zone, sample.cursor);
        // Open on a view rather than as the orb.
        let open = self.island.borrow().presence() == Presence::Expanded && !self.drop_box.get();
        self.show_drop_box(
            drop_box_wanted(corner, circle, near, open, dragging, self.latch.running()),
            now,
        );
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
            self.close_card_into_disc();
            if !self.has_content() {
                after = self.island.borrow_mut().settle(f.virtual_island);
            }
        }
        // Not paused: there is no Resume to click.
        let news = self.news_rects(&f);
        self.card_hover(
            after,
            hits(&f, after, news.as_ref(), None, sample.cursor),
            now,
        );
        // Opened by resting on it (by a push, on the virtual island): with
        // an empty Pocket, on the view the wings show. A push while a result
        // shows opens Activity instead.
        if before != Presence::Expanded
            && after == Presence::Expanded
            && !dragging
            && !pinned
            && !self.card.is_up()
        {
            let result_up = self
                .news_view
                .borrow()
                .as_ref()
                .is_some_and(|v| v.stage == Stage::Result);
            if result_up {
                self.open_activity();
            }
            let last = self.shown_view();
            let undo_view = self.undo.borrow().as_ref().map(|(u, _)| u.view());
            let v = hover_view(
                self.pocket.borrow().is_empty(),
                self.clip_has(),
                self.shots_have(),
                undo_view,
                last,
            );
            if v != last && !self.activity.get() {
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
        if dragging && hits(&f, after, news.as_ref(), None, sample.cursor) && headed_in {
            self.set_view(View::Pocket);
        }
        // One over the Pocket list (not the list's own drag out) goes back
        // to the kinds, which say what a drop does.
        if dragging
            && !self.latch.running()
            && hits(&f, after, news.as_ref(), None, sample.cursor)
            && self.list_shown()
        {
            self.close_pocket_list();
        }
        let f = self.island_frames().unwrap_or(f);
        self.window.set_bar_hover(
            home.slot.is_some()
                && !f.virtual_island
                && after == Presence::Collapsed
                && geometry::contains(&f.collapsed, sample.cursor),
        );
        // The push against the top edge fills under the resting island; set
        // only when it moves, so nothing redraws at rest.
        if f.virtual_island {
            let progress = if after == Presence::Collapsed {
                self.island.borrow().push_progress(now)
            } else {
                0.0
            };
            if self.window.get_push_progress() != progress {
                self.window.set_push_progress(progress);
            }
        }
        if !self.shape_input(after) {
            // A held grip keeps the pointer, even where the island lags
            // behind it.
            let inside = hits(&f, after, self.news_rects(&f).as_ref(), None, sample.cursor)
                || self.drag.get().is_some();
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

    /// While paused, lets the virtual island's window take the pointer where
    /// the pill's Resume is, and nowhere else. The other islands keep what
    /// pausing set.
    fn follow_resume(&self, cursor: Point) {
        let Some(f) = self.island_frames().filter(|f| f.virtual_island) else {
            return;
        };
        let presence = self.island.borrow().presence();
        self.show_rest_round(&f, presence, cursor);
        if !self.shape_input(presence) {
            let inside = hits(&f, presence, None, self.resume_rect(&f), cursor);
            if inside != self.hittest.get() {
                self.set_hittest(inside);
            }
        }
    }

    /// Asks whether the front window has a bar under the virtual island, at
    /// most every [`FRONT_CHECK`]. No front window (or one of vento's own,
    /// where only the focused one is known) means no bar: the pill.
    fn check_front(&self, home: &IslandHome, now: Instant) {
        if self
            .front_checked
            .get()
            .is_some_and(|t| now.saturating_duration_since(t) < FRONT_CHECK)
        {
            return;
        }
        self.front_checked.set(Some(now));
        let mid_x = virtual_mid_x(home, self.spot.get());
        let rest_w = if self.circle_virtual(home) {
            geometry::CIRCLE_D
        } else {
            geometry::PILL_W
        };
        let under = geometry::bar_under(self.platform.front_window_frame(), home, mid_x, rest_w);
        if self.bar_under.replace(under) != under {
            debug!("pocket bar under the island: {under}");
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

    /// Re-reads where the island lives (the virtual island on the screen
    /// in the settings, at its spot there) and moves the window if that
    /// changed. A drag of the grip keeps the screen and the spot it has
    /// taken.
    fn rehome(&self) {
        let homes = self.platform.island_homes();
        let dragging = self.drag.get().is_some();
        let key = if dragging {
            self.home.get().map(|h| screen_key(&h.screen))
        } else {
            self.settings.borrow().island_screen.clone()
        };
        let home = home_for(
            self.mode.get(),
            &homes,
            key.as_deref(),
            self.platform.pocket_home(),
            self.platform.virtual_island_supported(),
        );
        *self.homes.borrow_mut() = homes;
        let Some(home) = home else {
            return;
        };
        let spot_moved = !dragging && {
            let key = screen_key(&home.screen);
            let spot = self.settings.borrow().island_spot(Some(&key));
            self.spot.replace(spot) != spot
        };
        if self.home.get() != Some(home) || spot_moved && self.virtual_spot(&home).is_some() {
            debug!("pocket home is now {home:?}, {:?}", self.spot.get());
            self.home.set(Some(home));
            self.apply_home(&home);
        }
    }

    /// The grip of the open virtual island is pressed: a drag starts from
    /// the cursor's global x (the window moves under the pointer, so not its
    /// own) and the island's middle.
    fn grip_pressed(&self) {
        let Some(home) = self.home.get().filter(|h| self.virtual_spot(h).is_some()) else {
            return;
        };
        *self.homes.borrow_mut() = self.platform.island_homes();
        let spot = self.spot.get();
        self.drag.set(Some(GripDrag {
            press_x: self.cursor_now().x,
            start_mid: virtual_mid_x(&home, spot),
            from: (home, spot),
            moving: false,
        }));
        debug!("pocket grip pressed at {spot:?}");
    }

    /// The held grip moved: the island follows the cursor at the next frame
    /// (once a frame, however many moves come in), where the platform can
    /// read the cursor alone; elsewhere it follows at the samples.
    fn grip_moved(&self) {
        if !self.follows_frames || self.drag.get().is_none() || self.drag_frame.running() {
            return;
        }
        let this = self.this.clone();
        self.drag_frame
            .start(TimerMode::SingleShot, DRAG_FRAME, move || {
                if let Some(c) = this.upgrade()
                    && let Some((x, y)) = c.platform.cursor()
                {
                    c.follow_drag(Point { x, y });
                }
            });
    }

    /// The grip is let go: the island stays where the cursor took it, a
    /// move still waiting for its frame included.
    fn grip_released(&self) {
        if self.drag.get().is_some() {
            self.follow_drag(self.cursor_now());
            self.end_drag();
        }
    }

    /// Where the cursor is now: read alone where the platform can, else
    /// at a sample.
    fn cursor_now(&self) -> Point {
        self.platform.cursor().map_or_else(
            || self.platform.sample_input().cursor,
            |(x, y)| Point { x, y },
        )
    }

    /// The grip is double-clicked: the island goes back to the middle of
    /// its screen, and stays there.
    fn grip_centre(&self) {
        self.drag.set(None);
        self.drag_frame.stop();
        let Some(home) = self.home.get().filter(|h| self.virtual_spot(h).is_some()) else {
            return;
        };
        let spot = IslandSpot::Side(IslandPlace::Centre);
        let before = self.spot.replace(spot);
        if before != spot {
            self.apply_home(&home);
        }
        let key = screen_key(&home.screen);
        if !centre_saved(
            before,
            self.settings.borrow().island_screen.as_deref(),
            &key,
        ) {
            self.save_place(&home, spot);
        }
    }

    /// Moves the dragged island with the cursor at `cursor`, once it has
    /// moved [`DRAG_SLOP`] from the press: along the top edge, snapping to
    /// the left, the centre and the right, and onto the screen without a
    /// notch the cursor comes onto (the middle follows the cursor in global
    /// coordinates, kept within that screen's places).
    fn follow_drag(&self, cursor: Point) {
        let Some(mut drag) = self.drag.get() else {
            return;
        };
        let Some(mut home) = self.home.get().filter(|h| self.virtual_spot(h).is_some()) else {
            // No longer the virtual island: nothing to drag.
            self.drag.set(None);
            return;
        };
        if !drag.moving {
            if !drag_started(drag.press_x, cursor.x) {
                return;
            }
            drag.moving = true;
            self.drag.set(Some(drag));
        }
        let onto = {
            let homes = self.homes.borrow();
            home_at(&homes, cursor.x, cursor.y)
                .map(|i| homes[i])
                .filter(|h| h.screen != home.screen)
        };
        if let Some(other) = onto {
            debug!("pocket dragged onto {:?}", other.screen);
            home = other;
            self.home.set(Some(home));
        }
        let (_, spot) = drag_spot(&home, drag_mid(drag.start_mid, drag.press_x, cursor.x));
        if self.spot.replace(spot) != spot || onto.is_some() {
            self.apply_home(&home);
        }
    }

    /// Ends the drag of the grip, saving the screen and the spot the island
    /// was dropped on when it moved and they changed.
    fn end_drag(&self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.drag_frame.stop();
        if !drag.moving {
            return;
        }
        let Some(home) = self.home.get().filter(|h| self.virtual_spot(h).is_some()) else {
            return;
        };
        let spot = self.spot.get();
        debug!("pocket grip released at {spot:?}");
        if drag.from != (home, spot) {
            self.save_place(&home, spot);
        }
    }

    /// Saves `spot` on `home`'s screen as where the island now lives.
    fn save_place(&self, home: &IslandHome, spot: IslandSpot) {
        self.saving_place.set(true);
        if let Some(f) = self.on_place_dropped.borrow().as_ref() {
            f(Some(screen_key(&home.screen)), spot);
        }
        self.saving_place.set(false);
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
        let rects = self.input_rects(presence);
        if self.input_shape.borrow().as_ref() == Some(&rects) {
            return true;
        }
        let result = match self.window.window().window_handle().window_handle() {
            Ok(handle) => self.platform.set_input_rects(handle.as_raw(), &rects),
            Err(e) => Err(PlatformError::Failed(e.to_string())),
        };
        match result {
            Ok(()) => {
                debug!("pocket input shape {rects:?}");
                *self.input_shape.borrow_mut() = Some(rects);
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

    /// Where the window takes the pointer for `presence`, in physical
    /// pixels from its top-left corner; none when hidden or not placed yet.
    fn input_rects(&self, presence: Presence) -> Vec<Rect> {
        let Some(f) = self.island_frames() else {
            return Vec::new();
        };
        let window = self.window_frame(&f);
        let scale = self.window.window().scale_factor() as f64;
        interactive_rects(
            &f,
            presence,
            self.news_rects(&f).as_ref(),
            self.resume_rect(&f),
        )
        .into_iter()
        .map(|r| Rect {
            x: (r.x - window.x) * scale,
            y: (r.y - window.y) * scale,
            w: r.w * scale,
            h: r.h * scale,
        })
        .collect()
    }

    fn pinned_open(&self, now: Instant) -> bool {
        if self.pin_until.get().is_some_and(|until| now >= until) {
            self.pin_until.set(None);
        }
        self.pinned.get()
            || self.pin_until.get().is_some()
            || self.card.holds_open(now)
            || self.drag.get().is_some()
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
        // Activity closes with the island, like any open view.
        if after != Presence::Expanded {
            self.close_activity();
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
        let spot = self.virtual_spot(home);
        self.keep_slot(spot.is_none());
        let was_virtual = self.laid_out.replace(spot).is_some();
        if was_virtual != spot.is_some() {
            if let Some(f) = self.on_virtual.borrow().as_ref() {
                f(spot.is_some());
            }
            // The first moment comes one period after the island starts
            // resting; an island that stops being virtual plays none.
            self.arm_shimmer();
        }
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
        self.window.set_virtual_style(f.virtual_island);
        // The open virtual island has its grip on top; the orb has none.
        self.window.set_grip(f.virtual_island && !f.orb);
        self.window.set_grip_h(geometry::GRIP_H as f32);
        // The circle, and the half-moon over a bar; never the other islands.
        self.window.set_circle_style(f.circle);
        self.window.set_lip(f.lip);
        // A push under way ends with the virtual island.
        if !f.virtual_island && self.window.get_push_progress() != 0.0 {
            self.window.set_push_progress(0.0);
        }
        self.place_window(&f);
    }

    /// Has the desktop keep room for the island in its top bar (GNOME's)
    /// only while the island is not the virtual one. Asked only when that
    /// changes: the call blocks.
    fn keep_slot(&self, keep: bool) {
        if self.slot_kept.replace(keep) != keep {
            debug!("pocket keeps its top bar slot: {keep}");
            self.platform.reserve_island_slot(keep);
        }
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
            self.toast.notify(
                Notice::error(text).with_icon(ActionId::Pocket),
                self.toast_at(),
            );
        }
    }

    /// The Pocket and Clipboard shortcuts: keeps the island open on view
    /// `v`; switches to it if the island is held open on another view;
    /// lets it close if it already shows `v`.
    pub fn open_view(&self, v: View) {
        self.close_card();
        self.close_activity();
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

    /// Follows the island's mode, screen, place, shape and Glass in the
    /// settings: the island moves, changes form or rests in sight at once
    /// (its own drop of a dragged place leaves it open). The desktop's room
    /// in its top bar is kept only for an island that is not the virtual
    /// one.
    /// An island that is not the virtual one before or after is left as it
    /// is.
    pub fn island_settings_changed(&self) {
        let is_virtual = |c: &Self| c.home.get().is_some_and(|h| c.virtual_spot(&h).is_some());
        let was_virtual = is_virtual(self);
        let glass = {
            let s = self.settings.borrow();
            self.mode.set(s.island_mode);
            self.shape.set(s.island_shape);
            s.island_glass
        };
        self.window.set_glass(glass_alpha(glass));
        self.follow_reduce_setting();
        // The screen and the spot there come with the home.
        self.rehome();
        let now_virtual = is_virtual(self);
        if !was_virtual && !now_virtual {
            return;
        }
        self.keep_slot(!now_virtual);
        // On GNOME the home depends on the slot kept in the top bar.
        self.rehome();
        let Some(home) = self.home.get() else {
            return;
        };
        // The place may have moved under another part of the front window:
        // ask now, so the island doesn't show a stale form for a sample.
        if now_virtual {
            self.front_checked.set(None);
            self.check_front(&home, Instant::now());
        }
        self.apply_home(&home);
        if self.warm.get() && !self.saving_place.get() {
            let after = self.settle_now();
            debug!("pocket island settings changed: {after:?}");
        }
        // A new style or period starts the wait again; Off ends it.
        self.arm_shimmer();
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
        let count = i32::try_from(self.item_count()).unwrap_or(i32::MAX);
        self.window.set_count(count);
        self.window.set_rest_count(count);
        self.window
            .set_rest_badge(badge_text(self.item_count()).into());
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
            self.toast.notify(
                Notice::error(text).with_icon(ActionId::Pocket),
                self.toast_at(),
            );
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

    /// Where the virtual island sits at `home`, or `None` when the island
    /// is not the virtual one there.
    fn virtual_spot(&self, home: &IslandHome) -> Option<VirtualSpot> {
        virtual_spot_of(
            self.mode.get(),
            home,
            self.platform.virtual_island_supported(),
            self.spot.get(),
            self.bar_under.get(),
            self.shape.get(),
        )
    }

    /// Whether the island at `home` is the virtual one in its circle shape.
    fn circle_virtual(&self, home: &IslandHome) -> bool {
        self.virtual_spot(home)
            .is_some_and(|s| s.shape == IslandShape::Circle)
    }

    /// The island's frames for `home`, sized for the view it shows now.
    fn frames_of(&self, home: &IslandHome) -> Frames {
        match self.virtual_spot(home) {
            Some(spot) => virtual_frames(home, spot, self.panel()),
            None => frames(home, self.panel()),
        }
    }

    fn island_frames(&self) -> Option<Frames> {
        self.home.get().map(|home| self.frames_of(&home))
    }

    fn hide_window(&self) {
        if let Err(e) = self.window.hide() {
            warn!("cannot hide the pocket: {e}");
        }
    }

    /// Notes that someone is at the computer when the cursor or the left
    /// button differs from the last sample. Back after a long time away, the
    /// next shimmer moment waits a whole period from now.
    fn note_input(&self, sample: &InputSample) {
        let seen = Some((sample.cursor, sample.left_down));
        if self.last_seen.replace(seen) != seen {
            let now = Instant::now();
            let idle_before = now.saturating_duration_since(self.last_input.replace(now));
            if rearm_on_return(idle_before) {
                self.arm_shimmer();
            }
        }
    }

    /// Reads the system's Reduce motion and tells the window whether motion
    /// is reduced, only when that changes.
    fn refresh_reduce_motion(&self, now: Instant) {
        let system = self.platform.reduce_motion();
        self.reduce_cache.set((Some(now), system));
        self.show_reduce_motion(system);
    }

    /// Follows the Follow Reduce motion setting with the system's Reduce
    /// motion as last read, without asking the system again (it is asked
    /// every [`REDUCE_CHECK`]); read now if it never was.
    fn follow_reduce_setting(&self) {
        match self.reduce_cache.get() {
            (Some(_), system) => self.show_reduce_motion(system),
            (None, _) => self.refresh_reduce_motion(Instant::now()),
        }
    }

    /// Tells the window whether motion is reduced with the system's Reduce
    /// motion `system`, only when that changes.
    fn show_reduce_motion(&self, system: bool) {
        let follow = self.settings.borrow().island_reduce_motion;
        let reduced = motion_reduced(follow, system);
        if self.window.get_reduce_motion() != reduced {
            self.window.set_reduce_motion(reduced);
        }
    }

    /// Waits one period for the next shimmer moment, or stops waiting: the
    /// island is not the virtual one, or the style is Off. One one-shot
    /// timer, never a repeating one.
    fn arm_shimmer(&self) {
        self.shimmer_timer.stop();
        if self.laid_out.get().is_none() {
            return;
        }
        let (style, every) = {
            let s = self.settings.borrow();
            (s.island_shimmer, s.island_shimmer_every)
        };
        let Some(wait) = next_arm(style, every) else {
            return;
        };
        let this = self.this.clone();
        self.shimmer_timer
            .start(TimerMode::SingleShot, wait, move || {
                if let Some(c) = this.upgrade() {
                    c.shimmer_fired();
                }
            });
    }

    /// A shimmer moment is due: plays it if the rules allow it now, by
    /// bumping the window's `shimmer-id`. Either way the next one waits a
    /// whole period.
    fn shimmer_fired(&self) {
        let c = self.moment_conditions(Instant::now());
        if moment_allowed(&c) {
            debug!("pocket shimmer {:?}", c.style);
            let w = &self.window;
            w.set_shimmer_style(shimmer_index(c.style));
            w.set_shimmer_id(w.get_shimmer_id().wrapping_add(1).max(1));
        }
        self.arm_shimmer();
    }

    /// What the shimmer rules look at, read at `now`. The system is asked
    /// here, when a moment is due, not at every sample.
    fn moment_conditions(&self, now: Instant) -> MomentConditions {
        let (style, needs_items, follow_reduce) = {
            let s = self.settings.borrow();
            (
                s.island_shimmer,
                s.island_shimmer_needs_items,
                s.island_reduce_motion,
            )
        };
        MomentConditions {
            style,
            virtual_resting: virtual_resting(self.warm.get(), self.laid_out.get(), self.expanded()),
            news_or_job: self.news_view.borrow().is_some(),
            paused: self.paused.get(),
            pocket_items: self.item_count(),
            needs_items,
            follow_reduce,
            system_reduce: self.platform.reduce_motion(),
            quiet_time: self.platform.quiet_time(),
            power_saving: self.platform.power_saving(),
            idle_for: now.saturating_duration_since(self.last_input.get()),
        }
    }

    /// Whether the island takes notices and jobs now: it is the virtual one,
    /// ready and not paused (each sample shows the news then).
    fn takes_news(&self) -> bool {
        self.warm.get()
            && !self.paused.get()
            && self
                .home
                .get()
                .is_some_and(|home| self.virtual_spot(&home).is_some())
    }

    /// Tells the window whether the circle's news pill rests as the round
    /// disc, only when that changes. Only the circle stretches; `hovered` is
    /// the pointer on the news pill.
    fn show_news_round(&self, home: &IslandHome, hovered: bool) {
        let round = news_round_next(
            self.news_round.get(),
            self.circle_virtual(home),
            self.news_view.borrow().as_ref(),
            hovered,
        );
        if self.news_round.replace(round) != round {
            self.window.set_news_round(round);
        }
    }

    /// Tells the window whether the circle's resting pill rests round: it
    /// stretches to its text (paused with Resume, Keep Awake's time) while
    /// the pointer is on it, as drawn.
    fn show_rest_round(&self, f: &Frames, presence: Presence, cursor: Point) {
        let hovered = geometry::rest_hovered(
            f,
            presence,
            f64::from(self.window.get_rest_pill_w()),
            cursor,
        );
        let round =
            f.circle && rest_round(self.paused.get(), self.window.get_rest_awake(), hovered);
        if self.rest_round.replace(round) != round {
            self.window.set_rest_round(round);
        }
    }

    /// Gives `view` to the window, only when it differs from the last.
    fn show_news(&self, view: Option<NewsView>) {
        if *self.news_view.borrow() == view {
            return;
        }
        let w = &self.window;
        match &view {
            Some(v) => {
                w.set_news_stage(stage_index(v.stage));
                w.set_news_tone(tone_index(v.tone));
                w.set_news_icon(self.action_icon(v.icon));
                // Only to tell a new error from the same one shown again.
                w.set_news_id((v.id % i32::MAX as u64) as i32);
                w.set_news_text(v.text.as_str().into());
                w.set_news_sub(v.sub.as_str().into());
                w.set_news_button(v.button_text.as_deref().unwrap_or_default().into());
                w.set_news_quiet(v.quiet_button);
                w.set_news_extra(i32::try_from(v.extra).unwrap_or(i32::MAX));
                w.set_news_fraction(v.fraction.unwrap_or(-1.0));
                w.set_news_count(v.count_text.as_str().into());
                w.set_news_short(v.short_count.as_str().into());
            }
            // The rest stays, for the pill to fade out with.
            None => w.set_news_stage(0),
        }
        let now_shown = view.is_some();
        let was_shown = self.news_view.replace(view).is_some();
        // News over, the next shimmer moment waits a whole period.
        if rearm_on_news_end(was_shown, now_shown) {
            self.arm_shimmer();
        }
    }

    /// Where the news the window shows lies with `f`, as the window lays it
    /// out; `None` while it shows none.
    fn news_rects(&self, f: &Frames) -> Option<NewsRects> {
        if !f.virtual_island || self.news_view.borrow().is_none() {
            return None;
        }
        let w = &self.window;
        let width = |l: f32| (l > 0.0).then_some(f64::from(l));
        let button_w = width(w.get_news_button_w());
        let chip_w = width(w.get_news_chip_w());
        Some(geometry::news_rects(
            f,
            f64::from(w.get_news_pill_w()),
            button_w,
            button_w.map(|_| f64::from(w.get_news_button_x())),
            chip_w,
            chip_w.map(|_| f64::from(w.get_news_chip_x())),
            geometry::WING_W,
        ))
    }

    /// Where the paused pill's Resume lies with `f`, as the window lays it
    /// out; `None` while not paused, and where no pill rests (the lip).
    fn resume_rect(&self, f: &Frames) -> Option<Rect> {
        if !self.paused.get() {
            return None;
        }
        let w = &self.window;
        let button_w = f64::from(w.get_rest_button_w());
        if button_w <= 0.0 {
            return None;
        }
        geometry::resume_rect(
            f,
            f64::from(w.get_rest_pill_w()),
            button_w,
            f64::from(w.get_rest_button_x()),
        )
    }

    /// Resume on the paused pill: vento resumes.
    fn resume_clicked(&self) {
        if let Some(on) = self.on_resume.borrow().as_ref() {
            on();
        }
    }

    /// The news pill's button: runs what it does once, outside the borrow
    /// (an Undo posts news of its own).
    fn news_button_clicked(&self) {
        let on = self.news.borrow_mut().press_button();
        if let Some(on) = on {
            on();
        }
    }

    /// Opens the virtual island on Activity: today's jobs and results, over
    /// the view. The "+N" chip, an error's Details and a push while a result
    /// shows open it, never hover; with nothing to list it stays shut. It
    /// closes with the island.
    pub fn open_activity(&self) {
        if !self.takes_news() {
            return;
        }
        let rows = self.activity_now();
        if rows.is_empty() {
            return;
        }
        self.close_card();
        self.activity.set(true);
        self.window.set_activity(true);
        self.show_activity(rows);
        // Clicked on the resting pill: the island opens at the next sample.
        self.hold_open_briefly();
    }

    /// Takes Activity down, back to the view.
    fn close_activity(&self) {
        if !self.activity.replace(false) {
            return;
        }
        self.window.set_activity(false);
        self.show_activity(Vec::new());
    }

    /// Activity's rows now: what the island took today (since local
    /// midnight, by the wall clock), and the executor's undo serial for its
    /// Undo.
    fn activity_now(&self) -> Vec<ActivityRowData> {
        let now = jiff::Timestamp::now();
        let today = today_start(now, &jiff::tz::TimeZone::system());
        let last_serial = self.last_serial.borrow().as_ref().and_then(|f| f());
        self.news.borrow().activity(now, today, last_serial)
    }

    /// Follows what changed in Activity while it is open: the ages, jobs
    /// ending, new posts and buttons pressed.
    fn refresh_activity(&self) {
        let rows = self.activity_now();
        if *self.activity_rows.borrow() != rows {
            self.show_activity(rows);
        }
    }

    /// Gives Activity's `rows` to the window, and fits the island to them.
    fn show_activity(&self, rows: Vec<ActivityRowData>) {
        let shown = rows
            .iter()
            .map(|r| ActivityRow {
                stage: stage_index(r.stage),
                tone: tone_index(r.tone),
                icon: self.action_icon(r.icon),
                text: r.text.as_str().into(),
                age: r.age.as_str().into(),
                button: r.button.map(ButtonLabel::text).unwrap_or_default().into(),
            })
            .collect();
        set_rows_in_place(&self.activity_model, shown);
        *self.activity_rows.borrow_mut() = rows;
        self.fit_panel();
    }

    /// A button of Activity's row `index`: runs what it does once, outside
    /// the borrow (an Undo posts news of its own); the button goes at once.
    fn activity_button_clicked(&self, index: usize) {
        let Some(id) = self.activity_rows.borrow().get(index).map(|r| r.id) else {
            return;
        };
        let last_serial = self.last_serial.borrow().as_ref().and_then(|f| f());
        let on = self.news.borrow_mut().press_row(id, last_serial);
        if let Some(on) = on {
            on();
        }
        if self.activity.get() {
            self.refresh_activity();
        }
    }

    /// The icon of `action` in the window, or none.
    fn action_icon(&self, action: Option<ActionId>) -> Image {
        let icons = self.window.global::<Icons>();
        action
            .map(|a| crate::wheel::action_icon(&icons, a))
            .unwrap_or_default()
    }
}

/// How the window numbers a news stage: 1 at work, 2 a result.
fn stage_index(stage: Stage) -> i32 {
    match stage {
        Stage::Work => 1,
        Stage::Result => 2,
    }
}

/// How the window numbers a tone: 0 done, 1 info, 2 error, 3 busy.
fn tone_index(tone: Tone) -> i32 {
    match tone {
        Tone::Done => 0,
        Tone::Info => 1,
        Tone::Error => 2,
        Tone::Busy => 3,
    }
}

/// The virtual island takes every notice and job; the other modes, and the
/// island before it is ready or while paused, leave them toasts and progress
/// windows.
impl NoticeSink for PocketController {
    fn notice(&self, n: Notice) -> Result<(), Notice> {
        if !self.takes_news() {
            return Err(n);
        }
        let this = self.this.clone();
        let n = with_details(n, move || {
            if let Some(c) = this.upgrade() {
                c.open_activity();
            }
        });
        self.news
            .borrow_mut()
            .post(n, Instant::now(), jiff::Timestamp::now());
        Ok(())
    }

    fn job_started(&self, job: JobNotice) -> bool {
        if !self.takes_news() {
            return false;
        }
        self.news
            .borrow_mut()
            .start(job, Instant::now(), jiff::Timestamp::now());
        true
    }

    fn job_ended(&self, id: u64) {
        self.news.borrow_mut().end(id);
    }
}

#[cfg(test)]
mod tests {
    use super::geometry::Panel;
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
    fn wings_show_what_hover_opens() {
        use View::{Clipboard, Pocket, Screenshots};
        // Items in the Pocket: its icon and count, whatever else there is.
        for last in [Pocket, Clipboard, Screenshots] {
            assert_eq!(wing_view(false, true, true, last), Pocket);
        }
        // An empty Pocket, some history and some screenshots: the view
        // hover opens.
        for last in [Pocket, Clipboard, Screenshots] {
            let opens = hover_view(true, true, true, None, last);
            assert_eq!(wing_view(true, true, true, last), opens, "{last:?}");
        }
        assert_eq!(wing_view(true, true, true, Pocket), Screenshots);
        assert_eq!(wing_view(true, true, true, Clipboard), Clipboard);
        assert_eq!(wing_view(true, true, true, Screenshots), Screenshots);
        // Only screenshots, or only history.
        assert_eq!(wing_view(true, false, true, Clipboard), Screenshots);
        assert_eq!(wing_view(true, true, false, Clipboard), Clipboard);
        assert_eq!(wing_view(true, true, false, Screenshots), Clipboard);
        // Hover opens a view with nothing (the empty Pocket, or Screenshots
        // with none): the wings show what has something, as before.
        assert_eq!(wing_view(true, true, false, Pocket), Clipboard);
        // Nothing anywhere.
        assert_eq!(wing_view(true, false, false, Clipboard), Pocket);
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

    #[test]
    fn virtual_mode_needs_no_notch_and_support() {
        let plain = IslandHome {
            screen: Rect {
                x: 0.0,
                y: 0.0,
                w: 1440.0,
                h: 900.0,
            },
            notch: None,
            menu_bar_h: 24.0,
            slot: None,
            tray_corner: None,
        };
        let notched = IslandHome {
            notch: Some(Rect {
                x: 660.0,
                y: 0.0,
                w: 180.0,
                h: 32.0,
            }),
            ..plain
        };
        assert!(virtual_active(IslandMode::Virtual, &plain, true));
        assert!(!virtual_active(IslandMode::Classic, &plain, true));
        assert!(!virtual_active(IslandMode::Virtual, &notched, true));
        assert!(!virtual_active(IslandMode::Virtual, &plain, false));
    }

    #[test]
    fn badge_text_caps_at_99() {
        assert_eq!(badge_text(0), "");
        assert_eq!(badge_text(7), "7");
        assert_eq!(badge_text(99), "99");
        assert_eq!(badge_text(100), "99+");
        assert_eq!(badge_text(5000), "99+");
    }

    #[test]
    fn rest_round_stretches_only_with_something_to_say() {
        // Nothing to say: hover changes nothing.
        assert!(rest_round(false, false, false));
        assert!(rest_round(false, false, true));
        // Paused or awake: round until hovered, then the pill with its text.
        assert!(rest_round(true, false, false));
        assert!(!rest_round(true, false, true));
        assert!(rest_round(false, true, false));
        assert!(!rest_round(false, true, true));
        assert!(!rest_round(true, true, true));
    }

    /// A plain screen, for the circle's layout tests.
    fn plain_home() -> IslandHome {
        IslandHome {
            screen: Rect {
                x: 0.0,
                y: 0.0,
                w: 1440.0,
                h: 900.0,
            },
            notch: None,
            menu_bar_h: 24.0,
            slot: None,
            tray_corner: None,
        }
    }

    #[test]
    fn drag_mid_follows_cursor() {
        assert_eq!(drag_mid(720.0, 700.0, 760.0), 780.0);
        assert_eq!(drag_mid(720.0, 700.0, 640.0), 660.0);
        assert_eq!(drag_mid(720.0, 700.0, 700.0), 720.0);
    }

    #[test]
    fn drag_ends_when_button_up() {
        assert!(drag_ended(true, false));
        assert!(!drag_ended(true, true));
        assert!(!drag_ended(false, false));
        assert!(!drag_ended(false, true));
    }

    #[test]
    fn drag_starts_after_3px() {
        assert!(!drag_started(700.0, 700.0));
        assert!(!drag_started(700.0, 702.9));
        assert!(!drag_started(700.0, 697.5));
        assert!(drag_started(700.0, 703.0));
        assert!(drag_started(700.0, 697.0));
        assert!(drag_started(700.0, 900.0));
    }

    /// The primary screen and a second one on its right, for the screen key
    /// tests.
    fn two_homes() -> [IslandHome; 2] {
        let primary = plain_home();
        let second = IslandHome {
            screen: Rect {
                x: 1440.0,
                y: 0.0,
                w: 1920.0,
                h: 1080.0,
            },
            ..primary
        };
        [primary, second]
    }

    #[test]
    fn classic_ignores_screen_key() {
        let homes = two_homes();
        assert_eq!(
            home_for(
                IslandMode::Classic,
                &homes,
                Some("1920x1080+1440+0"),
                Some(homes[0]),
                true
            ),
            Some(homes[0])
        );
    }

    #[test]
    fn virtual_uses_screen_key() {
        let homes = two_homes();
        assert_eq!(
            home_for(
                IslandMode::Virtual,
                &homes,
                Some("1920x1080+1440+0"),
                Some(homes[0]),
                true
            ),
            Some(homes[1])
        );
        // No key: the primary one.
        assert_eq!(
            home_for(IslandMode::Virtual, &homes, None, Some(homes[0]), true),
            Some(homes[0])
        );
    }

    #[test]
    fn virtual_unknown_key_uses_fallback() {
        let homes = two_homes();
        assert_eq!(
            home_for(
                IslandMode::Virtual,
                &homes,
                Some("800x600+0+0"),
                Some(homes[0]),
                true
            ),
            Some(homes[0])
        );
        // A screen with a notch keeps the notch island: never taken by key.
        let notched = IslandHome {
            notch: Some(Rect {
                x: 2300.0,
                y: 0.0,
                w: 180.0,
                h: 32.0,
            }),
            ..homes[1]
        };
        assert_eq!(
            home_for(
                IslandMode::Virtual,
                &[homes[0], notched],
                Some("1920x1080+1440+0"),
                Some(homes[0]),
                true
            ),
            Some(homes[0])
        );
        assert_eq!(
            home_for(IslandMode::Virtual, &[], Some("x"), None, true),
            None
        );
    }

    #[test]
    fn notched_primary_ignores_screen_key() {
        // A Mac with a notch keeps its notch island, external screens
        // included: the key of a notch-free screen is not taken.
        let [primary, second] = two_homes();
        let notched = IslandHome {
            notch: Some(Rect {
                x: 630.0,
                y: 0.0,
                w: 180.0,
                h: 32.0,
            }),
            ..primary
        };
        assert_eq!(
            home_for(
                IslandMode::Virtual,
                &[notched, second],
                Some("1920x1080+1440+0"),
                Some(notched),
                true
            ),
            Some(notched)
        );
    }

    #[test]
    fn unsupported_ignores_screen_key() {
        // Where the platform can't place the virtual island (Wayland), the
        // island stays in the primary's slot.
        let homes = two_homes();
        assert_eq!(
            home_for(
                IslandMode::Virtual,
                &homes,
                Some("1920x1080+1440+0"),
                Some(homes[0]),
                false
            ),
            Some(homes[0])
        );
    }

    #[test]
    fn island_screen_key_is_the_screen_it_is_on() {
        let homes = two_homes();
        let key = |mode, stored| island_screen_key(mode, &homes, stored, Some(homes[0]), true);
        assert_eq!(
            key(IslandMode::Virtual, Some("1920x1080+1440+0")).as_deref(),
            Some("1920x1080+1440+0")
        );
        // A screen that is gone: the primary's, where the island now is.
        assert_eq!(
            key(IslandMode::Virtual, Some("800x600+0+0")).as_deref(),
            Some("1440x900+0+0")
        );
        assert_eq!(
            key(IslandMode::Virtual, None).as_deref(),
            Some("1440x900+0+0")
        );
        assert_eq!(
            key(IslandMode::Classic, Some("1920x1080+1440+0")).as_deref(),
            Some("1440x900+0+0")
        );
        assert_eq!(
            island_screen_key(IslandMode::Virtual, &[], None, None, true),
            None
        );
    }

    #[test]
    fn centre_saved_already_skips_the_save() {
        let centre = IslandSpot::Side(IslandPlace::Centre);
        let key = "1440x900+0+0";
        assert!(centre_saved(centre, Some(key), key));
        // Somewhere else, or saved for another screen (or none): saved.
        assert!(!centre_saved(
            IslandSpot::Side(IslandPlace::Left),
            Some(key),
            key
        ));
        assert!(!centre_saved(IslandSpot::Free(300), Some(key), key));
        assert!(!centre_saved(centre, Some("1920x1080+1440+0"), key));
        assert!(!centre_saved(centre, None, key));
    }

    #[test]
    fn shape_change_relays_out() {
        let home = plain_home();
        let spot = |shape| {
            virtual_spot_of(
                IslandMode::Virtual,
                &home,
                true,
                IslandSpot::Side(IslandPlace::Centre),
                false,
                shape,
            )
        };
        let pill = spot(IslandShape::Island).expect("virtual");
        let circle = spot(IslandShape::Circle).expect("virtual");
        // The shape is part of where the island is laid out, so a change of
        // it is laid out again at the next sample.
        assert_ne!(pill, circle);
        assert!(!virtual_frames(&home, pill, Panel::Kinds { groups: 2 }).circle);
        assert!(virtual_frames(&home, circle, Panel::Kinds { groups: 2 }).circle);
        // Classic never takes the shape.
        let classic = virtual_spot_of(
            IslandMode::Classic,
            &home,
            true,
            IslandSpot::Side(IslandPlace::Centre),
            false,
            IslandShape::Circle,
        );
        assert_eq!(classic, None);
    }

    #[test]
    fn drag_on_the_circle_shows_the_orb_and_ends_it() {
        let home = plain_home();
        let circle = VirtualSpot {
            place: IslandSpot::Side(IslandPlace::Centre),
            bar_under: false,
            shape: IslandShape::Circle,
        };
        // A drag in the zone on the virtual circle brings the drop box out;
        // one far from it does not. The corner's rule is unchanged: any
        // drag on its screen, near or not.
        assert!(drop_box_wanted(false, true, true, false, true, false));
        assert!(!drop_box_wanted(false, true, false, false, true, false));
        assert!(drop_box_wanted(true, false, false, false, true, false));
        assert!(drop_box_wanted(true, false, true, false, true, false));
        // The island already open on a view takes the drag as it is; the
        // orb already up (the drop box on, so not open on a view) stays up
        // while the drag is on it.
        assert!(!drop_box_wanted(false, true, true, true, true, false));
        assert!(drop_box_wanted(false, true, true, false, true, false));
        // The corner's drop box comes out over its open island too.
        assert!(drop_box_wanted(true, false, false, true, true, false));
        let panel = |on| {
            if on {
                Panel::Drop
            } else {
                Panel::Kinds { groups: 1 }
            }
        };
        assert!(virtual_frames(&home, circle, panel(true)).orb);
        // A sample with no drag ends it, and the drag out's own latch or
        // the pill never raises it.
        assert!(!drop_box_wanted(false, true, true, false, false, false));
        assert!(!drop_box_wanted(false, true, true, false, true, true));
        assert!(!drop_box_wanted(false, false, true, false, true, false));
        assert!(!virtual_frames(&home, circle, panel(false)).orb);
        // The zone holds the whole orb, so a drag on the orb stays near
        // and keeps it up.
        let orb = virtual_frames(&home, circle, panel(true));
        let e = orb.expanded;
        for (x, y) in [
            (e.x, e.y),
            (e.x + e.w, e.y),
            (e.x, e.y + e.h),
            (e.x + e.w, e.y + e.h),
        ] {
            assert!(geometry::contains(&orb.zone, Point { x, y }));
        }
    }

    #[test]
    fn circle_style_follows_the_layout() {
        let home = plain_home();
        let panel = Panel::Kinds { groups: 1 };
        let spot = |shape, bar_under| VirtualSpot {
            place: IslandSpot::Side(IslandPlace::Centre),
            bar_under,
            shape,
        };
        // The circle and the half-moon over a bar are circle-styled; the
        // pill and the other islands are not.
        assert!(virtual_frames(&home, spot(IslandShape::Circle, false), panel).circle);
        assert!(virtual_frames(&home, spot(IslandShape::Circle, true), panel).circle);
        assert!(!virtual_frames(&home, spot(IslandShape::Island, false), panel).circle);
        assert!(!frames(&home, panel).circle);
    }

    #[test]
    fn news_round_holds_through_the_fade() {
        let view = |tone, button: Option<&str>| NewsView {
            stage: Stage::Result,
            tone,
            icon: None,
            text: String::new(),
            sub: String::new(),
            id: 1,
            button_text: button.map(Into::into),
            quiet_button: false,
            extra: 0,
            fraction: None,
            count_text: String::new(),
            short_count: String::new(),
        };
        let stretched = view(Tone::Done, Some("Undo"));
        let error = view(Tone::Error, Some("Details"));
        // Shown, it follows the rule.
        assert!(!news_round_next(true, true, Some(&stretched), false));
        assert!(news_round_next(false, true, Some(&error), false));
        assert!(!news_round_next(true, true, Some(&error), true));
        // Gone, it keeps the form it had while it fades.
        assert!(!news_round_next(false, true, None, false));
        assert!(news_round_next(true, true, None, false));
        // Off the circle it is never round.
        assert!(!news_round_next(true, false, None, false));
        assert!(!news_round_next(true, false, Some(&error), false));
    }

    #[test]
    fn shimmer_index_matches_all_order() {
        for (i, s) in ShimmerStyle::ALL.into_iter().enumerate() {
            assert_eq!(shimmer_index(s), i as i32, "{s:?}");
        }
        assert_eq!(shimmer_index(ShimmerStyle::Off), 0);
        assert_eq!(shimmer_index(ShimmerStyle::Liquid), 5);
    }

    #[test]
    fn reduce_due_every_30s() {
        let now = Instant::now();
        assert!(reduce_due(None, now));
        assert!(!reduce_due(Some(now), now));
        assert!(!reduce_due(Some(now - Duration::from_secs(29)), now));
        assert!(reduce_due(Some(now - Duration::from_secs(30)), now));
        assert!(reduce_due(Some(now - Duration::from_secs(300)), now));
    }

    #[test]
    fn shimmer_only_on_the_resting_pill_or_circle() {
        let spot = |shape, bar_under| {
            Some(VirtualSpot {
                place: IslandSpot::Side(IslandPlace::Centre),
                bar_under,
                shape,
            })
        };
        let pill = spot(IslandShape::Island, false);
        let circle = spot(IslandShape::Circle, false);
        let lip = spot(IslandShape::Island, true);
        let half_moon = spot(IslandShape::Circle, true);
        assert!(virtual_resting(true, pill, false));
        assert!(virtual_resting(true, circle, false));
        assert!(!virtual_resting(true, lip, false));
        assert!(!virtual_resting(true, half_moon, false));
        // Open, not ready, or not the virtual island.
        assert!(!virtual_resting(true, pill, true));
        assert!(!virtual_resting(true, circle, true));
        assert!(!virtual_resting(false, pill, false));
        assert!(!virtual_resting(true, None, false));
    }

    #[test]
    fn coming_back_from_away_waits_a_period() {
        assert!(!rearm_on_return(Duration::ZERO));
        assert!(!rearm_on_return(Duration::from_secs(299)));
        assert!(rearm_on_return(Duration::from_secs(300)));
        assert!(rearm_on_return(Duration::from_secs(3600)));
    }

    #[test]
    fn news_ending_waits_a_period() {
        assert!(rearm_on_news_end(true, false));
        assert!(!rearm_on_news_end(true, true));
        assert!(!rearm_on_news_end(false, true));
        assert!(!rearm_on_news_end(false, false));
    }

    #[test]
    fn settings_change_rearms_or_stops() {
        for every in ShimmerEvery::ALL {
            assert_eq!(next_arm(ShimmerStyle::Off, every), None);
            for style in ShimmerStyle::ALL.into_iter().skip(1) {
                assert_eq!(next_arm(style, every), Some(every.period()));
            }
        }
    }
}
