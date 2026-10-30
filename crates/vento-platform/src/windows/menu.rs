//! Context menus (the Pocket's right click), as Win32 popup menus.
//!
//! A popup menu closes on a click elsewhere only while its owner window is
//! in front, and vento's island never takes the focus. So the menu belongs
//! to a hidden window of vento's own, brought to the front for as long as
//! the menu shows; the app that was in front gets the focus back after.

use std::cell::Cell;
use std::ptr;

use raw_window_handle::RawWindowHandle;
use vento_core::geometry::Point;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    GetForegroundWindow, HMENU, IsWindow, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    PostMessageW, RegisterClassW, SetForegroundWindow, TPM_LAYOUTRTL, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_sys::w;

use crate::MenuEntry;

thread_local! {
    /// The hidden window that owns vento's menus, made on first use.
    static OWNER: Cell<HWND> = const { Cell::new(ptr::null_mut()) };
}

/// The menus' owner window, made once on this (the main) thread.
fn owner() -> Option<HWND> {
    let made = OWNER.with(Cell::get);
    if !made.is_null() {
        return Some(made);
    }
    // SAFETY: a plain window class and a hidden window, kept for good.
    let window = unsafe {
        let module = GetModuleHandleW(ptr::null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: module,
            lpszClassName: w!("io.github.fathialamre.Vento.Menu"),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.lpszClassName,
            ptr::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null(),
        )
    };
    if window.is_null() {
        log::warn!(
            "cannot make the menus' window: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    OWNER.with(|o| o.set(window));
    Some(window)
}

/// `entries` as a popup menu; item ids are one up, as 0 means none.
fn build(entries: &[MenuEntry]) -> HMENU {
    // SAFETY: a new menu, filled with live strings.
    unsafe {
        let menu = CreatePopupMenu();
        for entry in entries {
            match entry {
                MenuEntry::Item {
                    id, title, enabled, ..
                } => {
                    let text = wide(title);
                    let grey = if *enabled { 0 } else { MF_GRAYED };
                    AppendMenuW(menu, MF_STRING | grey, *id as usize + 1, text.as_ptr());
                }
                MenuEntry::Submenu {
                    title,
                    enabled,
                    children,
                } => {
                    let text = wide(title);
                    let grey = if *enabled { 0 } else { MF_GRAYED };
                    let sub = build(children);
                    AppendMenuW(menu, MF_POPUP | grey, sub as usize, text.as_ptr());
                }
                MenuEntry::Separator => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                }
            }
        }
        menu
    }
}

/// `text` NUL-terminated, for Win32.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Shows `entries` at `at` (logical, from `window`'s top-left) and waits
/// for a choice: the chosen item's id, or `None`.
pub(super) fn show(window: RawWindowHandle, at: Point, entries: &[MenuEntry]) -> Option<u32> {
    let hwnd = super::overlay::hwnd_of(window).ok()?;
    let owner = owner()?;
    // SAFETY: plain calls on live windows; the menu is destroyed (with its
    // submenus) once chosen from.
    unsafe {
        let scale = f64::from(GetDpiForWindow(hwnd).max(96)) / 96.0;
        let mut pt = POINT {
            x: (at.x * scale).round() as i32,
            y: (at.y * scale).round() as i32,
        };
        ClientToScreen(hwnd, &mut pt);
        let menu = build(entries);
        let before = GetForegroundWindow();
        SetForegroundWindow(owner);
        let rtl = if vento_core::i18n::current().rtl {
            TPM_LAYOUTRTL
        } else {
            0
        };
        let chosen = TrackPopupMenuEx(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | rtl,
            pt.x,
            pt.y,
            owner,
            ptr::null(),
        );
        // Lets the menu close properly (Win32's documented step).
        PostMessageW(owner, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if !before.is_null() && !super::is_vento(before) && IsWindow(before) != 0 {
            SetForegroundWindow(before);
        }
        u32::try_from(chosen).ok().and_then(|c| c.checked_sub(1))
    }
}
