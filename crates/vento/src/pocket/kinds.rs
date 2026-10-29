//! The Pocket view: the items grouped by kind, in [`PocketKind::ALL`] order,
//! each group a header and a row of tiles. Pictures get narrow tiles that
//! show the picture itself; every other kind gets wide tiles with a name. A
//! group's name and its "+N" tile open the Pocket list on that kind; its
//! "Drag N" drags the group's items out.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use log::debug;
use slint::{ComponentHandle, Image, Model, ModelRc, VecModel};
use vento_core::i18n::tr;
use vento_core::info::human_size;
use vento_core::pocket::PocketItem;
use vento_core::pocket_kind::{PocketKind, kind_of};

use crate::ui::{Icons, PocketGroup, PocketIsland, PocketTile};

use super::clip_view::set_rows_in_place;
use super::list_view::{ListItem, list_items};
use super::out::Press;
use super::{PocketController, display_name, to_image};

/// The view's side padding, as `pocket.slint` draws it.
const SIDE_PAD: f64 = 16.0;
/// A tile's width: a narrow picture tile, or a wide one with a name.
const NARROW_W: f64 = 92.0;
const WIDE_W: f64 = 150.0;
/// The gap between tiles, and the "+N" tile after them.
const TILE_GAP: f64 = 6.0;
const MORE_W: f64 = 56.0;
/// A picture tile's height in pixels at 2x: its picture is cut to fill it.
const TILE_THUMB_PX: u32 = 116;

/// The items a drag of `kind`'s group takes: those of that kind whose file
/// is still there, in Pocket order. The selection plays no part.
pub(super) fn kind_ids(items: &[ListItem], kind: PocketKind) -> Vec<u64> {
    items
        .iter()
        .filter(|(_, path, is_dir, missing)| !missing && kind_of(path, *is_dir) == kind)
        .map(|(id, ..)| *id)
        .collect()
}

/// The window's number for `kind`: its place in [`PocketKind::ALL`].
pub(super) fn kind_index(kind: PocketKind) -> i32 {
    PocketKind::ALL
        .iter()
        .position(|&k| k == kind)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(-1)
}

/// The kind at window number `i`, or `None` for a number no kind has.
pub(super) fn kind_at(i: i32) -> Option<PocketKind> {
    usize::try_from(i)
        .ok()
        .and_then(|i| PocketKind::ALL.get(i))
        .copied()
}

/// Whether `kind` gets the narrow tiles that show the picture itself.
pub(super) fn shows_pictures(kind: PocketKind) -> bool {
    kind == PocketKind::Image
}

/// How many of `count` tiles `tile_w` wide fit a row `room` wide: every one
/// when they all fit, otherwise as many as fit beside the "+N" tile (at
/// least one).
fn tiles_shown(count: usize, tile_w: f64, room: f64) -> usize {
    let fit = |room: f64| ((room + TILE_GAP) / (tile_w + TILE_GAP)).floor().max(0.0) as usize;
    if count <= fit(room) {
        count
    } else {
        fit(room - MORE_W - TILE_GAP).clamp(1, count)
    }
}

/// One kind's group: the places of its items in the Pocket, in Pocket order,
/// and how many of them get a tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub(super) kind: PocketKind,
    pub(super) indices: Vec<usize>,
    pub(super) shown: usize,
}

/// The groups of `items` in a view `panel_w` wide: one per kind present, in
/// [`PocketKind::ALL`] order.
pub(super) fn groups(items: &[ListItem], panel_w: f64) -> Vec<Group> {
    let room = panel_w - 2.0 * SIDE_PAD;
    PocketKind::ALL
        .iter()
        .filter_map(|&kind| {
            let indices: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, (_, path, is_dir, _))| kind_of(path, *is_dir) == kind)
                .map(|(i, _)| i)
                .collect();
            if indices.is_empty() {
                return None;
            }
            let tile_w = if shows_pictures(kind) {
                NARROW_W
            } else {
                WIDE_W
            };
            let shown = tiles_shown(indices.len(), tile_w, room);
            Some(Group {
                kind,
                indices,
                shown,
            })
        })
        .collect()
}

/// The line under a wide tile's name, and a list row's: what the item is,
/// read from its file (`is_dir`, and missing when `gone`).
pub(super) fn item_meta(path: &Path, is_dir: bool, gone: bool) -> String {
    if gone {
        return tr("Missing");
    }
    if is_dir {
        return tr("Folder");
    }
    std::fs::metadata(path).map_or_else(|_| String::new(), |m| human_size(m.len()))
}

