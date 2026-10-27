//! A PDF window's thumbnails: the thread that draws them and the
//! bookkeeping of which page to ask for next, which result still fits the
//! grid, and which images to keep.
//!
//! The thread owns a [`PdfRenderer`], which is not `Send`, so it is made
//! on the thread itself and dies with it: dropping the [`ThumbWorker`]
//! closes its queue and the thread ends after the page it is drawing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use log::warn;
use slint::{Rgba8Pixel, SharedPixelBuffer};
use vento_core::pdf::{PageRef, ThumbQueue};
use vento_platform::{PdfRenderer, RgbaImage};

/// A drawn page, ready to become a `slint::Image` on the main thread, or
/// `None` for a page that could not be drawn.
pub type Rendered = Option<SharedPixelBuffer<Rgba8Pixel>>;

/// Called on the thumbnail thread with each result: the request's id, its
/// page and the drawing. It hands the result to the main thread.
type Deliver = dyn Fn(u64, PageRef, Rendered) + Send + Sync;

/// One page to draw.
struct Request {
    id: u64,
    page: PageRef,
    max_px: u32,
}

/// The window's thumbnail thread. Requests are drawn one at a time, in the
/// order they were made.
pub struct ThumbWorker {
    /// `None` when the thread could not start.
    requests: Option<Sender<Request>>,
    deliver: Arc<Deliver>,
}

/// Starts the thumbnail thread for a window's sources (their bytes, in the
/// window's order). Each result goes to `deliver` on that thread; a request
/// the thread cannot take (it never started, or it is gone) is answered at
/// once as a failed drawing, so no page waits for ever.
pub fn spawn(
    sources: Vec<Arc<[u8]>>,
    deliver: impl Fn(u64, PageRef, Rendered) + Send + Sync + 'static,
) -> ThumbWorker {
    let deliver: Arc<Deliver> = Arc::new(deliver);
    let (tx, rx) = mpsc::channel::<Request>();
    let on_thread = deliver.clone();
    let spawned = std::thread::Builder::new()
        .name("vento-pdf-thumbs".into())
        .spawn(move || {
            let renderer = match PdfRenderer::new(&sources) {
                Ok(renderer) => Some(renderer),
                Err(e) => {
                    warn!("cannot open the PDFs for thumbnails: {e}");
                    None
                }
            };
            // Ends when the worker is dropped and the queue is empty.
            while let Ok(request) = rx.recv() {
                let drawn = renderer.as_ref().and_then(|renderer| {
                    catch_unwind(AssertUnwindSafe(|| {
                        renderer.render(request.page, request.max_px)
                    }))
                    .unwrap_or_else(|_| {
                        warn!("drawing a PDF page panicked");
                        None
                    })
                });
                on_thread(request.id, request.page, drawn.and_then(to_buffer));
            }
        });
    let requests = match spawned {
        Ok(_) => Some(tx),
        Err(e) => {
            warn!("cannot start the PDF thumbnail thread: {e}");
            None
        }
    };
    ThumbWorker { requests, deliver }
}

impl ThumbWorker {
    /// Asks for `page` drawn with its longer side `max_px` pixels; the
    /// result comes back with `id`.
    pub fn request(&self, id: u64, page: PageRef, max_px: u32) {
        let sent = self
            .requests
            .as_ref()
            .is_some_and(|tx| tx.send(Request { id, page, max_px }).is_ok());
        if !sent {
            (self.deliver)(id, page, None);
        }
    }
}

/// The pixels of a drawn page as a Slint buffer, or `None` when they do not
/// add up.
fn to_buffer(image: RgbaImage) -> Rendered {
    let len = image.width as usize * image.height as usize * 4;
    if len == 0 || image.pixels.len() != len {
        return None;
    }
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &image.pixels,
        image.width,
        image.height,
    ))
}

/// What the window does after a change to its thumbnails: rows to empty,
/// rows to show an image in (`None`: the page could not be drawn), and
/// pages to ask the worker for, with their ticket.
#[derive(Debug, PartialEq)]
pub struct Plan<T> {
    pub clear: Vec<usize>,
    pub fill: Vec<(usize, Option<T>)>,
    pub send: Vec<(u64, PageRef)>,
}

