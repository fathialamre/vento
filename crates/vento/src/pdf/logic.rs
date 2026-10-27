//! The PDF window's pure rules: its texts, where the arrow keys move the
//! focus, where added pages go, how a drag scrolls and where its orange bar
//! stands, and which pages stay selected after an undo. Nothing here
//! touches Slint, so all of it is tested on its own.

use std::collections::{HashMap, HashSet};

use vento_core::i18n::{fill, tr, trn};
use vento_core::pdf::{GridMetrics, PageRef, page_ranges};

/// How far, in points, the pointer moves with the button down on a page
/// before it drags the page.
pub const DRAG_START: f32 = 4.0;
/// How near, in points, the pointer comes to the grid's top or bottom edge
/// while dragging before the grid scrolls.
pub const SCROLL_EDGE: f32 = 40.0;
/// The fastest the grid scrolls while dragging, in points per frame.
pub const SCROLL_MAX: f32 = 20.0;

/// Arrow keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// The window title's two parts: the files' names (`report.pdf`,
/// `report.pdf + scan.pdf`, `report.pdf + 2 more`) and the page count, with
/// "edited" once the page list changed.
pub fn title_text(names: &[&str], pages: usize, edited: bool) -> (String, String) {
    let names_text = match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, second] => fill(
            &tr("{first} + {second}"),
            &[("first", first), ("second", second)],
        ),
        [first, rest @ ..] => fill(
            &trn("{name} + {n} more", "{name} + {n} more", rest.len() as u64),
            &[("name", first)],
        ),
    };
    let count = trn("{n} page", "{n} pages", pages as u64);
    let meta = if edited {
        fill(&tr("{pages} · edited"), &[("pages", &count)])
    } else {
        count
    };
    (names_text, meta)
}

/// The footer's text for the selected positions (zero-based, sorted).
pub fn footer_text(selected: &[usize]) -> String {
    if selected.is_empty() {
        return tr("Nothing selected");
    }
    fill(
        &trn(
            "{n} selected · page {pages}",
            "{n} selected · pages {pages}",
            selected.len() as u64,
        ),
        &[("pages", &page_ranges(selected))],
    )
}

/// The in-window banner after deleting `n` pages.
pub fn removed_text(n: usize) -> String {
    trn("{n} page removed", "{n} pages removed", n as u64)
}

/// The letter of source `i`: A to Z, then A2 to Z2, and so on.
pub fn source_letter(i: usize) -> String {
    let letter = char::from(b'A' + (i % 26) as u8);
    match i / 26 {
        0 => letter.to_string(),
        round => format!("{letter}{}", round + 1),
    }
}

/// The tint of source `i` in a window of `sources` files: 0 (none) with a
/// single file, else its place from 1.
pub fn tint_of(i: usize, sources: usize) -> i32 {
    if sources > 1 {
        i32::try_from(i % 1000 + 1).unwrap_or(1)
    } else {
        0
    }
}

/// Where an arrow key moves the focus in a grid of `count` pages in
/// `columns`: Left and Right step through reading order (mirrored right to
/// left), Up and Down a row; the focus stops at the edges. Without a focus
/// any arrow focuses the first page.
pub fn moved_focus(
    focus: Option<usize>,
    arrow: Arrow,
    columns: usize,
    count: usize,
    rtl: bool,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let Some(focus) = focus else {
        return Some(0);
    };
    let focus = focus.min(count - 1);
    let columns = columns.max(1);
    let forward = match (arrow, rtl) {
        (Arrow::Right, false) | (Arrow::Left, true) => Some(true),
        (Arrow::Left, false) | (Arrow::Right, true) => Some(false),
        _ => None,
    };
    let moved = match (forward, arrow) {
        (Some(true), _) => (focus + 1).min(count - 1),
        (Some(false), _) => focus.saturating_sub(1),
        (None, Arrow::Down) if focus / columns < (count - 1) / columns => {
            (focus + columns).min(count - 1)
        }
        (None, Arrow::Up) if focus >= columns => focus - columns,
        _ => focus,
    };
    Some(moved)
}

