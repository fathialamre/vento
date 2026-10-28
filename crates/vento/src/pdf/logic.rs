//! The PDF window's pure rules: its texts, where the arrow keys move the
//! focus, where added pages go, how a drag scrolls and where its orange bar
//! stands, and which pages stay selected after an undo. Nothing here
//! touches Slint, so all of it is tested on its own.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use vento_core::i18n::{fill, tr, trn};
use vento_core::pdf::{
    GridMetrics, ImageKind, ImageOptions, PageRef, SplitMode, page_ranges, split_plan,
};

/// How far, in points, the pointer moves with the button down on a page
/// before it drags the page.
pub const DRAG_START: f32 = 4.0;
/// How near, in points, the pointer comes to the grid's top or bottom edge
/// while dragging before the grid scrolls.
pub const SCROLL_EDGE: f32 = 40.0;
/// The fastest the grid scrolls while dragging, in points per frame.
pub const SCROLL_MAX: f32 = 20.0;

/// Who waits for the closing question's answer (quitting): told `true` once
/// the window has closed, `false` on Cancel.
pub type Answered = Box<dyn FnOnce(bool)>;

/// Puts `new` in `slot` as the one to tell. An earlier waiter that has not
/// been answered gives way: it hears `false` (the window did not close for
/// it), so a quit that asked first never waits for ever. The earlier is
/// called after the slot's borrow ends.
pub fn replace_answer(slot: &RefCell<Option<Answered>>, new: Answered) {
    let earlier = slot.borrow_mut().replace(new);
    if let Some(earlier) = earlier {
        earlier(false);
    }
}

/// Whether the closing question's Save may be pressed: the window has pages
/// to write and no write of its own runs (the sheet's own Save, or a write
/// started before it came up). Discard and Cancel are never held back: a
/// write that runs keeps going without the window, or is stopped by Cancel.
pub fn sheet_save_allowed(has_pages: bool, busy: bool) -> bool {
    has_pages && !busy
}

/// Whether a finished save may close its window: only when it wrote the
/// grid as it stands now (`written` equals `now`) and no added files are
/// still on their way (`adding`). Otherwise the window stays open, so
/// pages added while the save ran are never lost.
pub fn closes_after_save(written: &[PageRef], now: &[PageRef], adding: bool) -> bool {
    !adding && written == now
}

/// The most pixels the large view draws a page's longer side with: about
/// 64 MB of RGBA, sharp on any screen without asking for hundreds.
pub const LARGE_MAX_PX: u32 = 4096;

/// The space kept around the large page, in points (matches `pdf.slint`).
pub const LARGE_INSET: f32 = 16.0;

/// The longer side, in pixels, the large view needs for a page in a grid
/// area `width` × `height` points at `scale` pixels per point: the page,
/// `shape` (width, height) as its thumbnail has it, fitted inside the area
/// less its inset, at most [`LARGE_MAX_PX`]. Without a shape the area's
/// longer side is used.
pub fn large_side_px(width: f32, height: f32, shape: Option<(u32, u32)>, scale: f32) -> u32 {
    let w = (width - 2.0 * LARGE_INSET).max(1.0);
    let h = (height - 2.0 * LARGE_INSET).max(1.0);
    let long = match shape.filter(|&(pw, ph)| pw > 0 && ph > 0) {
        Some((pw, ph)) => {
            let aspect = pw as f32 / ph as f32;
            let fit_w = w.min(h * aspect);
            fit_w.max(fit_w / aspect)
        }
        None => w.max(h),
    };
    ((long * scale).ceil() as u32).clamp(1, LARGE_MAX_PX)
}

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

