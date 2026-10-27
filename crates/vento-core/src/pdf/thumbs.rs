//! The order in which a window renders page thumbnails: what is on screen
//! first, then the pages just outside it, and which finished ones are far
//! enough away to free.

use std::ops::Range;

/// Tracks which page positions have a thumbnail and which to render next.
///
/// It works on grid positions, so after any edit that reorders, deletes or
/// rotates pages the caller resizes it and invalidates everything; the
/// caller's own cache keyed by `(source, page, turns)` makes re-queued pages
/// that were already rendered cheap.
pub struct ThumbQueue {
    done: Vec<bool>,
    visible: Range<usize>,
    margin: usize,
}

impl ThumbQueue {
    /// A queue over `count` pages that keeps `margin` pages either side of
    /// the visible range warm. Nothing is visible until [`set_visible`].
    ///
    /// [`set_visible`]: Self::set_visible
    pub fn new(count: usize, margin: usize) -> ThumbQueue {
        ThumbQueue {
            done: vec![false; count],
            visible: 0..0,
            margin,
        }
    }

    /// Sets the range of positions on screen, clamped to the page count.
    pub fn set_visible(&mut self, range: Range<usize>) {
        let end = range.end.min(self.done.len());
        self.visible = range.start.min(end)..end;
    }

    /// The next page to render, or `None` when everything visible and within
    /// the margin is done. Visible pages come first in reading order, then
    /// the margin pages nearest the visible range first (after the range
    /// before the same distance before it). It does not mark the page, so
    /// the caller reports each result with [`mark_done`]; until then the
    /// same page is offered again.
    ///
    /// [`mark_done`]: Self::mark_done
    // Not an `Iterator`: the queue is refilled by `set_visible` and can
    // yield again after `invalidate`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<usize> {
        if self.visible.is_empty() {
            return None;
        }
        let Range { start, end } = self.visible;
        if let Some(i) = (start..end).find(|&i| !self.done[i]) {
            return Some(i);
        }
        (1..=self.margin).find_map(|d| {
            let after = end - 1 + d;
            let before = start.checked_sub(d);
            (after < self.done.len() && !self.done[after])
                .then_some(after)
                .or_else(|| before.filter(|&b| !self.done[b]))
        })
    }

    /// Records that position `i` now has a thumbnail.
    pub fn mark_done(&mut self, i: usize) {
        if let Some(d) = self.done.get_mut(i) {
            *d = true;
        }
    }

    /// Forgets the thumbnail of position `i`, so it is offered again.
    pub fn invalidate(&mut self, i: usize) {
        if let Some(d) = self.done.get_mut(i) {
            *d = false;
        }
    }

    /// Forgets every thumbnail.
    pub fn invalidate_all(&mut self) {
        self.done.fill(false);
    }

    /// Finished positions further than the margin from the visible range,
    /// ascending. The caller frees their images and calls [`invalidate`] on
    /// each; this does not change the queue. While nothing is visible, as
    /// before the first layout, nothing is dropped.
    ///
    /// [`invalidate`]: Self::invalidate
    pub fn to_drop(&self) -> Vec<usize> {
        if self.visible.is_empty() {
            return Vec::new();
        }
        let keep = self.visible.start.saturating_sub(self.margin)
            ..self.visible.end.saturating_add(self.margin);
        (0..self.done.len())
            .filter(|i| self.done[*i] && !keep.contains(i))
            .collect()
    }

    /// Changes the page count. Marks for positions that remain are kept; new
    /// positions start undone.
    pub fn resize(&mut self, count: usize) {
        self.done.resize(count, false);
        self.set_visible(self.visible.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_queue_serves_visible_first() {
        let mut q = ThumbQueue::new(300, 8);
        q.set_visible(100..112);
        assert_eq!(q.next(), Some(100));
        for i in 100..112 {
            q.mark_done(i);
        }
        let n = q.next().unwrap();
        assert!((92..100).contains(&n) || (112..120).contains(&n));
    }

    #[test]
    fn thumb_queue_drops_far_pages() {
        let mut q = ThumbQueue::new(300, 8);
        q.set_visible(0..12);
        for i in 0..20 {
            q.mark_done(i);
        }
        q.set_visible(200..212);
        assert_eq!(q.to_drop(), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn visible_pages_come_in_reading_order_and_skip_done() {
        let mut q = ThumbQueue::new(50, 4);
        q.set_visible(10..14);
        q.mark_done(10);
        q.mark_done(12);
        assert_eq!(q.next(), Some(11));
        q.mark_done(11);
        assert_eq!(q.next(), Some(13));
    }

    #[test]
    fn margin_is_nearest_first_after_before_before() {
        let mut q = ThumbQueue::new(50, 3);
        q.set_visible(10..12);
        for i in 10..12 {
            q.mark_done(i);
        }
        let mut order = Vec::new();
        while let Some(i) = q.next() {
            order.push(i);
            q.mark_done(i);
        }
        assert_eq!(order, vec![12, 9, 13, 8, 14, 7]);
    }

    #[test]
    fn nothing_left_is_none_and_far_pages_are_not_offered() {
        let mut q = ThumbQueue::new(50, 2);
        q.set_visible(10..12);
        for i in 8..14 {
            q.mark_done(i);
        }
        assert_eq!(q.next(), None);
    }

    #[test]
    fn margin_stops_at_both_ends_of_the_document() {
        let mut q = ThumbQueue::new(5, 3);
        q.set_visible(0..2);
        q.mark_done(0);
        q.mark_done(1);
        let mut order = Vec::new();
        while let Some(i) = q.next() {
            order.push(i);
            q.mark_done(i);
        }
        assert_eq!(order, vec![2, 3, 4]);
        q.set_visible(3..5);
        q.mark_done(3);
        assert_eq!(q.next(), None);
        q.invalidate(2);
        assert_eq!(q.next(), Some(2));
    }

    #[test]
    fn invalidate_requeues_and_invalidate_all_clears() {
        let mut q = ThumbQueue::new(10, 1);
        q.set_visible(0..2);
        q.mark_done(0);
        q.mark_done(1);
        q.mark_done(2);
        assert_eq!(q.next(), None);
        q.invalidate(1);
        assert_eq!(q.next(), Some(1));
        q.mark_done(1);
        q.invalidate_all();
        assert_eq!(q.next(), Some(0));
    }

    #[test]
    fn to_drop_does_not_mutate_and_keeps_the_margin() {
        let mut q = ThumbQueue::new(100, 5);
        q.set_visible(50..60);
        for i in [10, 44, 45, 64, 65, 90] {
            q.mark_done(i);
        }
        assert_eq!(q.to_drop(), vec![10, 44, 65, 90]);
        assert_eq!(q.to_drop(), vec![10, 44, 65, 90]);
        q.invalidate(10);
        assert_eq!(q.to_drop(), vec![44, 65, 90]);
    }

    #[test]
    fn nothing_visible_means_no_work_and_no_drops() {
        let mut q = ThumbQueue::new(10, 2);
        q.mark_done(3);
        assert_eq!(q.next(), None);
        assert!(q.to_drop().is_empty());
    }

    #[test]
    fn resize_keeps_marks_and_clamps_the_visible_range() {
        let mut q = ThumbQueue::new(10, 1);
        q.set_visible(6..10);
        q.mark_done(6);
        q.resize(8);
        assert_eq!(q.next(), Some(7));
        q.mark_done(7);
        assert_eq!(q.next(), Some(5));
        // New positions start undone; the visible range stays as clamped.
        q.resize(12);
        q.mark_done(5);
        assert_eq!(q.next(), Some(8));
        q.mark_done(8);
        assert_eq!(q.next(), None);
        q.set_visible(6..12);
        assert_eq!(q.next(), Some(9));
        q.resize(0);
        assert_eq!(q.next(), None);
    }

    #[test]
    fn out_of_range_marks_are_ignored() {
        let mut q = ThumbQueue::new(3, 1);
        q.mark_done(9);
        q.invalidate(9);
        q.set_visible(1..99);
        assert_eq!(q.next(), Some(1));
    }
}
