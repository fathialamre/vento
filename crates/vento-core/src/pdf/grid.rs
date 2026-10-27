//! Geometry of the page grid: how many columns fit, which page is under a
//! point, where a dragged page would land, and which rows are on screen.
//!
//! All coordinates are in the grid's content space: the origin is the top
//! left of the grid area and `y` already includes the scroll offset.

use std::ops::Range;

/// The measurements of a grid of equally sized page cells.
///
/// Column `c` (left to right) spans `pad + c * (cell_w + gap)` for `cell_w`;
/// row `r` spans `pad + r * (cell_h + gap)` for `cell_h`. Right to left, page
/// positions fill each row from the right: column `c` sits where column
/// `columns - 1 - c` would sit left to right, inside the same
/// [`content_width`](Self::content_width).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMetrics {
    /// Cells per row, at least 1.
    pub columns: usize,
    /// Width of one cell.
    pub cell_w: f32,
    /// Height of one cell.
    pub cell_h: f32,
    /// Space between neighbouring cells, across and down.
    pub gap: f32,
    /// Space between the grid's edge and the outer cells.
    pub pad: f32,
    /// Whether the reading order runs right to left.
    pub rtl: bool,
}

impl GridMetrics {
    /// Picks the most columns whose width, `2 * pad + n * cell_w +
    /// (n - 1) * gap`, fits in `width`; never fewer than one.
    pub fn fit(width: f32, cell_w: f32, cell_h: f32, gap: f32, pad: f32, rtl: bool) -> GridMetrics {
        let stride = cell_w + gap;
        let fitting = ((width - 2.0 * pad + gap) / stride).floor();
        // A negative or NaN count saturates to 0 in the cast.
        let columns = (fitting as usize).max(1);
        GridMetrics {
            columns,
            cell_w,
            cell_h,
            gap,
            pad,
            rtl,
        }
    }

    /// The width the columns occupy, padding included.
    pub fn content_width(&self) -> f32 {
        2.0 * self.pad + self.columns as f32 * self.cell_w + (self.columns - 1) as f32 * self.gap
    }

    /// The number of rows needed for `count` pages.
    pub fn rows(&self, count: usize) -> usize {
        count.div_ceil(self.columns)
    }

    /// The page whose cell contains the point, or `None` over a gap, the
    /// padding, or a cell with no page in it.
    pub fn index_at(&self, x: f32, y: f32, count: usize) -> Option<usize> {
        let col = cell_along(self.reading_x(x), self.pad, self.cell_w, self.gap)?;
        let row = cell_along(y, self.pad, self.cell_h, self.gap)?;
        let index = row * self.columns + col;
        (col < self.columns && index < count).then_some(index)
    }

    /// Where a page dropped at the point would be inserted, `0..=count`: the
    /// gap nearest the pointer in the row it is over. Within a row the cut
    /// is the middle of a cell, so the leading half (left, or right in RTL)
    /// puts the page before it and the trailing half after it. Below the
    /// last row the page goes to the end.
    pub fn insertion_index(&self, x: f32, y: f32, count: usize) -> usize {
        let rows = self.rows(count);
        let row_stride = self.cell_h + self.gap;
        let last_bottom = self.pad + (rows as f32 - 1.0) * row_stride + self.cell_h;
        if rows == 0 || y >= last_bottom {
            return count;
        }
        // The gap between two rows belongs to the nearer one.
        let row = (((y - self.pad + self.gap / 2.0) / row_stride)
            .floor()
            .max(0.0) as usize)
            .min(rows - 1);
        let start = row * self.columns;
        let in_row = (count - start).min(self.columns);
        // Gap `j` lies between the centres of cells `j - 1` and `j`.
        let stride = self.cell_w + self.gap;
        let past_centres = (self.reading_x(x) - self.pad - self.cell_w / 2.0) / stride;
        let gap = (past_centres.floor() + 1.0).clamp(0.0, in_row as f32) as usize;
        start + gap
    }

