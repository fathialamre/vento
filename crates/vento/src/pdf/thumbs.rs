//! A PDF window's thumbnails: the thread that draws them and the
//! bookkeeping of which page to ask for next, which result still fits the
//! grid, and which images to keep.
//!
//! The thread owns a [`PdfRenderer`], which is not `Send`, so it is made
//! on the thread itself and dies with it: dropping the [`ThumbWorker`]
//! closes its queue, and the thread ends once it has drawn the requests
//! already queued (the window keeps at most a few in flight, and their
//! results are dropped).

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use log::warn;
use slint::{Rgba8Pixel, SharedPixelBuffer};
use vento_core::pdf::{PageRef, ThumbQueue};
use vento_platform::PdfRenderer;

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
            let mut queue = VecDeque::new();
            // Ends when the worker is dropped and the queue is empty.
            loop {
                if queue.is_empty() {
                    match rx.recv() {
                        Ok(request) => queue.push_back(request),
                        Err(_) => break,
                    }
                }
                queue.extend(rx.try_iter());
                drop_stale_large(&mut queue);
                let Some(request) = queue.pop_front() else {
                    continue;
                };
                let drawn = renderer.as_ref().and_then(|renderer| {
                    catch_unwind(AssertUnwindSafe(|| {
                        renderer.render_into(request.page, request.max_px, |w, h| {
                            Canvas(SharedPixelBuffer::new(w, h))
                        })
                    }))
                    .unwrap_or_else(|_| {
                        warn!("drawing a PDF page panicked");
                        None
                    })
                });
                on_thread(request.id, request.page, drawn.map(|canvas| canvas.0));
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

/// A Slint pixel buffer the renderer draws a page into, so the pixels are
/// never copied on their way to the window.
struct Canvas(SharedPixelBuffer<Rgba8Pixel>);

impl AsMut<[u8]> for Canvas {
    /// The buffer's RGBA bytes; a new buffer is not shared, so this does
    /// not copy.
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.make_mut_bytes()
    }
}

/// Keeps only the newest large-view request among those queued: an older
/// one is for a window size that has gone by, and drawing it would only
/// hold up the thumbnails behind it. Thumbnail requests stay in order.
fn drop_stale_large(queue: &mut VecDeque<Request>) {
    let Some(newest) = queue.iter().rposition(|r| r.id & LARGE != 0) else {
        return;
    };
    let mut at = 0;
    queue.retain(|r| {
        let keep = r.id & LARGE == 0 || at == newest;
        at += 1;
        keep
    });
}

/// `buffer` turned `quarter_turns` quarter turns clockwise (0 to 3), as
/// the page would be drawn with that many more turns: a lossless copy, so
/// a turned page shows at once instead of waiting for its drawing.
pub fn turned(
    buffer: &SharedPixelBuffer<Rgba8Pixel>,
    quarter_turns: u8,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = (buffer.width() as usize, buffer.height() as usize);
    let src = buffer.as_slice();
    let turns = quarter_turns % 4;
    let (out_w, out_h) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(out_w as u32, out_h as u32);
    let dst = out.make_mut_slice();
    for y in 0..h {
        for x in 0..w {
            // Where pixel (x, y) lands once turned.
            let (nx, ny) = match turns {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                3 => (y, w - 1 - x),
                _ => (x, y),
            };
            dst[ny * out_w + nx] = src[y * w + x];
        }
    }
    out
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

    /// Caches `image` as `page`'s thumbnail before a relayout that shows it:
    /// a page turned in the window gets its old thumbnail turned.
    pub fn seed(&mut self, page: PageRef, image: Option<T>) {
        self.cache.insert(key(page), image);
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

    fn req(id: u64) -> Request {
        Request {
            id,
            page: pages(1)[0],
            max_px: 100,
        }
    }

    fn ids(queue: &VecDeque<Request>) -> Vec<u64> {
        queue.iter().map(|r| r.id).collect()
    }

    #[test]
    fn a_newer_large_request_replaces_an_older_one_still_queued() {
        let mut queue: VecDeque<Request> = [
            req(1),
            req(LARGE | 1),
            req(2),
            req(LARGE | 2),
            req(3),
            req(LARGE | 3),
        ]
        .into_iter()
        .collect();
        drop_stale_large(&mut queue);
        assert_eq!(ids(&queue), [1, 2, 3, LARGE | 3]);
    }

    #[test]
    fn thumbnails_and_a_lone_large_request_stay_queued() {
        let mut queue: VecDeque<Request> = [req(1), req(LARGE | 5), req(2)].into_iter().collect();
        drop_stale_large(&mut queue);
        assert_eq!(ids(&queue), [1, LARGE | 5, 2]);
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
    fn a_seeded_turn_shows_without_drawing() {
        let list = pages(2);
        let mut s = ThumbState::<u32>::new(2, 8, 4);
        let mut turned_list = list.clone();
        turned_list[1].turns = 1;
        s.seed(turned_list[1], Some(42));
        s.relayout(&turned_list);
        let plan = s.update(&turned_list, 0..2);
        assert!(plan.fill.contains(&(1, Some(42))));
        assert_eq!(sent(&plan), vec![0]);
    }

    /// A 3 by 2 image whose pixel at (x, y) has red `10 * y + x`.
    fn grid_image() -> SharedPixelBuffer<Rgba8Pixel> {
        let mut pixels = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                pixels.extend([10 * y + x, 0, 0, 255]);
            }
        }
        SharedPixelBuffer::clone_from_slice(&pixels, 3, 2)
    }

    /// The red values of `buffer`, row by row.
    fn reds(buffer: &SharedPixelBuffer<Rgba8Pixel>) -> Vec<Vec<u8>> {
        let w = buffer.width() as usize;
        buffer
            .as_slice()
            .chunks(w)
            .map(|row| row.iter().map(|p| p.r).collect())
            .collect()
    }

    #[test]
    fn thumbnails_turn_by_quarters() {
        let image = grid_image();
        // 0  1  2
        // 10 11 12
        assert_eq!(
            reds(&turned(&image, 0)),
            vec![vec![0, 1, 2], vec![10, 11, 12]]
        );
        let cw = turned(&image, 1);
        assert_eq!((cw.width(), cw.height()), (2, 3));
        assert_eq!(reds(&cw), vec![vec![10, 0], vec![11, 1], vec![12, 2]]);
        assert_eq!(
            reds(&turned(&image, 2)),
            vec![vec![12, 11, 10], vec![2, 1, 0]]
        );
        let ccw = turned(&image, 3);
        assert_eq!((ccw.width(), ccw.height()), (2, 3));
        assert_eq!(reds(&ccw), vec![vec![2, 12], vec![1, 11], vec![0, 10]]);
    }

    #[test]
    fn an_empty_grid_asks_for_nothing() {
        let mut s = ThumbState::<u32>::new(0, 8, 4);
        let plan = s.update(&[], 0..0);
        assert_eq!(plan, Plan::default());
    }
}
