//! The island's Screenshots view: the recent screenshots by day as the
//! window draws them, their thumbnails (made on a worker thread), the day
//! chips and the peek; and what a tile, its buttons and the peek do.
//!
//! A tile's Trash hides the screenshot at once and offers the island's Undo;
//! the file goes to the Trash only once Undo is no longer offered (there is
//! no putting a file back from the Trash), or when vento quits.
//!
//! The card: when a screenshot is saved, the island drops down with its
//! picture and actions for a few seconds ([`ShotCard`]). Its Trash works as
//! a tile's, with the Undo in a toast since the card closes at once.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use log::{debug, info, warn};
use slint::{ComponentHandle, Image, ModelRc, Timer, VecModel};
use vento_core::clip::{ClipRep, ClipSnapshot, TYPE_FILE_URL, TYPE_PNG, path_to_file_url};
use vento_core::destinations::display_path;
use vento_core::i18n::{fill, tr, trc, with_language};
use vento_core::info::human_size;
use vento_core::screen_text::assemble;
use vento_core::screenshots::{
    Day, Shot, ShotFilter, counts, day_chip, day_heading, day_of, grouped, time_label,
};
use vento_platform::{Dropped, PlatformError, RgbaImage};

use crate::screen_text::ReadResult;
use crate::ui::{PocketIsland, ShotDay, ShotTile};

#[cfg(target_os = "linux")]
use crate::drag;

use super::clip_view::{no_picture, set_rows_in_place, tabs};
use super::out::{Press, drop_point, placeholder_icon, preview_png};
use super::shots::Access;
use super::{
    ICON_PX, MAIN_POCKET, PocketController, UNDO_WINDOW, Undo, View, display_name, to_image,
};
use vento_core::pocket_zone::Presence;

/// A tile's thumbnail: its shorter side in pixels (the tile is about
/// 165 × 104 points), and how much longer the other side may be.
const THUMB_PX: u32 = 208;
const THUMB_RATIO: u32 = 4;
/// Width in points of the picture a screenshot is dragged out with.
const DRAG_PT: u32 = 120;
/// The card's picture: its shorter side in pixels (the picture is
/// 212 × 133 points, drawn at twice that on most screens).
const CARD_PX: u32 = 280;
/// The peek's picture: its shorter side in pixels (the picture is about
/// 526 × 266 points, drawn at twice that on most screens).
const PEEK_PX: u32 = 600;

/// The day chips in the order the window shows them.
const FILTERS: [ShotFilter; 4] = [
    ShotFilter::All,
    ShotFilter::Day(Day::Today),
    ShotFilter::Day(Day::Yesterday),
    ShotFilter::Day(Day::Earlier),
];

/// The filter of the window's chip `i`.
fn filter_at(i: i32) -> Option<ShotFilter> {
    usize::try_from(i)
        .ok()
        .and_then(|i| FILTERS.get(i))
        .copied()
}

/// The window's chip of `filter`.
fn filter_index(filter: ShotFilter) -> usize {
    FILTERS.iter().position(|&f| f == filter).unwrap_or(0)
}

/// What Copy puts on the clipboard for the screenshot at `path`, as one
/// item: the picture's PNG bytes (`png`, for a PNG file) and the file
/// itself, so it pastes as a picture into a chat and as a file into a
/// folder; for other formats, the file alone.
pub(super) fn shot_snapshot(path: &Path, png: Option<Vec<u8>>) -> ClipSnapshot {
    let file = ClipRep {
        ty: TYPE_FILE_URL.to_owned(),
        data: path_to_file_url(path).into_bytes(),
    };
    let item = match png {
        Some(data) => vec![
            ClipRep {
                ty: TYPE_PNG.to_owned(),
                data,
            },
            file,
        ],
        None => vec![file],
    };
    ClipSnapshot { items: vec![item] }
}

