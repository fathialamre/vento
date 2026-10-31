//! The island's Clipboard view: the history's rows as the window draws them,
//! what a row, its buttons and its peek do, and the bottom bar's tabs
//! between the views (the Clipboard, the Pocket and the Screenshots).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use log::{debug, warn};
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use vento_core::clip_history::ClipEntry;

use crate::ui::{ClipRow, PeekLine, PocketIsland};

use super::clip::{ClipKind, ClipRowData, PEEK_PICTURE_PX, is_web, now_unix};
use super::geometry::Panel;
use super::shots::Access;
use super::{PocketController, Undo, View, to_image};

/// How long the island stays open after a switch button, wherever the
/// pointer is.
const SWITCH_GRACE: Duration = Duration::from_secs(1);

/// Which rows the Clipboard view lists, chosen in the row above them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ClipFilter {
    #[default]
    All,
    Pinned,
    Text,
    Links,
    Images,
    Files,
}

/// The filters in the order the window's row shows them.
const FILTERS: [ClipFilter; 6] = [
    ClipFilter::All,
    ClipFilter::Pinned,
    ClipFilter::Text,
    ClipFilter::Links,
    ClipFilter::Images,
    ClipFilter::Files,
];

impl ClipFilter {
    /// The filter at window index `i`.
    fn from_index(i: i32) -> Option<Self> {
        usize::try_from(i)
            .ok()
            .and_then(|i| FILTERS.get(i))
            .copied()
    }

    /// Its index in the window's row.
    fn index(self) -> i32 {
        FILTERS
            .iter()
            .position(|&f| f == self)
            .map_or(0, |i| i as i32)
    }

    /// Whether the view lists `row` under this filter.
    fn admits(self, row: &ClipRowData) -> bool {
        match self {
            ClipFilter::All => true,
            ClipFilter::Pinned => row.pinned,
            ClipFilter::Text => row.kind == ClipKind::Text,
            ClipFilter::Links => row.kind == ClipKind::Link,
            ClipFilter::Images => row.kind == ClipKind::Image,
            ClipFilter::Files => row.kind == ClipKind::Files,
        }
    }
}

/// How many of `rows` each filter lists, in the window's order.
fn filter_counts(rows: &[ClipRowData]) -> Vec<i32> {
    FILTERS
        .iter()
        .map(|f| {
            let n = rows.iter().filter(|r| f.admits(r)).count();
            i32::try_from(n).unwrap_or(i32::MAX)
        })
        .collect()
}

/// What the open peek shows, made once for its entry: its text, picture
/// and lines.
struct PeekShown {
    id: u64,
    text: String,
    picture: Image,
    lines: Vec<PeekLine>,
}

/// The rows the Clipboard view shows, the filter choosing them, the image
/// thumbnails already made for them, and the open peek.
pub(super) struct ClipView {
    rows: Rc<VecModel<ClipRow>>,
    /// Each filter's count, in the window's order.
    counts: Rc<VecModel<i32>>,
    filter: Cell<ClipFilter>,
    /// Thumbnails by entry id, made once; an empty image when the picture
    /// could not be read.
    thumbs: RefCell<HashMap<u64, Image>>,
    /// The entry whose peek is open.
    peek: Cell<Option<u64>>,
    /// What it shows, once made.
    peek_shown: RefCell<Option<PeekShown>>,
    peek_lines: Rc<VecModel<PeekLine>>,
}

impl ClipView {
    /// The view's rows, bound to `window`'s list.
    pub(super) fn new(window: &PocketIsland) -> Self {
        let rows = Rc::new(VecModel::default());
        window.set_clip_rows(ModelRc::from(rows.clone()));
        let counts = Rc::new(VecModel::from(vec![0; FILTERS.len()]));
        window.set_clip_filter_counts(ModelRc::from(counts.clone()));
        let peek_lines = Rc::new(VecModel::default());
        window.set_clip_peek_lines(ModelRc::from(peek_lines.clone()));
        Self {
            rows,
            counts,
            filter: Cell::new(ClipFilter::All),
            thumbs: RefCell::new(HashMap::new()),
            peek: Cell::new(None),
            peek_shown: RefCell::new(None),
            peek_lines,
        }
    }

