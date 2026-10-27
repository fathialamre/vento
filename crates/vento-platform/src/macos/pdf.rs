//! Reading, writing and drawing PDF pages with PDFKit, for the PDF action.
//! Each document is opened from the bytes read when its file was opened and
//! used on one thread only; every call drains what PDFKit autoreleases.

use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::{AnyThread, MainThreadMarker};
use objc2_core_foundation::{CFRetained, CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImageAlphaInfo,
    CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSCopying, NSData, NSUInteger};
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage};
use vento_core::convert::{ConvertError, ImageFormat};
use vento_core::pdf::{ImageKind, ImageOptions, PageRef, PdfBackend, PdfError, Source};

use super::image_io;
use crate::RgbaImage;

/// The box pages are measured and drawn by: the one Preview shows.
const PAGE_BOX: PDFDisplayBox = PDFDisplayBox::CropBox;

/// The most pixels a page is drawn with (1 GiB of RGBA): an A0 page at
/// 300 DPI fits; a larger page fails rather than ask for many gigabytes.
const MAX_PIXELS: usize = 1 << 28;

/// The PDF backend, for worker threads: every call opens its documents
/// afresh from the sources' bytes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPdf;

impl PdfBackend for SystemPdf {
    fn inspect(&self, bytes: &[u8]) -> Result<usize, PdfError> {
        autoreleasepool(|_| {
            let doc = open(&NSData::with_bytes(bytes))?;
            // SAFETY: `doc` is a valid, unlocked document.
            Ok(unsafe { doc.pageCount() })
        })
    }

    fn write_pdf(&self, sources: &[Source], pages: &[PageRef], dst: &Path) -> Result<(), PdfError> {
        autoreleasepool(|_| write_pdf(sources, pages, dst))
    }

    fn write_image(
        &self,
        source: &Source,
        page: PageRef,
        opts: &ImageOptions,
        dst: &Path,
    ) -> Result<(), PdfError> {
        autoreleasepool(|_| write_image(source, page, opts, dst))
    }
}

/// Draws page thumbnails for one PDF window. Not `Send`: it lives on the
/// window's thumbnail thread, holding every source open.
pub struct PdfRenderer {
    /// The window's sources, in the window's order.
    docs: Vec<Retained<PDFDocument>>,
}

impl PdfRenderer {
    /// Opens every source; fails on the first that is locked or not a PDF.
    pub fn new(sources: &[Arc<[u8]>]) -> Result<PdfRenderer, PdfError> {
        autoreleasepool(|_| {
            let docs = sources
                .iter()
                .map(|bytes| open(&shared_data(bytes)))
                .collect::<Result<_, _>>()?;
            Ok(PdfRenderer { docs })
        })
    }

    /// `page` on white, turned by its own rotation plus its `turns`, its
    /// longer side `max_px` pixels. `None` if there is no such page or it
    /// cannot be drawn.
    pub fn render(&self, page: PageRef, max_px: u32) -> Option<RgbaImage> {
        autoreleasepool(|_| {
            let pdf_page = page_of(self.docs.get(page.source)?, page.page)?;
            let layout = Layout::of(&pdf_page, page.turns, Scale::LongSide(max_px))?;
            let mut pixels = vec![0u8; layout.width * layout.height * 4];
            {
                // Premultiplied RGBA, alpha last: the 8-bit RGBA layout a
                // bitmap context accepts. The page is drawn on opaque white,
                // so every pixel is opaque and already straight.
                // SAFETY: `pixels` holds `height` rows of `width * 4` bytes
                // and outlives the context, dropped at the end of this block.
                let context = unsafe {
                    bitmap_context(
                        pixels.as_mut_ptr().cast(),
                        layout.width,
                        layout.height,
                        CGImageAlphaInfo::PremultipliedLast,
                    )
                }?;
                layout.draw(&pdf_page, &context);
            }
            Some(RgbaImage {
                width: layout.width as u32,
                height: layout.height as u32,
                pixels,
            })
        })
    }
}

/// Asks for PDF files with the open panel, several at once, starting in
/// `start`. Main thread only.
pub(crate) fn pick_pdfs(start: Option<&Path>) -> Vec<PathBuf> {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("pick_pdfs called off the main thread; ignored");
        return Vec::new();
    };
    super::screens::activate_app(mtm);
    let mut dialog = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_files().unwrap_or_default()
}

