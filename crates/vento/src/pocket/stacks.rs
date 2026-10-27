//! The Stacks layout of a full Pocket: one stack per kind present stands in
//! for the cells, in [`PocketKind::ALL`] order. Dragging a stack drags the
//! items of its kind; clicking it opens the Pocket list on that kind, whose
//! back control returns here.

use std::rc::Rc;

use log::debug;
use slint::{ComponentHandle, Image, Model, ModelRc, VecModel};
use vento_core::pocket_kind::{PocketKind, kind_of};
use vento_core::settings::PocketLayout;

use crate::ui::{Icons, PocketIsland, PocketStack};

use super::PocketController;
use super::clip_view::set_rows_in_place;
use super::geometry::MAX_CELLS;
use super::list_view::{ListItem, kind_counts, list_items};
use super::out::Press;

/// The items a drag of `kind`'s stack takes: those of that kind whose file
/// is still there, in Pocket order. The cell selection plays no part.
pub(super) fn stack_ids(items: &[ListItem], kind: PocketKind) -> Vec<u64> {
    items
        .iter()
        .filter(|(_, path, is_dir, missing)| !missing && kind_of(path, *is_dir) == kind)
        .map(|(id, ..)| *id)
        .collect()
}

/// Whether a Pocket of `count` items shows stacks instead of cells under
/// `layout`: only under Stacks, and only once it holds more than
/// [`MAX_CELLS`], so a Pocket of eight or fewer looks as it always has.
pub(super) fn shows_stacks(layout: PocketLayout, count: usize) -> bool {
    layout == PocketLayout::Stacks && count > MAX_CELLS
}

/// The window's number for `kind`: its place in [`PocketKind::ALL`].
fn kind_index(kind: PocketKind) -> i32 {
    PocketKind::ALL
        .iter()
        .position(|&k| k == kind)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(-1)
}

/// The kind at window number `i`, or `None` for a number no kind has.
fn kind_at(i: i32) -> Option<PocketKind> {
    usize::try_from(i)
        .ok()
        .and_then(|i| PocketKind::ALL.get(i))
        .copied()
}

/// The icon a stack of `kind` shows.
fn stack_icon(icons: &Icons<'_>, kind: PocketKind) -> Image {
    match kind {
        PocketKind::Image => icons.get_photo(),
        PocketKind::Video => icons.get_video(),
        PocketKind::Audio => icons.get_audio(),
        PocketKind::Document => icons.get_document(),
        PocketKind::Archive => icons.get_archive(),
        PocketKind::Folder => icons.get_folder(),
        PocketKind::Other => icons.get_file(),
    }
}

/// The stacks the island shows, empty while it shows cells.
pub(super) struct Stacks {
    model: Rc<VecModel<PocketStack>>,
}

impl Stacks {
    /// No stacks yet, their model bound to `window`.
    pub(super) fn new(window: &PocketIsland) -> Self {
        let model = Rc::new(VecModel::default());
        window.set_stacks(ModelRc::from(model.clone()));
        Self { model }
    }

    /// How many stacks are shown (0 while the island shows cells).
    pub(super) fn len(&self) -> usize {
        self.model.row_count()
    }
}

impl PocketController {
    /// Routes the stacks' presses through the cells' latch, so a stack
    /// drags or clicks as a cell does.
    pub(super) fn connect_stacks(&self) {
        let w = &self.window;
        let this = self.this.clone();
        let with = move || this.upgrade();
        let c = with.clone();
        w.on_stack_pressed(move |k| {
            if let (Some(c), Some(kind)) = (c(), kind_at(k)) {
                c.latch.press(Press::Stack(kind));
            }
        });
        let c = with.clone();
        w.on_stack_moved(move |_, dx, dy| {
            if let Some(c) = c() {
                c.moved(dx, dy);
            }
        });
        w.on_stack_released(move |_| {
            if let Some(c) = with() {
                c.released();
            }
        });
    }

