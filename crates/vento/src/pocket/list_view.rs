//! The Pocket list: the Pocket's items one row each, shown by the bar's look
//! toggle (or by the settings, when the island opens) or on one kind from
//! its group, with a chip per kind to filter it. It is part of the Pocket
//! view: the bar shows the Pocket as current over it. A row's arrow opens a
//! peek under it: the picture, what a folder holds, or what the item is and
//! where it lives.

use std::cell::{Cell, RefCell};
use std::iter;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use log::{debug, warn};
use slint::{ComponentHandle, Image, Model, ModelRc, VecModel};
use vento_core::destinations::display_path;
use vento_core::i18n::{fill, tr};
use vento_core::pocket::{Pocket, PocketItem};
use vento_core::pocket_kind::{PocketKind, kind_of};
use vento_core::settings::PocketView;

use crate::ui::{Icons, PeekLine, PocketChip, PocketIsland, PocketRow};

use super::clip::age_label_now;
use super::clip_view::{no_picture, peek_picture, set_rows_in_place, view_index};
use super::kinds::{item_meta, shows_pictures};
use super::out::{Press, drag_ids};
use super::{PocketController, UNDO_WINDOW, View, display_name};

/// The window's number for the Pocket list (0, 1 and 3 are the Pocket's
/// cells, the Clipboard and the Screenshots).
const LIST_VIEW: i32 = 2;

/// One item as the list sees it: its id, its path, whether it is a folder,
/// and whether its file is missing.
pub(super) type ListItem = (u64, PathBuf, bool, bool);

/// Every item of `pocket` as the list and the stacks see it; `missing` is
/// the ids of those whose file is gone. A folder is told apart by a look at
/// its file.
pub(super) fn list_items(pocket: &Pocket, missing: &[u64]) -> Vec<ListItem> {
    pocket
        .items()
        .iter()
        .map(|i| {
            let gone = missing.contains(&i.id);
            (i.id, i.path.clone(), i.path.is_dir(), gone)
        })
        .collect()
}

/// The ids of the items the list shows under `filter` (every item for
/// `None`), in Pocket order. A missing item is listed like any other: its
/// row is shown dimmed.
pub(super) fn list_rows(items: &[ListItem], filter: Option<PocketKind>) -> Vec<u64> {
    items
        .iter()
        .filter(|(_, path, is_dir, _)| filter.is_none_or(|k| kind_of(path, *is_dir) == k))
        .map(|(id, ..)| *id)
        .collect()
}

/// The items a press on row `pressed` drags (see [`drag_ids`]), picked
/// only among the rows `filter` lists, so a selection never takes items the
/// chosen chip hides. Under All, as from the cells.
pub(super) fn listed_drag_ids(
    pressed: u64,
    items: &[ListItem],
    filter: Option<PocketKind>,
    selected: &[u64],
) -> Vec<u64> {
    let missing: Vec<u64> = items
        .iter()
        .filter(|(.., gone)| *gone)
        .map(|(id, ..)| *id)
        .collect();
    drag_ids(Some(pressed), &list_rows(items, filter), selected, &missing)
}

/// How many items of each kind the Pocket holds: only the kinds present, in
/// [`PocketKind::ALL`] order, as the list's chips show them.
pub(super) fn kind_counts(items: &[ListItem]) -> Vec<(PocketKind, usize)> {
    let kinds: Vec<PocketKind> = items
        .iter()
        .map(|(_, path, is_dir, _)| kind_of(path, *is_dir))
        .collect();
    PocketKind::ALL
        .iter()
        .map(|&k| (k, kinds.iter().filter(|&&of| of == k).count()))
        .filter(|&(_, n)| n > 0)
        .collect()
}

/// Names at most this many of a folder's items in its peek.
const FOLDER_NAMES: usize = 4;

/// The peek's lines for the folder at `path`: its name and how many items
/// it holds, then the first few names in name order (hidden files left
/// out), and how many more there are.
fn folder_lines(path: &Path) -> Vec<(String, String)> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| n.to_lowercase());
    let count = names.len().to_string();
    let head = fill(
        &tr("{name} · {n} items"),
        &[("name", &display_name(path)), ("n", &count)],
    );
    let more = names.len().saturating_sub(FOLDER_NAMES);
    iter::once(head)
        .chain(names.into_iter().take(FOLDER_NAMES))
        .chain((more > 0).then(|| fill(&tr("+{n} more"), &[("n", &more.to_string())])))
        .map(|line| (String::new(), line))
        .collect()
}