fn write_pdf(sources: &[Source], pages: &[PageRef], dst: &Path) -> Result<(), PdfError> {
    let anchor = pages
        .first()
        .ok_or_else(|| PdfError::Backend("no pages to write".into()))?;
    let mut docs: Vec<Option<Retained<PDFDocument>>> = vec![None; sources.len()];
    // SAFETY: a new, empty document.
    let out = unsafe { PDFDocument::new() };
    for (at, &page) in pages.iter().enumerate() {
        let doc = source_doc(sources, &mut docs, page.source)?;
        let original = page_of(&doc, page.page).ok_or_else(|| missing_page(page))?;
        let copy = original.copy();
        // SAFETY: valid pages; `at` is the end of `out`, which has `at`
        // pages so far.
        unsafe {
            copy.setRotation(turned(original.rotation(), page.turns) as isize);
            out.insertPage_atIndex(&copy, at as NSUInteger);
        }
    }
    let anchor = source_doc(sources, &mut docs, anchor.source)?;
    // SAFETY: the anchor's attributes are a dictionary PDFKit made, and the
    // setter copies it.
    unsafe { out.setDocumentAttributes(anchor.documentAttributes().as_deref()) };
    // Not `writeToURL:`: it goes through a temporary file named after
    // `dst`'s last component in a folder shared by the whole process, so two
    // threads writing `x.pdf` into different folders trip over each other
    // (one fails, or gets the other's file).
    // SAFETY: a valid document.
    let data = unsafe { out.dataRepresentation() }
        .ok_or_else(|| PdfError::Backend("couldn't write the PDF".into()))?;
    // SAFETY: nothing mutates `data` while it is borrowed here.
    fs::write(dst, unsafe { data.as_bytes_unchecked() })?;
    Ok(())
}

fn write_image(
    source: &Source,
    page: PageRef,
    opts: &ImageOptions,
    dst: &Path,
) -> Result<(), PdfError> {
    let doc = open(&shared_data(&source.bytes))?;
    let pdf_page = page_of(&doc, page.page).ok_or_else(|| missing_page(page))?;
    let failed = || PdfError::Backend("couldn't draw the page".into());
    let layout = Layout::of(&pdf_page, page.turns, Scale::Dpi(opts.dpi)).ok_or_else(failed)?;
    // No alpha: the page lies on opaque white, as JPEG needs anyway.
    // SAFETY: a null buffer makes the context allocate (and own) its pixels.
    let context = unsafe {
        bitmap_context(
            ptr::null_mut(),
            layout.width,
            layout.height,
            CGImageAlphaInfo::NoneSkipLast,
        )
    }
    .ok_or_else(failed)?;
    layout.draw(&pdf_page, &context);
    let image = CGBitmapContextCreateImage(Some(&context)).ok_or_else(failed)?;
    let format = match opts.kind {
        ImageKind::Png => ImageFormat::Png,
        ImageKind::Jpeg => ImageFormat::Jpeg,
    };
    image_io::encode(&image, dst, format, Some(opts.dpi)).map_err(|e| match e {
        ConvertError::Io(e) => PdfError::Io(e),
        other => PdfError::Backend(other.to_string()),
    })
}

/// Source `index` of `sources`, opened on first use and kept in `docs`.
fn source_doc(
    sources: &[Source],
    docs: &mut [Option<Retained<PDFDocument>>],
    index: usize,
) -> Result<Retained<PDFDocument>, PdfError> {
    let source = sources
        .get(index)
        .ok_or_else(|| PdfError::Backend(format!("no source {index}")))?;
    if let Some(doc) = &docs[index] {
        return Ok(doc.clone());
    }
    let doc = open(&shared_data(&source.bytes))?;
    docs[index] = Some(doc.clone());
    Ok(doc)
}

fn missing_page(page: PageRef) -> PdfError {
    PdfError::Backend(format!(
        "no page {} in source {}",
        page.page + 1,
        page.source
    ))
}

/// The PDF in `data`: `Locked` if it needs a password, `NotPdf` if PDFKit
/// can't read it or it has no pages.
fn open(data: &NSData) -> Result<Retained<PDFDocument>, PdfError> {
    // SAFETY: a valid data object; PDFKit returns nil for what it can't read.
    let doc =
        unsafe { PDFDocument::initWithData(PDFDocument::alloc(), data) }.ok_or(PdfError::NotPdf)?;
    // SAFETY: `doc` is a valid document.
    if unsafe { doc.isLocked() } {
        return Err(PdfError::Locked);
    }
    // SAFETY: as above.
    if unsafe { doc.pageCount() } == 0 {
        return Err(PdfError::NotPdf);
    }
    Ok(doc)
}