    /// The pages on rows that intersect the vertical window
    /// `[scroll_y, scroll_y + height)`, clamped to `count`.
    pub fn rows_visible(&self, scroll_y: f32, height: f32, count: usize) -> Range<usize> {
        let row_stride = self.cell_h + self.gap;
        // First row whose bottom edge is below the window's top.
        let above = (scroll_y - self.pad - self.cell_h) / row_stride;
        let first_row = if above < 0.0 {
            0
        } else {
            above.floor() as usize + 1
        };
        // One past the last row whose top edge is above the window's bottom.
        let below = (scroll_y + height - self.pad) / row_stride;
        let end_row = if below <= 0.0 {
            0
        } else {
            below.ceil() as usize
        };
        let start = (first_row * self.columns).min(count);
        let end = (end_row * self.columns).min(count).max(start);
        start..end
    }

    /// Mirrors `x` across the columns when reading right to left, so the
    /// left to right column maths also holds for right to left.
    fn reading_x(&self, x: f32) -> f32 {
        if self.rtl {
            self.content_width() - x
        } else {
            x
        }
    }
}

/// The index of the cell `pos` falls in along one axis, or `None` over the
/// padding before the first cell or a gap between cells.
fn cell_along(pos: f32, pad: f32, size: f32, gap: f32) -> Option<usize> {
    let rel = pos - pad;
    if rel < 0.0 {
        return None;
    }
    let stride = size + gap;
    let cell = (rel / stride).floor();
    (rel - cell * stride < size).then_some(cell as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(rtl: bool) -> GridMetrics {
        GridMetrics::fit(
            4.0 * 100.0 + 3.0 * 10.0 + 2.0 * 14.0,
            100.0,
            140.0,
            10.0,
            14.0,
            rtl,
        )
    }

    #[test]
    fn fit_counts_columns() {
        assert_eq!(m(false).columns, 4);
        assert_eq!(
            GridMetrics::fit(50.0, 100.0, 140.0, 10.0, 14.0, false).columns,
            1
        );
    }

    #[test]
    fn fit_one_pixel_short_drops_a_column() {
        let w = 4.0 * 100.0 + 3.0 * 10.0 + 2.0 * 14.0;
        assert_eq!(
            GridMetrics::fit(w - 1.0, 100.0, 140.0, 10.0, 14.0, false).columns,
            3
        );
        assert_eq!(
            GridMetrics::fit(w + 50.0, 100.0, 140.0, 10.0, 14.0, false).columns,
            4
        );
    }

    #[test]
    fn insertion_between_first_two() {
        assert_eq!(m(false).insertion_index(14.0 + 100.0 + 5.0, 50.0, 8), 1);
    }

    #[test]
    fn insertion_past_end() {
        assert_eq!(m(false).insertion_index(400.0, 1000.0, 8), 8);
    }

    #[test]
    fn insertion_halves_of_a_cell() {
        let g = m(false);
        // Cell 1 spans 124..224, centre 174.
        assert_eq!(g.insertion_index(150.0, 50.0, 8), 1);
        assert_eq!(g.insertion_index(200.0, 50.0, 8), 2);
        // Left of everything and right of a full row stay in that row.
        assert_eq!(g.insertion_index(0.0, 50.0, 8), 0);
        assert_eq!(g.insertion_index(457.0, 50.0, 8), 4);
        // Second row.
        assert_eq!(g.insertion_index(0.0, 14.0 + 150.0 + 20.0, 8), 4);
        assert_eq!(g.insertion_index(457.0, 14.0 + 150.0 + 20.0, 8), 8);
    }

    #[test]
    fn insertion_clamps_to_a_short_last_row() {
        let g = m(false);
        // 6 pages: the second row holds pages 4 and 5 only.
        let y = 14.0 + 150.0 + 20.0;
        assert_eq!(g.insertion_index(457.0, y, 6), 6);
        assert_eq!(g.insertion_index(0.0, y, 6), 4);
    }

    #[test]
    fn insertion_above_the_grid_and_with_no_pages() {
        assert_eq!(m(false).insertion_index(20.0, -30.0, 8), 0);
        assert_eq!(m(false).insertion_index(100.0, 20.0, 0), 0);
    }

    #[test]
    fn insertion_in_a_row_gap_picks_the_nearer_row() {
        let g = m(false);
        // Row 0 ends at 154, row 1 starts at 164.
        assert_eq!(g.insertion_index(0.0, 156.0, 8), 0);
        assert_eq!(g.insertion_index(0.0, 162.0, 8), 4);
    }

    #[test]
    fn insertion_rtl_leads_from_the_right() {
        let g = m(true);
        // Mirror of the LTR case: between the first two pages from the right.
        assert_eq!(g.insertion_index(458.0 - 119.0, 50.0, 8), 1);
        assert_eq!(g.insertion_index(457.0, 50.0, 8), 0);
        assert_eq!(g.insertion_index(0.0, 50.0, 8), 4);
        // Pointer on the leading (right) half of page 0 goes before it.
        assert_eq!(g.insertion_index(430.0, 50.0, 8), 0);
        // On the trailing (left) half goes after it.
        assert_eq!(g.insertion_index(370.0, 50.0, 8), 1);
    }

    #[test]
    fn rtl_mirrors_columns() {
        assert_eq!(m(true).index_at(440.0, 50.0, 8), Some(0));
        assert_eq!(m(false).index_at(440.0, 50.0, 8), Some(3));
    }

    #[test]
    fn index_at_is_none_off_the_cells() {
        let g = m(false);
        assert_eq!(g.index_at(5.0, 50.0, 8), None); // left padding
        assert_eq!(g.index_at(119.0, 50.0, 8), None); // column gap
        assert_eq!(g.index_at(50.0, 158.0, 8), None); // row gap
        assert_eq!(g.index_at(50.0, 5.0, 8), None); // top padding
        assert_eq!(g.index_at(50.0, 1000.0, 8), None); // below
        assert_eq!(g.index_at(50.0, 50.0, 8), Some(0));
        assert_eq!(g.index_at(50.0, 14.0 + 150.0 + 10.0, 8), Some(4));
    }

    #[test]
    fn index_at_is_none_past_the_last_page() {
        let g = m(false);
        let y = 14.0 + 150.0 + 10.0;
        assert_eq!(g.index_at(14.0 + 110.0 + 10.0, y, 6), Some(5));
        assert_eq!(g.index_at(14.0 + 220.0 + 10.0, y, 6), None);
        // RTL: the short row sits at the right, so the left is empty.
        assert_eq!(m(true).index_at(20.0, y, 6), None);
        assert_eq!(m(true).index_at(440.0, y, 6), Some(4));
    }

    #[test]
    fn rows_visible_picks_intersecting_rows() {
        let g = m(false);
        // Rows: 14..154, 164..304, 314..454.
        assert_eq!(g.rows_visible(0.0, 100.0, 12), 0..4);
        assert_eq!(g.rows_visible(0.0, 160.0, 12), 0..4);
        assert_eq!(g.rows_visible(0.0, 165.0, 12), 0..8);
        assert_eq!(g.rows_visible(153.0, 20.0, 12), 0..8);
        assert_eq!(g.rows_visible(154.0, 10.0, 12), 4..4);
    }

    #[test]
    fn rows_visible_clamps_and_handles_empty() {
        let g = m(false);
        assert_eq!(g.rows_visible(0.0, 5000.0, 6), 0..6);
        assert_eq!(g.rows_visible(0.0, 100.0, 0), 0..0);
        assert_eq!(g.rows_visible(5000.0, 100.0, 6), 6..6);
        assert_eq!(g.rows_visible(0.0, 0.0, 6), 0..0);
    }
}