/// The Pocket view's groups and tiles as the window draws them, and the
/// picture thumbnails made for them.
pub(super) struct Kinds {
    groups: Rc<VecModel<PocketGroup>>,
    tiles: Rc<VecModel<PocketTile>>,
    /// Picture thumbnails by item id, made once; `None` when the picture
    /// could not be read, so it is not tried again.
    thumbs: RefCell<HashMap<u64, Option<Image>>>,
}

impl Kinds {
    /// No groups yet, their models bound to `window`.
    pub(super) fn new(window: &PocketIsland) -> Self {
        let groups = Rc::new(VecModel::default());
        window.set_groups(ModelRc::from(groups.clone()));
        let tiles = Rc::new(VecModel::default());
        window.set_tiles(ModelRc::from(tiles.clone()));
        Self {
            groups,
            tiles,
            thumbs: RefCell::new(HashMap::new()),
        }
    }

    /// How many groups are shown.
    pub(super) fn len(&self) -> usize {
        self.groups.row_count()
    }

    /// Marks the tile of the item at `index` in the Pocket, if it has one,
    /// as selected or not.
    pub(super) fn set_selected(&self, index: usize, on: bool) {
        let Ok(index) = i32::try_from(index) else {
            return;
        };
        let found = self
            .tiles
            .iter()
            .enumerate()
            .find(|(_, t)| t.index == index);
        if let Some((i, mut tile)) = found {
            tile.selected = on;
            self.tiles.set_row_data(i, tile);
        }
    }

    /// The thumbnail of picture `item`, made the first time; `None` when it
    /// can't be read.
    pub(super) fn thumb(&self, item: &PocketItem) -> Option<Image> {
        self.thumbs
            .borrow_mut()
            .entry(item.id)
            .or_insert_with(|| {
                vento_platform::picture_thumbnail(&item.path, TILE_THUMB_PX, 4)
                    .as_ref()
                    .and_then(to_image)
            })
            .clone()
    }

    /// Forgets the thumbnails of items no longer in the Pocket.
    pub(super) fn retain(&self, keep: impl Fn(u64) -> bool) {
        self.thumbs.borrow_mut().retain(|&id, _| keep(id));
    }
}

impl PocketController {
    /// Routes the groups' names, "+N" tiles and "Drag N" to the controller;
    /// "Drag N" goes through the tiles' latch, so it drags as a tile does.
    pub(super) fn connect_kinds(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        let c = with.clone();
        w.on_kind_open(move |k| {
            if let (Some(c), Some(kind)) = (c(), kind_at(k)) {
                c.open_pocket_list(Some(kind));
                c.hold_open_briefly();
            }
        });
        let c = with.clone();
        w.on_kind_pressed(move |k| {
            if let (Some(c), Some(kind)) = (c(), kind_at(k)) {
                c.latch.press(Press::Kind(kind));
            }
        });
        let c = with.clone();
        w.on_kind_moved(move |_, dx, dy| {
            if let Some(c) = c() {
                c.moved(dx, dy);
            }
        });
        w.on_kind_released(move |_| {
            if let Some(c) = with() {
                c.released();
            }
        });
    }

    /// Pushes the groups and their tiles into the window. They cost a look
    /// at each file, so they are made only while the island is expanded (a
    /// hidden or collapsed island shows none); [`present`](Self::present)
    /// refreshes as it expands. Only rows that changed are touched, so the
    /// tile under the pointer keeps its hover.
    pub(super) fn refresh_kinds(&self) {
        if !self.expanded() {
            return;
        }
        let pocket = self.pocket.borrow();
        let missing = pocket.missing();
        let items = list_items(&pocket, &missing);
        self.kinds.retain(|id| pocket.get(id).is_some());
        let panel_w = f64::from(self.window.get_panel_w());
        let groups = groups(&items, panel_w);
        let selected = self.selected.borrow();
        let mut icons = self.icons.borrow_mut();
        let fallback = self.window.global::<Icons>().get_file();
        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        let mut tiles = Vec::new();
        for (g, group) in groups.iter().enumerate() {
            let pictures = shows_pictures(group.kind);
            for (slot, &index) in group.indices.iter().take(group.shown).enumerate() {
                let item = &pocket.items()[index];
                let (_, path, is_dir, gone) = &items[index];
                let thumb = (pictures && !gone)
                    .then(|| self.kinds.thumb(item))
                    .flatten();
                tiles.push(PocketTile {
                    index: count(index),
                    group: count(g),
                    slot: count(slot),
                    name: display_name(path).into(),
                    meta: item_meta(path, *is_dir, *gone).into(),
                    thumb: thumb.is_some(),
                    icon: thumb
                        .unwrap_or_else(|| self.item_icon(&mut icons, item, *gone, &fallback)),
                    missing: *gone,
                    selected: selected.contains(&item.id),
                    wide: !pictures,
                });
            }
        }
        let rows = groups
            .iter()
            .map(|group| PocketGroup {
                kind: kind_index(group.kind),
                label: group.kind.label().into(),
                count: count(group.indices.len()),
                wide: !shows_pictures(group.kind),
                shown: count(group.shown),
            })
            .collect();
        set_rows_in_place(&self.kinds.groups, rows);
        set_rows_in_place(&self.kinds.tiles, tiles);
    }