/// `bytes` as data, without copying them: the data holds on to its own
/// reference, so PDFKit may keep it for as long as it likes.
fn shared_data(bytes: &Arc<[u8]>) -> Retained<NSData> {
    let keep = Arc::clone(bytes);
    let start = NonNull::from(&keep[..]).cast::<c_void>();
    let len = keep.len();
    // Dropped, with the reference it holds, when the data is freed.
    let release = RcBlock::new(move |_: NonNull<c_void>, _: NSUInteger| {
        let _ = &keep;
    });
    // SAFETY: `start` points at `len` bytes that the deallocator block keeps
    // alive until the data no longer needs them; immutable data never
    // writes through the pointer.
    unsafe {
        NSData::initWithBytesNoCopy_length_deallocator(NSData::alloc(), start, len, Some(&release))
    }
}

/// Page `index` of `doc`, or `None` past its end (PDFKit would raise).
fn page_of(doc: &PDFDocument, index: usize) -> Option<Retained<PDFPage>> {
    // SAFETY: `doc` is a valid document, and `index` is checked against its
    // length first.
    unsafe {
        (index < doc.pageCount())
            .then(|| doc.pageAtIndex(index))
            .flatten()
    }
}

/// A page's own rotation plus `turns` quarter turns clockwise, as 0, 90, 180
/// or 270.
fn turned(rotation: isize, turns: u8) -> u32 {
    (rotation + 90 * isize::from(turns)).rem_euclid(360) as u32 / 90 * 90
}

/// How large a page is drawn.
#[derive(Clone, Copy)]
enum Scale {
    /// The longer side, turned, this many pixels.
    LongSide(u32),
    /// This many pixels per inch (72 points).
    Dpi(u32),
}

/// Where a page lands in its bitmap: its box as PDFKit shows it, the extra
/// turns and the pixel size.
struct Layout {
    /// The box's size as PDFKit draws it: already turned by the page's own
    /// rotation.
    shown: CGSize,
    /// Extra quarter turns clockwise, 0..=3.
    turns: u8,
    /// The bitmap's width in pixels, at least 1.
    width: usize,
    /// The bitmap's height in pixels, at least 1.
    height: usize,
}

impl Layout {
    /// How `page`, turned by its own rotation plus `turns`, is drawn at
    /// `scale`. `None` for a page without a usable box or too large to draw.
    fn of(page: &PDFPage, turns: u8, scale: Scale) -> Option<Layout> {
        // SAFETY: `page` is a valid page.
        let (bounds, own) = unsafe { (page.boundsForBox(PAGE_BOX), page.rotation()) };
        let (w, h) = (bounds.size.width, bounds.size.height);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return None;
        }
        let shown = if turned(own, 0).is_multiple_of(180) {
            CGSize::new(w, h)
        } else {
            CGSize::new(h, w)
        };
        let turns = turns % 4;
        let (w, h) = Self::turned_size(shown, turns);
        let factor = match scale {
            Scale::LongSide(px) => f64::from(px) / w.max(h),
            Scale::Dpi(dpi) => f64::from(dpi) / 72.0,
        };
        // Saturates for absurd sizes, which the limit below then refuses.
        let pixels = |side: f64| (side * factor).round().max(1.0) as usize;
        let (width, height) = (pixels(w), pixels(h));
        if width.saturating_mul(height) > MAX_PIXELS {
            return None;
        }
        Some(Layout {
            shown,
            turns,
            width,
            height,
        })
    }

    /// `shown`'s width and height after `turns` more quarter turns.
    fn turned_size(shown: CGSize, turns: u8) -> (f64, f64) {
        if turns.is_multiple_of(2) {
            (shown.width, shown.height)
        } else {
            (shown.height, shown.width)
        }
    }

    /// Fills `context` (this layout's size) with white and draws `page` over
    /// it, turned and scaled to fill it.
    fn draw(&self, page: &PDFPage, context: &CGContext) {
        let (width, height) = (self.width as f64, self.height as f64);
        let full = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height));
        CGContext::set_rgb_fill_color(Some(context), 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(Some(context), full);
        CGContext::set_interpolation_quality(Some(context), CGInterpolationQuality::High);
        let (turned_w, turned_h) = Self::turned_size(self.shown, self.turns);
        CGContext::scale_ctm(Some(context), width / turned_w, height / turned_h);
        // `drawWithBox:toContext:` already turns the page by its own
        // rotation and puts the box's corner at the origin; only the extra
        // turns are added, about that shown box (y up). Each quarter turn
        // clockwise carries the top-left corner to the top-right.
        let CGSize {
            width: w,
            height: h,
        } = self.shown;
        let (a, b, c, d, tx, ty) = match self.turns {
            1 => (0.0, -1.0, 1.0, 0.0, 0.0, w),
            2 => (-1.0, 0.0, 0.0, -1.0, w, h),
            3 => (0.0, 1.0, -1.0, 0.0, h, 0.0),
            _ => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
        };
        let transform = CGAffineTransform { a, b, c, d, tx, ty };
        CGContext::concat_ctm(Some(context), transform);
        // SAFETY: valid page and context.
        unsafe { page.drawWithBox_toContext(PAGE_BOX, context) };
    }
}