impl<T> Default for Plan<T> {
    fn default() -> Self {
        Self {
            clear: Vec::new(),
            fill: Vec::new(),
            send: Vec::new(),
        }
    }
}

/// The cache key of a page: the same page turned differently is another
/// image.
type Key = (usize, usize, u8);

fn key(page: PageRef) -> Key {
    (page.source, page.page, page.turns)
}

/// Marks a request id that is not a thumbnail (the large view of one
/// page). Thumbnail tickets never carry it: their generation keeps to 31
/// bits.
pub const LARGE: u64 = 1 << 63;

/// The largest layout generation; the next one starts again at 0.
const MAX_GENERATION: u32 = (1 << 31) - 1;

/// A request's ticket: the layout generation it was made in (high half)
/// and the grid position it was for (low half).
fn ticket(generation: u32, position: usize) -> u64 {
    (u64::from(generation) << 32) | (position as u64 & 0xffff_ffff)
}

/// The generation and position of a ticket.
fn unticket(ticket: u64) -> (u32, usize) {
    ((ticket >> 32) as u32, (ticket & 0xffff_ffff) as usize)
}

/// Which thumbnails a window shows, asks for and keeps, by grid position.
///
/// Every row that shows an image is done in the queue; a page is asked for
/// once and at most `max_in_flight` requests are out at a time, so the
/// worker never holds a backlog of pages scrolled past. Results are tagged
/// with the layout generation they were asked in: one from before an edit
/// is dropped. Images are cached by `(source, page, turns)` while their page
/// is on screen or within the margin, so an edit or a scroll back shows
/// them again without drawing. `T` is the image type (a `slint::Image` in
/// the window).
pub struct ThumbState<T> {
    queue: ThumbQueue,
    margin: usize,
    max_in_flight: usize,
    generation: u32,
    /// Requests the worker has not answered yet, of any generation.
    in_flight: usize,
    /// Positions handed out by the queue but not sent yet, in order.
    pending: VecDeque<usize>,
    cache: HashMap<Key, Option<T>>,
}

impl<T: Clone> ThumbState<T> {
    /// No thumbnails yet for a grid of `count` pages; `margin` pages either
    /// side of the visible ones are kept warm.
    pub fn new(count: usize, margin: usize, max_in_flight: usize) -> Self {
        Self {
            queue: ThumbQueue::new(count, margin),
            margin,
            max_in_flight: max_in_flight.max(1),
            generation: 0,
            in_flight: 0,
            pending: VecDeque::new(),
            cache: HashMap::new(),
        }
    }

    /// Starts a new layout after the page list changed (an edit, a
    /// reorder, pages added): results asked before now are dropped, every
    /// position waits again, and images of pages no longer in the list are
    /// forgotten. The window empties every row and calls
    /// [`update`](Self::update), which fills the cached ones at once.
    pub fn relayout(&mut self, pages: &[PageRef]) {
        self.generation = if self.generation == MAX_GENERATION {
            0
        } else {
            self.generation + 1
        };
        self.pending.clear();
        self.queue.resize(pages.len());
        self.queue.invalidate_all();
        let present: HashSet<Key> = pages.iter().map(|&p| key(p)).collect();
        self.cache.retain(|k, _| present.contains(k));
    }