    /// A press on `kind`'s "Drag N" moved far enough: drags its items out.
    pub(super) fn drag_kind(&self, kind: PocketKind) {
        let ids = {
            let pocket = self.pocket.borrow();
            kind_ids(&list_items(&pocket, &pocket.missing()), kind)
        };
        debug!("pocket kind {kind:?}: {} item(s) to drag", ids.len());
        self.start_drag_out(ids);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn item(id: u64, path: &str, missing: bool) -> ListItem {
        (id, PathBuf::from(path), false, missing)
    }

    #[test]
    fn a_group_drags_its_present_items_in_pocket_order() {
        let items = vec![
            item(1, "/tmp/vento-test/notes.pdf", false),
            item(2, "/tmp/vento-test/beach.jpg", false),
            (3, PathBuf::from("/tmp/vento-test/Photos"), true, false),
            item(4, "/tmp/vento-test/gone.png", true),
            item(5, "/tmp/vento-test/Screenshot.PNG", false),
            item(6, "/tmp/vento-test/report.docx", false),
        ];
        assert_eq!(kind_ids(&items, PocketKind::Image), [2, 5]);
        assert_eq!(kind_ids(&items, PocketKind::Document), [1, 6]);
        assert_eq!(kind_ids(&items, PocketKind::Folder), [3]);
        assert!(kind_ids(&items, PocketKind::Audio).is_empty());
        // Every item of the kind missing: nothing to drag.
        let gone = [item(7, "/tmp/vento-test/song.mp3", true)];
        assert!(kind_ids(&gone, PocketKind::Audio).is_empty());
    }

    #[test]
    fn groups_follow_the_kind_order_and_keep_pocket_order_inside() {
        let items = vec![
            item(1, "/tmp/vento-test/notes.pdf", false),
            item(2, "/tmp/vento-test/beach.jpg", false),
            (3, PathBuf::from("/tmp/vento-test/Photos"), true, false),
            item(4, "/tmp/vento-test/Screenshot.PNG", true),
        ];
        let g = groups(&items, 560.0);
        let kinds: Vec<PocketKind> = g.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            [PocketKind::Image, PocketKind::Document, PocketKind::Folder]
        );
        // A missing item keeps its tile, by its place in the Pocket.
        assert_eq!(g[0].indices, [1, 3]);
        assert_eq!(g[1].indices, [0]);
        assert!(g.iter().all(|g| g.shown == g.indices.len()));
        assert!(groups(&[], 560.0).is_empty());
    }

    #[test]
    fn a_full_row_keeps_room_for_its_more_tile() {
        // 528 pt of room in a 560 pt island.
        let room = 560.0 - 2.0 * SIDE_PAD;
        // Five pictures fit; a sixth leaves four beside "+2".
        assert_eq!(tiles_shown(5, NARROW_W, room), 5);
        assert_eq!(tiles_shown(6, NARROW_W, room), 4);
        // Three wide tiles fit; a fourth leaves three beside "+1".
        assert_eq!(tiles_shown(3, WIDE_W, room), 3);
        assert_eq!(tiles_shown(4, WIDE_W, room), 3);
        assert_eq!(tiles_shown(40, WIDE_W, room), 3);
        // However narrow the island, one tile shows beside "+N".
        assert_eq!(tiles_shown(9, WIDE_W, 120.0), 1);
        assert_eq!(tiles_shown(0, WIDE_W, room), 0);
    }

    #[test]
    fn each_kind_has_its_window_number() {
        // As `pocket.slint` passes a group's kind back.
        for (i, kind) in PocketKind::ALL.into_iter().enumerate() {
            let n = i32::try_from(i).unwrap();
            assert_eq!(kind_index(kind), n);
            assert_eq!(kind_at(n), Some(kind));
        }
        assert_eq!(kind_at(-1), None);
        assert_eq!(kind_at(7), None);
    }
}