    /// Shows the rows of `data` the filter admits, touching only the rows
    /// that changed so hover stays where it is, and gives `window` the
    /// filter and each filter's count. Thumbnails of entries that left the
    /// history are dropped. Returns the place among the rows shown of the
    /// entry whose peek is open, closing a peek whose entry is no longer
    /// listed.
    pub(super) fn show(&self, window: &PocketIsland, data: &[ClipRowData]) -> Option<usize> {
        let filter = self.filter.get();
        window.set_clip_filter(filter.index());
        set_rows_in_place(&self.counts, filter_counts(data));
        let mut thumbs = self.thumbs.borrow_mut();
        let ids: HashSet<u64> = data.iter().map(|d| d.id).collect();
        thumbs.retain(|id, _| ids.contains(id));
        let rows = data
            .iter()
            .filter(|d| filter.admits(d))
            .map(|d| {
                let thumb = match &d.thumb {
                    Some(path) => thumb_for(&mut thumbs, d.id, path),
                    None => no_picture(),
                };
                clip_row(d, thumb)
            })
            .collect::<Vec<_>>();
        let open = self.peek.get().and_then(|id| {
            let id = i32::try_from(id).ok()?;
            rows.iter().position(|r| r.id == id)
        });
        if open.is_none() {
            self.peek.set(None);
        }
        set_rows_in_place(&self.rows, rows);
        open
    }
}

/// A larger picture of the file at `path` for a peek, or `None` when it
/// can't be read.
pub(super) fn peek_picture(path: &Path) -> Option<Image> {
    vento_platform::picture_thumbnail(path, PEEK_PICTURE_PX, 4)
        .as_ref()
        .and_then(to_image)
}

/// Makes `model` hold `rows` without rebuilding it: rows are added or
/// dropped at the end and the others rewritten only where they changed.
/// Each row the window draws stays where it is, so the one under the
/// pointer keeps its hover when a copy pushes a new entry in on top.
pub(super) fn set_rows_in_place<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
    for (i, row) in rows.into_iter().enumerate() {
        if i >= model.row_count() {
            model.push(row);
        } else if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
}

/// The cached thumbnail of entry `id`, made from `path` the first time. Not
/// cached while the file isn't there yet (its entry is still being stored).
fn thumb_for(thumbs: &mut HashMap<u64, Image>, id: u64, path: &Path) -> Image {
    if let Some(t) = thumbs.get(&id) {
        return t.clone();
    }
    if !path.exists() {
        return no_picture();
    }
    let t = thumbnail(path);
    thumbs.insert(id, t.clone());
    t
}

/// The small thumbnail the store worker wrote at `path`, or an empty image
/// if it can't be read: the row then shows the photo icon.
fn thumbnail(path: &Path) -> Image {
    Image::load_from_path(path).unwrap_or_else(|_| {
        debug!("clipboard: no thumbnail at {}", path.display());
        no_picture()
    })
}

/// The thumbnail of a row that has none (or a peek's missing picture):
/// empty, and equal to every other one, so an unchanged row compares equal.
pub(super) fn no_picture() -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(0, 0))
}

/// The window's row for `d`. An id too big for the window's ints becomes -1,
/// which no action takes.
fn clip_row(d: &ClipRowData, thumb: Image) -> ClipRow {
    ClipRow {
        id: i32::try_from(d.id).unwrap_or(-1),
        kind: match d.kind {
            ClipKind::Text => 0,
            ClipKind::Files => 1,
            ClipKind::Link => 2,
            ClipKind::Image => 3,
        },
        label: d.label.as_str().into(),
        meta: d.meta.as_str().into(),
        missing: d.missing,
        thumb,
        pinned: d.pinned,
        files: d.files,
        web: d.web,
    }
}

