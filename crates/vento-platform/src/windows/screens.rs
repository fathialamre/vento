//! Monitors and the pointer, in vento's logical coordinates.
//!
//! vento is per-monitor DPI aware (winit makes it so), so Windows speaks to
//! it in physical pixels across one virtual desktop. Each monitor's logical
//! frame is its physical frame divided by its own scale, which is exact
//! while all monitors share one scale; monitors of different scales can
//! overlap or leave gaps in logical space, which the overlay work (W2)
//! takes up.

use std::ptr;

use vento_core::geometry::{Point, Rect};
use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows_sys::core::BOOL;

/// The DPI Windows calls 100 %.
const BASE_DPI: f64 = 96.0;

/// One monitor, in physical pixels.
#[derive(Clone, Copy)]
pub(super) struct Monitor {
    pub frame: RECT,
    /// The frame less the taskbar and docked toolbars.
    pub work: RECT,
    /// Physical pixels per logical point.
    pub scale: f64,
    pub primary: bool,
}

impl Monitor {
    /// The work area in logical points.
    pub fn logical_work(&self) -> Rect {
        logical(&self.work, self.scale)
    }

    /// The whole monitor in logical points.
    pub fn logical_frame(&self) -> Rect {
        logical(&self.frame, self.scale)
    }

    fn holds(&self, x: i32, y: i32) -> bool {
        x >= self.frame.left && x < self.frame.right && y >= self.frame.top && y < self.frame.bottom
    }
}

/// `r` divided by `scale`.
fn logical(r: &RECT, scale: f64) -> Rect {
    Rect {
        x: f64::from(r.left) / scale,
        y: f64::from(r.top) / scale,
        w: f64::from(r.right - r.left) / scale,
        h: f64::from(r.bottom - r.top) / scale,
    }
}

/// Every monitor, the primary first.
pub(super) fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _dc: HDC,
        _clip: *mut RECT,
        found: LPARAM,
    ) -> BOOL {
        // SAFETY: `found` is the `Vec` `monitors` passed in, alive for the
        // whole enumeration, which runs on this thread.
        let found = unsafe { &mut *(found as *mut Vec<Monitor>) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `monitor` comes from the enumeration; `info` is sized.
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return 1;
        }
        let (mut dpi_x, mut dpi_y) = (0, 0);
        // SAFETY: as above; the out-pointers are live locals.
        let scale =
            if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } == 0
            {
                f64::from(dpi_x) / BASE_DPI
            } else {
                1.0
            };
        found.push(Monitor {
            frame: info.rcMonitor,
            work: info.rcWork,
            scale,
            primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
        1
    }

    let mut found: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only touches `found`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(each),
            &mut found as *mut Vec<Monitor> as LPARAM,
        )
    };
    found.sort_by_key(|m| !m.primary);
    found
}

/// The monitor holding the physical point `(x, y)`, else the primary.
pub(super) fn monitor_at(monitors: &[Monitor], x: i32, y: i32) -> Option<&Monitor> {
    monitors.iter().find(|m| m.holds(x, y)).or(monitors.first())
}

/// The physical point `p` in logical points, by its monitor's scale.
pub(super) fn to_logical(monitors: &[Monitor], p: POINT) -> Point {
    let scale = monitor_at(monitors, p.x, p.y).map_or(1.0, |m| m.scale);
    Point {
        x: f64::from(p.x) / scale,
        y: f64::from(p.y) / scale,
    }
}

/// The monitor whose logical frame holds the logical point `p`.
pub(super) fn monitor_at_logical(monitors: &[Monitor], p: Point) -> Option<&Monitor> {
    monitors
        .iter()
        .find(|m| {
            let f = logical(&m.frame, m.scale);
            p.x >= f.x && p.x < f.x + f.w && p.y >= f.y && p.y < f.y + f.h
        })
        .or(monitors.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(left: i32, top: i32, w: i32, h: i32, scale: f64, primary: bool) -> Monitor {
        let frame = RECT {
            left,
            top,
            right: left + w,
            bottom: top + h,
        };
        Monitor {
            frame,
            work: RECT {
                bottom: frame.bottom - 48,
                ..frame
            },
            scale,
            primary,
        }
    }

    #[test]
    fn a_point_is_divided_by_its_own_monitors_scale() {
        let monitors = [
            monitor(0, 0, 2880, 1800, 2.0, true),
            monitor(2880, 0, 1920, 1080, 1.0, false),
        ];
        let p = to_logical(&monitors, POINT { x: 1000, y: 500 });
        assert_eq!((p.x, p.y), (500.0, 250.0));
        let p = to_logical(&monitors, POINT { x: 3000, y: 10 });
        assert_eq!((p.x, p.y), (3000.0, 10.0));
    }

    #[test]
    fn the_work_area_leaves_out_the_taskbar() {
        let m = monitor(0, 0, 2880, 1800, 1.5, true);
        let work = m.logical_work();
        assert_eq!((work.x, work.y, work.w, work.h), (0.0, 0.0, 1920.0, 1168.0));
    }

    #[test]
    fn a_point_off_every_monitor_falls_back_to_the_primary() {
        let monitors = [monitor(0, 0, 1920, 1080, 1.25, true)];
        let found = monitor_at(&monitors, -50, -50).expect("the primary");
        assert!(found.primary);
        let found = monitor_at_logical(&monitors, Point { x: 9e4, y: 0.0 }).unwrap();
        assert!(found.primary);
    }

    #[test]
    fn this_machine_has_a_monitor() {
        let monitors = monitors();
        assert!(!monitors.is_empty());
        assert!(monitors[0].primary);
        assert!(monitors.iter().all(|m| m.scale >= 1.0));
    }
}
