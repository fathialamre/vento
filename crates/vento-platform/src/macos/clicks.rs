//! A watch on mouse presses in other apps, through a global `NSEvent`
//! monitor. Global monitors only observe events sent to other apps, and
//! mouse events need no Accessibility or Input Monitoring permission.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask};

/// An installed monitor; dropping it removes it.
pub(crate) struct ClickMonitor {
    monitor: Retained<AnyObject>,
}

impl ClickMonitor {
    /// Calls `f` for each left, right or other mouse-button press in
    /// another app, on the main thread, until the monitor is dropped.
    /// `None` when AppKit refuses the monitor.
    pub(crate) fn install(_mtm: MainThreadMarker, f: Box<dyn Fn()>) -> Option<Self> {
        let mask =
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
        let handler = RcBlock::new(move |_event: NonNull<NSEvent>| f());
        // AppKit copies the block and keeps it until `removeMonitor`.
        let Some(monitor) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &handler)
        else {
            log::warn!("cannot watch for clicks outside vento");
            return None;
        };
        log::debug!("watching for clicks outside vento");
        Some(Self { monitor })
    }
}

impl Drop for ClickMonitor {
    fn drop(&mut self) {
        // SAFETY: `monitor` is the object `addGlobalMonitorForEvents…`
        // returned, removed exactly once, here, on the main thread (it is
        // only created there and is not `Send`).
        unsafe { NSEvent::removeMonitor(&self.monitor) };
        log::debug!("stopped watching for clicks outside vento");
    }
}