/// The window's number for chip `filter` among the chips of `kinds` (0 is
/// "All"); a kind not among them is "All".
fn filter_index(filter: Option<PocketKind>, kinds: &[PocketKind]) -> i32 {
    filter
        .and_then(|k| kinds.iter().position(|&of| of == k))
        .and_then(|i| i32::try_from(i + 1).ok())
        .unwrap_or(0)
}

/// The filter of the chip at window index `i` among the chips of `kinds`:
/// `None` for "All" (or an index no chip has).
fn filter_at(i: i32, kinds: &[PocketKind]) -> Option<PocketKind> {
    usize::try_from(i)
        .ok()
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| kinds.get(i))
        .copied()
}

/// Whether the list is open, its filter, its open peek, and the rows and
/// chips it shows.
pub(super) struct ListView {
    /// Opened by the look toggle or a group, closed by the toggle, its back
    /// control or a drag headed for the island; the island opens on the
    /// look the settings say.
    open: Cell<bool>,
    /// The kind listed, or every item for `None`.
    filter: Cell<Option<PocketKind>>,
    rows: Rc<VecModel<PocketRow>>,
    chips: Rc<VecModel<PocketChip>>,
    /// The kinds the chips after "All" stand for, in the order shown.
    kinds: RefCell<Vec<PocketKind>>,
    /// The item whose peek is open.
    peek: Cell<Option<u64>>,
    peek_lines: Rc<VecModel<PeekLine>>,
}

impl ListView {
    /// The list, open if `look` says so, its rows and chips bound to
    /// `window`.
    pub(super) fn new(window: &PocketIsland, look: PocketView) -> Self {
        let rows = Rc::new(VecModel::default());
        window.set_list_rows(ModelRc::from(rows.clone()));
        let chips = Rc::new(VecModel::default());
        window.set_list_chips(ModelRc::from(chips.clone()));
        let peek_lines = Rc::new(VecModel::default());
        window.set_list_peek_lines(ModelRc::from(peek_lines.clone()));
        Self {
            open: Cell::new(look == PocketView::List),
            filter: Cell::new(None),
            rows,
            chips,
            kinds: RefCell::new(Vec::new()),
            peek: Cell::new(None),
            peek_lines,
        }
    }

    /// Marks the row of item `id`, if listed, as selected or not.
    pub(super) fn set_selected(&self, id: u64, on: bool) {
        let Ok(id) = i32::try_from(id) else {
            return;
        };
        if let Some((i, mut row)) = self.rows.iter().enumerate().find(|(_, r)| r.id == id) {
            row.selected = on;
            self.rows.set_row_data(i, row);
        }
    }
}