/// Where added pages go in a list `len` long: after the focused page, or
/// at the end when no page has the focus.
pub fn insert_point(focused: Option<usize>, len: usize) -> usize {
    focused.map_or(len, |i| (i + 1).min(len))
}

/// Whether the pointer, `dx` and `dy` points from where the button went
/// down on a page, has moved far enough to drag it.
pub fn drag_started(dx: f32, dy: f32) -> bool {
    dx.hypot(dy) >= DRAG_START
}

/// The pages a drag that began on page `pressed` carries: the whole
/// selection when the page is in it, else that page alone.
pub fn dragged_pages(selected: &[usize], pressed: usize) -> Vec<usize> {
    if selected.contains(&pressed) {
        selected.to_vec()
    } else {
        vec![pressed]
    }
}

/// Where a drag of pages stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragPhase {
    /// No drag.
    Idle,
    /// Pages follow the pointer.
    Moving,
    /// Cancelled with Escape while the button is still down: the release
    /// still to come does nothing.
    Cancelled,
}

/// What happens to a page under the pointer during a drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragEvent {
    /// The pointer moved with a button down on the page: `left` when it is
    /// the left button, and `dx`, `dy` from its press.
    Moved { left: bool, dx: f32, dy: f32 },
    /// A button went up; `left` when it was the left one.
    Up { left: bool },
    /// The page lost the pointer without a release (Slint's cancel).
    Cancel,
    /// Escape was pressed.
    Escape,
}

/// What the window does for a [`DragEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragAction {
    Nothing,
    /// Lift the pages and follow the pointer.
    Start,
    /// Move the lifted pages and the bar to the pointer.
    Follow,
    /// Drop the pages where the bar stands.
    Drop,
    /// Put the pages back: nothing moves.
    End,
}

/// The next phase of a drag and what to do, for `event` in `phase`. Only
/// the left button drags; it starts once the pointer is [`DRAG_START`]
/// from the press. Any release or cancel ends a drag, so it can never
/// outlive the press that began it; only a left release drops the pages.
pub fn drag_step(phase: DragPhase, event: DragEvent) -> (DragPhase, DragAction) {
    use DragAction as A;
    use DragPhase as P;
    match (phase, event) {
        (P::Idle, DragEvent::Moved { left, dx, dy }) if left && drag_started(dx, dy) => {
            (P::Moving, A::Start)
        }
        (P::Moving, DragEvent::Moved { .. }) => (P::Moving, A::Follow),
        (P::Moving, DragEvent::Up { left: true }) => (P::Idle, A::Drop),
        (P::Moving, DragEvent::Up { left: false } | DragEvent::Cancel) => (P::Idle, A::End),
        (P::Moving, DragEvent::Escape) => (P::Cancelled, A::End),
        (P::Cancelled, DragEvent::Up { .. } | DragEvent::Cancel) => (P::Idle, A::Nothing),
        (phase, _) => (phase, A::Nothing),
    }
}

/// How far the grid scrolls this frame while dragging with the pointer at
/// `y` in a grid area `height` tall (both in the area's own points, not
/// scrolled): up (negative) within [`SCROLL_EDGE`] of the top, down within
/// it of the bottom, faster the deeper the pointer is, at most
/// [`SCROLL_MAX`]; 0 elsewhere.
pub fn auto_scroll_step(y: f32, height: f32) -> f32 {
    let speed = |depth: f32| (depth / SCROLL_EDGE * SCROLL_MAX).min(SCROLL_MAX);
    if y < SCROLL_EDGE {
        -speed(SCROLL_EDGE - y)
    } else if y > height - SCROLL_EDGE {
        speed(y - (height - SCROLL_EDGE))
    } else {
        0.0
    }
}