/// Whether `path` names a PNG file.
fn is_png(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

/// The place in `shots` of the screenshot whose peek is open (`peek`), or
/// `None` when no peek is open or its file is no longer listed.
pub(super) fn peek_place(shots: &[Shot], peek: Option<&Path>) -> Option<usize> {
    let peek = peek?;
    shots.iter().position(|s| s.path == peek)
}

/// The window's number for a peek at `place`: -1 for none.
pub(super) fn peek_index(place: Option<usize>) -> i32 {
    place.map_or(-1, |i| i32::try_from(i).unwrap_or(-1))
}

/// What the view shows for `access` with `count` screenshots listed: 0 the
/// grid, 1 the line for an empty folder, 2 the line asking to see the
/// folder again after an update. While a listing is under way, the grid
/// (empty until it answers).
pub(super) fn shots_state(access: Access, count: usize) -> i32 {
    match access {
        Access::WaitForOpen => 2,
        Access::Granted if count == 0 => 1,
        _ => 0,
    }
}

/// The peek's title for the shot saved at `modified_unix`: "Today 14:02",
/// or "Mon 17:20" for an earlier day (whose time label names the day).
fn peek_title(modified_unix: u64, now_unix: u64, tz: &TimeZone) -> String {
    let time = time_label(modified_unix, now_unix, tz);
    match day_of(modified_unix, now_unix, tz) {
        Day::Earlier => time,
        day => fill(
            &trc("shot", "{day} {time}"),
            &[("day", &day_chip(day)), ("time", &time)],
        ),
    }
}

/// What the peek shows of the screenshot at `path` saved at `mtime`: its
/// large picture once made for that file and time (`made`); until then the
/// tile's thumbnail when one was made for that time (`thumb`, with the time
/// it was made for), or nothing. The flag says whether the large picture is
/// still to be made. The picture's box keeps its size either way.
fn peek_shown(
    made: Option<&(PathBuf, u64, Image)>,
    thumb: Option<(u64, &Image)>,
    path: &Path,
    mtime: u64,
) -> (Image, bool) {
    if let Some((p, m, image)) = made
        && p == path
        && *m == mtime
    {
        return (image.clone(), false);
    }
    let stand_in = thumb
        .filter(|(made_for, _)| *made_for == mtime)
        .map_or_else(no_picture, |(_, image)| image.clone());
    (stand_in, true)
}

/// The line beside the peek's title: `width × height · size`.
fn peek_meta(shot: &Shot) -> String {
    let size = human_size(shot.len);
    match shot.dims {
        Some((w, h)) => format!("{w} × {h} · {size}"),
        None => size,
    }
}

/// The line under the card's title: `width × height · size · folder`.
fn card_meta(shot: &Shot, folder: &str) -> String {
    format!("{} · {folder}", peek_meta(shot))
}

/// What a tile marks for the thumbnail made of its file: `made` is whether
/// a picture came out and whether the file was gone then (`None` while not
/// made yet). Returns `(missing, unreadable)`: the alert only for a file
/// that is gone, a plain file icon for one whose picture can't be made
/// (such as a PDF screenshot).
fn tile_marks(made: Option<(bool, bool)>) -> (bool, bool) {
    match made {
        Some((false, gone)) => (gone, !gone),
        _ => (false, false),
    }
}

/// When the card closes by itself: `secs` after it was shown (`shown_at`),
/// or after the last moment the pointer was over it (`hovered_at`).
pub(super) fn card_deadline(hovered_at: Option<Instant>, shown_at: Instant, secs: u32) -> Instant {
    hovered_at.map_or(shown_at, |h| h.max(shown_at)) + Duration::from_secs(u64::from(secs))
}

/// What decides whether a new screenshot shows the card.
#[derive(Clone, Copy, Debug)]
pub(super) struct CardGate {
    /// The screenshots are on.
    pub(super) feature: bool,
    /// The card is on in the settings.
    pub(super) card: bool,
    /// The folder could be read.
    pub(super) granted: bool,
    /// The island is open on a view (the card itself does not count).
    pub(super) expanded: bool,
    pub(super) paused: bool,
    /// A drag of any kind is under way.
    pub(super) dragging: bool,
    /// The island's own drag out is running.
    pub(super) drag_out: bool,
    /// The island by the clock shows its drop box.
    pub(super) drop_box: bool,
}

/// Whether a new screenshot shows the card: both settings on, the folder
/// read, and the island neither open on a view, paused nor in a drag.
pub(super) fn card_allowed(g: CardGate) -> bool {
    g.feature
        && g.card
        && g.granted
        && !g.expanded
        && !g.paused
        && !g.dragging
        && !g.drag_out
        && !g.drop_box
}

/// The card of the screenshot just saved: its file, when it was shown, and
/// until when it keeps the island open.
#[derive(Default)]
pub(super) struct ShotCard {
    card: RefCell<Option<PathBuf>>,
    shown_at: Cell<Option<Instant>>,
    card_until: Cell<Option<Instant>>,
    /// The picture it shows, kept for its drag.
    picture: RefCell<Option<Image>>,
}

impl ShotCard {
    /// Shows the card for `path` at `now`, for `secs`: a card already up
    /// takes the new file and waits again from now.
    pub(super) fn show(&self, path: PathBuf, now: Instant, secs: u32) {
        *self.card.borrow_mut() = Some(path);
        self.picture.take();
        self.shown_at.set(Some(now));
        self.card_until.set(Some(card_deadline(None, now, secs)));
    }

    /// The pointer is over the card at `now`: it waits `secs` from now.
    pub(super) fn hovered(&self, now: Instant, secs: u32) {
        if let Some(shown) = self.shown_at.get() {
            self.card_until
                .set(Some(card_deadline(Some(now), shown, secs)));
        }
    }

    /// The file on the card, if one is up.
    pub(super) fn path(&self) -> Option<PathBuf> {
        self.card.borrow().clone()
    }

    pub(super) fn is_up(&self) -> bool {
        self.card.borrow().is_some()
    }

    /// When the card closes by itself.
    #[cfg(test)]
    fn until(&self) -> Option<Instant> {
        self.card_until.get()
    }

    /// Whether the card still keeps the island open at `now`.
    pub(super) fn holds_open(&self, now: Instant) -> bool {
        self.card_until.get().is_some_and(|until| until > now)
    }

    /// Takes the card down; returns its file, if one was up.
    pub(super) fn close(&self) -> Option<PathBuf> {
        self.shown_at.set(None);
        self.card_until.set(None);
        self.picture.take();
        self.card.take()
    }

    /// Whether the card may stay with `shots` listed: none is up, or its
    /// file is among them.
    pub(super) fn listed_in(&self, shots: &[Shot]) -> bool {
        self.card
            .borrow()
            .as_ref()
            .is_none_or(|p| shots.iter().any(|s| &s.path == p))
    }
}

/// Screenshots taken out of the grid whose files wait to go to the Trash,
/// each under its own job.
#[derive(Default)]
pub(super) struct PendingShots {
    next: Cell<u64>,
    jobs: RefCell<Vec<(u64, PathBuf)>>,
}

impl PendingShots {
    /// Holds `path` under a new job, returned.
    pub(super) fn add(&self, path: PathBuf) -> u64 {
        let job = self.next.get();
        self.next.set(job + 1);
        self.jobs.borrow_mut().push((job, path));
        job
    }

    /// The file of `job`, if it still waits (it was not undone).
    pub(super) fn take(&self, job: u64) -> Option<PathBuf> {
        let mut jobs = self.jobs.borrow_mut();
        let at = jobs.iter().position(|(j, _)| *j == job)?;
        Some(jobs.remove(at).1)
    }

    /// Lets every job on `path` go: its Trash was undone.
    pub(super) fn cancel(&self, path: &Path) {
        self.jobs.borrow_mut().retain(|(_, p)| p != path);
    }

    /// Every file still waiting.
    pub(super) fn take_all(&self) -> Vec<PathBuf> {
        self.jobs.take().into_iter().map(|(_, p)| p).collect()
    }
}

/// The view's days and tiles, the chosen chip, the thumbnails already
/// made, the open peek and the screenshots waiting for the Trash.
pub(super) struct ShotsView {
    days: Rc<VecModel<ShotDay>>,
    /// Each shown day's tiles, kept so a refresh touches only what changed.
    day_tiles: RefCell<Vec<Rc<VecModel<ShotTile>>>>,
    /// Each chip's count, in the window's order.
    counts: Rc<VecModel<i32>>,
    filter: Cell<ShotFilter>,
    /// The screenshots as last shown: a tile's index is its place here.
    shown: RefCell<Vec<Shot>>,
    /// Thumbnails by path, with the modification time they were made for;
    /// an empty image when the picture could not be read, then whether the
    /// file was gone.
    thumbs: RefCell<HashMap<PathBuf, (u64, Image, bool)>>,
    /// Thumbnails being made on the worker.
    making: RefCell<HashSet<PathBuf>>,
    /// A refresh is due for thumbnails that came in.
    refresh_due: Cell<bool>,
    /// The screenshot whose peek is open.
    peek: RefCell<Option<PathBuf>>,
    /// The peek's large picture, made once for its file and time.
    peek_picture: RefCell<Option<(PathBuf, u64, Image)>>,
    /// The file and time whose large picture is being made on the worker.
    peek_making: RefCell<Option<(PathBuf, u64)>>,
    /// Trashed screenshots waiting for their Undo window to end.
    pending: PendingShots,
    /// Copy text can run here.
    can_read: bool,
}

impl ShotsView {
    /// The view's days and chip counts, bound to `window`'s.
    pub(super) fn new(window: &PocketIsland) -> Self {
        let days = Rc::new(VecModel::default());
        window.set_shot_days(ModelRc::from(days.clone()));
        let counts = Rc::new(VecModel::from(vec![0; FILTERS.len()]));
        window.set_shots_filter_counts(ModelRc::from(counts.clone()));
        let can_read = vento_platform::screen_text().supported();
        window.set_shots_can_read(can_read);
        Self {
            days,
            day_tiles: RefCell::new(Vec::new()),
            counts,
            filter: Cell::new(ShotFilter::All),
            shown: RefCell::new(Vec::new()),
            thumbs: RefCell::new(HashMap::new()),
            making: RefCell::new(HashSet::new()),
            refresh_due: Cell::new(false),
            peek: RefCell::new(None),
            peek_picture: RefCell::new(None),
            peek_making: RefCell::new(None),
            pending: PendingShots::default(),
            can_read,
        }
    }

    /// Shows `shots` (newest first) by day under the chosen chip, at
    /// `now_unix`, touching only the tiles that changed. A chip whose day
    /// holds nothing falls back to All. Thumbnails not made yet are asked
    /// for; those of screenshots no longer listed are dropped. A peek whose
    /// file is no longer listed closes.
    pub(super) fn show(&self, window: &PocketIsland, shots: &[Shot], now_unix: u64) {
        let tz = TimeZone::system();
        let n = counts(shots, now_unix, &tz);
        let mut filter = self.filter.get();
        if n[filter_index(filter)] == 0 {
            filter = ShotFilter::All;
            self.filter.set(filter);
        }
        window.set_shots_filter(i32::try_from(filter_index(filter)).unwrap_or(0));
        let count = |c: usize| i32::try_from(c).unwrap_or(i32::MAX);
        set_rows_in_place(&self.counts, n.iter().map(|&c| count(c)).collect());
        let mut wanted = Vec::new();
        {
            let mut thumbs = self.thumbs.borrow_mut();
            thumbs.retain(|p, _| shots.iter().any(|s| &s.path == p));
            let groups = grouped(shots, filter, now_unix, &tz);
            let mut models = self.day_tiles.borrow_mut();
            models.truncate(groups.len());
            while models.len() < groups.len() {
                models.push(Rc::new(VecModel::default()));
            }
            let mut days = Vec::with_capacity(groups.len());
            for ((day, at), model) in groups.iter().zip(models.iter()) {
                let tiles = at
                    .iter()
                    .map(|&i| {
                        let s = &shots[i];
                        let (picture, made) = match thumbs.get(&s.path) {
                            Some((made_for, picture, gone)) => {
                                if *made_for != s.modified_unix {
                                    wanted.push((s.path.clone(), s.modified_unix));
                                }
                                (picture.clone(), Some((picture.size().width > 0, *gone)))
                            }
                            None => {
                                wanted.push((s.path.clone(), s.modified_unix));
                                (no_picture(), None)
                            }
                        };
                        let (missing, unreadable) = tile_marks(made);
                        ShotTile {
                            index: count(i),
                            time: time_label(s.modified_unix, now_unix, &tz).into(),
                            size: human_size(s.len).into(),
                            picture,
                            missing,
                            unreadable,
                        }
                    })
                    .collect();
                set_rows_in_place(model, tiles);
                days.push(ShotDay {
                    label: day_heading(*day).into(),
                    count: count(at.len()),
                    tiles: ModelRc::from(model.clone()),
                });
            }
            set_rows_in_place(&self.days, days);
        }
        self.make_thumbs(wanted);
        *self.shown.borrow_mut() = shots.to_vec();
        self.show_peek(window, shots, now_unix, &tz);
    }

    /// Gives `window` the open peek, closing one whose file is no longer
    /// in `shots`. Its large picture is made on a worker thread, once per
    /// file and time; the tile's thumbnail stands in meanwhile.
    fn show_peek(&self, window: &PocketIsland, shots: &[Shot], now_unix: u64, tz: &TimeZone) {
        let place = peek_place(shots, self.peek.borrow().as_deref());
        let mut picture = self.peek_picture.borrow_mut();
        match place.map(|i| &shots[i]) {
            Some(s) => {
                let (image, wanted) = {
                    let thumbs = self.thumbs.borrow();
                    let thumb = thumbs.get(&s.path).map(|(m, image, _)| (*m, image));
                    peek_shown(picture.as_ref(), thumb, &s.path, s.modified_unix)
                };
                if wanted {
                    self.make_peek_picture(s.path.clone(), s.modified_unix);
                }
                window.set_shot_peek_picture(image);
                window.set_shot_peek_title(peek_title(s.modified_unix, now_unix, tz).into());
                window.set_shot_peek_meta(peek_meta(s).into());
                let home = std::env::var_os("HOME").map(PathBuf::from);
                window.set_shot_peek_path(display_path(&s.path, home.as_deref()).into());
            }
            None => {
                self.peek.take();
                *picture = None;
                window.set_shot_peek_picture(no_picture());
                window.set_shot_peek_title("".into());
                window.set_shot_peek_meta("".into());
                window.set_shot_peek_path("".into());
            }
        }
        window.set_shot_peek_index(peek_index(place));
    }

    /// Makes the thumbnails of `wanted` (path and modification time) on a
    /// worker thread, one after the other, each handed to the controller on
    /// the main thread as it is done. Those already being made are skipped.
    fn make_thumbs(&self, wanted: Vec<(PathBuf, u64)>) {
        let wanted: Vec<(PathBuf, u64)> = {
            let mut making = self.making.borrow_mut();
            wanted
                .into_iter()
                .filter(|(p, _)| making.insert(p.clone()))
                .collect()
        };
        if wanted.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = wanted.iter().map(|(p, _)| p.clone()).collect();
        let spawned = std::thread::Builder::new()
            .name("vento-shot-thumbs".into())
            .spawn(move || {
                for (path, mtime) in wanted {
                    let made = std::panic::catch_unwind(|| {
                        vento_platform::picture_thumbnail(&path, THUMB_PX, THUMB_RATIO)
                    })
                    .ok()
                    .flatten();
                    let gone = made.is_none() && !path.exists();
                    let delivered = slint::invoke_from_event_loop(move || {
                        if let Some(c) = MAIN_POCKET.with(|slot| slot.borrow().upgrade()) {
                            c.shot_thumb_made(path, mtime, made.as_ref(), gone);
                        }
                    });
                    if let Err(e) = delivered {
                        warn!("cannot hand a screenshot thumbnail over: {e}");
                        return;
                    }
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start making screenshot thumbnails: {e}");
            let mut making = self.making.borrow_mut();
            for p in &paths {
                making.remove(p);
            }
        }
    }

    /// Makes the peek's large picture of `path` saved at `mtime` on a
    /// worker thread, handed to the controller on the main thread; unless
    /// it is being made already.
    fn make_peek_picture(&self, path: PathBuf, mtime: u64) {
        let job = Some((path.clone(), mtime));
        if *self.peek_making.borrow() == job {
            return;
        }
        *self.peek_making.borrow_mut() = job;
        let spawned = std::thread::Builder::new()
            .name("vento-shot-peek".into())
            .spawn(move || {
                let made = std::panic::catch_unwind(|| {
                    vento_platform::picture_thumbnail(&path, PEEK_PX, THUMB_RATIO)
                })
                .ok()
                .flatten();
                let delivered = slint::invoke_from_event_loop(move || {
                    if let Some(c) = MAIN_POCKET.with(|slot| slot.borrow().upgrade()) {
                        c.shot_peek_made(path, mtime, made.as_ref());
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot hand the peek's picture over: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start making the peek's picture: {e}");
            self.peek_making.take();
        }
    }

    /// The file of the screenshot shown at tile `index`.
    pub(super) fn path_at(&self, index: i32) -> Option<PathBuf> {
        let i = usize::try_from(index).ok()?;
        self.shown.borrow().get(i).map(|s| s.path.clone())
    }

    /// Closes the peek; false if none was open.
    pub(super) fn close_peek(&self) -> bool {
        self.peek.take().is_some()
    }

    /// Takes the screenshots still waiting for the Trash.
    pub(super) fn take_pending(&self) -> Vec<PathBuf> {
        self.pending.take_all()
    }

    /// Forgets the Trash of `path`: its Undo was used.
    pub(super) fn cancel_pending(&self, path: &Path) {
        self.pending.cancel(path);
    }
}

impl PocketController {
    /// Routes the Screenshots view's chips, tiles, buttons and peek to the
    /// controller.
    pub(super) fn connect_shots(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        let c = with.clone();
        w.on_shots_filter_chosen(move |i| {
            if let (Some(c), Some(f)) = (c(), filter_at(i)) {
                c.shots_view.filter.set(f);
                c.refresh_shots();
            }
        });
        let c = with.clone();
        w.on_shots_folder(move || {
            if let Some(c) = c() {
                let folder = c.shots.folder();
                if let Err(e) = c.platform.open_paths(&[folder]) {
                    warn!("cannot open the screenshot folder: {e}");
                    let text = fill(
                        &tr("Couldn't open: {reason}"),
                        &[("reason", &e.to_string())],
                    );
                    c.toast.show(&text, None, c.toast_at());
                }
            }
        });
        let c = with.clone();
        w.on_shot_pressed(move |i| {
            if let (Some(c), Ok(i)) = (c(), u32::try_from(i)) {
                c.latch.press(Press::Shot(i));
            }
        });
        let c = with.clone();
        w.on_shot_moved(move |_, dx, dy| {
            if let Some(c) = c() {
                c.moved(dx, dy);
            }
        });
        let c = with.clone();
        w.on_shot_released(move |_| {
            if let Some(c) = c() {
                c.released();
            }
        });
        let c = with.clone();
        w.on_shot_copy(move |i| {
            if let Some(c) = c()
                && let Some(path) = c.shots_view.path_at(i)
            {
                c.shot_copy(&path);
            }
        });
        let c = with.clone();
        w.on_shot_copy_text(move |i| {
            if let Some(c) = c()
                && let Some(path) = c.shots_view.path_at(i)
            {
                c.shot_copy_text(path);
            }
        });
        let c = with.clone();
        w.on_shot_pocket(move |i| {
            if let Some(c) = c()
                && let Some(path) = c.shots_view.path_at(i)
            {
                c.shot_to_pocket(path);
            }
        });
        let c = with.clone();
        w.on_shot_reveal(move |i| {
            if let Some(c) = c()
                && let Some(path) = c.shots_view.path_at(i)
            {
                c.shot_reveal(&path);
            }
        });
        let c = with.clone();
        w.on_shot_trash(move |i| {
            if let Some(c) = c()
                && let Some(path) = c.shots_view.path_at(i)
            {
                c.shot_trash(path);
            }
        });
        let c = with.clone();
        w.on_shot_peek_back(move || {
            if let Some(c) = c()
                && c.shots_view.close_peek()
            {
                c.refresh_shots();
            }
        });
        let c = with.clone();
        w.on_shots_allow(move || {
            if let Some(c) = c() {
                c.shots.touch();
                c.refresh();
            }
        });
        self.connect_card(with);
    }

    /// Routes the card's picture and buttons to the controller. Each button
    /// acts at once, then the card closes.
    fn connect_card(&self, with: impl Fn() -> Option<Rc<Self>> + Clone + 'static) {
        let w = &self.window;
        let c = with.clone();
        w.on_card_pressed(move || {
            if let Some(c) = c() {
                c.latch.press(Press::Card);
            }
        });
        let c = with.clone();
        w.on_card_moved(move |dx, dy| {
            if let Some(c) = c() {
                c.moved(dx, dy);
            }
        });
        let c = with.clone();
        w.on_card_released(move || {
            if let Some(c) = c() {
                c.released();
            }
        });
        let c = with.clone();
        w.on_card_copy(move || {
            if let Some(c) = c()
                && let Some(path) = c.card.path()
            {
                c.shot_copy(&path);
                c.finish_card(|_| {});
            }
        });
        let c = with.clone();
        w.on_card_copy_text(move || {
            if let Some(c) = c()
                && let Some(path) = c.card.path()
            {
                c.shot_copy_text(path);
                c.finish_card(|_| {});
            }
        });
        let c = with.clone();
        w.on_card_pocket(move || {
            if let Some(c) = c()
                && let Some(path) = c.card.path()
            {
                c.finish_card(move |c| c.shot_to_pocket(path));
            }
        });
        w.on_card_trash(move || {
            if let Some(c) = with() {
                c.card_trash();
            }
        });
    }

    /// Whether the island has a Screenshots tab: the feature is on and the
    /// folder was not refused.
    pub(super) fn shots_on(&self) -> bool {
        tabs(false, self.shots.access()).2
    }

    /// Pushes the screenshots' count, folder and state into the window, and
    /// the days and tiles while the view is in sight: a tile costs a
    /// thumbnail, so none are made for a hidden view. A card whose file is
    /// no longer listed closes.
    pub(super) fn refresh_shots(&self) {
        let w = &self.window;
        let on = self.shots_on();
        if self.card.is_up() && !(on && self.card.listed_in(&self.shots.shots())) {
            debug!("screenshot card: its file is gone");
            self.close_card();
        }
        let count = if on { self.shots.count() } else { 0 };
        w.set_shots_count(i32::try_from(count).unwrap_or(i32::MAX));
        w.set_shots_state(shots_state(self.shots.access(), count));
        w.set_shots_folder_name(display_name(&self.shots.folder()).into());
        self.set_wings();
        if !on {
            self.shots_view.show(w, &[], super::clip::now_unix());
        } else if self.expanded() && self.shown_view() == View::Screenshots {
            self.shots_view
                .show(w, &self.shots.shots(), super::clip::now_unix());
        }
    }

    /// A thumbnail came in from the worker (`made`, `None` when the picture
    /// could not be read, `gone` when its file was gone then): kept, and
    /// shown on the next turn of the event loop, with any others that came
    /// in meanwhile.
    fn shot_thumb_made(&self, path: PathBuf, mtime: u64, made: Option<&RgbaImage>, gone: bool) {
        let view = &self.shots_view;
        view.making.borrow_mut().remove(&path);
        let picture = made.and_then(to_image).unwrap_or_else(|| {
            debug!("screenshots: no thumbnail of {}", path.display());
            no_picture()
        });
        view.thumbs
            .borrow_mut()
            .insert(path, (mtime, picture, gone));
        if view.refresh_due.replace(true) {
            return;
        }
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(c) = this.upgrade() {
                c.shots_view.refresh_due.set(false);
                c.refresh_shots();
            }
        });
    }

    /// The peek's large picture of `path` saved at `mtime` came in from the
    /// worker (`None` when it could not be made): kept and shown, if the
    /// peek is still on that file.
    fn shot_peek_made(&self, path: PathBuf, mtime: u64, made: Option<&RgbaImage>) {
        let view = &self.shots_view;
        if view
            .peek_making
            .borrow()
            .as_ref()
            .is_some_and(|(p, m)| *p == path && *m == mtime)
        {
            view.peek_making.take();
        }
        if view.peek.borrow().as_deref() != Some(path.as_path()) {
            return;
        }
        let picture = made.and_then(to_image).unwrap_or_else(|| {
            debug!("screenshots: no peek picture of {}", path.display());
            no_picture()
        });
        *view.peek_picture.borrow_mut() = Some((path, mtime, picture));
        self.refresh_shots();
    }

    /// A click on tile `index` (a press that never moved far): opens its
    /// peek. A click on the peek's own picture does nothing.
    pub(super) fn shot_clicked(&self, index: u32) {
        let Some(path) = self.shots_view.path_at(i32::try_from(index).unwrap_or(-1)) else {
            return;
        };
        if self.shots_view.peek.borrow().as_ref() == Some(&path) {
            return;
        }
        *self.shots_view.peek.borrow_mut() = Some(path);
        self.refresh_shots();
    }

    /// Copy: the picture and the file on the clipboard, as one item (see
    /// [`shot_snapshot`]). The history records it as any copy.
    pub(super) fn shot_copy(&self, path: &Path) {
        let png = if is_png(path) {
            match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(e) => {
                    warn!("cannot read the screenshot {}: {e}", path.display());
                    let text = fill(
                        &tr("Couldn't copy: {reason}"),
                        &[("reason", &vento_core::i18n::errors::io_reason(&e))],
                    );
                    self.toast.show(&text, None, self.toast_at());
                    return;
                }
            }
        } else {
            None
        };
        match self.platform.write_clipboard(&shot_snapshot(path, png)) {
            Ok(_) => {
                info!("screenshot copied");
                self.toast.show(&tr("Copied"), None, self.toast_at());
            }
            Err(e) => self.copy_failed(&e),
        }
    }

    /// Says a copy failed.
    fn copy_failed(&self, e: &PlatformError) {
        warn!(
            "cannot copy a screenshot: {}",
            with_language("en", || e.to_string())
        );
        let text = fill(
            &tr("Couldn't copy: {reason}"),
            &[("reason", &e.to_string())],
        );
        self.toast.show(&text, None, self.toast_at());
    }

    /// Copy text: reads the text in the picture on a worker thread with
    /// Copy from screen's engine and line joining, and copies it.
    pub(super) fn shot_copy_text(&self, path: PathBuf) {
        if !self.shots_view.can_read {
            return;
        }
        let this = self.this.clone();
        crate::screen_text::read_on_worker_with(
            Box::new(move || vento_platform::read_image_text(&path)),
            Box::new(move |result| {
                if let Some(c) = this.upgrade() {
                    c.shot_text_read(result);
                }
            }),
        );
    }

    /// Copies the text a Copy text found, and says how it went.
    fn shot_text_read(&self, result: ReadResult) {
        let text = match result {
            Ok(boxes) => assemble(&boxes),
            Err(e) => {
                warn!(
                    "cannot read the text in a screenshot: {}",
                    with_language("en", || e.to_string())
                );
                self.toast
                    .show(&tr("Couldn't read the picture"), None, self.toast_at());
                return;
            }
        };
        if text.is_empty() {
            self.toast.show(&tr("No text found"), None, self.toast_at());
            return;
        }
        match self.platform.copy_text(&text) {
            Ok(()) => self.toast.show(&tr("Copied text"), None, self.toast_at()),
            Err(e) => self.copy_failed(&e),
        }
    }

    /// Pocket: adds the file as a drop on the Pocket does.
    pub(super) fn shot_to_pocket(&self, path: PathBuf) {
        self.add_dropped(Dropped::Files(vec![path]));
    }

    /// Show in Finder.
    pub(super) fn shot_reveal(&self, path: &Path) {
        self.platform.reveal_in_finder(&[path.to_path_buf()]);
    }

    /// Move to Trash: the screenshot leaves the grid at once, with the
    /// island's Undo; its file goes to the Trash once Undo is no longer
    /// offered.
    pub(super) fn shot_trash(&self, path: PathBuf) {
        debug!("screenshot to the Trash: {}", path.display());
        self.offer_undo(Undo::Shot(path.clone()));
        self.trash_after_undo(path);
    }

    /// Takes the screenshot at `path` out of the grid at once; its file goes
    /// to the Trash once Undo is no longer offered.
    fn trash_after_undo(&self, path: PathBuf) {
        self.shots.hide(&path);
        let job = self.shots_view.pending.add(path);
        self.refresh_shots();
        let this = self.this.clone();
        Timer::single_shot(UNDO_WINDOW, move || {
            if let Some(c) = this.upgrade()
                && let Some(path) = c.shots_view.pending.take(job)
            {
                c.trash_shot(&path);
            }
        });
    }

    /// Moves the screenshot at `path` to the Trash now.
    pub(super) fn trash_shot(&self, path: &Path) {
        match self.platform.trash(path) {
            Ok(()) => info!("screenshot moved to the Trash"),
            Err(e) => {
                warn!("cannot move {} to the Trash: {e}", path.display());
                // It is still in the folder: list it again.
                self.shots.unhide(path);
                self.refresh_shots();
            }
        }
    }

    /// Undo of a screenshot's Trash: it is listed again and stays.
    pub(super) fn undo_shot_trash(&self, path: &Path) {
        self.shots_view.cancel_pending(path);
        self.shots.unhide(path);
        self.refresh_shots();
    }

    /// Starts one drag session with the screenshot at `path`, its
    /// thumbnail (or the card's picture) as the image. The drag never
    /// removes anything. The press latch is already marked running.
    pub(super) fn start_shot_drag(&self, path: PathBuf) {
        let thumb = self
            .shots_view
            .thumbs
            .borrow()
            .get(&path)
            .map(|(_, image, _)| image.clone())
            .filter(|image| image.size().width > 0);
        let card = || {
            (self.card.path().as_ref() == Some(&path))
                .then(|| self.card.picture.borrow().clone())
                .flatten()
        };
        let picture = thumb
            .or_else(card)
            .and_then(|image| image.to_rgba8())
            .map(|buffer| RgbaImage {
                width: buffer.width(),
                height: buffer.height(),
                pixels: buffer.as_bytes().to_vec(),
            });
        let preview = picture
            .as_ref()
            .and_then(|p| preview_png(p, DRAG_PT))
            .or_else(|| {
                let icon = self
                    .platform
                    .file_icon(&path, ICON_PX)
                    .unwrap_or_else(|| placeholder_icon(ICON_PX));
                preview_png(&icon, ICON_PX)
            });
        let Some(preview) = preview else {
            self.latch.ended();
            self.card_drag.set(false);
            warn!("cannot make a drag preview");
            return;
        };
        info!("screenshot drag out start");
        let result = drag::start_drag(
            &self.window.window().window_handle(),
            drag::DragItem::Files(vec![path]),
            drag::Image::Raw(preview),
            move |result, pos| {
                let at = drop_point(&pos);
                let delivered = slint::invoke_from_event_loop(move || {
                    if let Some(c) = MAIN_POCKET.with(|slot| slot.borrow().upgrade()) {
                        c.latch.ended();
                        info!(
                            "screenshot drag out end: {result:?} at ({}, {})",
                            at.x, at.y
                        );
                        c.release_grab(at);
                        // A drag from the card is its action: it closes.
                        if c.card_drag.replace(false) {
                            c.finish_card(|_| {});
                        }
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot report the end of a screenshot drag: {e}");
                }
            },
            drag::Options::default(),
        );
        if let Err(e) = result {
            warn!("cannot start the screenshot drag: {e}");
            self.latch.ended();
            self.card_drag.set(false);
        }
    }

    /// Whether a screenshot saved now shows the card (see [`card_allowed`]).
    pub(super) fn card_allowed(&self) -> bool {
        let (feature, card) = {
            let s = self.settings.borrow();
            (s.screenshots, s.screenshot_card)
        };
        self.warm.get()
            && card_allowed(CardGate {
                feature,
                card,
                granted: self.shots.enabled(),
                expanded: self.expanded() && !self.card.is_up(),
                paused: self.paused.get(),
                dragging: self.was_dragging.get(),
                drag_out: self.latch.running(),
                drop_box: self.drop_box.get(),
            })
    }

    /// A screenshot was saved at `path`: the island drops down with its
    /// card, unless it may not now. A card already up takes the new one.
    pub(super) fn show_card(&self, path: PathBuf) {
        if !self.card_allowed() {
            debug!("screenshot card not shown now");
            return;
        }
        let Some(shot) = self.shots.shots().into_iter().find(|s| s.path == path) else {
            return;
        };
        let secs = self.settings.borrow().screenshot_card_secs;
        self.card.show(path.clone(), Instant::now(), secs);
        let w = &self.window;
        let folder = display_name(&self.shots.folder());
        w.set_card_meta(card_meta(&shot, &folder).into());
        w.set_card_picture(no_picture());
        w.set_card(true);
        self.make_card_picture(path);
        self.fit_panel();
        self.run_undo_timer();
        info!("screenshot card shown");
    }

    /// Takes the card down, leaving the island as it is.
    pub(super) fn close_card(&self) {
        if self.card.close().is_none() {
            return;
        }
        debug!("screenshot card closed");
        let w = &self.window;
        w.set_card(false);
        w.set_card_picture(no_picture());
        self.fit_panel();
        self.run_undo_timer();
    }

    /// After an action on the card: on the next turn of the event loop,
    /// once the window has finished with the click, the card closes, `then`
    /// runs, and the island goes back to rest.
    pub(super) fn finish_card(&self, then: impl FnOnce(&Self) + 'static) {
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            let Some(c) = this.upgrade() else {
                return;
            };
            c.close_card();
            then(&c);
            if c.warm.get() && !c.paused.get() && c.expanded() {
                c.settle_now();
            }
        });
    }

    /// A click on the card's picture: the Screenshots view opens with that
    /// screenshot peeked, on the next turn of the event loop, once the
    /// window has finished with the click.
    pub(super) fn card_clicked(&self) {
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            let Some(c) = this.upgrade() else {
                return;
            };
            let Some(path) = c.card.path() else {
                return;
            };
            c.close_card();
            *c.shots_view.peek.borrow_mut() = Some(path);
            c.set_view(View::Screenshots);
            c.hold_open_briefly();
        });
    }

    /// The card's Move to Trash: the card closes and a toast offers Undo,
    /// as the island's own ring can't be seen once the card is gone. The
    /// file goes to the Trash as a tile's does.
    fn card_trash(&self) {
        let Some(path) = self.card.path() else {
            return;
        };
        let at = self.toast_at();
        debug!("screenshot to the Trash from the card");
        self.finish_card(move |c| {
            c.trash_after_undo(path.clone());
            let this = c.this.clone();
            let undo = move || {
                if let Some(c) = this.upgrade() {
                    c.undo_shot_trash(&path);
                }
            };
            c.toast
                .show(&tr("Moved to Trash"), Some(Box::new(undo)), at);
        });
    }

    /// Makes the card's picture of `path` on a worker thread and shows it,
    /// if the card still shows that file.
    fn make_card_picture(&self, path: PathBuf) {
        let spawned = std::thread::Builder::new()
            .name("vento-shot-card".into())
            .spawn(move || {
                let made = std::panic::catch_unwind(|| {
                    vento_platform::picture_thumbnail(&path, CARD_PX, THUMB_RATIO)
                })
                .ok()
                .flatten();
                let delivered = slint::invoke_from_event_loop(move || {
                    if let Some(c) = MAIN_POCKET.with(|slot| slot.borrow().upgrade()) {
                        c.card_picture_made(&path, made.as_ref());
                    }
                });
                if let Err(e) = delivered {
                    warn!("cannot hand the card's picture over: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start making the card's picture: {e}");
        }
    }

    /// The card's picture came in (`None` when it could not be made).
    fn card_picture_made(&self, path: &Path, made: Option<&RgbaImage>) {
        if self.card.path().as_deref() != Some(path) {
            return;
        }
        let Some(picture) = made.and_then(to_image) else {
            debug!("screenshots: no card picture of {}", path.display());
            return;
        };
        *self.card.picture.borrow_mut() = Some(picture.clone());
        self.window.set_card_picture(picture);
    }

    /// While the card is up and the island shows it (`presence`), the
    /// pointer over it (`over`) at `now` keeps it the full wait longer.
    pub(super) fn card_hover(&self, presence: Presence, over: bool, now: Instant) {
        if over && presence == Presence::Expanded && self.card.is_up() {
            let secs = self.settings.borrow().screenshot_card_secs;
            self.card.hovered(now, secs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{Rgba8Pixel, SharedPixelBuffer};
    use vento_core::clip::{TYPE_FILE_URL, TYPE_PNG, path_to_file_url};

    fn shot(path: &str) -> Shot {
        Shot {
            path: PathBuf::from(path),
            modified_unix: 1_000,
            len: 10,
            dims: None,
        }
    }

    #[test]
    fn shot_copy_snapshot_has_png_and_file_url() {
        let path = Path::new("/tmp/Screenshot 1.png");
        let s = shot_snapshot(path, Some(vec![1, 2, 3]));
        assert_eq!(s.items.len(), 1, "one pasteboard item");
        let types: Vec<&str> = s.items[0].iter().map(|r| r.ty.as_str()).collect();
        assert_eq!(types, [TYPE_PNG, TYPE_FILE_URL]);
        assert_eq!(s.items[0][0].data, [1, 2, 3]);
        assert_eq!(s.items[0][1].data, path_to_file_url(path).into_bytes());
    }

    #[test]
    fn shot_copy_snapshot_of_a_jpg_has_file_url_only() {
        let path = Path::new("/tmp/Screenshot 1.jpg");
        let s = shot_snapshot(path, None);
        assert_eq!(s.items.len(), 1);
        let types: Vec<&str> = s.items[0].iter().map(|r| r.ty.as_str()).collect();
        assert_eq!(types, [TYPE_FILE_URL]);
        assert!(is_png(Path::new("/tmp/a.PNG")));
        assert!(!is_png(Path::new("/tmp/a.jpg")));
    }

    #[test]
    fn peek_closes_when_the_file_vanishes() {
        let a = Path::new("/tmp/a.png");
        let shots = [shot("/tmp/a.png"), shot("/tmp/b.png")];
        assert_eq!(peek_place(&shots, Some(a)), Some(0));
        assert_eq!(peek_index(peek_place(&shots, Some(a))), 0);
        // A refresh without that file: the peek closes (index -1).
        let after = [shot("/tmp/b.png")];
        assert_eq!(peek_place(&after, Some(a)), None);
        assert_eq!(peek_index(peek_place(&after, Some(a))), -1);
        assert_eq!(peek_place(&after, None), None);
    }

    #[test]
    fn chips_map_to_filters_in_window_order() {
        for (i, f) in FILTERS.iter().enumerate() {
            assert_eq!(filter_at(i as i32), Some(*f));
            assert_eq!(filter_index(*f), i);
        }
        assert_eq!(filter_at(4), None);
        assert_eq!(filter_at(-1), None);
    }

    #[test]
    fn the_view_says_why_it_is_empty() {
        assert_eq!(shots_state(Access::WaitForOpen, 0), 2);
        assert_eq!(shots_state(Access::Granted, 0), 1);
        assert_eq!(shots_state(Access::Granted, 3), 0);
        // A listing under way shows the (empty) grid, not the empty line.
        assert_eq!(shots_state(Access::Asking, 0), 0);
    }

    #[test]
    fn a_trashed_shot_waits_until_its_job_ends_or_is_undone() {
        let pending = PendingShots::default();
        let a = PathBuf::from("/tmp/a.png");
        let first = pending.add(a.clone());
        // Undone, then trashed again: the first job's timer finds nothing.
        pending.cancel(&a);
        let second = pending.add(a.clone());
        assert_eq!(pending.take(first), None);
        assert_eq!(pending.take(second), Some(a.clone()));
        assert_eq!(pending.take(second), None);
        // At quit, every file still waiting.
        pending.add(a.clone());
        assert_eq!(pending.take_all(), [a]);
        assert!(pending.take_all().is_empty());
    }

    #[test]
    fn peek_shows_the_thumbnail_until_its_picture_is_made() {
        let picture = |w| Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(w, 1));
        let width = |i: &Image| i.size().width;
        let a = PathBuf::from("/tmp/a.png");
        let thumb = picture(208);
        // Nothing made yet: the tile's thumbnail, and the picture is asked for.
        let (shown, wanted) = peek_shown(None, Some((5, &thumb)), &a, 5);
        assert_eq!((width(&shown), wanted), (208, true));
        // No thumbnail either (or one of an older time): nothing.
        let (shown, wanted) = peek_shown(None, None, &a, 5);
        assert_eq!((width(&shown), wanted), (0, true));
        let (shown, wanted) = peek_shown(None, Some((4, &thumb)), &a, 5);
        assert_eq!((width(&shown), wanted), (0, true));
        // Made for this file and time: the large picture.
        let made = (a.clone(), 5, picture(600));
        let (shown, wanted) = peek_shown(Some(&made), Some((5, &thumb)), &a, 5);
        assert_eq!((width(&shown), wanted), (600, false));
        // Made for another file, or the file changed since: made again.
        let b = PathBuf::from("/tmp/b.png");
        let (shown, wanted) = peek_shown(Some(&made), Some((5, &thumb)), &b, 5);
        assert_eq!((width(&shown), wanted), (208, true));
        let (_, wanted) = peek_shown(Some(&made), None, &a, 6);
        assert!(wanted);
        // Sharp on a 2x screen: the 266-point box needs 532 pixels.
        const { assert!(PEEK_PX >= 532) };
    }

    #[test]
    fn peek_meta_has_dimensions_when_known() {
        let mut s = shot("/tmp/a.png");
        s.len = 0;
        assert_eq!(peek_meta(&s), human_size(0));
        s.dims = Some((1512, 982));
        assert_eq!(peek_meta(&s), format!("1512 × 982 · {}", human_size(0)));
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn card_waits_while_hovered_then_closes() {
        let shown = Instant::now();
        // Never hovered: it stays the wait from when it was shown.
        assert_eq!(card_deadline(None, shown, 4), shown + secs(4));
        // Hovered later: the full wait again from the last hover.
        let hovered = shown + secs(3);
        assert_eq!(card_deadline(Some(hovered), shown, 4), hovered + secs(4));
        // A card that is hovered keeps being held open; once the pointer
        // leaves, it closes after the wait.
        let card = ShotCard::default();
        card.show(PathBuf::from("/tmp/a.png"), shown, 4);
        assert!(card.holds_open(shown + secs(3)));
        assert!(!card.holds_open(shown + secs(4)));
        card.hovered(hovered, 4);
        assert!(card.holds_open(shown + secs(6)));
        assert!(!card.holds_open(hovered + secs(4)));
        assert_eq!(card.until(), Some(hovered + secs(4)));
    }

    #[test]
    fn no_card_when_expanded_paused_or_dragging() {
        let open = CardGate {
            feature: true,
            card: true,
            granted: true,
            expanded: false,
            paused: false,
            dragging: false,
            drag_out: false,
            drop_box: false,
        };
        assert!(card_allowed(open));
        for gate in [
            CardGate {
                feature: false,
                ..open
            },
            CardGate {
                card: false,
                ..open
            },
            CardGate {
                granted: false,
                ..open
            },
            CardGate {
                expanded: true,
                ..open
            },
            CardGate {
                paused: true,
                ..open
            },
            CardGate {
                dragging: true,
                ..open
            },
            CardGate {
                drag_out: true,
                ..open
            },
            CardGate {
                drop_box: true,
                ..open
            },
        ] {
            assert!(!card_allowed(gate), "{gate:?}");
        }
    }

    #[test]
    fn second_shot_replaces_the_card() {
        let t0 = Instant::now();
        let card = ShotCard::default();
        assert!(!card.is_up());
        card.show(PathBuf::from("/tmp/a.png"), t0, 4);
        card.hovered(t0 + secs(1), 4);
        let t1 = t0 + secs(2);
        card.show(PathBuf::from("/tmp/b.png"), t1, 4);
        assert_eq!(card.path(), Some(PathBuf::from("/tmp/b.png")));
        // The wait starts again from the second shot, not the old hover.
        assert_eq!(card.until(), Some(t1 + secs(4)));
        assert_eq!(card.close(), Some(PathBuf::from("/tmp/b.png")));
        assert!(!card.is_up());
        assert_eq!(card.until(), None);
        assert!(!card.holds_open(t1));
    }

    #[test]
    fn card_closes_when_its_file_vanishes() {
        let card = ShotCard::default();
        // No card: nothing to close.
        assert!(card.listed_in(&[]));
        card.show(PathBuf::from("/tmp/a.png"), Instant::now(), 4);
        assert!(card.listed_in(&[shot("/tmp/b.png"), shot("/tmp/a.png")]));
        // A refresh without the card's file: it must close.
        assert!(!card.listed_in(&[shot("/tmp/b.png")]));
    }

    #[test]
    fn card_meta_names_the_folder() {
        let mut s = shot("/tmp/a.png");
        s.len = 0;
        s.dims = Some((1512, 982));
        assert_eq!(
            card_meta(&s, "Desktop"),
            format!("1512 × 982 · {} · Desktop", human_size(0))
        );
        s.dims = None;
        assert_eq!(
            card_meta(&s, "Desktop"),
            format!("{} · Desktop", human_size(0))
        );
    }

    #[test]
    fn a_tile_is_missing_only_when_its_file_is_gone() {
        // Not made yet: neither.
        assert_eq!(tile_marks(None), (false, false));
        // Made: neither.
        assert_eq!(tile_marks(Some((true, false))), (false, false));
        // No picture, the file is there (a PDF screenshot): a plain file.
        assert_eq!(tile_marks(Some((false, false))), (false, true));
        // No picture and the file is gone: missing.
        assert_eq!(tile_marks(Some((false, true))), (true, false));
    }
}