    /// Whether the island shows stacks rather than cells.
    pub(super) fn stacks_shown(&self) -> bool {
        self.stacks.len() > 0
    }

    /// Pushes the stacks into the window: one per kind present while the
    /// layout shows them, none otherwise. They cost a look at each file, so
    /// they are made only while the island is expanded (a hidden or collapsed
    /// island shows none); [`present`](Self::present) refreshes as it
    /// expands. Only stacks that changed are touched, so the one under the
    /// pointer keeps its hover.
    pub(super) fn refresh_stacks(&self) {
        if !shows_stacks(self.layout.get(), self.item_count()) {
            if self.stacks_shown() {
                self.stacks.model.set_vec(Vec::new());
            }
            return;
        }
        if !self.expanded() {
            return;
        }
        let items = {
            let pocket = self.pocket.borrow();
            list_items(&pocket, &pocket.missing())
        };
        let icons = self.window.global::<Icons>();
        let stacks = kind_counts(&items)
            .into_iter()
            .map(|(kind, n)| PocketStack {
                kind: kind_index(kind),
                label: kind.label().into(),
                icon: stack_icon(&icons, kind),
                count: i32::try_from(n).unwrap_or(i32::MAX),
            })
            .collect();
        set_rows_in_place(&self.stacks.model, stacks);
    }

    /// A press on `kind`'s stack moved far enough: drags its items out.
    pub(super) fn drag_stack(&self, kind: PocketKind) {
        let ids = {
            let pocket = self.pocket.borrow();
            stack_ids(&list_items(&pocket, &pocket.missing()), kind)
        };
        debug!("pocket stack {kind:?}: {} item(s) to drag", ids.len());
        self.start_drag_out(ids);
    }

    /// A click on `kind`'s stack: the Pocket list, on that kind.
    pub(super) fn stack_clicked(&self, kind: PocketKind) {
        self.open_pocket_list(Some(kind));
        self.hold_open_briefly();
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
    fn a_stack_drags_its_present_items_in_pocket_order() {
        let items = vec![
            item(1, "/tmp/vento-test/notes.pdf", false),
            item(2, "/tmp/vento-test/beach.jpg", false),
            (3, PathBuf::from("/tmp/vento-test/Photos"), true, false),
            item(4, "/tmp/vento-test/gone.png", true),
            item(5, "/tmp/vento-test/Screenshot.PNG", false),
            item(6, "/tmp/vento-test/report.docx", false),
        ];
        assert_eq!(stack_ids(&items, PocketKind::Image), [2, 5]);
        assert_eq!(stack_ids(&items, PocketKind::Document), [1, 6]);
        assert_eq!(stack_ids(&items, PocketKind::Folder), [3]);
        assert!(stack_ids(&items, PocketKind::Audio).is_empty());
        // Every item of the kind missing: nothing to drag.
        let gone = [item(7, "/tmp/vento-test/song.mp3", true)];
        assert!(stack_ids(&gone, PocketKind::Audio).is_empty());
    }

    #[test]
    fn only_a_full_pocket_under_stacks_shows_stacks() {
        assert!(shows_stacks(PocketLayout::Stacks, 9));
        assert!(shows_stacks(PocketLayout::Stacks, 40));
        // Eight or fewer: the cells, as today.
        assert!(!shows_stacks(PocketLayout::Stacks, 8));
        assert!(!shows_stacks(PocketLayout::Stacks, 0));
        assert!(!shows_stacks(PocketLayout::List, 12));
        assert!(!shows_stacks(PocketLayout::Scroll, 12));
    }

    #[test]
    fn each_kind_has_its_window_number() {
        // As `pocket.slint` passes a stack's kind back.
        for (i, kind) in PocketKind::ALL.into_iter().enumerate() {
            let n = i32::try_from(i).unwrap();
            assert_eq!(kind_index(kind), n);
            assert_eq!(kind_at(n), Some(kind));
        }
        assert_eq!(kind_at(-1), None);
        assert_eq!(kind_at(7), None);
    }
}
