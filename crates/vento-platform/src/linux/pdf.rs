//! Reading and drawing PDF pages with poppler-glib and cairo, for the PDF
//! action; writing them is lopdf's ([`crate::pdf_write`]), as on Windows.
//! Every call that waits on poppler runs on a worker thread, never the main
//! one.

use std::f64::consts::FRAC_PI_2;
use std::fs;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;

use gio::glib;
use image::RgbImage;
use vento_core::pdf::{ImageOptions, PageRef, PdfBackend, PdfError, Source};

use crate::RgbaImage;
use crate::pdf_image::MAX_PIXELS;

/// poppler measures pages in points, 72 to the inch.
const POINTS_PER_INCH: f64 = 72.0;

/// The PDF backend, for worker threads: every call opens its documents
/// afresh from the sources' bytes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPdf;

impl PdfBackend for SystemPdf {
    fn inspect(&self, bytes: &[u8]) -> Result<usize, PdfError> {
        Ok(open(glib::Bytes::from(bytes))?.n_pages() as usize)
    }

    fn write_pdf(&self, sources: &[Source], pages: &[PageRef], dst: &Path) -> Result<(), PdfError> {
        let bytes = crate::pdf_write::assemble(sources, pages)?;
        fs::write(dst, bytes)?;
        Ok(())
    }

    fn write_image(
        &self,
        source: &Source,
        page: PageRef,
        opts: &ImageOptions,
        dst: &Path,
    ) -> Result<(), PdfError> {
        let doc = open(bytes_of(&source.bytes))?;
        let pdf_page = page_of(&doc, page.page).ok_or_else(|| missing_page(page))?;
        let drawn = draw(&pdf_page, page.turns, Scale::Dpi(opts.dpi))
            .ok_or_else(|| PdfError::Backend("couldn't draw the page".into()))?;
        let mut rgb = RgbImage::new(drawn.width, drawn.height);
        drawn.copy_to(rgb.as_mut(), 3);
        fs::write(dst, crate::pdf_image::encode(&rgb, opts)?)?;
        Ok(())
    }
}

/// Draws page thumbnails for one PDF window. Not `Send`, like the Mac's:
/// it lives on the window's thumbnail thread, holding every source open.
pub struct PdfRenderer {
    /// The window's sources, in the window's order.
    docs: Vec<poppler::Document>,
    _not_send: PhantomData<*const ()>,
}

impl PdfRenderer {
    /// Opens every source; fails on the first that is locked or not a PDF.
    pub fn new(sources: &[Arc<[u8]>]) -> Result<PdfRenderer, PdfError> {
        let docs = sources
            .iter()
            .map(|bytes| open(bytes_of(bytes)))
            .collect::<Result<_, _>>()?;
        Ok(PdfRenderer {
            docs,
            _not_send: PhantomData,
        })
    }

    /// `page` on white, turned by its own rotation plus its `turns`, its
    /// longer side `max_px` pixels. `None` if there is no such page or it
    /// cannot be drawn.
    pub fn render(&self, page: PageRef, max_px: u32) -> Option<RgbaImage> {
        self.render_into(page, max_px, |width, height| RgbaImage {
            width,
            height,
            pixels: vec![0u8; width as usize * height as usize * 4],
        })
    }

    /// Draws as [`render`](Self::render) does, into the buffer
    /// `alloc(width, height)` makes. The buffer must hold
    /// `width * height * 4` bytes of RGBA; any other length draws nothing.
    pub fn render_into<B: AsMut<[u8]>>(
        &self,
        page: PageRef,
        max_px: u32,
        alloc: impl FnOnce(u32, u32) -> B,
    ) -> Option<B> {
        let pdf_page = page_of(self.docs.get(page.source)?, page.page)?;
        let drawn = draw(&pdf_page, page.turns, Scale::LongSide(max_px))?;
        let mut buffer = alloc(drawn.width, drawn.height);
        let pixels = buffer.as_mut();
        if pixels.len() != drawn.width as usize * drawn.height as usize * 4 {
            return None;
        }
        drawn.copy_to(pixels, 4);
        Some(buffer)
    }
}

/// `bytes` as poppler takes them, without a copy.
fn bytes_of(bytes: &Arc<[u8]>) -> glib::Bytes {
    glib::Bytes::from_owned(bytes.clone())
}

/// The PDF in `bytes`: `Locked` if it needs a password, `NotPdf` if poppler
/// can't read it or it has no pages.
fn open(bytes: glib::Bytes) -> Result<poppler::Document, PdfError> {
    let doc = poppler::Document::from_bytes(&bytes, None).map_err(|e| {
        if e.matches(poppler::Error::Encrypted) {
            PdfError::Locked
        } else {
            PdfError::NotPdf
        }
    })?;
    if doc.n_pages() <= 0 {
        return Err(PdfError::NotPdf);
    }
    Ok(doc)
}

