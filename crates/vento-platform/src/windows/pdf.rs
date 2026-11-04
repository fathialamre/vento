//! Reading and drawing PDF pages with Windows.Data.Pdf, for the PDF action;
//! writing them is lopdf's ([`crate::pdf_write`]). Windows.Data.Pdf draws a
//! page into an encoded image, here a BMP, which is read back into pixels.
//! Every call that waits on it runs on a worker thread, never the main one.

use std::fs;
use std::io::Cursor;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;

use image::codecs::jpeg::{JpegEncoder, PixelDensity};
use image::{ImageEncoder, RgbImage, RgbaImage as Pixels, imageops};
use vento_core::pdf::{ImageKind, ImageOptions, PageRef, PdfBackend, PdfError, Source};
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
use windows::UI::Color;
use windows::core::HRESULT;

use crate::RgbaImage;

/// The most pixels a page is drawn with (1 GiB of RGBA), as on the Mac: an
/// A0 page at 300 DPI fits; a larger page fails rather than ask for many
/// gigabytes.
const MAX_PIXELS: usize = 1 << 28;

/// The quality JPEG pages are written at, as Convert writes JPEGs.
const JPEG_QUALITY: u8 = 90;

/// What loading a PDF that needs a password fails with:
/// `HRESULT_FROM_WIN32(ERROR_WRONG_PASSWORD)`.
const WRONG_PASSWORD: HRESULT = HRESULT(0x8007_052B_u32 as i32);

/// Windows.Data.Pdf measures pages in device-independent pixels, 96 to the
/// inch.
const DIPS_PER_INCH: f64 = 96.0;

/// The PDF backend, for worker threads: every call opens its documents
/// afresh from the sources' bytes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPdf;

impl PdfBackend for SystemPdf {
    fn inspect(&self, bytes: &[u8]) -> Result<usize, PdfError> {
        Ok(open(bytes)?.PageCount().map_err(failed)? as usize)
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
        let doc = open(&source.bytes)?;
        let pdf_page = page_of(&doc, page.page).ok_or_else(|| missing_page(page))?;
        let drawn = draw(&pdf_page, page.turns, Scale::Dpi(opts.dpi))
            .ok_or_else(|| PdfError::Backend("couldn't draw the page".into()))?;
        let rgb = RgbImage::from_fn(drawn.width(), drawn.height(), |x, y| {
            let [r, g, b, _] = drawn.get_pixel(x, y).0;
            image::Rgb([r, g, b])
        });
        let encode_failed =
            |e: &dyn std::fmt::Display| PdfError::Backend(format!("couldn't write the image: {e}"));
        let bytes = match opts.kind {
            ImageKind::Png => png_with_dpi(&rgb, opts.dpi).map_err(|e| encode_failed(&e))?,
            ImageKind::Jpeg => jpeg_with_dpi(&rgb, opts.dpi).map_err(|e| encode_failed(&e))?,
        };
        fs::write(dst, bytes)?;
        Ok(())
    }
}

/// Draws page thumbnails for one PDF window. Not `Send`, like the Mac's:
/// it lives on the window's thumbnail thread, holding every source open.
pub struct PdfRenderer {
    /// The window's sources, in the window's order.
    docs: Vec<PdfDocument>,
    _not_send: PhantomData<*const ()>,
}

impl PdfRenderer {
    /// Opens every source; fails on the first that is locked or not a PDF.
    pub fn new(sources: &[Arc<[u8]>]) -> Result<PdfRenderer, PdfError> {
        let docs = sources
            .iter()
            .map(|bytes| open(bytes))
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
        let mut buffer = alloc(drawn.width(), drawn.height());
        let pixels = buffer.as_mut();
        if pixels.len() != drawn.as_raw().len() {
            return None;
        }
        pixels.copy_from_slice(drawn.as_raw());
        Some(buffer)
    }
}

/// The PDF in `bytes`: `Locked` if it needs a password, `NotPdf` if Windows
/// can't read it or it has no pages.
fn open(bytes: &[u8]) -> Result<PdfDocument, PdfError> {
    let stream = stream_of(bytes).map_err(failed)?;
    let doc = PdfDocument::LoadFromStreamAsync(&stream)
        .and_then(|load| load.join())
        .map_err(|e| {
            if e.code() == WRONG_PASSWORD {
                PdfError::Locked
            } else {
                PdfError::NotPdf
            }
        })?;
    if doc.PageCount().map_err(failed)? == 0 {
        return Err(PdfError::NotPdf);
    }
    Ok(doc)
}

/// `bytes` copied into a stream Windows.Data.Pdf can read, at its start.
fn stream_of(bytes: &[u8]) -> windows::core::Result<InMemoryRandomAccessStream> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    // The stream stays open when the writer goes.
    writer.DetachStream()?;
    stream.Seek(0)?;
    Ok(stream)
}