impl PocketController {
    /// Routes the list's rows, their buttons, its chips and back control
    /// to the controller.
    pub(super) fn connect_list(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        // A row's press goes through the tiles' latch, by the item's place
        // in the Pocket, so it drags and selects as a tile does.
        let c = with.clone();
        w.on_list_pressed(move |id| {
            let Some(c) = c() else {
                return;
            };
            let index = u64::try_from(id).ok().and_then(|id| {
                let pocket = c.pocket.borrow();
                pocket.items().iter().position(|i| i.id == id)
            });
            if let Some(index) = index.and_then(|i| u32::try_from(i).ok()) {
                c.latch.press(Press::Cell(index));
            }
        });
        let c = with.clone();
        w.on_list_moved(move |_, dx, dy| {
            if let Some(c) = c() {
                c.moved(dx, dy);
            }
        });
        let c = with.clone();
        w.on_list_released(move |_| {
            if let Some(c) = c() {
                c.released();
            }
        });
        let c = with.clone();
        w.on_list_remove(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                c.remove_items(&[id], UNDO_WINDOW, true);
            }
        });
        let c = with.clone();
        w.on_list_peek(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id)) {
                c.toggle_list_peek(id);
            }
        });
        let c = with.clone();
        w.on_list_reveal(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id))
                && let Some(path) = c.item_path(id)
            {
                c.platform.reveal_in_finder(&[path]);
            }
        });
        let c = with.clone();
        w.on_list_copy_path(move |id| {
            if let (Some(c), Ok(id)) = (c(), u64::try_from(id))
                && let Some(path) = c.item_path(id)
            {
                c.copy_item_path(&path);
            }
        });
        let c = with.clone();
        w.on_list_filter_chosen(move |i| {
            if let Some(c) = c() {
                let filter = filter_at(i, &c.list.kinds.borrow());
                c.list.filter.set(filter);
                c.list.peek.set(None);
                c.refresh_list();
            }
        });
        w.on_list_back(move || {
            if let Some(c) = with() {
                c.close_pocket_list();
                c.hold_open_briefly();
            }
        });
    }

    /// Shows the Pocket list, listing the items of `filter` (every item for
    /// `None`). An empty Pocket has nothing to list.
    pub(super) fn open_pocket_list(&self, filter: Option<PocketKind>) {
        if self.pocket.borrow().is_empty() {
            return;
        }
        debug!("pocket list open on {filter:?}");
        self.list.open.set(true);
        self.list.filter.set(filter);
        self.list.peek.set(None);
        self.window.set_view(self.window_view());
        self.refresh();
    }

    /// Goes back from the Pocket list to the kinds; false if it wasn't open
    /// (and nothing was refreshed).
    pub(super) fn close_pocket_list(&self) -> bool {
        if !self.list.open.replace(false) {
            return false;
        }
        debug!("pocket list closed");
        self.list.filter.set(None);
        self.list.peek.set(None);
        self.window.set_view(self.window_view());
        self.refresh();
        true
    }

    /// Back to the look the settings say, on every item, with no peek open
    /// in either view: what the island opens on.
    pub(super) fn reset_look(&self) {
        let list = self.pocket_view.get() == PocketView::List;
        let shot_peek = self.shots_view.close_peek();
        let changed = self.list.open.replace(list) != list
            || self.list.filter.take().is_some()
            || self.list.peek.take().is_some()
            || self.close_clip_peek()
            || shot_peek;
        if changed {
            debug!("pocket look reset to {:?}", self.pocket_view.get());
            self.window.set_view(self.window_view());
            self.refresh();
        }
    }

    /// The kind the list shows while it is in sight on one kind.
    pub(super) fn list_kind(&self) -> Option<PocketKind> {
        self.list.filter.get().filter(|_| self.list_shown())
    }

    /// Opens the peek of the list's row of item `id`, or closes it when it
    /// is the one open.
    fn toggle_list_peek(&self, id: u64) {
        let open = self.list.peek.get() != Some(id);
        self.list.peek.set(open.then_some(id));
        self.refresh_list();
    }

    /// The path of item `id`, while the Pocket holds it.
    pub(super) fn item_path(&self, id: u64) -> Option<PathBuf> {
        self.pocket.borrow().get(id).map(|i| i.path.clone())
    }

    /// Puts `path` on the clipboard as text, in the format the Copy path
    /// action uses.
    fn copy_item_path(&self, path: &Path) {
        let format = self.settings.borrow().copy_path_format;
        let text = vento_core::paths::format_paths(&[path.to_path_buf()], format);
        if let Err(e) = self.platform.copy_text(&text) {
            warn!("cannot copy the path of a pocket item: {e}");
            let text = fill(
                &tr("Couldn't copy: {reason}"),
                &[("reason", &e.to_string())],
            );
            self.toast.show(&text, None, self.toast_at());
        }
    }

    /// The peek of `item`, its file missing when `gone`: a picture's
    /// picture, a folder's first few names, or what the item is, how big,
    /// where it lives and when it came in.
    fn item_peek(&self, item: &PocketItem, gone: bool) -> (Image, Vec<(String, String)>) {
        let is_dir = item.path.is_dir();
        let kind = kind_of(&item.path, is_dir);
        if !gone
            && shows_pictures(kind)
            && let Some(picture) = peek_picture(&item.path)
        {
            return (picture, Vec::new());
        }
        if !gone && is_dir {
            return (no_picture(), folder_lines(&item.path));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let place = item
            .path
            .parent()
            .map(|p| display_path(p, home.as_deref()))
            .unwrap_or_default();
        let what = self
            .platform
            .kind_description(&item.path)
            .unwrap_or_else(|| kind.label());
        let lines = vec![
            (tr("Kind"), what),
            (tr("Size"), item_meta(&item.path, is_dir, gone)),
            (tr("Where"), place),
            (tr("Added"), age_label_now(item.added_unix)),
        ];
        (no_picture(), lines)
    }

    /// The items a press on the list's row of item `pressed` drags: see
    /// [`listed_drag_ids`].
    pub(super) fn list_drag_ids(&self, pressed: u64) -> Vec<u64> {
        let pocket = self.pocket.borrow();
        let items = list_items(&pocket, &pocket.missing());
        listed_drag_ids(
            pressed,
            &items,
            self.list.filter.get(),
            &self.selected.borrow(),
        )
    }

    /// Whether the island shows the Pocket list (it stays open, unseen,
    /// while the Clipboard or the Screenshots view is shown).
    pub(super) fn list_shown(&self) -> bool {
        self.list.open.get() && self.shown_view() == View::Pocket
    }

    /// The window's number for what the island shows.
    pub(super) fn window_view(&self) -> i32 {
        if self.list_shown() {
            LIST_VIEW
        } else {
            view_index(self.shown_view())
        }
    }

    /// Pushes the list's chips and rows into the window while it is in
    /// sight (rows cost a look at each file, so none are made for a hidden
    /// list), and its open peek. A filter whose kind is gone falls back to
    /// All; a peek whose item is no longer listed closes. Only rows that
    /// changed are touched, so the row under the pointer keeps its hover.
    pub(super) fn refresh_list(&self) {
        if !self.expanded() || !self.list_shown() {
            return;
        }
        let pocket = self.pocket.borrow();
        let missing = pocket.missing();
        let items = list_items(&pocket, &missing);
        let counts = kind_counts(&items);
        let kinds: Vec<PocketKind> = counts.iter().map(|&(k, _)| k).collect();
        let filter = self.list.filter.get().filter(|k| kinds.contains(k));
        self.list.filter.set(filter);
        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        let all = PocketChip {
            label: tr("All").into(),
            count: count(items.len()),
        };
        let chips = iter::once(all)
            .chain(counts.iter().map(|&(k, n)| PocketChip {
                label: k.label().into(),
                count: count(n),
            }))
            .collect();
        set_rows_in_place(&self.list.chips, chips);
        self.window.set_list_filter(filter_index(filter, &kinds));
        *self.list.kinds.borrow_mut() = kinds;
        let selected = self.selected.borrow();
        let mut icons = self.icons.borrow_mut();
        let fallback = self.window.global::<Icons>().get_file();
        let listed = list_rows(&items, filter);
        let rows = listed
            .iter()
            .filter_map(|&id| pocket.get(id))
            .map(|item| {
                let gone = missing.contains(&item.id);
                let is_dir = item.path.is_dir();
                let kind = kind_of(&item.path, is_dir);
                let thumb = (shows_pictures(kind) && !gone)
                    .then(|| self.kinds.thumb(item))
                    .flatten();
                let place = item.path.parent().map(display_name).unwrap_or_default();
                let size = item_meta(&item.path, is_dir, gone);
                PocketRow {
                    // An id too big for the window's ints takes no action.
                    id: i32::try_from(item.id).unwrap_or(-1),
                    name: display_name(&item.path).into(),
                    meta: [kind.label(), size, place]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ")
                        .into(),
                    thumb: thumb.is_some(),
                    icon: thumb
                        .unwrap_or_else(|| self.item_icon(&mut icons, item, gone, &fallback)),
                    missing: gone,
                    selected: selected.contains(&item.id),
                }
            })
            .collect();
        set_rows_in_place(&self.list.rows, rows);
        let peek = self
            .list
            .peek
            .get()
            .and_then(|id| listed.iter().position(|&l| l == id).map(|i| (id, i)));
        self.list.peek.set(peek.map(|(id, _)| id));
        let (picture, lines) = peek.and_then(|(id, _)| pocket.get(id)).map_or_else(
            || (no_picture(), Vec::new()),
            |item| self.item_peek(item, missing.contains(&item.id)),
        );
        self.window.set_list_peek_picture(picture);
        set_rows_in_place(
            &self.list.peek_lines,
            lines
                .into_iter()
                .map(|(label, value)| PeekLine {
                    label: label.into(),
                    value: value.into(),
                })
                .collect(),
        );
        self.window
            .set_list_peek_index(peek.map_or(-1, |(_, i)| i32::try_from(i).unwrap_or(-1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, path: &str) -> ListItem {
        (id, PathBuf::from(path), false, false)
    }

    fn folder(id: u64, path: &str) -> ListItem {
        (id, PathBuf::from(path), true, false)
    }

    fn sample() -> Vec<ListItem> {
        vec![
            item(1, "/tmp/vento-test/notes.pdf"),
            item(2, "/tmp/vento-test/beach.jpg"),
            folder(3, "/tmp/vento-test/Photos"),
            item(4, "/tmp/vento-test/Screenshot.PNG"),
            item(5, "/tmp/vento-test/README"),
        ]
    }

    #[test]
    fn every_item_is_listed_in_pocket_order_without_a_filter() {
        assert_eq!(list_rows(&sample(), None), [1, 2, 3, 4, 5]);
        assert!(list_rows(&[], None).is_empty());
    }

    #[test]
    fn a_kind_filter_lists_only_that_kind() {
        assert_eq!(list_rows(&sample(), Some(PocketKind::Image)), [2, 4]);
        assert_eq!(list_rows(&sample(), Some(PocketKind::Folder)), [3]);
        assert!(list_rows(&sample(), Some(PocketKind::Audio)).is_empty());
    }

    #[test]
    fn counts_list_the_kinds_present_in_their_fixed_order() {
        assert_eq!(
            kind_counts(&sample()),
            [
                (PocketKind::Image, 2),
                (PocketKind::Document, 1),
                (PocketKind::Folder, 1),
                (PocketKind::Other, 1),
            ]
        );
        assert!(kind_counts(&[]).is_empty());
    }

    #[test]
    fn a_missing_item_is_still_listed() {
        let mut items = sample();
        items[1].3 = true;
        assert_eq!(list_rows(&items, None), [1, 2, 3, 4, 5]);
        assert_eq!(list_rows(&items, Some(PocketKind::Image)), [2, 4]);
        assert_eq!(kind_counts(&items)[0], (PocketKind::Image, 2));
    }

    #[test]
    fn a_selection_dragged_from_a_filtered_list_takes_only_the_rows_shown() {
        let items = sample();
        // Under Images, a selected image drags the selected images alone,
        // not the selected PDF the chip hides.
        let image = Some(PocketKind::Image);
        assert_eq!(listed_drag_ids(4, &items, image, &[1, 4, 2]), [2, 4]);
        // A pressed row outside the selection drags itself alone.
        assert_eq!(listed_drag_ids(2, &items, image, &[1, 4]), [2]);
        // Under All, the whole selection, as from the cells.
        assert_eq!(listed_drag_ids(4, &items, None, &[1, 4, 2]), [1, 2, 4]);
        // A missing item is listed but never dragged.
        let mut gone = sample();
        gone[1].3 = true;
        assert_eq!(listed_drag_ids(4, &gone, image, &[2, 4]), [4]);
    }

    #[test]
    fn a_folder_peek_names_its_first_items_and_counts_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.txt", "A.txt", ".hidden", "c.txt", "d.txt", "e.txt"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let lines: Vec<String> = folder_lines(dir.path())
            .into_iter()
            .map(|(label, value)| {
                assert!(label.is_empty());
                value
            })
            .collect();
        assert!(lines[0].ends_with(" · 5 items"), "{lines:?}");
        assert_eq!(lines[1..5], ["A.txt", "b.txt", "c.txt", "d.txt"]);
        assert_eq!(lines[5], "+1 more");
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn chips_map_to_filters_after_all() {
        let kinds = [PocketKind::Image, PocketKind::Folder];
        assert_eq!(filter_at(0, &kinds), None);
        assert_eq!(filter_at(1, &kinds), Some(PocketKind::Image));
        assert_eq!(filter_at(2, &kinds), Some(PocketKind::Folder));
        // An index no chip has lists everything.
        assert_eq!(filter_at(3, &kinds), None);
        assert_eq!(filter_at(-1, &kinds), None);
        assert_eq!(filter_index(None, &kinds), 0);
        assert_eq!(filter_index(Some(PocketKind::Folder), &kinds), 2);
        // A kind with no chip (its last item went) shows "All" chosen.
        assert_eq!(filter_index(Some(PocketKind::Audio), &kinds), 0);
    }
}
