//! Keeping vento's own windows off the taskbar and out of focus.
//!
//! winit writes a window's whole extended style again on every change it
//! makes (showing, hiding, turning hit-testing on and off), which would
//! drop any style set from outside. So each window vento changes gets a
//! subclass that adds vento's styles back to every new extended style as it
//! is set (`WM_STYLECHANGING`), and that turns activation away for a window
//! that must never take focus. Its always-on-top comes from Slint's
//! `always-on-top`, which winit keeps itself.

use std::ptr;

use raw_window_handle::RawWindowHandle;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, IsWindowVisible, MA_NOACTIVATE, STYLESTRUCT, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOWPOS, WM_MOUSEACTIVATE, WM_NCDESTROY,
    WM_STYLECHANGING, WM_WINDOWPOSCHANGING, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::PlatformError;

/// The subclass's id among any others on the window.
const SUBCLASS_ID: usize = 0x7665_6e74;

/// No taskbar button, and out of Alt+Tab.
pub(super) const OFF_TASKBAR: usize = 1;
/// Never activated: not by a click, not by being shown.
pub(super) const NO_ACTIVATE: usize = 2;

/// The window behind a Win32 handle.
pub(super) fn hwnd_of(window: RawWindowHandle) -> Result<HWND, PlatformError> {
    match window {
        RawWindowHandle::Win32(h) => Ok(h.hwnd.get() as HWND),
        _ => Err(PlatformError::Failed("not a Win32 window".into())),
    }
}

/// The extended style bits `keep` adds.
fn styles(keep: usize) -> u32 {
    let mut ex = 0;
    if keep & OFF_TASKBAR != 0 {
        ex |= WS_EX_TOOLWINDOW;
    }
    if keep & NO_ACTIVATE != 0 {
        ex |= WS_EX_NOACTIVATE;
    }
    ex
}

/// The extended style `old` with what `keep` asks for.
fn with_kept(old: u32, keep: usize) -> u32 {
    let mut ex = old | styles(keep);
    if keep & OFF_TASKBAR != 0 {
        // It would put the window on the taskbar all the same.
        ex &= !WS_EX_APPWINDOW;
    }
    ex
}

/// Keeps `keep` (`OFF_TASKBAR`, `NO_ACTIVATE`) on `window` from now on,
/// with whatever it kept before. Main thread only.
pub(super) fn keep(window: RawWindowHandle, keep: usize) -> Result<(), PlatformError> {
    let hwnd = hwnd_of(window)?;
    // SAFETY: `hwnd` is a live window of this thread (the caller holds its
    // handle), and the subclass procedure matches `SUBCLASSPROC`.
    unsafe {
        let mut kept = 0;
        let keep = if GetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, &mut kept) != 0 {
            kept | keep
        } else {
            keep
        };
        if SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, keep) == 0 {
            return Err(PlatformError::Failed(format!(
                "cannot subclass the window: {}",
                std::io::Error::last_os_error()
            )));
        }
        let old = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let new = with_kept(old, keep);
        if new != old {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new as isize);
            // The taskbar drops a button only when the window shows again.
            if old & WS_EX_TOOLWINDOW == 0
                && new & WS_EX_TOOLWINDOW != 0
                && IsWindowVisible(hwnd) != 0
            {
                ShowWindow(hwnd, SW_HIDE);
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            SetWindowPos(
                hwnd,
                ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
    Ok(())
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    keep: usize,
) -> LRESULT {
    match msg {
        WM_STYLECHANGING if wparam as i32 == GWL_EXSTYLE => {
            // SAFETY: for this message `lparam` points to the new styles.
            let change = unsafe { &mut *(lparam as *mut STYLESTRUCT) };
            change.styleNew = with_kept(change.styleNew, keep);
        }
        WM_MOUSEACTIVATE if keep & NO_ACTIVATE != 0 => return MA_NOACTIVATE as LRESULT,
        WM_WINDOWPOSCHANGING if keep & NO_ACTIVATE != 0 => {
            // SAFETY: for this message `lparam` points to the new position.
            let pos = unsafe { &mut *(lparam as *mut WINDOWPOS) };
            pos.flags |= SWP_NOACTIVATE;
        }
        WM_NCDESTROY => {
            // SAFETY: removes this very subclass from a window going away.
            unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID) };
        }
        _ => {}
    }
    // SAFETY: the arguments are the ones Windows passed in.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_the_taskbar_is_a_tool_window_and_never_an_app_window() {
        let ex = with_kept(WS_EX_APPWINDOW, OFF_TASKBAR);
        assert_eq!(ex & WS_EX_APPWINDOW, 0);
        assert_ne!(ex & WS_EX_TOOLWINDOW, 0);
        assert_eq!(ex & WS_EX_NOACTIVATE, 0);
    }

    #[test]
    fn both_keep_both_and_leave_other_styles() {
        let other = 0x0000_0010; // WS_EX_ACCEPTFILES
        let ex = with_kept(other, OFF_TASKBAR | NO_ACTIVATE);
        assert_eq!(ex, other | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE);
        assert_eq!(with_kept(other, 0), other);
    }
}