/// Everything `stream` holds.
fn bytes_of(stream: &InMemoryRandomAccessStream) -> windows::core::Result<Vec<u8>> {
    let len = u32::try_from(stream.Size()?).map_err(|_| windows::core::Error::empty())?;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(len)?.join()?;
    let mut bytes = vec![0u8; len as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}

fn failed(e: windows::core::Error) -> PdfError {
    PdfError::Backend(e.message())
}

fn missing_page(page: PageRef) -> PdfError {
    PdfError::Backend(format!(
        "no page {} in source {}",
        page.page + 1,
        page.source
    ))
}

/// Page `index` of `doc`, or `None` past its end.
fn page_of(doc: &PdfDocument, index: usize) -> Option<PdfPage> {
    let index = u32::try_from(index).ok()?;
    if index >= doc.PageCount().ok()? {
        return None;
    }
    doc.GetPage(index).ok()
}

/// How large a page is drawn.
#[derive(Clone, Copy)]
enum Scale {
    /// The longer side, turned, this many pixels.
    LongSide(u32),
    /// This many pixels per inch.
    Dpi(u32),
}

/// `page` drawn on white at `scale`, turned by its own rotation (which
/// Windows applies) and then `turns` more quarter turns clockwise. `None`
/// for a page without a usable size, too large to draw, or that Windows
/// cannot draw.
fn draw(page: &PdfPage, turns: u8, scale: Scale) -> Option<Pixels> {
    // The crop box, already turned by the page's own rotation.
    let size = page.Size().ok()?;
    let (w, h) = (f64::from(size.Width), f64::from(size.Height));
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let turns = turns % 4;
    let factor = match scale {
        // Quarter turns keep the longer side.
        Scale::LongSide(px) => f64::from(px) / w.max(h),
        Scale::Dpi(dpi) => f64::from(dpi) / DIPS_PER_INCH,
    };
    // Saturates for absurd sizes, which the limit below then refuses.
    let pixels = |side: f64| (side * factor).round().max(1.0) as u32;
    let (width, height) = (pixels(w), pixels(h));
    if (width as usize).saturating_mul(height as usize) > MAX_PIXELS {
        return None;
    }
    let options = PdfPageRenderOptions::new().ok()?;
    options.SetDestinationWidth(width).ok()?;
    options.SetDestinationHeight(height).ok()?;
    let white = Color {
        A: 255,
        R: 255,
        G: 255,
        B: 255,
    };
    options.SetBackgroundColor(white).ok()?;
    // BMP: no compression to undo when it is read back.
    options
        .SetBitmapEncoderId(BitmapEncoder::BmpEncoderId().ok()?)
        .ok()?;
    let out = InMemoryRandomAccessStream::new().ok()?;
    page.RenderWithOptionsToStreamAsync(&out, &options)
        .and_then(|render| render.join())
        .map_err(|e| log::warn!("cannot draw a PDF page: {e}"))
        .ok()?;
    let encoded = bytes_of(&out).ok()?;
    let mut drawn = image::load_from_memory_with_format(&encoded, image::ImageFormat::Bmp)
        .map_err(|e| log::warn!("cannot read a drawn PDF page: {e}"))
        .ok()?
        .to_rgba8();
    // The page lies on opaque white; a BMP may still carry an empty alpha.
    for pixel in drawn.pixels_mut() {
        pixel.0[3] = 255;
    }
    Some(match turns {
        1 => imageops::rotate90(&drawn),
        2 => imageops::rotate180(&drawn),
        3 => imageops::rotate270(&drawn),
        _ => drawn,
    })
}

/// `image` as a PNG that records `dpi`.
fn png_with_dpi(image: &RgbImage, dpi: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width(), image.height());
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let per_metre = (f64::from(dpi) / 0.0254).round() as u32;
        encoder.set_pixel_dims(Some(png::PixelDimensions {
            xppu: per_metre,
            yppu: per_metre,
            unit: png::Unit::Meter,
        }));
        let mut writer = encoder.write_header()?;
        writer.write_image_data(image.as_raw())?;
    }
    Ok(out)
}

/// `image` as a JPEG that records `dpi`.
fn jpeg_with_dpi(image: &RgbImage, dpi: u32) -> Result<Vec<u8>, image::ImageError> {
    let mut out = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    encoder.set_pixel_density(PixelDensity::dpi(u16::try_from(dpi).unwrap_or(u16::MAX)));
    encoder.write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out.into_inner())
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
            path: PathBuf::from(r"C:\nowhere\report.pdf"),
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
    fn written_pdfs_open_in_windows() {
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