impl PocketController {
    /// Routes the Clipboard view's rows and buttons, and the bottom switch,
    /// to the controller.
    pub(super) fn connect_clip(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        let c = with.clone();
        w.on_view_chosen(move |v| {
            if let Some(c) = c() {
                c.choose_view(v);
            }
        });
        let c = with.clone();
        w.on_clip_pressed(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                c.clip_pressed(id);
            }
        });
        let c = with.clone();
        w.on_clip_pocket(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id))
                && let Some(d) = c.clip.to_pocket(id)
            {
                c.add_dropped(d);
            }
        });
        let c = with.clone();
        w.on_clip_pin(move |id, pinned| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                c.clip.set_pinned(id, pinned);
            }
        });
        let c = with.clone();
        w.on_clip_filter_chosen(move |i| {
            if let (Some(c), Some(f)) = (c(), ClipFilter::from_index(i)) {
                c.clip_view.filter.set(f);
                c.refresh_clip();
            }
        });
        let c = with.clone();
        w.on_clip_peek(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                let open = c.clip_view.peek.get() != Some(id);
                c.clip_view.peek.set(open.then_some(id));
                c.refresh_clip();
            }
        });
        let c = with.clone();
        w.on_clip_open(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id))
                && let Some(url) = c.clip.link(id).filter(|u| is_web(u))
            {
                open_link(&url);
            }
        });
        let c = with.clone();
        w.on_clip_reveal(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                let files = c.clip.files(id);
                if !files.is_empty() {
                    c.platform.reveal_in_finder(&files);
                }
            }
        });
        let c = with.clone();
        w.on_clip_remove(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                let removed = c.clip.remove(&[id]);
                c.offer_clip_undo(removed);
            }
        });
        w.on_clip_clear(move || {
            if let Some(c) = with() {
                let removed = c.clip.clear();
                c.offer_clip_undo(removed);
            }
        });
    }

    /// A row was clicked: its entry goes back on the clipboard and the
    /// island closes on the next turn of the event loop, once the window
    /// has finished with the click (closing inside it would leave the row's
    /// hover behind). A row with nothing to put back does nothing.
    fn clip_pressed(&self, id: u64) {
        if !self.clip.put_back(id) {
            return;
        }
        self.pinned.set(false);
        let this = self.this.clone();
        slint::Timer::single_shot(Duration::ZERO, move || {
            if let Some(c) = this.upgrade()
                && c.warm.get()
                && !c.paused.get()
            {
                c.settle_now();
            }
        });
    }

    /// A tab or the Pocket's look toggle was clicked, for the window's view
    /// `v` (0 the kinds, 1 the Clipboard, 2 the list, 3 the Screenshots).
    /// Going to a shorter view leaves the pointer below the island, so it
    /// stays open a moment to be reached again.
    fn choose_view(&self, v: i32) {
        match v {
            1 => self.set_view(View::Clipboard),
            3 => self.set_view(View::Screenshots),
            2 => {
                self.set_view(View::Pocket);
                self.open_pocket_list(None);
            }
            _ if self.shown_view() == View::Pocket => {
                self.close_pocket_list();
            }
            _ => self.set_view(View::Pocket),
        }
        self.hold_open_briefly();
    }

    /// Keeps the island open a moment after a button that changed its
    /// shape, wherever the pointer now is, so it can be reached again.
    pub(super) fn hold_open_briefly(&self) {
        let until = Instant::now() + SWITCH_GRACE;
        if self.pin_until.get().is_none_or(|t| t < until) {
            self.pin_until.set(Some(until));
        }
    }

    /// Offers Undo for entries just taken out of the history.
    fn offer_clip_undo(&self, removed: Vec<ClipEntry>) {
        if !removed.is_empty() {
            self.offer_undo(Undo::Clip(removed));
        }
    }

    /// Shows view `v` (kept for the next opening) and refits the island.
    /// The Screenshots read their folder again each time they are shown:
    /// after an update, that is what asks for access.
    pub(super) fn set_view(&self, v: View) {
        if v == View::Screenshots {
            self.shots.touch();
        }
        if self.view.replace(v) != v {
            debug!("pocket view {v:?}");
            // Its rows are made only while it is in sight.
            match v {
                View::Clipboard => self.refresh_clip(),
                View::Screenshots => {}
                View::Pocket => self.refresh_list(),
            }
        }
        if v == View::Screenshots {
            self.refresh_shots();
        }
        self.window.set_view(self.window_view());
        self.fit_panel();
        self.run_undo_timer();
    }

    /// The view the island shows: the Pocket while the view asked for is
    /// off (the history, or the screenshots).
    pub(super) fn shown_view(&self) -> View {
        match self.view.get() {
            View::Clipboard if self.clip.enabled() => View::Clipboard,
            View::Screenshots if self.shots_on() => View::Screenshots,
            _ => View::Pocket,
        }
    }

    /// What the expanded island holds below the menu bar.
    pub(super) fn panel(&self) -> Panel {
        if self.drop_box.get() {
            return Panel::Drop;
        }
        match self.shown_view() {
            View::Clipboard => Panel::Clipboard,
            View::Screenshots => Panel::Screenshots,
            View::Pocket if self.list_shown() => Panel::List,
            View::Pocket => Panel::Kinds {
                groups: self.kinds.len(),
            },
        }
    }

    /// Closes the Clipboard view's peek; false if none was open.
    pub(super) fn close_clip_peek(&self) -> bool {
        self.clip_view.peek.take().is_some()
    }

    /// Pushes the history's size, the tabs and the view into the window,
    /// and the rows and counts while the Clipboard view is in sight. Rows
    /// cost a look at each listed file, so none are made for a hidden list:
    /// the island refreshes as it expands or switches to the view.
    pub(super) fn refresh_clip(&self) {
        let on = self.clip.enabled();
        if !on {
            self.clip_view.show(&self.window, &[]);
            self.show_clip_peek(None);
        } else if self.expanded() && self.shown_view() == View::Clipboard {
            let open = self
                .clip_view
                .show(&self.window, &self.clip.rows(now_unix()));
            self.show_clip_peek(open);
        }
        let w = &self.window;
        w.set_clip_count(i32::try_from(self.clip.len()).unwrap_or(i32::MAX));
        let (switch, clip_tab, shots_tab) = tabs(on, self.shots.access());
        w.set_show_switch(switch);
        w.set_show_clip_tab(clip_tab);
        w.set_show_shots_tab(shots_tab);
        w.set_view(self.window_view());
        w.set_clip_wing(on && !self.clip.is_empty() && self.pocket.borrow().is_empty());
    }

    /// Gives the window the open peek: the place of its row among those
    /// shown (`open`, `None` for no peek) and what it shows, read from the
    /// entry once while it stays open.
    fn show_clip_peek(&self, open: Option<usize>) {
        let id = open.and(self.clip_view.peek.get());
        let mut shown = self.clip_view.peek_shown.borrow_mut();
        if shown.as_ref().map(|p| p.id) != id {
            *shown = id.map(|id| {
                let peek = self.clip.peek(id).unwrap_or_default();
                PeekShown {
                    id,
                    text: peek.text,
                    picture: peek
                        .picture
                        .as_ref()
                        .and_then(to_image)
                        .unwrap_or_else(no_picture),
                    lines: peek
                        .lines
                        .into_iter()
                        .map(|(label, value)| PeekLine {
                            label: label.into(),
                            value: value.into(),
                        })
                        .collect(),
                }
            });
        }
        let w = &self.window;
        match shown.as_ref() {
            Some(p) => {
                w.set_clip_peek_text(p.text.as_str().into());
                w.set_clip_peek_picture(p.picture.clone());
                set_rows_in_place(&self.clip_view.peek_lines, p.lines.clone());
            }
            None => {
                w.set_clip_peek_text("".into());
                w.set_clip_peek_picture(no_picture());
                set_rows_in_place(&self.clip_view.peek_lines, Vec::new());
            }
        }
        w.set_clip_peek_index(open.map_or(-1, |i| i32::try_from(i).unwrap_or(-1)));
    }
}

