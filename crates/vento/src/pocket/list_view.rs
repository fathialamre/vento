//! The Pocket list: every item the Pocket holds as one row each, opened from
//! the "All N" button of a full Pocket, with a chip per kind to filter it.
//! It is part of the Pocket view: the bottom switch shows the Pocket as
//! current over it, and it is kept open (in memory) while the Clipboard view
//! is shown, and across a collapse while the Pocket still holds more than
//! eight items (so dragging rows out one by one needs no new click on
//! "All N"). Otherwise a collapse closes it, so the island opens on its
//! cells, as it always has with eight items or fewer.

use std::cell::{Cell, RefCell};
use std::iter;
use std::path::PathBuf;
use std::rc::Rc;

use log::debug;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use vento_core::i18n::tr;
use vento_core::pocket::Pocket;
use vento_core::pocket_kind::{PocketKind, kind_of};
use vento_core::pocket_zone::Presence;

use crate::ui::{Icons, PocketChip, PocketIsland, PocketRow};

use super::clip_view::{set_rows_in_place, view_index};
use super::geometry::MAX_CELLS;
use super::out::{Press, drag_ids};
use super::{PocketController, UNDO_WINDOW, View, display_name};

/// The window's number for the Pocket list (0 and 1 are the Pocket's cells
/// and the Clipboard).
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

/// Whether an open list stays open in `presence` with `count` items in the
/// Pocket: never once it is empty; while the island is expanded, whatever
/// the count; collapsed or hidden, only while the Pocket still holds more
/// than [`MAX_CELLS`], so the next opening returns to it. Otherwise that
/// opening shows the cells, as a Pocket of eight items or fewer always has.
pub(super) fn list_stays_open(presence: Presence, count: usize) -> bool {
    count > 0 && (presence == Presence::Expanded || count > MAX_CELLS)
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

/// Whether the list is open, its filter, and the rows and chips it shows.
pub(super) struct ListView {
    /// Opened by "All N", closed by its back control, a new layout, a drag
    /// headed for the island, the island collapsing with eight items or
    /// fewer left, or an empty Pocket.
    open: Cell<bool>,
    /// The kind listed, or every item for `None`.
    filter: Cell<Option<PocketKind>>,
    rows: Rc<VecModel<PocketRow>>,
    chips: Rc<VecModel<PocketChip>>,
    /// The kinds the chips after "All" stand for, in the order shown.
    kinds: RefCell<Vec<PocketKind>>,
}

impl ListView {
    /// A closed list, its rows and chips bound to `window`.
    pub(super) fn new(window: &PocketIsland) -> Self {
        let rows = Rc::new(VecModel::default());
        window.set_list_rows(ModelRc::from(rows.clone()));
        let chips = Rc::new(VecModel::default());
        window.set_list_chips(ModelRc::from(chips.clone()));
        Self {
            open: Cell::new(false),
            filter: Cell::new(None),
            rows,
            chips,
            kinds: RefCell::new(Vec::new()),
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
    /// Routes "All N" and the list's rows, chips and back control to the
    /// controller.
    pub(super) fn connect_list(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        let c = with.clone();
        w.on_list_open(move || {
            if let Some(c) = c() {
                c.open_pocket_list(None);
                c.hold_open_briefly();
            }
        });
        // A row's press goes through the cells' latch, by the item's place
        // in the Pocket, so it drags and selects as a cell does.
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
        w.on_list_filter_chosen(move |i| {
            if let Some(c) = c() {
                let filter = filter_at(i, &c.list.kinds.borrow());
                c.list.filter.set(filter);
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
        self.refresh();
    }

    /// Goes back from the Pocket list to the cells; false if it wasn't open
    /// (and nothing was refreshed).
    pub(super) fn close_pocket_list(&self) -> bool {
        if !self.list.open.replace(false) {
            return false;
        }
        debug!("pocket list closed");
        self.list.filter.set(None);
        self.refresh();
        true
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
    /// while the Clipboard view is shown).
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

    /// Closes the list once it no longer stays open ([`list_stays_open`]:
    /// the Pocket emptied, or a drop that removed items left eight or fewer
    /// while the island was collapsed), and pushes its chips and rows into
    /// the window while it is in sight (rows cost a look at each file, so
    /// none are made for a hidden list). A filter whose kind is gone falls
    /// back to All. Only rows that changed are touched, so the row under the
    /// pointer keeps its hover.
    pub(super) fn refresh_list(&self) {
        let presence = self.island.borrow().presence();
        if self.list.open.get() && !list_stays_open(presence, self.item_count()) {
            debug!(
                "pocket list closed: {} item(s), {presence:?}",
                self.item_count()
            );
            self.list.open.set(false);
            self.list.filter.set(None);
        }
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
        let rows = list_rows(&items, filter)
            .into_iter()
            .filter_map(|id| pocket.get(id))
            .map(|item| {
                let gone = missing.contains(&item.id);
                PocketRow {
                    // An id too big for the window's ints takes no action.
                    id: i32::try_from(item.id).unwrap_or(-1),
                    name: display_name(&item.path).into(),
                    icon: self.item_icon(&mut icons, item, gone, &fallback),
                    missing: gone,
                    selected: selected.contains(&item.id),
                }
            })
            .collect();
        set_rows_in_place(&self.list.rows, rows);
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
    fn a_collapse_keeps_the_list_only_while_the_pocket_is_full() {
        // Expanded, it stays whatever the count (removing down to eight
        // keeps it in sight).
        assert!(list_stays_open(Presence::Expanded, 12));
        assert!(list_stays_open(Presence::Expanded, 5));
        // Collapsed with more than eight: the next opening returns to it.
        assert!(list_stays_open(Presence::Collapsed, 9));
        assert!(list_stays_open(Presence::Collapsed, 40));
        // Eight or fewer: the next opening shows the cells, as today.
        assert!(!list_stays_open(Presence::Collapsed, 8));
        assert!(!list_stays_open(Presence::Collapsed, 1));
        // Empty: closed, even while a clear's Undo holds the island open.
        assert!(!list_stays_open(Presence::Hidden, 0));
        assert!(!list_stays_open(Presence::Collapsed, 0));
        assert!(!list_stays_open(Presence::Expanded, 0));
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