fn missing_page(page: PageRef) -> PdfError {
    PdfError::Backend(format!(
        "no page {} in source {}",
        page.page + 1,
        page.source
    ))
}

/// Page `index` of `doc`, or `None` past its end.
fn page_of(doc: &poppler::Document, index: usize) -> Option<poppler::Page> {
    let index = i32::try_from(index).ok()?;
    if index >= doc.n_pages() {
        return None;
    }
    doc.page(index)
}

/// How large a page is drawn.
#[derive(Clone, Copy)]
enum Scale {
    /// The longer side, turned, this many pixels.
    LongSide(u32),
    /// This many pixels per inch.
    Dpi(u32),
}

/// A page drawn by cairo: opaque pixels, each a native-endian `0xAARRGGBB`
/// word, rows `stride` bytes apart.
struct Drawn {
    width: u32,
    height: u32,
    stride: usize,
    data: cairo::ImageSurfaceDataOwned,
}

impl Drawn {
    /// Copies the pixels into `out`, rows top to bottom, as RGB (`channels`
    /// 3) or RGBA (`channels` 4). `out` holds `width * height * channels`
    /// bytes.
    fn copy_to(&self, out: &mut [u8], channels: usize) {
        let row_len = self.width as usize * channels;
        for (row, out_row) in self.data.chunks(self.stride).zip(out.chunks_mut(row_len)) {
            let (words, _) = row.as_chunks::<4>();
            for (word, px) in words.iter().zip(out_row.chunks_exact_mut(channels)) {
                let argb = u32::from_ne_bytes(*word);
                let [_, r, g, b] = argb.to_be_bytes();
                px[..3].copy_from_slice(&[r, g, b]);
                if channels == 4 {
                    // Drawn on opaque white.
                    px[3] = 255;
                }
            }
        }
    }
}