/// An 8-bit sRGB bitmap of `width`×`height` in `data`, or in pixels it
/// allocates itself when `data` is null.
///
/// # Safety
///
/// `data` is null, or holds `height` rows of `width * 4` bytes that outlive
/// the context.
unsafe fn bitmap_context(
    data: *mut c_void,
    width: usize,
    height: usize,
    alpha: CGImageAlphaInfo,
) -> Option<CFRetained<CGContext>> {
    // SAFETY: a valid system colour space name.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    // SAFETY: `data` is as the caller promises; 8 bits per component and 4
    // bytes per pixel match `alpha`'s layouts.
    unsafe { CGBitmapContextCreate(data, width, height, 8, width * 4, Some(&space), alpha.0) }
}

/// Builds test PDFs with Core Graphics and PDFKit, so no binary fixtures are
/// kept.
#[cfg(test)]
mod test_support {
    use std::path::Path;
    use std::ptr;

    use objc2::AnyThread;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_core_foundation::{
        CFDictionary, CFMutableData, CFString, CFType, CGPoint, CGRect, CGSize,
    };
    use objc2_core_graphics::{
        CGContext, CGDataConsumer, CGPDFContextClose, CGPDFContextCreate, kCGPDFContextTitle,
    };
    use objc2_foundation::{NSData, NSDictionary, NSString, NSURL};
    use objc2_pdf_kit::{
        PDFDisplayBox, PDFDocument, PDFDocumentOwnerPasswordOption, PDFDocumentTitleAttribute,
        PDFDocumentUserPasswordOption,
    };

    /// A PDF of `pages`, each `(width, height, rotate)` in points and
    /// degrees, with a red block in the top-left quarter of each page (away
    /// from the corners), locked with `password` when given.
    pub(super) fn fixture(pages: &[(f64, f64, isize)], password: Option<&str>) -> Vec<u8> {
        build(pages, password, password, None, None)
    }

    /// An unlocked [`fixture`] whose document title is `title`.
    pub(super) fn titled(pages: &[(f64, f64, isize)], title: &str) -> Vec<u8> {
        build(pages, None, None, Some(title), None)
    }

    /// A [`fixture`] that opens without a password but restricts changes
    /// with `owner`.
    pub(super) fn owner_locked(pages: &[(f64, f64, isize)], owner: &str) -> Vec<u8> {
        build(pages, None, Some(owner), None, None)
    }

    /// A one-page [`fixture`] whose crop box is `crop` (x, y, width, height
    /// in page coordinates) rather than the whole page.
    pub(super) fn cropped(page: (f64, f64, isize), crop: [f64; 4]) -> Vec<u8> {
        build(&[page], None, None, None, Some(crop))
    }