/// Where added pages go in a list `len` long: after the focused page when
/// its focus ring shows (`shown`), or at the end. A click moves the focus
/// without showing it, so a page merely clicked earlier does not count.
pub fn insert_point(focused: Option<usize>, shown: bool, len: usize) -> usize {
    focused.filter(|_| shown).map_or(len, |i| (i + 1).min(len))
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

/// The positions of `source`'s pages in the list, in list order: what a
/// click on its row in the files sidebar selects, and how many pages the
/// row counts.
pub fn pages_of_source(pages: &[PageRef], source: usize) -> Vec<usize> {
    pages
        .iter()
        .enumerate()
        .filter(|(_, page)| page.source == source)
        .map(|(i, _)| i)
        .collect()
}

/// Whether the files sidebar opens by itself: when there come to be
/// several files where there were fewer (the window opened with several,
/// or a file was added). Otherwise it stays as it was left.
pub fn opens_files(before: usize, now: usize) -> bool {
    now > 1 && now > before
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

/// How near, in points, a click in cut mode must come to the middle of the
/// gap between two pages to place or remove a cut there.
pub const CUT_REACH: f32 = 24.0;

/// Cut mode's footer pieces for `count` pages cut at `cuts` (a cut `c`
/// falls between positions `c - 1` and `c`): the page ranges of the pieces
/// (`"1–2 | 3–5 | 6–8"`) and the button's text (`"Split into 3"`). No cut
/// is one piece.
pub fn cut_summary(count: usize, cuts: &[usize]) -> (String, String) {
    let pieces: Vec<String> = split_plan(count, &SplitMode::AtCuts(cuts.to_vec()))
        .into_iter()
        .map(|range| page_ranges(&range.collect::<Vec<_>>()))
        .collect();
    let n = pieces.len().max(1) as u64;
    (
        pieces.join(" | "),
        trn("Split into {n}", "Split into {n}", n),
    )
}

/// Cut mode's footer text: how to place a cut, then the pieces.
pub fn cut_footer(pieces: &str) -> String {
    fill(
        &tr("Click between pages to add or remove a cut · {pieces}"),
        &[("pieces", pieces)],
    )
}

/// The cut a click at (`x`, `y`) in the grid's content space (before the
/// columns' centring offset, scroll included) places or removes: the gap
/// between two pages within [`CUT_REACH`] of the click, as an insertion
/// index `1..count`. `None` far from a gap, before the first page and after
/// the last. The gap at the start of a row is the cut after the last page
/// of the row above.
pub fn cut_at(metrics: &GridMetrics, x: f32, y: f32, count: usize) -> Option<usize> {
    if count < 2 {
        return None;
    }
    let columns = metrics.columns.max(1);
    let row_stride = metrics.cell_h + metrics.gap;
    // The band of a row reaches half way into the gaps above and below it.
    let row = ((y - metrics.pad + metrics.gap / 2.0) / row_stride).floor();
    if row < 0.0 || row as usize >= metrics.rows(count) {
        return None;
    }
    let start = row as usize * columns;
    let in_row = (count - start).min(columns);
    let along = if metrics.rtl {
        metrics.content_width() - x
    } else {
        x
    };
    // Gap `j` of the row has its middle at `pad + j * stride - gap / 2`.
    let stride = metrics.cell_w + metrics.gap;
    let gap = ((along - metrics.pad + metrics.gap / 2.0) / stride)
        .round()
        .clamp(0.0, in_row as f32);
    let middle = metrics.pad + gap * stride - metrics.gap / 2.0;
    if (along - middle).abs() > CUT_REACH {
        return None;
    }
    let cut = start + gap as usize;
    (cut > 0 && cut < count).then_some(cut)
}

/// Where the marker of `cut` stands, as [`insertion_bar`] gives it: in the
/// gap after page `cut - 1`, in that page's row.
pub fn cut_marker(metrics: &GridMetrics, cut: usize, count: usize) -> (f32, f32) {
    let columns = metrics.columns.max(1);
    let row = cut.saturating_sub(1) / columns;
    let top = metrics.pad + row as f32 * (metrics.cell_h + metrics.gap);
    insertion_bar(metrics, cut, count, top)
}

/// Places `cut` in `cuts`, or removes it when it is there; `cuts` stays
/// sorted.
pub fn toggle_cut(cuts: &mut Vec<usize>, cut: usize) {
    match cuts.binary_search(&cut) {
        Ok(i) => {
            cuts.remove(i);
        }
        Err(i) => cuts.insert(i, cut),
    }
}

/// The pages per piece "Every N pages" offers for `count` pages: from 2 to
/// one less than the count (2 at least).
pub fn split_n_range(count: usize) -> (usize, usize) {
    (2, count.saturating_sub(1).max(2))
}

/// The Split popover's heading: "Split report.pdf".
pub fn split_heading(name: &str) -> String {
    fill(&tr("Split {name}"), &[("name", name)])
}

/// The Split popover's second choice, before its number's buttons:
/// "Every 2 pages".
pub fn every_n_text(n: usize) -> String {
    trn("Every {n} page", "Every {n} pages", n as u64)
}

/// What a choice makes, after it: "→ 4 files".
pub fn files_text(files: usize) -> String {
    trn("→ {n} file", "→ {n} files", files as u64)
}

/// The pages a write takes, under its popover's heading: "Pages 3–5" for
/// the positions (from 0) given, in order.
pub fn pages_text(positions: &[usize]) -> String {
    fill(
        &trn("Page {pages}", "Pages {pages}", positions.len() as u64),
        &[("pages", &page_ranges(positions))],
    )
}

/// The To images popover's heading ("3 pages to images") and its button
/// ("Save images") for `n` pages.
pub fn images_texts(n: usize) -> (String, String) {
    let n = n as u64;
    (
        trn("{n} page to image", "{n} pages to images", n),
        trn("Save image", "Save images", n),
    )
}

/// To images' format and resolution from the settings' `pdf_image_format`
/// and `pdf_image_dpi`: JPEG only for `"jpeg"`, 150 DPI only for 150.
pub fn image_options(format: &str, dpi: u32) -> ImageOptions {
    ImageOptions {
        kind: if format == "jpeg" {
            ImageKind::Jpeg
        } else {
            ImageKind::Png
        },
        dpi: if dpi == 150 { 150 } else { 300 },
    }
}

/// The settings' `pdf_image_format` value for `kind`.
pub fn format_key(kind: ImageKind) -> &'static str {
    match kind {
        ImageKind::Png => "png",
        ImageKind::Jpeg => "jpeg",
    }
}

/// How far a write job is, from its counts of files: `None` while there is
/// no measure of it (one file, or nothing counted yet), when the footer's
/// bar shows only that it is busy.
pub fn job_fraction(done: u64, total: u64) -> Option<f32> {
    (total > 1).then(|| (done as f64 / total as f64).min(1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn noting(log: &Rc<RefCell<Vec<(u8, bool)>>>, id: u8) -> Answered {
        let log = log.clone();
        Box::new(move |answer| log.borrow_mut().push((id, answer)))
    }

    #[test]
    fn a_new_waiter_tells_the_earlier_one_no() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let slot = RefCell::new(None);
        replace_answer(&slot, noting(&log, 1));
        assert!(log.borrow().is_empty(), "nobody to replace yet");
        replace_answer(&slot, noting(&log, 2));
        assert_eq!(*log.borrow(), [(1, false)]);
        // The newer one is still waiting for the real answer.
        let waiting = slot.borrow_mut().take().expect("the newer waiter");
        waiting(true);
        assert_eq!(*log.borrow(), [(1, false), (2, true)]);
    }

    #[test]
    fn the_earlier_waiter_may_ask_again_while_told() {
        // `earlier(false)` can start another quit that fills the slot at
        // once: the slot must not be borrowed meanwhile.
        let slot: Rc<RefCell<Option<Answered>>> = Rc::new(RefCell::new(None));
        let told = Rc::new(Cell::new(false));
        let (again, flag) = (slot.clone(), told.clone());
        replace_answer(
            &slot,
            Box::new(move |_| {
                replace_answer(&again, Box::new(|_| {}));
                flag.set(true);
            }),
        );
        replace_answer(&slot, Box::new(|_| {}));
        assert!(told.get());
        assert!(slot.borrow().is_some());
    }

    #[test]
    fn the_sheets_save_waits_for_a_free_window_but_nothing_else_does() {
        assert!(sheet_save_allowed(true, false));
        assert!(!sheet_save_allowed(true, true), "a write already runs");
        assert!(!sheet_save_allowed(false, false), "nothing to write");
    }

    #[test]
    fn the_large_page_fits_its_shape_inside_the_inset_box() {
        // A portrait page (1:2) in a 432 × 632 pt box, inset 16 pt: the
        // height limits it, so its long side is 600 pt.
        assert_eq!(large_side_px(432.0, 632.0, Some((100, 200)), 1.0), 600);
        assert_eq!(large_side_px(432.0, 632.0, Some((100, 200)), 2.0), 1200);
        // A landscape page (2:1) in the same box: the width limits it.
        assert_eq!(large_side_px(432.0, 632.0, Some((200, 100)), 1.0), 400);
        // A wide window does not make a portrait page wider than it fits.
        assert_eq!(large_side_px(3032.0, 632.0, Some((100, 200)), 1.0), 600);
    }

    #[test]
    fn the_large_page_is_capped() {
        // A portrait page on a maximised 5K window at 2×: its height.
        assert_eq!(large_side_px(2560.0, 1400.0, Some((100, 141)), 2.0), 2736);
        // A landscape page there would be 5056 pixels wide: capped.
        assert_eq!(
            large_side_px(2560.0, 1440.0, Some((200, 100)), 2.0),
            LARGE_MAX_PX
        );
        assert_eq!(large_side_px(9000.0, 9000.0, None, 2.0), LARGE_MAX_PX);
    }

    #[test]
    fn the_large_page_without_a_shape_uses_the_longer_side() {
        assert_eq!(large_side_px(432.0, 632.0, None, 1.0), 600);
        assert_eq!(large_side_px(432.0, 632.0, Some((0, 0)), 1.0), 600);
        assert_eq!(large_side_px(10.0, 10.0, None, 1.0), 1, "never zero");
    }

    #[test]
    fn a_save_closes_the_window_only_when_it_wrote_the_grid_as_it_is() {
        let written = [page(0, 0, 0), page(0, 1, 0)];
        assert!(closes_after_save(&written, &written, false));
        let grown = [page(0, 0, 0), page(0, 1, 0), page(1, 0, 0)];
        assert!(
            !closes_after_save(&written, &grown, false),
            "pages added while it wrote"
        );
        assert!(
            !closes_after_save(&written, &written, true),
            "added files are still being read"
        );
    }
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
    fn cut_mode_summary() {
        assert_eq!(
            cut_summary(8, &[2, 5]),
            ("1–2 | 3–5 | 6–8".into(), "Split into 3".into())
        );
        assert_eq!(cut_summary(8, &[]).1, "Split into 1");
    }

    #[test]
    fn cut_summary_of_single_pages_and_stray_cuts() {
        assert_eq!(cut_summary(8, &[]).0, "1–8");
        assert_eq!(cut_summary(3, &[1, 2]).0, "1 | 2 | 3");
        // Cuts out of range or repeated change nothing.
        assert_eq!(cut_summary(3, &[0, 2, 2, 9]).0, "1–2 | 3");
        assert_eq!(
            cut_footer("1–2 | 3"),
            "Click between pages to add or remove a cut · 1–2 | 3"
        );
    }

    #[test]
    fn a_click_in_a_gap_finds_its_cut() {
        // 4 columns: gap lines at x = 125, 235, 345 (pad 20, cell 100, gap 10).
        let m = metrics(4, false);
        assert_eq!(cut_at(&m, 125.0, 100.0, 10), Some(1));
        assert_eq!(cut_at(&m, 125.0 + CUT_REACH, 100.0, 10), Some(1));
        assert_eq!(cut_at(&m, 237.0, 100.0, 10), Some(2));
        // The middle of a page is far from every gap.
        assert_eq!(cut_at(&m, 180.0, 100.0, 10), None);
        // Before the first page and after the last there is nothing to cut.
        assert_eq!(cut_at(&m, 15.0, 100.0, 10), None);
        // Second row (y 180..330): its start is the cut after page 4.
        assert_eq!(cut_at(&m, 15.0, 250.0, 10), Some(4));
        assert_eq!(cut_at(&m, 455.0, 100.0, 10), Some(4));
        // Third row holds pages 9 and 10: after page 10 is the end.
        assert_eq!(cut_at(&m, 125.0, 400.0, 10), Some(9));
        assert_eq!(cut_at(&m, 235.0, 400.0, 10), None);
        // Below the last row.
        assert_eq!(cut_at(&m, 125.0, 700.0, 10), None);
        assert_eq!(cut_at(&m, 125.0, 100.0, 1), None);
    }

    #[test]
    fn cuts_mirror_right_to_left() {
        // Content width 470: the first gap is at 470 - 125 = 345.
        let m = metrics(4, true);
        assert_eq!(cut_at(&m, 345.0, 100.0, 10), Some(1));
        assert_eq!(cut_at(&m, 125.0, 100.0, 10), Some(3));
    }

    #[test]
    fn cut_markers_stand_after_the_page_before_the_cut() {
        let m = metrics(4, false);
        assert_eq!(cut_marker(&m, 1, 10), (125.0, 20.0));
        // A cut at a row's start is drawn at the end of the row above.
        assert_eq!(cut_marker(&m, 4, 10), (455.0, 20.0));
        assert_eq!(cut_marker(&m, 5, 10), (125.0, 180.0));
        let rtl = metrics(4, true);
        assert_eq!(cut_marker(&rtl, 1, 10), (345.0, 20.0));
    }

    #[test]
    fn cuts_toggle_and_stay_sorted() {
        let mut cuts = Vec::new();
        toggle_cut(&mut cuts, 5);
        toggle_cut(&mut cuts, 2);
        assert_eq!(cuts, [2, 5]);
        toggle_cut(&mut cuts, 5);
        assert_eq!(cuts, [2]);
    }

    #[test]
    fn split_choices_word_their_files() {
        assert_eq!(split_n_range(8), (2, 7));
        assert_eq!(split_n_range(2), (2, 2));
        assert_eq!(split_n_range(1), (2, 2));
        assert_eq!(split_heading("report.pdf"), "Split report.pdf");
        assert_eq!(every_n_text(2), "Every 2 pages");
        assert_eq!(files_text(4), "→ 4 files");
        assert_eq!(files_text(1), "→ 1 file");
    }

    #[test]
    fn popovers_name_the_pages_they_take() {
        assert_eq!(pages_text(&[2, 3, 4]), "Pages 3–5");
        assert_eq!(pages_text(&[0, 2]), "Pages 1, 3");
        assert_eq!(pages_text(&[6]), "Page 7");
    }

    #[test]
    fn images_popover_texts() {
        assert_eq!(
            images_texts(1),
            ("1 page to image".into(), "Save image".into())
        );
        assert_eq!(
            images_texts(8),
            ("8 pages to images".into(), "Save images".into())
        );
    }

    #[test]
    fn image_settings_pick_the_options() {
        let o = image_options("jpeg", 150);
        assert_eq!((o.kind, o.dpi), (ImageKind::Jpeg, 150));
        let o = image_options("png", 300);
        assert_eq!((o.kind, o.dpi), (ImageKind::Png, 300));
        let o = image_options("gif", 72);
        assert_eq!((o.kind, o.dpi), (ImageKind::Png, 300));
        assert_eq!(format_key(ImageKind::Jpeg), "jpeg");
        assert_eq!(format_key(ImageKind::Png), "png");
    }

    #[test]
    fn job_fraction_needs_several_files() {
        assert_eq!(job_fraction(0, 0), None);
        assert_eq!(job_fraction(0, 1), None);
        assert_eq!(job_fraction(1, 4), Some(0.25));
        assert_eq!(job_fraction(5, 4), Some(1.0));
    }

    #[test]
    fn writing_texts_in_arabic() {
        with_language("ar", || {
            let (pieces, button) = cut_summary(8, &[2, 5]);
            assert_eq!(pieces, "1–2 | 3–5 | 6–8");
            assert!(!button.starts_with("Split"));
            assert!(button.contains('3'));
            assert!(!cut_footer("1–8").starts_with("Click"));
            assert!(!pages_text(&[2, 3, 4]).starts_with("Pages"));
            assert!(!every_n_text(2).starts_with("Every"));
            assert!(!files_text(4).contains("files"));
            assert!(!images_texts(3).0.contains("pages"));
            assert!(!images_texts(3).1.starts_with("Save"));
            assert!(split_heading("r.pdf").contains("r.pdf"));
            assert!(!split_heading("r.pdf").starts_with("Split"));
        });
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
        assert_eq!(insert_point(Some(2), true, 8), 3);
        assert_eq!(insert_point(None, true, 8), 8);
    }

    #[test]
    fn add_ignores_a_focus_that_shows_no_ring() {
        // A click moves the focus without showing it: the pages go at the end.
        assert_eq!(insert_point(Some(2), false, 8), 8);
        assert_eq!(insert_point(None, false, 8), 8);
    }

    #[test]
    fn removed_banner() {
        assert_eq!(removed_text(1), "1 page removed");
        assert_eq!(removed_text(3), "3 pages removed");
    }

    #[test]
    fn add_after_a_stale_focus_goes_to_the_end() {
        assert_eq!(insert_point(Some(7), true, 8), 8);
        assert_eq!(insert_point(Some(20), true, 8), 8);
        assert_eq!(insert_point(None, true, 0), 0);
        assert_eq!(insert_point(Some(0), true, 0), 0);
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
    fn a_source_row_selects_its_pages_wherever_they_are() {
        let pages = [page(1, 0, 0), page(0, 0, 0), page(1, 2, 1), page(2, 0, 0)];
        assert_eq!(pages_of_source(&pages, 1), vec![0, 2]);
        assert_eq!(pages_of_source(&pages, 2), vec![3]);
        assert_eq!(pages_of_source(&pages, 4), Vec::<usize>::new());
    }

    #[test]
    fn the_files_sidebar_opens_when_files_come_to_be_several() {
        assert!(!opens_files(0, 1), "one file: no sidebar");
        assert!(opens_files(0, 3), "opened with several");
        assert!(opens_files(1, 2), "a file was added");
        assert!(!opens_files(2, 2), "nothing new: left as it was");
        assert!(!opens_files(3, 2), "a file went: left as it was");
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