/// `page` drawn on white at `scale`, turned by its own rotation (which
/// poppler applies) and then `turns` more quarter turns clockwise. `None`
/// for a page without a usable size, too large to draw, or that cairo
/// cannot draw on.
fn draw(page: &poppler::Page, turns: u8, scale: Scale) -> Option<Drawn> {
    // The crop box, already turned by the page's own rotation.
    let (w, h) = page.size();
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let turns = turns % 4;
    let factor = match scale {
        // Quarter turns keep the longer side.
        Scale::LongSide(px) => f64::from(px) / w.max(h),
        Scale::Dpi(dpi) => f64::from(dpi) / POINTS_PER_INCH,
    };
    // Saturates for absurd sizes, which the limit below then refuses.
    let pixels = |side: f64| (side * factor).round().max(1.0) as u32;
    let (width, height) = (pixels(w), pixels(h));
    if (width as usize).saturating_mul(height as usize) > MAX_PIXELS {
        return None;
    }
    let (out_w, out_h) = if turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };
    let surface = cairo::ImageSurface::create(
        cairo::Format::Rgb24,
        i32::try_from(out_w).ok()?,
        i32::try_from(out_h).ok()?,
    )
    .map_err(|e| log::warn!("cannot make room to draw a PDF page: {e}"))
    .ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        // Quarter turns clockwise (y grows downwards) about the top left,
        // then moved back into view.
        let (fw, fh) = (f64::from(width), f64::from(height));
        match turns {
            1 => cr.translate(fh, 0.0),
            2 => cr.translate(fw, fh),
            3 => cr.translate(0.0, fw),
            _ => {}
        }
        cr.rotate(FRAC_PI_2 * f64::from(turns));
        cr.scale(fw / w, fh / h);
        page.render(&cr);
        if let Err(e) = cr.status() {
            log::warn!("cannot draw a PDF page: {e}");
            return None;
        }
    }
    surface.flush();
    let stride = usize::try_from(surface.stride()).ok()?;
    let data = surface.take_data().ok()?;
    Some(Drawn {
        width: out_w,
        height: out_h,
        stride,
        data,
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use lopdf::Document;
    use vento_core::pdf::{ImageKind, ImageOptions, PageRef, PdfBackend, PdfError, Source};

    use super::{PdfRenderer, SystemPdf};
    use crate::RgbaImage;
    use crate::pdf_write::test_support::{cropped, fixture, owner_locked};

    const LETTER: (f64, f64, i64) = (612.0, 792.0, 0);

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

    /// The image file at `path` as RGBA.
    fn decoded(path: &Path) -> RgbaImage {
        let img = image::open(path).unwrap().to_rgba8();
        RgbaImage {
            width: img.width(),
            height: img.height(),
            pixels: img.into_raw(),
        }
    }

    fn png_info(path: &Path) -> png::Info<'static> {
        let file = std::fs::File::open(path).unwrap();
        let reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .unwrap();
        reader.info().clone()
    }

    /// The resolution a PNG file records, in pixels per inch, rounded.
    fn png_dpi(path: &Path) -> Option<(u32, u32)> {
        let dims = png_info(path).pixel_dims?;
        let ppi = |per_metre: u32| (f64::from(per_metre) * 0.0254).round() as u32;
        (dims.unit == png::Unit::Meter).then(|| (ppi(dims.xppu), ppi(dims.yppu)))
    }

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

    fn rotation(path: &Path, index: u32) -> i64 {
        let doc = Document::load(path).unwrap();
        let id = doc.get_pages()[&(index + 1)];
        let page = doc.get_dictionary(id).unwrap();
        page.get(b"Rotate").unwrap().as_i64().unwrap()
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
    fn written_pdfs_open_in_poppler() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[(612.0, 792.0, 0), (500.0, 700.0, 0)], None));
        let b = source(owner_locked(&[(300.0, 400.0, 0)], "x"));
        let dst = partial(dir.path(), ".out.pdf.vento-partial-1");
        SystemPdf
            .write_pdf(
                &[a, b],
                &[page(1, 0, 0), page(0, 1, 0), page(0, 0, 0)],
                &dst,
            )
            .unwrap();
        let written = std::fs::read(&dst).unwrap();
        assert_eq!(SystemPdf.inspect(&written).unwrap(), 3);
        // The pages come out as they went in: the first is the small one.
        let renderer = PdfRenderer::new(&[Arc::from(written)]).unwrap();
        let first = renderer.render(page(0, 0, 0), 400).unwrap();
        assert_eq!((first.width, first.height), (300, 400));
        assert_eq!(quarters(&first), TOP_LEFT);
    }

    #[test]
    fn write_creates_a_missing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(fixture(&[LETTER; 2], None));
        let dst = dir.path().join("new.pdf");
        SystemPdf.write_pdf(&[a], &[page(0, 1, 0)], &dst).unwrap();
        assert_eq!(SystemPdf.inspect(&std::fs::read(&dst).unwrap()).unwrap(), 1);
    }

    #[test]
    fn writes_on_several_threads_keep_apart() {
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
                        assert_eq!(rotation(&dst, 0), 90 * i64::from(turns));
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn write_adds_turns_to_intrinsic_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = fixture(&[(612.0, 792.0, 90)], None);
        let a = source(bytes.clone());
        let dst = partial(dir.path(), "out.pdf");
        SystemPdf.write_pdf(&[a], &[page(0, 0, 1)], &dst).unwrap();
        assert_eq!(rotation(&dst, 0), 180);
        // A 612×792 portrait page: 90 + 90 = 180, still portrait.
        let renderer = PdfRenderer::new(&[Arc::from(bytes)]).unwrap();
        let img = renderer.render(page(0, 0, 1), 200).unwrap();
        assert!(img.height > img.width, "{}×{}", img.width, img.height);
        let img = renderer.render(page(0, 0, 0), 200).unwrap();
        assert!(img.width > img.height, "{}×{}", img.width, img.height);
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
    fn render_into_draws_in_the_callers_buffer_of_the_right_size_only() {
        let renderer = PdfRenderer::new(&[Arc::from(fixture(&[LETTER], None))]).unwrap();
        let mut asked = None;
        let pixels = renderer
            .render_into(page(0, 0, 0), 200, |w, h| {
                asked = Some((w, h));
                vec![0u8; w as usize * h as usize * 4]
            })
            .unwrap();
        assert_eq!(asked, Some((155, 200)));
        assert_eq!(&pixels[..4], [255, 255, 255, 255]);
        assert!(
            renderer
                .render_into(page(0, 0, 0), 200, |_, _| vec![0u8; 3])
                .is_none()
        );
    }

    #[test]
    fn render_turns_move_the_content_clockwise() {
        let renderer = PdfRenderer::new(&[Arc::from(fixture(&[LETTER], None))]).unwrap();
        let at = |turns| quarters(&renderer.render(page(0, 0, turns), 200).unwrap());
        assert_eq!(at(0), TOP_LEFT);
        assert_eq!(at(1), TOP_RIGHT);
        assert_eq!(at(2), BOTTOM_RIGHT);
        assert_eq!(at(3), BOTTOM_LEFT);
    }

    #[test]
    fn render_shows_the_page_own_rotation_then_the_turns() {
        // /Rotate 90 shows the block top right; one more turn carries it on
        // to the bottom right.
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
        let info = png_info(&dst);
        assert_eq!((info.width, info.height), (1275, 1650));
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
        let info = png_info(&dst);
        assert_eq!((info.width, info.height), (792, 612));
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
        let img = decoded(&dst);
        let [r, g, b, a] = pixel(&img, 0, 0);
        assert!(
            r >= 250 && g >= 250 && b >= 250 && a == 255,
            "{r} {g} {b} {a}"
        );
        let [r, g, b, a] = pixel(&img, img.width * 3 / 4, img.height * 3 / 4);
        assert!(
            r >= 250 && g >= 250 && b >= 250 && a == 255,
            "{r} {g} {b} {a}"
        );
        assert_eq!(quarters(&img), TOP_LEFT);
    }
}