    fn build(
        pages: &[(f64, f64, isize)],
        user: Option<&str>,
        owner: Option<&str>,
        title: Option<&str>,
        crop: Option<[f64; 4]>,
    ) -> Vec<u8> {
        let data = CFMutableData::new(None, 0).unwrap();
        let consumer = CGDataConsumer::with_cf_data(Some(&data)).unwrap();
        let info = title.map(|title| {
            let title = CFString::from_str(title);
            CFDictionary::<CFString, CFType>::from_slices(
                &[unsafe { kCGPDFContextTitle }],
                &[title.as_ref()],
            )
        });
        let info = info.as_deref().map(|info| info.as_opaque());
        let context = unsafe { CGPDFContextCreate(Some(&consumer), ptr::null(), info) }.unwrap();
        for &(w, h, _) in pages {
            let media = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h));
            unsafe { CGContext::begin_page(Some(&context), &media) };
            CGContext::set_rgb_fill_color(Some(&context), 1.0, 0.0, 0.0, 1.0);
            let block = CGRect::new(
                CGPoint::new(w * 0.1, h * 0.6),
                CGSize::new(w * 0.3, h * 0.3),
            );
            CGContext::fill_rect(Some(&context), block);
            CGContext::end_page(Some(&context));
        }
        CGPDFContextClose(Some(&context));
        drop(context);
        let bytes = data.to_vec();
        if owner.is_none() && crop.is_none() && pages.iter().all(|&(_, _, r)| r == 0) {
            return bytes;
        }
        let doc = document(&bytes);
        for (i, &(_, _, rotate)) in pages.iter().enumerate() {
            let page = unsafe { doc.pageAtIndex(i) }.unwrap();
            unsafe { page.setRotation(rotate) };
            if let Some([x, y, w, h]) = crop {
                let crop = CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
                unsafe { page.setBounds_forBox(crop, PDFDisplayBox::CropBox) };
            }
        }
        let Some(owner) = owner else {
            return unsafe { doc.dataRepresentation() }.unwrap().to_vec();
        };
        // PDFKit encrypts only when given an owner password; without a user
        // password the file opens freely.
        let owner = NSString::from_str(owner);
        let user = NSString::from_str(user.unwrap_or(""));
        let options = NSDictionary::<NSString, AnyObject>::from_slices(
            &[unsafe { PDFDocumentOwnerPasswordOption }, unsafe {
                PDFDocumentUserPasswordOption
            }],
            &[owner.as_ref(), user.as_ref()],
        );
        unsafe { doc.dataRepresentationWithOptions(options.cast_unchecked()) }
            .unwrap()
            .to_vec()
    }

    pub(super) fn document(bytes: &[u8]) -> Retained<PDFDocument> {
        let data = NSData::with_bytes(bytes);
        unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &data) }.expect("a PDF")
    }

    pub(super) fn read_back(path: &Path) -> Retained<PDFDocument> {
        let url = NSURL::from_file_path(path).unwrap();
        unsafe { PDFDocument::initWithURL(PDFDocument::alloc(), &url) }.expect("a PDF")
    }

    /// Each page's crop box size, in points, before rotation.
    pub(super) fn sizes(doc: &PDFDocument) -> Vec<(f64, f64)> {
        (0..unsafe { doc.pageCount() })
            .map(|i| {
                let page = unsafe { doc.pageAtIndex(i) }.unwrap();
                let bounds = unsafe { page.boundsForBox(PDFDisplayBox::CropBox) };
                (bounds.size.width, bounds.size.height)
            })
            .collect()
    }

    /// The document's title attribute.
    pub(super) fn title(doc: &PDFDocument) -> Option<String> {
        let attrs = unsafe { doc.documentAttributes() }?;
        let key: &AnyObject = unsafe { PDFDocumentTitleAttribute }.as_ref();
        let title = attrs.objectForKey(key)?;
        Some(title.downcast::<NSString>().ok()?.to_string())
    }

    pub(super) fn rotation(doc: &PDFDocument, page: usize) -> isize {
        unsafe { doc.pageAtIndex(page).unwrap().rotation() }
    }

    /// The pixel size of a PNG file.
    pub(super) fn png_size(path: &Path) -> (u32, u32) {
        let info = png_info(path);
        (info.width, info.height)
    }

    /// The resolution a PNG file records, in pixels per inch, rounded.
    pub(super) fn png_dpi(path: &Path) -> Option<(u32, u32)> {
        let dims = png_info(path).pixel_dims?;
        let ppi = |per_metre: u32| (f64::from(per_metre) * 0.0254).round() as u32;
        (dims.unit == png::Unit::Meter).then(|| (ppi(dims.xppu), ppi(dims.yppu)))
    }

    /// The image file at `path` (PNG, JPEG) drawn in sRGB as straight RGBA,
    /// rows top to bottom; every page export is opaque, so premultiplied is
    /// straight here.
    pub(super) fn decoded(path: &Path) -> crate::RgbaImage {
        use objc2_core_foundation::CFURL;
        use objc2_core_graphics::{
            CGBitmapContextCreate, CGColorSpace, CGImage, CGImageAlphaInfo, kCGColorSpaceSRGB,
        };
        use objc2_image_io::CGImageSource;

        let url = CFURL::from_file_path(path).unwrap();
        let source = unsafe { CGImageSource::with_url(&url, None) }.expect("an image");
        let image = unsafe { source.image_at_index(0, None) }.expect("a frame");
        let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        let mut pixels = vec![0u8; w * h * 4];
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                w,
                h,
                8,
                w * 4,
                Some(&space),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .unwrap();
        let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
        CGContext::draw_image(Some(&context), rect, Some(&image));
        drop(context);
        crate::RgbaImage {
            width: w as u32,
            height: h as u32,
            pixels,
        }
    }

    fn png_info(path: &Path) -> png::Info<'static> {
        let file = std::fs::File::open(path).unwrap();
        let reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .unwrap();
        reader.info().clone()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use vento_core::pdf::{ImageKind, ImageOptions, PageRef, PdfBackend, PdfError, Source};

    use super::test_support::{
        cropped, decoded, fixture, owner_locked, png_dpi, png_size, read_back, rotation, sizes,
        title, titled,
    };
    use super::{PdfRenderer, SystemPdf};
    use crate::RgbaImage;
    use crate::macos::image_io::test_support::corner_pixel;

    const LETTER: (f64, f64, isize) = (612.0, 792.0, 0);

    fn source(bytes: Vec<u8>) -> Source {
        let page_count = SystemPdf.inspect(&bytes).unwrap();
        Source {
            path: PathBuf::from("/nowhere/report.pdf"),
            bytes: Arc::from(bytes),
            page_count,
        }
    }

    fn page(source: usize, page: usize, turns: u8) -> PageRef {
        PageRef {
            source,
            page,
            turns,
        }
    }

    /// An existing empty file in `dir`, as the jobs claim one.
    fn partial(dir: &Path, name: &str) -> PathBuf {
        let dst = dir.join(name);
        std::fs::write(&dst, b"").unwrap();
        dst
    }

    /// The pixel at `(x, y)`, rows top to bottom.
    fn pixel(img: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        img.pixels[i..i + 4].try_into().unwrap()
    }

    fn is_red(p: [u8; 4]) -> bool {
        p[0] > 200 && p[1] < 60 && p[2] < 60
    }

    /// Which quarters of `img` show the red block, sampled at their centres:
    /// top left, top right, bottom right, bottom left.
    fn quarters(img: &RgbaImage) -> [bool; 4] {
        let (w, h) = (img.width, img.height);
        [
            is_red(pixel(img, w / 4, h / 4)),
            is_red(pixel(img, w * 3 / 4, h / 4)),
            is_red(pixel(img, w * 3 / 4, h * 3 / 4)),
            is_red(pixel(img, w / 4, h * 3 / 4)),
        ]
    }

    const TOP_LEFT: [bool; 4] = [true, false, false, false];
    const TOP_RIGHT: [bool; 4] = [false, true, false, false];
    const BOTTOM_RIGHT: [bool; 4] = [false, false, true, false];
    const BOTTOM_LEFT: [bool; 4] = [false, false, false, true];

    /// A crop box inside a letter page holding the red block in its own
    /// top-left quarter: x 40–440, y 300–750.
    const CROP: [f64; 4] = [40.0, 300.0, 400.0, 450.0];

    /// `page` of `bytes` exported as a PNG at 36 DPI, read back.
    fn exported(bytes: Vec<u8>, page: PageRef) -> RgbaImage {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("p.png");
        let opts = ImageOptions {
            kind: ImageKind::Png,
            dpi: 36,
        };
        SystemPdf
            .write_image(&source(bytes), page, &opts, &dst)
            .unwrap();
        decoded(&dst)
    }

    #[test]
    fn inspect_counts_pages() {
        assert_eq!(SystemPdf.inspect(&fixture(&[LETTER; 3], None)).unwrap(), 3);
    }

    #[test]
    fn inspect_locked() {
        assert!(matches!(
            SystemPdf.inspect(&fixture(&[LETTER], Some("x"))),
            Err(PdfError::Locked)
        ));
    }

    #[test]
    fn inspect_opens_a_pdf_that_only_restricts_changes() {
        assert_eq!(
            SystemPdf.inspect(&owner_locked(&[LETTER; 2], "x")).unwrap(),
            2
        );
    }

    #[test]
    fn inspect_garbage() {
        assert!(matches!(SystemPdf.inspect(b"hello"), Err(PdfError::NotPdf)));
    }

    #[test]
    fn write_merges_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[(612.0, 792.0, 0), (500.0, 700.0, 0)], None));
        let b = source(fixture(&[(300.0, 400.0, 0)], None));
        let dst = partial(dir.path(), ".out.pdf.vento-partial-1");
        SystemPdf
            .write_pdf(
                &[a, b],
                &[page(1, 0, 0), page(0, 1, 0), page(0, 0, 0)],
                &dst,
            )
            .unwrap();
        assert_eq!(
            sizes(&read_back(&dst)),
            [(300.0, 400.0), (500.0, 700.0), (612.0, 792.0)]
        );
    }

    #[test]
    fn write_creates_a_missing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[LETTER; 2], None));
        let dst = dir.path().join("new.pdf");
        SystemPdf.write_pdf(&[a], &[page(0, 1, 0)], &dst).unwrap();
        assert_eq!(sizes(&read_back(&dst)), [(612.0, 792.0)]);
    }

    #[test]
    fn writes_on_several_threads_keep_apart() {
        // Same file name, different folders, at the same time: each output
        // must be its own.
        let handles: Vec<_> = (0..4u8)
            .map(|turns| {
                std::thread::spawn(move || {
                    let dir = tempfile::tempdir().unwrap();
                    let a = source(fixture(&[LETTER], None));
                    for _ in 0..10 {
                        let dst = partial(dir.path(), "out.pdf");
                        SystemPdf
                            .write_pdf(std::slice::from_ref(&a), &[page(0, 0, turns)], &dst)
                            .unwrap();
                        assert_eq!(rotation(&read_back(&dst), 0), 90 * isize::from(turns));
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn write_rejects_a_page_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[LETTER], None));
        let dst = partial(dir.path(), "out.pdf");
        let result = SystemPdf.write_pdf(&[a], &[page(0, 1, 0)], &dst);
        assert!(matches!(result, Err(PdfError::Backend(_))), "{result:?}");
        let result = SystemPdf.write_pdf(&[], &[page(0, 0, 0)], &dst);
        assert!(matches!(result, Err(PdfError::Backend(_))), "{result:?}");
    }

    #[test]
    fn write_keeps_the_anchor_title() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(titled(&[LETTER], "Alpha"));
        let b = source(titled(&[LETTER], "Beta"));
        let dst = partial(dir.path(), "out.pdf");
        SystemPdf
            .write_pdf(&[a, b], &[page(1, 0, 0), page(0, 0, 0)], &dst)
            .unwrap();
        assert_eq!(title(&read_back(&dst)).as_deref(), Some("Beta"));
    }

    #[test]
    fn write_adds_turns_to_intrinsic_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = fixture(&[(612.0, 792.0, 90)], None);
        let a = source(bytes.clone());
        let dst = partial(dir.path(), "out.pdf");
        SystemPdf.write_pdf(&[a], &[page(0, 0, 1)], &dst).unwrap();
        assert_eq!(rotation(&read_back(&dst), 0), 180);
        // A 612×792 portrait page: 90 + 90 = 180, still portrait.
        let renderer = PdfRenderer::new(&[Arc::from(bytes)]).unwrap();
        let img = renderer.render(page(0, 0, 1), 200).unwrap();
        assert!(img.height > img.width, "{}×{}", img.width, img.height);
        let img = renderer.render(page(0, 0, 0), 200).unwrap();
        assert!(img.width > img.height, "{}×{}", img.width, img.height);
    }

    #[test]
    fn write_turns_wrap_around() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[(612.0, 792.0, 270)], None));
        let dst = partial(dir.path(), "out.pdf");
        SystemPdf.write_pdf(&[a], &[page(0, 0, 3)], &dst).unwrap();
        assert_eq!(rotation(&read_back(&dst), 0), 180);
    }

    #[test]
    fn render_fits_the_long_side() {
        let renderer = PdfRenderer::new(&[Arc::from(fixture(&[LETTER], None))]).unwrap();
        let img = renderer.render(page(0, 0, 0), 200).unwrap();
        assert_eq!((img.width, img.height), (155, 200));
        assert_eq!(img.pixels.len(), 155 * 200 * 4);
        assert_eq!(pixel(&img, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn render_turns_move_the_content_clockwise() {
        let renderer = PdfRenderer::new(&[Arc::from(fixture(&[LETTER], None))]).unwrap();
        // The red block sits in the top-left quarter of the upright page;
        // each quarter turn clockwise carries it to the next corner.
        let at = |turns| quarters(&renderer.render(page(0, 0, turns), 200).unwrap());
        assert_eq!(at(0), TOP_LEFT);
        assert_eq!(at(1), TOP_RIGHT);
        assert_eq!(at(2), BOTTOM_RIGHT);
        assert_eq!(at(3), BOTTOM_LEFT);
    }

    #[test]
    fn render_shows_the_page_own_rotation_then_the_turns() {
        // /Rotate 90 shows the block top right, as Preview does; one more
        // turn carries it on to the bottom right.
        let rotated = fixture(&[(612.0, 792.0, 90)], None);
        let renderer = PdfRenderer::new(&[Arc::from(rotated)]).unwrap();
        let img = renderer.render(page(0, 0, 0), 200).unwrap();
        assert_eq!((img.width, img.height), (200, 155));
        assert_eq!(quarters(&img), TOP_RIGHT);
        let img = renderer.render(page(0, 0, 1), 200).unwrap();
        assert_eq!((img.width, img.height), (155, 200));
        assert_eq!(quarters(&img), BOTTOM_RIGHT);
        // /Rotate 270 shows it bottom left.
        let rotated = fixture(&[(612.0, 792.0, 270)], None);
        let renderer = PdfRenderer::new(&[Arc::from(rotated)]).unwrap();
        assert_eq!(
            quarters(&renderer.render(page(0, 0, 0), 200).unwrap()),
            BOTTOM_LEFT
        );
    }

    #[test]
    fn render_draws_the_crop_box() {
        let renderer = PdfRenderer::new(&[Arc::from(cropped(LETTER, CROP))]).unwrap();
        let img = renderer.render(page(0, 0, 0), 200).unwrap();
        assert_eq!((img.width, img.height), (178, 200));
        assert_eq!(quarters(&img), TOP_LEFT);
        let img = renderer.render(page(0, 0, 1), 200).unwrap();
        assert_eq!((img.width, img.height), (200, 178));
        assert_eq!(quarters(&img), TOP_RIGHT);
    }

    #[test]
    fn export_shows_the_page_own_rotation_then_the_turns() {
        let rotated = || fixture(&[(612.0, 792.0, 90)], None);
        let img = exported(rotated(), page(0, 0, 0));
        assert_eq!((img.width, img.height), (396, 306));
        assert_eq!(quarters(&img), TOP_RIGHT);
        let img = exported(rotated(), page(0, 0, 1));
        assert_eq!((img.width, img.height), (306, 396));
        assert_eq!(quarters(&img), BOTTOM_RIGHT);
    }

    #[test]
    fn export_draws_the_crop_box() {
        let img = exported(cropped(LETTER, CROP), page(0, 0, 0));
        assert_eq!((img.width, img.height), (200, 225));
        assert_eq!(quarters(&img), TOP_LEFT);
        let img = exported(cropped(LETTER, CROP), page(0, 0, 1));
        assert_eq!((img.width, img.height), (225, 200));
        assert_eq!(quarters(&img), TOP_RIGHT);
    }

    #[test]
    fn a_page_too_large_to_draw_fails() {
        // 200 inches a side at 300 DPI: 60,000 pixels square.
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[(14400.0, 14400.0, 0)], None));
        let dst = partial(dir.path(), "p1.png");
        let opts = ImageOptions {
            kind: ImageKind::Png,
            dpi: 300,
        };
        let result = SystemPdf.write_image(&a, page(0, 0, 0), &opts, &dst);
        assert!(matches!(result, Err(PdfError::Backend(_))), "{result:?}");
    }

    #[test]
    fn render_out_of_range_is_none() {
        let renderer = PdfRenderer::new(&[Arc::from(fixture(&[LETTER], None))]).unwrap();
        assert!(renderer.render(page(0, 1, 0), 200).is_none());
        assert!(renderer.render(page(1, 0, 0), 200).is_none());
    }

    #[test]
    fn renderer_refuses_a_locked_source() {
        let locked = Arc::from(fixture(&[LETTER], Some("x")));
        assert!(matches!(PdfRenderer::new(&[locked]), Err(PdfError::Locked)));
    }

    #[test]
    fn image_export_dpi() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[LETTER], None));
        let dst = partial(dir.path(), "p1.png");
        let opts = ImageOptions {
            kind: ImageKind::Png,
            dpi: 150,
        };
        SystemPdf
            .write_image(&a, page(0, 0, 0), &opts, &dst)
            .unwrap();
        assert_eq!(png_size(&dst), (1275, 1650));
        assert_eq!(png_dpi(&dst), Some((150, 150)));
    }

    #[test]
    fn image_export_applies_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[(612.0, 792.0, 90)], None));
        let dst = dir.path().join("p1.png");
        let opts = ImageOptions {
            kind: ImageKind::Png,
            dpi: 72,
        };
        SystemPdf
            .write_image(&a, page(0, 0, 0), &opts, &dst)
            .unwrap();
        assert_eq!(png_size(&dst), (792, 612));
    }

    #[test]
    fn jpeg_export_is_white_not_black() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[LETTER], None));
        let dst = partial(dir.path(), "p1.jpg");
        let opts = ImageOptions {
            kind: ImageKind::Jpeg,
            dpi: 150,
        };
        SystemPdf
            .write_image(&a, page(0, 0, 0), &opts, &dst)
            .unwrap();
        assert_eq!(corner_pixel(&dst), [255, 255, 255, 255]);
        // Away from the corner too: the bottom-right quarter holds no page
        // content, so it shows the background; the block is still drawn.
        let img = decoded(&dst);
        let [r, g, b, a] = pixel(&img, img.width * 3 / 4, img.height * 3 / 4);
        assert!(
            r >= 250 && g >= 250 && b >= 250 && a == 255,
            "{r} {g} {b} {a}"
        );
        assert_eq!(quarters(&img), TOP_LEFT);
    }
}