/// How far the grid of `count` pages scrolls at most in an area `height`
/// tall (matches the Flickable's content height in `pdf.slint`).
pub fn max_scroll(metrics: &GridMetrics, count: usize, height: f32) -> f32 {
    let rows = metrics.rows(count);
    let content = 2.0 * metrics.pad
        + rows as f32 * metrics.cell_h
        + rows.saturating_sub(1) as f32 * metrics.gap;
    (content - height).max(0.0)
}

/// Where the orange bar stands for a drop at insertion `index` in a grid of
/// `count` pages, with the pointer at content height `y`: the bar's centre
/// line `x` and its top, in the grid's content space (before the columns'
/// centring offset). A drop at the start of a row can also be read as the
/// end of the row above; the bar stays on the row the pointer is over.
pub fn insertion_bar(metrics: &GridMetrics, index: usize, count: usize, y: f32) -> (f32, f32) {
    let columns = metrics.columns.max(1);
    let index = index.min(count);
    let rows = metrics.rows(count).max(1);
    let row_stride = metrics.cell_h + metrics.gap;
    // The row under the pointer, as `GridMetrics::insertion_index` reads it.
    let pointer_row = (((y - metrics.pad + metrics.gap / 2.0) / row_stride)
        .floor()
        .max(0.0) as usize)
        .min(rows - 1);
    let holds = |row: usize| {
        let start = row * columns;
        let in_row = count.saturating_sub(start).min(columns);
        (start..=start + in_row).contains(&index)
    };
    let row = if holds(pointer_row) {
        pointer_row
    } else {
        (index / columns).min(rows - 1)
    };
    let gap = index - row * columns;
    let along = metrics.pad + gap as f32 * (metrics.cell_w + metrics.gap) - metrics.gap / 2.0;
    let x = if metrics.rtl {
        metrics.content_width() - along
    } else {
        along
    };
    (x, metrics.pad + row as f32 * row_stride)
}

/// The top left of page `i`'s cell in the grid's content space (before the
/// columns' centring offset), mirrored right to left as `pdf.slint` places
/// it.
pub fn cell_origin(metrics: &GridMetrics, i: usize) -> (f32, f32) {
    let columns = metrics.columns.max(1);
    let col = i % columns;
    let col = if metrics.rtl { columns - 1 - col } else { col };
    (
        metrics.pad + col as f32 * (metrics.cell_w + metrics.gap),
        metrics.pad + (i / columns) as f32 * (metrics.cell_h + metrics.gap),
    )
}

/// The sources that the pages use, in source order, out of `sources`. The
/// title, legend and tints name only these: after undoing an add, the
/// added file stays opened (for redo) but is not shown.
pub fn sources_in_use(pages: &[PageRef], sources: usize) -> Vec<usize> {
    let mut used = vec![false; sources];
    for page in pages {
        if let Some(slot) = used.get_mut(page.source) {
            *slot = true;
        }
    }
    (0..sources).filter(|&i| used[i]).collect()
}

/// The page's identity in a window: a page of a source is in the list at
/// most once (adding a file again opens it as a new source).
fn identity(page: &PageRef) -> (usize, usize) {
    (page.source, page.page)
}

/// The selection after an undo or redo turned the list `before` into
/// `after`, with `selected` positions in `before`: the pages selected stay
/// selected wherever they went, and pages that came back (an undone
/// delete, a redone add) are selected too.
pub fn follow_selection(before: &[PageRef], after: &[PageRef], selected: &[usize]) -> Vec<usize> {
    let was: HashSet<(usize, usize)> = before.iter().map(identity).collect();
    let kept: HashSet<(usize, usize)> = selected
        .iter()
        .filter_map(|&i| before.get(i))
        .map(identity)
        .collect();
    after
        .iter()
        .enumerate()
        .filter(|(_, page)| {
            let id = identity(page);
            kept.contains(&id) || !was.contains(&id)
        })
        .map(|(i, _)| i)
        .collect()
}

