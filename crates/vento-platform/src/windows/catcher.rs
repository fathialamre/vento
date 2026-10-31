//! The Pocket island's drop target: an invisible window of vento's own,
//! placed over the island during drags, that is an OLE drop target. winit's
//! own drop target takes only files; this one takes files, pictures, links
//! and text, as the Pocket does. OLE calls it on the main thread, from its
//! message loop.

use std::cell::{Cell, RefCell};
use std::ptr;
use std::rc::Rc;

use vento_core::geometry::Rect;
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, RegisterDragDrop,
    ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::core::{Ref, implement};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_TOPMOST, LWA_ALPHA, RegisterClassW, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetWindowPos, ShowWindow,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_sys::w;

use super::clipboard;
use super::screens;
use crate::{CatcherEvent, DropCatcher, Dropped};

/// What the catcher reports to.
type Handler = Rc<RefCell<Option<Box<dyn Fn(CatcherEvent)>>>>;

/// The formats a drop is read in, in the Pocket's order: files, a picture,
/// a web link, text.
const ORDER: [&str; 6] = [
    "CF_HDROP",
    "PNG",
    "CF_DIBV5",
    "CF_DIB",
    "UniformResourceLocatorW",
    "CF_UNICODETEXT",
];

/// The data of `format` (as the clipboard names it) in `data`, if it holds
/// it in memory.
fn read(data: &IDataObject, format: &str) -> Option<Vec<u8>> {
    let request = FORMATETC {
        cfFormat: clipboard::format_id(format) as u16,
        ptd: ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: COM calls on a live data object; the medium is released after
    // its memory was copied out while locked.
    unsafe {
        let mut medium = data.GetData(&request).ok()?;
        let global = medium.u.hGlobal;
        let size = GlobalSize(global);
        let at = GlobalLock(global);
        let bytes =
            (!at.is_null()).then(|| std::slice::from_raw_parts(at.cast::<u8>(), size).to_vec());
        if !at.is_null() {
            let _ = GlobalUnlock(global);
        }
        ReleaseStgMedium(&mut medium);
        bytes
    }
}

/// Whether `data` holds anything the Pocket takes.
fn takes(data: &IDataObject) -> bool {
    ORDER.iter().any(|format| {
        let request = FORMATETC {
            cfFormat: clipboard::format_id(format) as u16,
            ptd: ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        // SAFETY: COM call on a live data object.
        unsafe { data.QueryGetData(&request) }.is_ok()
    })
}

/// What was dropped, read in the Pocket's order.
fn fetch(data: &IDataObject) -> Option<Dropped> {
    if let Some(files) = read(data, "CF_HDROP").map(|d| clipboard::drop_list(&d))
        && !files.is_empty()
    {
        return Some(Dropped::Files(files.into_iter().map(Into::into).collect()));
    }
    let picture = read(data, "PNG")
        .filter(|b| !b.is_empty())
        .or_else(|| read(data, "CF_DIBV5").and_then(|d| clipboard::dib_to_png(&d)))
        .or_else(|| read(data, "CF_DIB").and_then(|d| clipboard::dib_to_png(&d)));
    if let Some(bytes) = picture {
        return Some(Dropped::Image { bytes, ext: "png" });
    }
    if let Some(url) = read(data, "UniformResourceLocatorW")
        .map(|d| clipboard::from_wide(&d))
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    {
        return Some(Dropped::Url(url));
    }
    read(data, "CF_UNICODETEXT")
        .map(|d| clipboard::from_wide(&d))
        .filter(|t| !t.is_empty())
        .map(Dropped::Text)
}

/// The OLE side of the catcher.
#[implement(IDropTarget)]
struct Target {
    handler: Handler,
    /// Whether the drag over the catcher holds something the Pocket takes.
    taken: Cell<bool>,
}

impl Target {
    fn report(&self, event: CatcherEvent) {
        if let Some(f) = self.handler.borrow().as_ref() {
            f(event);
        }
    }

    /// Sets the effect the drag shows: a copy while it is taken.
    fn effect(&self, effect: *mut DROPEFFECT) {
        let shown = if self.taken.get() {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        };
        if !effect.is_null() {
            // SAFETY: OLE passes a live out-pointer.
            unsafe { *effect = shown };
        }
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _at: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let taken = data.as_ref().is_some_and(takes);
        self.taken.set(taken);
        self.effect(effect);
        if taken {
            self.report(CatcherEvent::Entered);
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        _at: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        self.effect(effect);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if self.taken.replace(false) {
            self.report(CatcherEvent::Exited);
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _at: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let dropped = data.as_ref().and_then(fetch);
        self.taken.set(dropped.is_some());
        self.effect(effect);
        self.taken.set(false);
        self.report(match dropped {
            Some(d) => {
                log::debug!("dropped on the pocket: {d:?}");
                CatcherEvent::Dropped(d)
            }
            None => CatcherEvent::Exited,
        });
        Ok(())
    }
}

/// The Pocket's drop target on Windows.
pub(super) struct WindowsCatcher {
    window: windows_sys::Win32::Foundation::HWND,
    handler: Handler,
    /// Kept for as long as the window is registered with it.
    _target: IDropTarget,
}

impl std::fmt::Debug for WindowsCatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsCatcher").finish_non_exhaustive()
    }
}

impl WindowsCatcher {
    /// Makes the catcher's window, hidden, and registers it for drops.
    /// Main thread only, where OLE runs (winit started it).
    pub fn start() -> Result<Self, String> {
        let handler: Handler = Rc::new(RefCell::new(None));
        // SAFETY: a plain class and window, kept for vento's life; the
        // target is registered with the window that lives as long.
        unsafe {
            let module = GetModuleHandleW(ptr::null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: module,
                lpszClassName: w!("io.github.fathialamre.Vento.Catcher"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let window = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
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
            );
            if window.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            // Nearly clear, but there: a window drawn at 0 takes no drops.
            SetLayeredWindowAttributes(window, 0, 1, LWA_ALPHA);
            let target: IDropTarget = Target {
                handler: handler.clone(),
                taken: Cell::new(false),
            }
            .into();
            RegisterDragDrop(HWND(window), &target).map_err(|e| e.message())?;
            Ok(Self {
                window,
                handler,
                _target: target,
            })
        }
    }
}

impl DropCatcher for WindowsCatcher {
    fn show(&self, rect: Rect) {
        let monitors = screens::monitors();
        let scale = screens::monitor_at_logical(
            &monitors,
            vento_core::geometry::Point {
                x: rect.x,
                y: rect.y,
            },
        )
        .map_or(1.0, |m| m.scale);
        let px = |v: f64| (v * scale).round() as i32;
        // SAFETY: plain call on vento's own window.
        unsafe {
            SetWindowPos(
                self.window,
                HWND_TOPMOST,
                px(rect.x),
                px(rect.y),
                px(rect.w),
                px(rect.h),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    fn hide(&self) {
        // SAFETY: plain call on vento's own window.
        unsafe { ShowWindow(self.window, SW_HIDE) };
    }

    fn on_event(&self, f: Box<dyn Fn(CatcherEvent)>) {
        *self.handler.borrow_mut() = Some(f);
    }
}