    /// Takes the pages on screen now: empties the rows far from them,
    /// fills the ones with a cached image and asks for the rest, visible
    /// pages first.
    pub fn update(&mut self, pages: &[PageRef], visible: Range<usize>) -> Plan<T> {
        let mut plan = Plan::default();
        self.queue.set_visible(visible.clone());
        for i in self.queue.to_drop() {
            self.queue.invalidate(i);
            plan.clear.push(i);
        }
        let keep = visible.start.saturating_sub(self.margin)
            ..visible.end.saturating_add(self.margin).min(pages.len());
        let kept: HashSet<Key> = pages[keep.start.min(keep.end)..keep.end]
            .iter()
            .map(|&p| key(p))
            .collect();
        self.cache.retain(|k, _| kept.contains(k));
        // Pages handed out but not sent go back, so the queue orders them
        // again by what is on screen now.
        for i in self.pending.drain(..) {
            self.queue.invalidate(i);
        }
        while let Some(i) = self.queue.next() {
            let Some(page) = pages.get(i) else {
                self.queue.mark_done(i);
                continue;
            };
            match self.cache.get(&key(*page)) {
                Some(image) => {
                    plan.fill.push((i, image.clone()));
                    self.queue.mark_done(i);
                }
                None => self.pending.push_back(i),
            }
        }
        plan.send = self.send_more(pages);
        plan
    }

    /// Takes the worker's answer to `ticket`: fills its row if the grid
    /// still has `page` there in the same layout, and asks for the next
    /// page in the freed slot.
    pub fn arrived(
        &mut self,
        ticket: u64,
        page: PageRef,
        image: Option<T>,
        pages: &[PageRef],
    ) -> Plan<T> {
        self.in_flight = self.in_flight.saturating_sub(1);
        let mut plan = Plan::default();
        let (generation, i) = unticket(ticket);
        if generation == self.generation && pages.get(i) == Some(&page) {
            self.cache.insert(key(page), image.clone());
            self.queue.mark_done(i);
            plan.fill.push((i, image));
        }
        plan.send = self.send_more(pages);
        plan
    }

    /// The cached image of `page`: `Some(None)` for a page that could not
    /// be drawn, `None` when there is none.
    #[cfg(test)]
    pub fn cached(&self, page: PageRef) -> Option<&Option<T>> {
        self.cache.get(&key(page))
    }