/// Opens `url` (a web link) in the default browser.
fn open_link(url: &str) {
    #[cfg(target_os = "macos")]
    let opener: &[&str] = &["open"];
    #[cfg(target_os = "linux")]
    let opener: &[&str] = &["xdg-open"];
    #[cfg(windows)]
    let opener: &[&str] = &["rundll32", "url.dll,FileProtocolHandler"];
    match std::process::Command::new(opener[0])
        .args(&opener[1..])
        .arg(url)
        .spawn()
    {
        Ok(_) => debug!("clipboard: opened a link"),
        Err(e) => warn!("cannot open a link: {e}"),
    }
}

/// The window's number for `v`.
pub(super) fn view_index(v: View) -> i32 {
    match v {
        View::Pocket => 0,
        View::Clipboard => 1,
        View::Screenshots => 3,
    }
}

/// The bottom bar's tabs with the history on (`clip`) and the screenshots
/// at `shots`: whether it shows tabs at all (two views or more are on),
/// and whether it has the Clipboard's and the Screenshots' (the Pocket
/// always has one). The Screenshots are on once the feature is, unless the
/// folder was refused, which the Settings page says.
pub(super) fn tabs(clip: bool, shots: Access) -> (bool, bool, bool) {
    let shots = matches!(
        shots,
        Access::Granted | Access::Asking | Access::WaitForOpen
    );
    (clip || shots, clip, shots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(id: u64, kind: ClipKind) -> ClipRowData {
        ClipRowData {
            id,
            kind,
            label: "a".into(),
            age: "now".into(),
            meta: "Text · now".into(),
            files: false,
            web: false,
            missing: false,
            thumb: None,
            pinned: false,
        }
    }

    #[test]
    fn filters_admit_their_rows_and_count_them() {
        let pinned_link = ClipRowData {
            pinned: true,
            ..data(5, ClipKind::Link)
        };
        let rows = [
            data(1, ClipKind::Text),
            data(2, ClipKind::Text),
            data(3, ClipKind::Files),
            data(4, ClipKind::Image),
            pinned_link,
        ];
        let admitted = |f: ClipFilter| -> Vec<u64> {
            rows.iter().filter(|r| f.admits(r)).map(|r| r.id).collect()
        };
        assert_eq!(admitted(ClipFilter::All), [1, 2, 3, 4, 5]);
        assert_eq!(admitted(ClipFilter::Pinned), [5]);
        assert_eq!(admitted(ClipFilter::Text), [1, 2]);
        assert_eq!(admitted(ClipFilter::Links), [5]);
        assert_eq!(admitted(ClipFilter::Images), [4]);
        assert_eq!(admitted(ClipFilter::Files), [3]);
        // In the order the window's filter row lists them.
        assert_eq!(filter_counts(&rows), [5, 1, 2, 1, 1, 1]);
        for (i, f) in FILTERS.iter().enumerate() {
            assert_eq!(ClipFilter::from_index(i as i32), Some(*f));
            assert_eq!(f.index(), i as i32);
        }
        assert_eq!(ClipFilter::from_index(6), None);
        assert_eq!(ClipFilter::from_index(-1), None);
    }

    #[test]
    fn rows_carry_kind_and_id() {
        let kinds = [
            ClipKind::Text,
            ClipKind::Files,
            ClipKind::Link,
            ClipKind::Image,
        ];
        let rows: Vec<ClipRow> = kinds
            .iter()
            .zip(1..)
            .map(|(&k, id)| clip_row(&data(id, k), no_picture()))
            .collect();
        let got: Vec<(i32, i32)> = rows.iter().map(|r| (r.id, r.kind)).collect();
        assert_eq!(got, [(1, 0), (2, 1), (3, 2), (4, 3)]);
        // An id the window can't hold takes no action.
        let big = clip_row(&data(u64::MAX, ClipKind::Text), no_picture());
        assert_eq!(big.id, -1);
    }

    #[test]
    fn rows_without_thumbnails_compare_equal() {
        // So refreshing an unchanged list touches no row.
        let a = clip_row(&data(7, ClipKind::Text), no_picture());
        let b = clip_row(&data(7, ClipKind::Text), no_picture());
        assert_eq!(a, b);
        assert_eq!(a.thumb.size().width, 0);
    }

    #[test]
    fn rows_are_updated_in_place() {
        let model = VecModel::from(vec![1, 2, 3]);
        set_rows_in_place(&model, vec![1, 2, 3]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 2, 3]);
        // A new entry on top: every row moves down one, one is added.
        set_rows_in_place(&model, vec![0, 1, 2, 3]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [0, 1, 2, 3]);
        // One taken out of the middle.
        set_rows_in_place(&model, vec![0, 2, 3]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [0, 2, 3]);
        set_rows_in_place(&model, vec![]);
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn tabs_show_with_two_views_or_more() {
        // The Pocket alone: no tabs.
        assert_eq!(tabs(false, Access::Off), (false, false, false));
        assert_eq!(tabs(false, Access::Denied), (false, false, false));
        // With the history.
        assert_eq!(tabs(true, Access::Off), (true, true, false));
        assert_eq!(tabs(true, Access::Denied), (true, true, false));
        // With the screenshots, as soon as they are on and not refused.
        assert_eq!(tabs(false, Access::Granted), (true, false, true));
        assert_eq!(tabs(false, Access::Asking), (true, false, true));
        assert_eq!(tabs(false, Access::WaitForOpen), (true, false, true));
        assert_eq!(tabs(true, Access::Granted), (true, true, true));
    }

    #[test]
    fn missing_thumbnail_file_is_not_cached() {
        let mut thumbs = HashMap::new();
        let gone = Path::new("/nonexistent/vento-test/thumb.png");
        assert_eq!(thumb_for(&mut thumbs, 1, gone).size().width, 0);
        assert!(thumbs.is_empty());
    }
}
