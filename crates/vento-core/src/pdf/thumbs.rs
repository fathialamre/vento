//! The order in which a window renders page thumbnails: what is on screen
//! first, then the pages just outside it, and which finished ones are far
//! enough away to free.

use std::ops::Range;

/// Where a page is on its way to having a thumbnail.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    /// Not handed out yet.
    Waiting,
    /// Handed out by [`ThumbQueue::next`]; the result has not come back.
    InFlight,
    /// The caller reported the result with [`ThumbQueue::mark_done`].
    Done,
}

/// Tracks which page positions have a thumbnail and which to render next.
///
/// Every position is waiting, in flight or done. [`next`](Self::next) hands
/// each waiting page out once and marks it in flight; the caller reports
/// every result, a failed render included, with [`mark_done`](Self::mark_done).
/// Only [`invalidate`](Self::invalidate), [`invalidate_all`](Self::invalidate_all)
/// and [`resize`](Self::resize) make a page wait again.
///
/// It works on grid positions, so after any edit that reorders, deletes or
/// rotates pages the caller resizes it and invalidates everything; the
/// caller's own cache keyed by `(source, page, turns)` makes re-queued pages
/// that were already rendered cheap.
#[derive(Debug)]
pub struct ThumbQueue {
    state: Vec<State>,
    visible: Range<usize>,
    margin: usize,
}

impl ThumbQueue {
    /// A queue over `count` waiting pages that keeps `margin` pages either
    /// side of the visible range warm. Nothing is visible until
    /// [`set_visible`](Self::set_visible).
    pub fn new(count: usize, margin: usize) -> ThumbQueue {
        ThumbQueue {
            state: vec![State::Waiting; count],
            visible: 0..0,
            margin,
        }
    }

    /// Sets the range of positions on screen, clamped to the page count.
    pub fn set_visible(&mut self, range: Range<usize>) {
        let end = range.end.min(self.state.len());
        self.visible = range.start.min(end)..end;
    }

    /// Hands out the next waiting page and marks it in flight, or returns
    /// `None` when every page that is visible or within the margin has been
    /// handed out. Visible pages come first in reading order, then the
    /// margin pages nearest the visible range first (after the range before
    /// the one the same distance before it). A page is offered once, so
    /// `while let Some(i) = q.next() { send(i) }` ends; it is offered again
    /// only after [`invalidate`](Self::invalidate).
    // Not an `Iterator`: the queue is refilled by `set_visible` and can
    // yield again after `invalidate`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<usize> {
        let i = self.peek()?;
        self.state[i] = State::InFlight;
        Some(i)
    }

    /// The page [`next`](Self::next) would hand out.
    fn peek(&self) -> Option<usize> {
        let Range { start, end } = self.visible;
        if start == end {
            return None;
        }
        let waiting = |i: usize| self.state[i] == State::Waiting;
        if let Some(i) = (start..end).find(|&i| waiting(i)) {
            return Some(i);
        }
        // Beyond this distance both sides are off the ends of the list.
        let reach = self.margin.min(start.max(self.state.len() - end));
        (1..=reach).find_map(|d| {
            let after = (end - 1).saturating_add(d);
            let before = start.checked_sub(d);
            (after < self.state.len() && waiting(after))
                .then_some(after)
                .or_else(|| before.filter(|&b| waiting(b)))
        })
    }

    /// Records that position `i` came back from its render, whether or not
    /// the render worked.
    pub fn mark_done(&mut self, i: usize) {
        if let Some(s) = self.state.get_mut(i) {
            *s = State::Done;
        }
    }

    /// Forgets position `i`, in flight or done, so it is offered again.
    pub fn invalidate(&mut self, i: usize) {
        if let Some(s) = self.state.get_mut(i) {
            *s = State::Waiting;
        }
    }

    /// Forgets every position, so all are offered again.
    pub fn invalidate_all(&mut self) {
        self.state.fill(State::Waiting);
    }

    /// Done positions (never in-flight ones) further than the margin from
    /// the visible range, ascending. The caller frees their images and calls [`invalidate`] on
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
        (0..self.state.len())
            .filter(|i| self.state[*i] == State::Done && !keep.contains(i))
            .collect()
    }

    /// Changes the page count. Positions that remain keep their state; new
    /// positions wait.
    pub fn resize(&mut self, count: usize) {
        self.state.resize(count, State::Waiting);
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
        assert_eq!(q.next(), Some(13));
    }

    #[test]
    fn a_feeder_loop_ends_after_each_page_once() {
        let mut q = ThumbQueue::new(300, 8);
        q.set_visible(0..12);
        let mut sent = Vec::new();
        while let Some(i) = q.next() {
            sent.push(i);
        }
        assert_eq!(sent, (0..20).collect::<Vec<_>>());
        assert_eq!(q.next(), None);
    }

    #[test]
    fn an_in_flight_page_is_not_offered_again() {
        let mut q = ThumbQueue::new(10, 0);
        q.set_visible(2..4);
        assert_eq!(q.next(), Some(2));
        assert_eq!(q.next(), Some(3));
        assert_eq!(q.next(), None);
        q.mark_done(2);
        assert_eq!(q.next(), None);
    }

    #[test]
    fn invalidate_offers_an_in_flight_or_done_page_again() {
        let mut q = ThumbQueue::new(10, 0);
        q.set_visible(2..4);
        assert_eq!(q.next(), Some(2));
        assert_eq!(q.next(), Some(3));
        q.mark_done(3);
        q.invalidate(2);
        assert_eq!(q.next(), Some(2));
        q.invalidate_all();
        assert_eq!(q.next(), Some(2));
        assert_eq!(q.next(), Some(3));
    }

    #[test]
    fn to_drop_ignores_in_flight_pages() {
        let mut q = ThumbQueue::new(100, 2);
        q.set_visible(10..12);
        q.mark_done(1);
        q.set_visible(50..52);
        assert_eq!(q.next(), Some(50));
        assert_eq!(q.to_drop(), vec![1]);
    }

    #[test]
    fn a_huge_margin_does_not_overflow() {
        let mut q = ThumbQueue::new(6, usize::MAX);
        q.set_visible(2..4);
        let mut sent = Vec::new();
        while let Some(i) = q.next() {
            sent.push(i);
        }
        assert_eq!(sent, vec![2, 3, 4, 1, 5, 0]);
        assert!(q.to_drop().is_empty());
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