/// The pages of `after` that were in `before` turned differently: their
/// position in `before`, the page as it is now, and the quarter turns
/// clockwise from then to now (1 to 3). Their thumbnails can be turned
/// at once instead of drawn again.
pub fn turned_pages(before: &[PageRef], after: &[PageRef]) -> Vec<(usize, PageRef, u8)> {
    let was: HashMap<(usize, usize), (usize, u8)> = before
        .iter()
        .enumerate()
        .map(|(i, page)| (identity(page), (i, page.turns)))
        .collect();
    after
        .iter()
        .filter_map(|page| {
            let &(i, turns) = was.get(&identity(page))?;
            let delta = (page.turns + 4 - turns % 4) % 4;
            (delta != 0).then_some((i, *page, delta))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::i18n::with_language;

    fn page(source: usize, page: usize, turns: u8) -> PageRef {
        PageRef {
            source,
            page,
            turns,
        }
    }

    fn metrics(columns: usize, rtl: bool) -> GridMetrics {
        GridMetrics {
            columns,
            cell_w: 100.0,
            cell_h: 150.0,
            gap: 10.0,
            pad: 20.0,
            rtl,
        }
    }

    #[test]
    fn titles() {
        assert_eq!(title_text(&["report.pdf"], 8, false).0, "report.pdf");
        assert_eq!(
            title_text(&["report.pdf", "scan.pdf"], 8, false).0,
            "report.pdf + scan.pdf"
        );
        assert_eq!(
            title_text(&["a.pdf", "b.pdf", "c.pdf"], 9, true).0,
            "a.pdf + 2 more"
        );
        assert_eq!(title_text(&["a.pdf"], 5, true).1, "5 pages · edited");
    }

    #[test]
    fn title_counts_one_page_and_unedited() {
        assert_eq!(title_text(&["a.pdf"], 1, false).1, "1 page");
        assert_eq!(title_text(&["a.pdf"], 8, false).1, "8 pages");
    }

    #[test]
    fn footers() {
        assert_eq!(footer_text(&[]), "Nothing selected");
        assert_eq!(footer_text(&[1, 2, 6]), "3 selected · pages 2–3, 7");
    }

    #[test]
    fn footer_names_one_page_singular() {
        assert_eq!(footer_text(&[4]), "1 selected · page 5");
    }

    #[test]
    fn window_texts_in_arabic() {
        with_language("ar", || {
            assert_ne!(footer_text(&[]), "Nothing selected");
            assert_ne!(title_text(&["a.pdf"], 5, true).1, "5 pages · edited");
            assert!(
                title_text(&["a.pdf", "b.pdf", "c.pdf"], 9, false)
                    .0
                    .contains("a.pdf")
            );
            assert_ne!(removed_text(3), "3 pages removed");
            assert!(removed_text(3).contains('3'));
        });
    }

    #[test]
    fn letters_name_sources() {
        assert_eq!(source_letter(0), "A");
        assert_eq!(source_letter(1), "B");
        assert_eq!(source_letter(25), "Z");
        assert_eq!(source_letter(26), "A2");
        assert_eq!(source_letter(53), "B3");
    }

    #[test]
    fn arrows_move_focus_in_reading_order() {
        // 10 pages, 4 columns.
        assert_eq!(moved_focus(None, Arrow::Right, 4, 10, false), Some(0));
        assert_eq!(moved_focus(Some(0), Arrow::Right, 4, 10, false), Some(1));
        assert_eq!(moved_focus(Some(1), Arrow::Left, 4, 10, false), Some(0));
        assert_eq!(moved_focus(Some(0), Arrow::Left, 4, 10, false), Some(0));
        assert_eq!(moved_focus(Some(9), Arrow::Right, 4, 10, false), Some(9));
        assert_eq!(moved_focus(Some(1), Arrow::Down, 4, 10, false), Some(5));
        assert_eq!(moved_focus(Some(6), Arrow::Down, 4, 10, false), Some(9));
        assert_eq!(moved_focus(Some(9), Arrow::Down, 4, 10, false), Some(9));
        assert_eq!(moved_focus(Some(5), Arrow::Up, 4, 10, false), Some(1));
        assert_eq!(moved_focus(Some(2), Arrow::Up, 4, 10, false), Some(2));
        assert_eq!(moved_focus(Some(3), Arrow::Right, 4, 0, false), None);
    }

    #[test]
    fn arrows_mirror_right_to_left() {
        assert_eq!(moved_focus(Some(0), Arrow::Left, 4, 10, true), Some(1));
        assert_eq!(moved_focus(Some(1), Arrow::Right, 4, 10, true), Some(0));
        assert_eq!(moved_focus(Some(1), Arrow::Down, 4, 10, true), Some(5));
    }

    #[test]
    fn focus_past_the_end_is_clamped() {
        // After pages were removed the focus may point past the end: it
        // moves from the last page.
        assert_eq!(moved_focus(Some(12), Arrow::Left, 4, 10, false), Some(8));
        assert_eq!(moved_focus(Some(12), Arrow::Down, 4, 10, false), Some(9));
    }

    #[test]
    fn single_files_have_no_tint() {
        assert_eq!(tint_of(0, 1), 0);
        assert_eq!(tint_of(0, 2), 1);
        assert_eq!(tint_of(1, 2), 2);
    }

    #[test]
    fn add_goes_after_focus_or_end() {
        assert_eq!(insert_point(Some(2), 8), 3);
        assert_eq!(insert_point(None, 8), 8);
    }

    #[test]
    fn removed_banner() {
        assert_eq!(removed_text(1), "1 page removed");
        assert_eq!(removed_text(3), "3 pages removed");
    }

    #[test]
    fn add_after_a_stale_focus_goes_to_the_end() {
        assert_eq!(insert_point(Some(7), 8), 8);
        assert_eq!(insert_point(Some(20), 8), 8);
        assert_eq!(insert_point(None, 0), 0);
        assert_eq!(insert_point(Some(0), 0), 0);
    }

    #[test]
    fn drags_start_past_four_points() {
        assert!(!drag_started(0.0, 0.0));
        assert!(!drag_started(2.0, -3.0));
        assert!(drag_started(4.0, 0.0));
        assert!(drag_started(0.0, -4.0));
        assert!(drag_started(3.0, 3.0));
    }

    #[test]
    fn a_drag_carries_the_selection_or_the_page() {
        assert_eq!(dragged_pages(&[1, 3, 4], 3), vec![1, 3, 4]);
        assert_eq!(dragged_pages(&[1, 3, 4], 2), vec![2]);
        assert_eq!(dragged_pages(&[], 0), vec![0]);
    }

    #[test]
    fn dragging_scrolls_near_the_edges() {
        // A grid area 400 points tall.
        assert_eq!(auto_scroll_step(200.0, 400.0), 0.0);
        assert_eq!(auto_scroll_step(40.0, 400.0), 0.0);
        assert_eq!(auto_scroll_step(360.0, 400.0), 0.0);
        assert_eq!(auto_scroll_step(20.0, 400.0), -10.0);
        assert_eq!(auto_scroll_step(0.0, 400.0), -20.0);
        assert_eq!(auto_scroll_step(380.0, 400.0), 10.0);
        assert_eq!(auto_scroll_step(400.0, 400.0), 20.0);
        // Past the edge it keeps to the fastest.
        assert_eq!(auto_scroll_step(-60.0, 400.0), -20.0);
        assert_eq!(auto_scroll_step(520.0, 400.0), 20.0);
    }

    #[test]
    fn scrolling_stops_at_the_last_row() {
        // 4 columns, 10 pages: 3 rows, 2*20 + 3*150 + 2*10 = 510 tall.
        let m = metrics(4, false);
        assert_eq!(max_scroll(&m, 10, 400.0), 110.0);
        assert_eq!(max_scroll(&m, 10, 600.0), 0.0);
        assert_eq!(max_scroll(&m, 0, 400.0), 0.0);
    }

    #[test]
    fn the_bar_stands_in_the_gap_before_the_drop_spot() {
        // 4 columns, 10 pages; column c starts at 20 + 110 c.
        let m = metrics(4, false);
        // Before page 2 (index 1), first row: halfway between columns 0
        // and 1.
        assert_eq!(insertion_bar(&m, 1, 10, 50.0), (125.0, 20.0));
        // Before the first page.
        assert_eq!(insertion_bar(&m, 0, 10, 50.0), (15.0, 20.0));
        // Index 4 with the pointer on the first row: the end of that row.
        assert_eq!(insertion_bar(&m, 4, 10, 50.0), (455.0, 20.0));
        // Index 4 with the pointer on the second row: its start.
        assert_eq!(insertion_bar(&m, 4, 10, 200.0), (15.0, 180.0));
        // The end of the list, after page 10 on the third row.
        assert_eq!(insertion_bar(&m, 10, 10, 900.0), (235.0, 340.0));
    }

    #[test]
    fn the_bar_is_mirrored_right_to_left() {
        // Content width 2*20 + 4*100 + 3*10 = 470.
        let m = metrics(4, true);
        assert_eq!(insertion_bar(&m, 1, 10, 50.0), (470.0 - 125.0, 20.0));
        assert_eq!(insertion_bar(&m, 0, 10, 50.0), (470.0 - 15.0, 20.0));
    }

    #[test]
    fn the_bar_of_a_full_last_row_ends_it() {
        let m = metrics(4, false);
        assert_eq!(insertion_bar(&m, 8, 8, 900.0), (455.0, 180.0));
        assert_eq!(insertion_bar(&m, 0, 0, 0.0), (15.0, 20.0));
    }

    #[test]
    fn a_left_press_moved_far_enough_starts_a_drag() {
        let far = DragEvent::Moved {
            left: true,
            dx: 4.0,
            dy: 0.0,
        };
        let near = DragEvent::Moved {
            left: true,
            dx: 2.0,
            dy: 1.0,
        };
        assert_eq!(
            drag_step(DragPhase::Idle, near),
            (DragPhase::Idle, DragAction::Nothing)
        );
        assert_eq!(
            drag_step(DragPhase::Idle, far),
            (DragPhase::Moving, DragAction::Start)
        );
        assert_eq!(
            drag_step(DragPhase::Moving, near),
            (DragPhase::Moving, DragAction::Follow)
        );
    }

    #[test]
    fn other_buttons_never_start_a_drag() {
        let right = DragEvent::Moved {
            left: false,
            dx: 40.0,
            dy: 40.0,
        };
        assert_eq!(
            drag_step(DragPhase::Idle, right),
            (DragPhase::Idle, DragAction::Nothing)
        );
        assert_eq!(
            drag_step(DragPhase::Idle, DragEvent::Up { left: false }),
            (DragPhase::Idle, DragAction::Nothing)
        );
    }

    #[test]
    fn a_release_ends_every_drag() {
        assert_eq!(
            drag_step(DragPhase::Moving, DragEvent::Up { left: true }),
            (DragPhase::Idle, DragAction::Drop)
        );
        // Another button's release puts the pages back.
        assert_eq!(
            drag_step(DragPhase::Moving, DragEvent::Up { left: false }),
            (DragPhase::Idle, DragAction::End)
        );
        assert_eq!(
            drag_step(DragPhase::Cancelled, DragEvent::Up { left: true }),
            (DragPhase::Idle, DragAction::Nothing)
        );
        assert_eq!(
            drag_step(DragPhase::Cancelled, DragEvent::Up { left: false }),
            (DragPhase::Idle, DragAction::Nothing)
        );
    }

    #[test]
    fn a_cancel_ends_any_drag() {
        assert_eq!(
            drag_step(DragPhase::Moving, DragEvent::Cancel),
            (DragPhase::Idle, DragAction::End)
        );
        assert_eq!(
            drag_step(DragPhase::Cancelled, DragEvent::Cancel),
            (DragPhase::Idle, DragAction::Nothing)
        );
        assert_eq!(
            drag_step(DragPhase::Idle, DragEvent::Cancel),
            (DragPhase::Idle, DragAction::Nothing)
        );
    }

    #[test]
    fn escape_cancels_a_drag_until_the_release() {
        assert_eq!(
            drag_step(DragPhase::Moving, DragEvent::Escape),
            (DragPhase::Cancelled, DragAction::End)
        );
        assert_eq!(
            drag_step(DragPhase::Cancelled, DragEvent::Escape),
            (DragPhase::Cancelled, DragAction::Nothing)
        );
        assert_eq!(
            drag_step(DragPhase::Idle, DragEvent::Escape),
            (DragPhase::Idle, DragAction::Nothing)
        );
        // Moving on after Escape does not lift the pages again.
        let far = DragEvent::Moved {
            left: true,
            dx: 30.0,
            dy: 0.0,
        };
        assert_eq!(
            drag_step(DragPhase::Cancelled, far),
            (DragPhase::Cancelled, DragAction::Nothing)
        );
    }

    #[test]
    fn titles_name_only_the_sources_in_use() {
        let pages = [page(0, 0, 0), page(2, 1, 0), page(0, 1, 0)];
        assert_eq!(sources_in_use(&pages, 3), vec![0, 2]);
        assert_eq!(sources_in_use(&[], 2), Vec::<usize>::new());
        // A page of a source past the list is ignored.
        assert_eq!(sources_in_use(&[page(5, 0, 0)], 2), Vec::<usize>::new());
    }

    #[test]
    fn cells_sit_in_reading_order() {
        assert_eq!(cell_origin(&metrics(4, false), 0), (20.0, 20.0));
        assert_eq!(cell_origin(&metrics(4, false), 5), (130.0, 180.0));
        assert_eq!(cell_origin(&metrics(4, true), 0), (350.0, 20.0));
        assert_eq!(cell_origin(&metrics(4, true), 5), (240.0, 180.0));
    }

    #[test]
    fn undo_keeps_the_selected_pages_and_selects_returning_ones() {
        let a = page(0, 0, 0);
        let b = page(0, 1, 0);
        let c = page(0, 2, 0);
        let d = page(1, 0, 0);
        // Undoing a delete of b and c with a selected: a stays, b and c
        // come back selected.
        assert_eq!(
            follow_selection(&[a, d], &[a, b, c, d], &[0]),
            vec![0, 1, 2]
        );
        // Undoing a move: the selection follows its pages.
        assert_eq!(
            follow_selection(&[b, c, a, d], &[a, b, c, d], &[2]),
            vec![0]
        );
        // Undoing an add of d: d goes, the rest keeps.
        assert_eq!(follow_selection(&[a, b, d], &[a, b], &[1, 2]), vec![1]);
        // Undoing a turn: the same pages, turned back, stay selected.
        let b_turned = page(0, 1, 1);
        assert_eq!(follow_selection(&[a, b_turned], &[a, b], &[1]), vec![1]);
    }

    #[test]
    fn turned_pages_are_found_by_identity() {
        let a = page(0, 0, 0);
        let b = page(0, 1, 3);
        let b_now = page(0, 1, 0);
        let a_now = page(0, 0, 2);
        // b went from 3 turns to 0 (one more clockwise) and moved; a turned
        // twice.
        assert_eq!(
            turned_pages(&[a, b], &[b_now, a_now]),
            vec![(1, b_now, 1), (0, a_now, 2)]
        );
        assert_eq!(turned_pages(&[a, b], &[a, b]), vec![]);
        assert_eq!(turned_pages(&[a], &[b]), vec![]);
    }
}
