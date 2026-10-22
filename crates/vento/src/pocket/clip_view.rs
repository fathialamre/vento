//! The island's Clipboard view: the history's rows as the window draws them,
//! what a row and its buttons do, and the bottom switch between the views.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use log::debug;
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use vento_core::clip_history::ClipEntry;

use crate::ui::{ClipRow, PocketIsland};

use super::clip::{ClipKind, ClipRowData, now_unix};
use super::geometry::Panel;
use super::{PocketController, UNDO_WINDOW, Undo, View};

/// How long the island stays open after a switch button, wherever the
/// pointer is.
const SWITCH_GRACE: Duration = Duration::from_secs(1);

/// The rows the Clipboard view shows, and the image thumbnails already made
/// for them.
pub(super) struct ClipView {
    rows: Rc<VecModel<ClipRow>>,
    /// Thumbnails by entry id, made once; an empty image when the picture
    /// could not be read.
    thumbs: RefCell<HashMap<u64, Image>>,
}

impl ClipView {
    /// The view's rows, bound to `window`'s list.
    pub(super) fn new(window: &PocketIsland) -> Self {
        let rows = Rc::new(VecModel::default());
        window.set_clip_rows(ModelRc::from(rows.clone()));
        Self {
            rows,
            thumbs: RefCell::new(HashMap::new()),
        }
    }

    /// Shows `data`, touching only the rows that changed so hover stays
    /// where it is. Thumbnails of entries that left are dropped.
    pub(super) fn show(&self, data: &[ClipRowData]) {
        let mut thumbs = self.thumbs.borrow_mut();
        thumbs.retain(|id, _| data.iter().any(|d| d.id == *id));
        let rows = data
            .iter()
            .map(|d| {
                let thumb = match &d.thumb {
                    Some(path) => thumb_for(&mut thumbs, d.id, path),
                    None => no_thumb(),
                };
                clip_row(d, thumb)
            })
            .collect();
        set_rows_in_place(&self.rows, rows);
    }
}

/// Makes `model` hold `rows` without rebuilding it: rows are added or
/// dropped at the end and the others rewritten only where they changed.
/// Each row the window draws stays where it is, so the one under the
/// pointer keeps its hover when a copy pushes a new entry in on top.
fn set_rows_in_place<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
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
        return no_thumb();
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
        no_thumb()
    })
}

/// The thumbnail of a row that has none: empty, and equal to every other
/// one, so an unchanged row compares equal.
fn no_thumb() -> Image {
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
        age: d.age.as_str().into(),
        missing: d.missing,
        thumb,
        pinned: d.pinned,
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
                c.choose_view(if v == 1 {
                    View::Clipboard
                } else {
                    View::Pocket
                });
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
                let file = c.add_dropped(d);
                if file.is_some() {
                    c.clip.set_pocket_file(id, file);
                    c.refresh();
                }
            }
        });
        let c = with.clone();
        w.on_clip_unpin(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                c.unpin(id);
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

    /// A row's pinned mark was clicked: what the Pocket holds for the entry
    /// leaves it, the way the Pocket's own x takes an item out (vento's
    /// saved file goes to the Trash once the Undo window has passed). The
    /// entry stays in the history.
    fn unpin(&self, id: u64) {
        let ids: Vec<u64> = {
            let pocket = self.pocket.borrow();
            self.clip
                .pocket_paths(id)
                .iter()
                .flat_map(|p| pocket.holding(p))
                .collect()
        };
        self.clip.set_pocket_file(id, None);
        if ids.is_empty() {
            self.refresh();
        } else {
            self.remove_items(&ids, UNDO_WINDOW, false);
        }
    }

    /// A switch button was clicked. Going to the shorter Pocket view leaves
    /// the pointer below the island, so it stays open a moment to be
    /// reached again.
    fn choose_view(&self, v: View) {
        self.set_view(v);
        let until = Instant::now() + SWITCH_GRACE;
        if self.pin_until.get().is_none_or(|t| t < until) {
            self.pin_until.set(Some(until));
        }
    }

    /// Offers Undo for entries just taken out of the history.
    fn offer_clip_undo(&self, removed: Vec<ClipEntry>) {
        if !removed.is_empty() {
            self.offer_undo(Undo::Clip(removed), Vec::new());
        }
    }

    /// Shows view `v` (kept for the next opening) and refits the island.
    pub(super) fn set_view(&self, v: View) {
        if self.view.replace(v) != v {
            debug!("pocket view {v:?}");
        }
        self.window.set_view(view_index(self.shown_view()));
        self.fit_panel();
    }

    /// The view the island shows: the Pocket while the history is off.
    pub(super) fn shown_view(&self) -> View {
        if self.clip.enabled() {
            self.view.get()
        } else {
            View::Pocket
        }
    }

    /// What the expanded island holds below the menu bar.
    pub(super) fn panel(&self) -> Panel {
        match self.shown_view() {
            View::Clipboard => Panel::Clipboard,
            View::Pocket => Panel::Pocket {
                switch: self.clip.enabled(),
            },
        }
    }

    /// Pushes the history's rows and counts, the switch and the view into
    /// the window.
    pub(super) fn refresh_clip(&self) {
        let on = self.clip.enabled();
        let rows = if on {
            let pocket = self.pocket.borrow();
            self.clip.rows(now_unix(), |p| pocket.contains(p))
        } else {
            Vec::new()
        };
        self.clip_view.show(&rows);
        let w = &self.window;
        w.set_clip_count(i32::try_from(self.clip.len()).unwrap_or(i32::MAX));
        w.set_show_switch(on);
        w.set_view(view_index(self.shown_view()));
        w.set_clip_wing(on && !self.clip.is_empty() && self.pocket.borrow().is_empty());
    }
}

/// The window's number for `v`.
fn view_index(v: View) -> i32 {
    match v {
        View::Pocket => 0,
        View::Clipboard => 1,
    }
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
            missing: false,
            thumb: None,
            pinned: false,
        }
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
            .map(|(&k, id)| clip_row(&data(id, k), no_thumb()))
            .collect();
        let got: Vec<(i32, i32)> = rows.iter().map(|r| (r.id, r.kind)).collect();
        assert_eq!(got, [(1, 0), (2, 1), (3, 2), (4, 3)]);
        // An id the window can't hold takes no action.
        let big = clip_row(&data(u64::MAX, ClipKind::Text), no_thumb());
        assert_eq!(big.id, -1);
    }

    #[test]
    fn rows_without_thumbnails_compare_equal() {
        // So refreshing an unchanged list touches no row.
        let a = clip_row(&data(7, ClipKind::Text), no_thumb());
        let b = clip_row(&data(7, ClipKind::Text), no_thumb());
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
    fn missing_thumbnail_file_is_not_cached() {
        let mut thumbs = HashMap::new();
        let gone = Path::new("/nonexistent/vento-test/thumb.png");
        assert_eq!(thumb_for(&mut thumbs, 1, gone).size().width, 0);
        assert!(thumbs.is_empty());
    }
}
