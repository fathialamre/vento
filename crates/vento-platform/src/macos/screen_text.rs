//! Copy from screen: the Screen Recording permission, and capturing a
//! selected area with ScreenCaptureKit and reading its text with Vision.
//!
//! The binary must still start on macOS 13, where `SCScreenshotManager` and
//! `SCContentFilter.pointPixelScale` do not exist. objc2 looks every class
//! and selector up at run time, so nothing here fails until it is called,
//! and [`read_screen_text`] checks [`supported`] before it touches either.
//! Both frameworks exist on macOS 13, so linking them is safe.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CFRetained, Type};
use objc2_core_graphics::{CGImage, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSOperatingSystemVersion, NSPoint, NSProcessInfo, NSRect,
    NSSize, NSString, NSURL,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizeTextRequestRevision3, VNRequest,
    VNRequestTextRecognitionLevel,
};
use vento_core::geometry::Rect;
use vento_core::screen_text::{Selection, TextBox, local_rect};

use crate::{PlatformError, ScreenText};

/// Privacy & Security → Screen Recording in System Settings.
const ACCESS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// How long to wait for ScreenCaptureKit to answer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The macOS [`ScreenText`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MacScreenText;

impl ScreenText for MacScreenText {
    fn supported(&self) -> bool {
        supported()
    }

    fn has_access(&self) -> bool {
        CGPreflightScreenCaptureAccess()
    }

    fn request_access(&self) {
        // The answer only says whether access is granted now; the prompt
        // (shown once per app) does not wait for the user.
        let granted = CGRequestScreenCaptureAccess();
        log::debug!("screen recording requested, granted now: {granted}");
    }

    fn open_access_settings(&self) {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(ACCESS_SETTINGS_URL)) else {
            log::warn!("bad Screen Recording settings URL");
            return;
        };
        if !NSWorkspace::sharedWorkspace().openURL(&url) {
            log::warn!("cannot open the Screen Recording settings");
        }
    }

    fn select_area(&self, done: Box<dyn FnOnce(Option<Selection>)>) {
        // The selection overlay is not built yet: every selection is
        // cancelled.
        done(None);
    }
}

/// Whether this Mac runs macOS 14 or later, which Copy from screen needs.
pub(super) fn supported() -> bool {
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
        majorVersion: 14,
        minorVersion: 0,
        patchVersion: 0,
    })
}

/// Captures `sel` (vento's own windows left out) and reads the text in it.
/// The picture stays in memory and is dropped before this returns. Blocks
/// for up to twice [`TIMEOUT`]; call it off the main thread.
pub(crate) fn read_screen_text(sel: &Selection) -> Result<Vec<TextBox>, PlatformError> {
    if !supported() {
        return Err(PlatformError::Unsupported);
    }
    if MainThreadMarker::new().is_some() {
        log::warn!("read_screen_text called on the main thread; the UI waits for it");
    }
    // A worker thread has no autorelease pool of its own.
    autoreleasepool(|_| {
        let image = capture(sel)?;
        let boxes = recognise(&image);
        drop(image);
        boxes
    })
}

/// A value handed from a ScreenCaptureKit queue to the waiting thread.
struct Handed<T>(T);

// SAFETY: the value is created on ScreenCaptureKit's queue and, once sent,
// only used by the one thread that receives it; the queue keeps no
// reference it changes afterwards.
unsafe impl<T> Send for Handed<T> {}

/// Waits for a completion handler's answer; `what` names the step in errors.
fn wait<T>(rx: &Receiver<Result<T, String>>, what: &str) -> Result<T, PlatformError> {
    match rx.recv_timeout(TIMEOUT) {
        Ok(answer) => answer.map_err(PlatformError::Failed),
        Err(RecvTimeoutError::Timeout) => Err(PlatformError::Failed(format!("{what} timed out"))),
        Err(RecvTimeoutError::Disconnected) => {
            Err(PlatformError::Failed(format!("{what} gave no answer")))
        }
    }
}

/// Why a completion handler got no result.
fn reason(error: *mut NSError, fallback: &str) -> String {
    // SAFETY: ScreenCaptureKit passes a valid NSError or null.
    match unsafe { error.as_ref() } {
        Some(error) => error.localizedDescription().to_string(),
        None => fallback.to_owned(),
    }
}