    /// Sends waiting pages while there is room in flight.
    fn send_more(&mut self, pages: &[PageRef]) -> Vec<(u64, PageRef)> {
        let mut send = Vec::new();
        while self.in_flight < self.max_in_flight {
            let Some(i) = self.pending.pop_front() else {
                break;
            };
            let Some(&page) = pages.get(i) else {
                self.queue.mark_done(i);
                continue;
            };
            self.in_flight += 1;
            send.push((ticket(self.generation, i), page));
        }
        send
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(n: usize) -> Vec<PageRef> {
        (0..n)
            .map(|page| PageRef {
                source: 0,
                page,
                turns: 0,
            })
            .collect()
    }

    /// The positions a plan asks the worker for.
    fn sent(plan: &Plan<u32>) -> Vec<usize> {
        plan.send.iter().map(|(_, p)| p.page).collect()
    }

    #[test]
    fn tickets_carry_generation_and_position() {
        assert_eq!(unticket(ticket(7, 123)), (7, 123));
        assert_eq!(unticket(ticket(MAX_GENERATION, 0)), (MAX_GENERATION, 0));
        assert_eq!(ticket(MAX_GENERATION, 0xffff_ffff) & LARGE, 0);
    }

    #[test]
    fn visible_pages_are_asked_for_a_few_at_a_time() {
        let list = pages(100);
        let mut s = ThumbState::<u32>::new(100, 8, 3);
        let plan = s.update(&list, 10..20);
        assert_eq!(sent(&plan), vec![10, 11, 12]);
        assert!(plan.fill.is_empty() && plan.clear.is_empty());
    }

    #[test]
    fn a_result_fills_its_row_and_asks_for_the_next() {
        let list = pages(100);
        let mut s = ThumbState::<u32>::new(100, 8, 3);
        let plan = s.update(&list, 10..20);
        let (ticket, page) = plan.send[0];
        let plan = s.arrived(ticket, page, Some(7), &list);
        assert_eq!(plan.fill, vec![(10, Some(7))]);
        assert_eq!(sent(&plan), vec![13]);
        assert_eq!(s.cached(page), Some(&Some(7)));
    }

    #[test]
    fn a_broken_page_is_filled_as_broken_and_not_asked_again() {
        let list = pages(4);
        let mut s = ThumbState::<u32>::new(4, 8, 4);
        let plan = s.update(&list, 0..4);
        let (ticket, page) = plan.send[0];
        let plan = s.arrived(ticket, page, None, &list);
        assert_eq!(plan.fill, vec![(0, None)]);
        assert_eq!(s.cached(page), Some(&None));
        s.relayout(&list);
        let plan = s.update(&list, 0..4);
        assert!(plan.fill.contains(&(0, None)));
        assert!(!sent(&plan).contains(&0));
    }

    #[test]
    fn a_result_from_before_an_edit_is_dropped_but_frees_its_slot() {
        let list = pages(10);
        let mut s = ThumbState::<u32>::new(10, 8, 1);
        let plan = s.update(&list, 0..4);
        let (ticket, page) = plan.send[0];
        // An edit: the list is now in another order.
        let mut edited = list.clone();
        edited.reverse();
        s.relayout(&edited);
        let plan = s.update(&edited, 0..4);
        // The single slot is still taken by the old request.
        assert!(plan.send.is_empty());
        let plan = s.arrived(ticket, page, Some(1), &edited);
        assert!(plan.fill.is_empty());
        assert_eq!(s.cached(page), None);
        assert_eq!(sent(&plan), vec![9]);
    }

    #[test]
    fn cached_pages_come_back_without_asking_after_an_edit() {
        let list = pages(10);
        let mut s = ThumbState::<u32>::new(10, 8, 4);
        let plan = s.update(&list, 0..2);
        for (ticket, page) in plan.send.clone() {
            s.arrived(ticket, page, Some(page.page as u32), &list);
        }
        // Swap the first two pages.
        let mut edited = list.clone();
        edited.swap(0, 1);
        s.relayout(&edited);
        let plan = s.update(&edited, 0..2);
        assert!(plan.fill.contains(&(0, Some(1))));
        assert!(plan.fill.contains(&(1, Some(0))));
        assert!(!sent(&plan).contains(&0) && !sent(&plan).contains(&1));
    }

    #[test]
    fn far_pages_are_cleared_and_forgotten_then_asked_again() {
        let list = pages(100);
        let mut s = ThumbState::<u32>::new(100, 2, 10);
        let plan = s.update(&list, 0..2);
        for (ticket, page) in plan.send.clone() {
            s.arrived(ticket, page, Some(1), &list);
        }
        // The margin pages 2 and 3 were drawn too.
        assert_eq!(plan.send.len(), 4);
        let plan = s.update(&list, 50..52);
        assert_eq!(plan.clear, vec![0, 1, 2, 3]);
        assert_eq!(s.cached(list[0]), None);
        let plan = s.update(&list, 0..2);
        assert!(sent(&plan).contains(&0));
    }

    #[test]
    fn pages_not_yet_sent_follow_the_scroll() {
        let list = pages(100);
        let mut s = ThumbState::<u32>::new(100, 0, 1);
        let plan = s.update(&list, 0..5);
        assert_eq!(sent(&plan), vec![0]);
        let (ticket, page) = plan.send[0];
        // Scrolled away before 1..5 were sent: they are not sent later.
        let plan = s.update(&list, 60..65);
        assert!(plan.send.is_empty());
        let plan = s.arrived(ticket, page, Some(1), &list);
        assert_eq!(sent(&plan), vec![60]);
    }

    #[test]
    fn deleted_pages_leave_the_cache() {
        let list = pages(3);
        let mut s = ThumbState::<u32>::new(3, 8, 4);
        let plan = s.update(&list, 0..3);
        for (ticket, page) in plan.send.clone() {
            s.arrived(ticket, page, Some(1), &list);
        }
        let edited = vec![list[0], list[2]];
        s.relayout(&edited);
        assert_eq!(s.cached(list[1]), None);
        assert_eq!(s.cached(list[2]), Some(&Some(1)));
    }

    #[test]
    fn an_empty_grid_asks_for_nothing() {
        let mut s = ThumbState::<u32>::new(0, 8, 4);
        let plan = s.update(&[], 0..0);
        assert_eq!(plan, Plan::default());
    }
}
