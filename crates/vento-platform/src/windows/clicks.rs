//! Presses in other apps, through a low-level mouse hook on the main
//! thread, whose message loop (winit's) runs it. A press on one of vento's
//! own windows does not count, nor one in the taskbar's notification area,
//! where vento's tray icon is: that click toggles the Quick panel itself.

use std::cell::RefCell;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, FindWindowExW, FindWindowW, GA_ROOT, GetAncestor, GetClassNameW, GetWindowRect,
    GetWindowThreadProcessId, HC_ACTION, HHOOK, MSLLHOOKSTRUCT, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_RBUTTONDOWN,
    WM_XBUTTONDOWN, WindowFromPoint,
};
use windows_sys::w;

/// The hook, and what a press outside calls.
type Watch = (HHOOK, Box<dyn Fn()>);

thread_local! {
    /// The watch while one runs.
    static WATCH: RefCell<Option<Watch>> = const { RefCell::new(None) };
}

/// Calls `f` for each press outside vento until [`stop`]; replaces any
/// earlier watch. Main thread only.
pub(super) fn watch(f: Box<dyn Fn()>) {
    stop();
    // SAFETY: the hook procedure matches `HOOKPROC`; a low-level hook needs
    // this module's handle and no thread id.
    let hook = unsafe {
        SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(hook_proc),
            GetModuleHandleW(ptr::null()),
            0,
        )
    };
    if hook.is_null() {
        log::warn!(
            "cannot watch for clicks outside vento: {}",
            std::io::Error::last_os_error()
        );
        return;
    }
    WATCH.with(|w| *w.borrow_mut() = Some((hook, f)));
}

/// Ends the watch [`watch`] started, if any.
pub(super) fn stop() {
    if let Some((hook, _)) = WATCH.with(|w| w.borrow_mut().take()) {
        // SAFETY: `hook` is the live hook `watch` installed.
        unsafe { UnhookWindowsHookEx(hook) };
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let press = matches!(
        wparam as u32,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
    );
    if code == HC_ACTION as i32 && press {
        // SAFETY: for a low-level mouse hook `lparam` points to this.
        let at = unsafe { (*(lparam as *const MSLLHOOKSTRUCT)).pt };
        if !is_vento_or_tray(at) {
            // Taken out while it runs, so `f` may stop or replace the watch.
            let taken = WATCH.with(|w| w.borrow_mut().take());
            if let Some((hook, f)) = taken {
                f();
                WATCH.with(|w| {
                    let mut w = w.borrow_mut();
                    if w.is_none() {
                        *w = Some((hook, f));
                    }
                });
            }
        }
    }
    // SAFETY: passes the press on unchanged.
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// Whether the press at `at` (physical) lands on a window of vento's, or on
/// the notification area or its overflow of hidden icons.
fn is_vento_or_tray(at: POINT) -> bool {
    // SAFETY: plain calls with live arguments.
    unsafe {
        let root = GetAncestor(WindowFromPoint(at), GA_ROOT);
        if root.is_null() {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(root, &mut pid);
        if pid == GetCurrentProcessId() {
            return true;
        }
        match class_of(root).as_str() {
            // Windows 11's hidden icons, then Windows 10's.
            "TopLevelWindowForOverflowXamlIsland" | "NotifyIconOverflowWindow" => true,
            "Shell_TrayWnd" => in_notification_area(at),
            _ => false,
        }
    }
}

/// Whether `at` lies in the primary taskbar's notification area.
fn in_notification_area(at: POINT) -> bool {
    // SAFETY: plain calls with live arguments.
    unsafe {
        let taskbar = FindWindowW(w!("Shell_TrayWnd"), ptr::null());
        let area = FindWindowExW(taskbar, ptr::null_mut(), w!("TrayNotifyWnd"), ptr::null());
        let mut r = RECT::default();
        !area.is_null()
            && GetWindowRect(area, &mut r) != 0
            && at.x >= r.left
            && at.x < r.right
            && at.y >= r.top
            && at.y < r.bottom
    }
}

/// The window class of `window`.
fn class_of(window: HWND) -> String {
    let mut name = [0u16; 64];
    // SAFETY: the buffer's length is passed with it.
    let len = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..len.max(0) as usize])
}