/// The pixels of `sel` at its display's own scale. macOS 14 or later.
fn capture(sel: &Selection) -> Result<CFRetained<CGImage>, PlatformError> {
    let (tx, rx) = mpsc::channel();
    let listed = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: ScreenCaptureKit passes a valid object or null.
            let answer = match unsafe { Retained::retain(content) } {
                Some(content) => Ok(Handed(content)),
                None => Err(reason(error, "no shareable content")),
            };
            // The waiting thread may have given up: nothing to do then.
            let _ = tx.send(answer);
        },
    );
    // SAFETY: the handler has the signature the method expects, and copes
    // with being called on any queue.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            false, true, &listed,
        );
    }
    let content = wait(&rx, "listing the screens")?.0;

    // SAFETY: plain property reads of a valid SCShareableContent.
    let (display, ours) = unsafe {
        let display = content
            .displays()
            .iter()
            .find(|d| d.displayID() == sel.display_id);
        let pid = std::process::id() as i32;
        let ours = content.applications().iter().find(|a| a.processID() == pid);
        (display, ours)
    };
    let display = display.ok_or_else(|| {
        PlatformError::Failed(format!("display {} is not available", sel.display_id))
    })?;
    if ours.is_none() {
        log::debug!("vento has no windows to leave out of the capture");
    }
    let excluded = NSArray::from_retained_slice(ours.as_slice());

    // SAFETY: valid arguments; `pointPixelScale` exists from macOS 14 on,
    // which the caller checked.
    let (filter, scale) = unsafe {
        let filter = SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
            SCContentFilter::alloc(),
            &display,
            &excluded,
            &NSArray::new(),
        );
        let scale = f64::from(filter.pointPixelScale());
        (filter, scale)
    };

    let area = local_rect(sel);
    let pixels = |points: f64| ((points * scale).round() as usize).max(1);
    // SAFETY: plain property writes on a fresh configuration.
    let config = unsafe {
        let config = SCStreamConfiguration::new();
        config.setSourceRect(NSRect::new(
            NSPoint::new(area.x, area.y),
            NSSize::new(area.w, area.h),
        ));
        config.setWidth(pixels(area.w));
        config.setHeight(pixels(area.h));
        config.setShowsCursor(false);
        config
    };

    let (tx, rx) = mpsc::channel();
    let captured = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        // SAFETY: ScreenCaptureKit passes a valid image or null; retaining
        // it keeps it alive past the handler.
        let answer = match unsafe { image.as_ref() } {
            Some(image) => Ok(image.retain()),
            None => Err(reason(error, "no image")),
        };
        let _ = tx.send(answer);
    });
    // SAFETY: valid filter and configuration; the handler has the expected
    // signature and copes with any queue. `SCScreenshotManager` exists from
    // macOS 14 on, which the caller checked.
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            &filter,
            &config,
            Some(&captured),
        );
    }
    wait(&rx, "capture")
}

/// The text Vision reads in `image`, one box per observation, in image
/// pixels with a top-left origin.
fn recognise(image: &CGImage) -> Result<Vec<TextBox>, PlatformError> {
    let width = CGImage::width(Some(image)) as u32;
    let height = CGImage::height(Some(image)) as u32;
    // SAFETY: a valid image and an empty options dictionary.
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            image,
            &NSDictionary::new(),
        )
    };
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    // SAFETY: revision 3 exists from macOS 13 on.
    unsafe { request.setRevision(VNRecognizeTextRequestRevision3) };
    request.setAutomaticallyDetectsLanguage(true);
    request.setUsesLanguageCorrection(false);

    let as_request: &VNRequest = &request;
    // Synchronous: returns once Vision has read the whole image.
    handler
        .performRequests_error(&NSArray::from_slice(&[as_request]))
        .map_err(|e| PlatformError::Failed(e.localizedDescription().to_string()))?;

    let Some(observations) = request.results() else {
        return Ok(Vec::new());
    };
    Ok(observations
        .iter()
        .filter_map(|o| {
            let text = o.topCandidates(1).firstObject()?.string().to_string();
            // SAFETY: a plain property read of a valid observation.
            let b = unsafe { o.boundingBox() };
            let rect = to_pixels(
                (b.origin.x, b.origin.y, b.size.width, b.size.height),
                width,
                height,
            );
            Some(TextBox { rect, text })
        })
        .collect())
}

/// A Vision bounding box (`x, y, w, h`, normalised, bottom-left origin) in
/// pixels of a `width` × `height` image, with a top-left origin.
fn to_pixels(norm: (f64, f64, f64, f64), width: u32, height: u32) -> Rect {
    let (x, y, w, h) = norm;
    let (width, height) = (f64::from(width), f64::from(height));
    Rect {
        x: x * width,
        y: (1.0 - y - h) * height,
        w: w * width,
        h: h * height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_boxes_become_top_left_pixels() {
        // Vision: normalised, bottom-left origin.
        let got = to_pixels((0.25, 0.5, 0.5, 0.25), 400, 200);
        assert_eq!(
            got,
            Rect {
                x: 100.0,
                y: 50.0,
                w: 200.0,
                h: 50.0
            }
        );
    }
}
