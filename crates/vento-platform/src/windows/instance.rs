//! One Vento per session, and the session's end. The first Vento holds a
//! named mutex and keeps a hidden window on a thread of its own; a later
//! launch (from the Start menu, or a second start at login) finds that
//! window, posts it a message asking to open the settings, and goes. The
//! request waits for the main thread, which hands it on from
//! [`asked_to_open`] on every input sample, as on Linux.
//!
//! The window is a hidden top-level one, not a message-only one, so that it
//! also hears the session end (sign-out, shutdown): it holds Windows back,
//! for a few seconds at most, while the main thread tidies up
//! ([`asked_to_end`], [`ended`]).

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW,
    GetMessageW, GetWindowThreadProcessId, MSG, PostMessageW, RegisterClassW, WM_APP,
    WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_sys::w;

/// Held by the running Vento for the whole session ("Local\": this
/// session only, so another user's Vento does not count).
const MUTEX: windows_sys::core::PCWSTR = w!("Local\\io.github.fathialamre.Vento");
/// The class of the window a later launch looks for.
const CLASS: windows_sys::core::PCWSTR = w!("io.github.fathialamre.Vento.Instance");
/// Asks the running Vento to open its settings.
const WM_OPEN: u32 = WM_APP + 1;
/// The longest the session's end waits for vento to tidy up; Windows
/// itself waits about five seconds before offering to end it anyway.
const END_WAIT: Duration = Duration::from_secs(4);

/// Set when another launch asked this Vento to open.
static OPEN_ASKED: AtomicBool = AtomicBool::new(false);
/// Set when the session ends.
static END_ASKED: AtomicBool = AtomicBool::new(false);
/// Set by the main thread once it has tidied up.
static END_DONE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Whether this is the only Vento: true after taking the mutex (or when it
/// cannot be made, where there is nothing to ask), false after asking the
/// Vento that holds it to open its settings.
pub fn claim_instance() -> bool {
    // SAFETY: plain call; the name is a static wide string.
    let mutex = unsafe { CreateMutexW(ptr::null(), 0, MUTEX) };
    if mutex.is_null() {
        log::warn!(
            "cannot keep vento to one copy: {}",
            std::io::Error::last_os_error()
        );
        return true;
    }
    // SAFETY: read right after the call that set it.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        ask_to_open();
        return false;
    }
    // The handle stays open, and so the mutex held, until vento exits.
    serve();
    true
}

/// Asks the running Vento to open its settings, letting it come to the
/// front: Windows only lets the app the user just started take the focus.
fn ask_to_open() {
    // SAFETY: plain calls with live arguments.
    unsafe {
        let window = FindWindowW(CLASS, ptr::null());
        if window.is_null() {
            log::warn!("vento is already running, and did not answer");
            return;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        AllowSetForegroundWindow(pid);
        if PostMessageW(window, WM_OPEN, 0, 0) == 0 {
            log::warn!(
                "vento is already running, and did not answer: {}",
                std::io::Error::last_os_error()
            );
        } else {
            log::info!("vento is already running: asked it to open its settings");
        }
    }
}

/// Answers [`WM_OPEN`] and the session's end from a hidden window on a
/// thread of its own.
fn serve() {
    let spawned = std::thread::Builder::new()
        .name("vento-instance".into())
        .spawn(|| {
            // SAFETY: the class and window live as long as this thread,
            // which runs their message loop until the process ends.
            unsafe {
                let module = GetModuleHandleW(ptr::null());
                let class = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: module,
                    lpszClassName: CLASS,
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    log::warn!(
                        "cannot answer other launches: {}",
                        std::io::Error::last_os_error()
                    );
                    return;
                }
                let window = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    CLASS,
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
                    log::warn!(
                        "cannot answer other launches: {}",
                        std::io::Error::last_os_error()
                    );
                    return;
                }
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&msg);
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("cannot answer other launches: {e}");
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_OPEN => {
            OPEN_ASKED.store(true, Ordering::SeqCst);
            return 0;
        }
        // vento never stands in the session's way.
        WM_QUERYENDSESSION => return 1,
        WM_ENDSESSION if wparam != 0 => {
            END_ASKED.store(true, Ordering::SeqCst);
            let (done, ended) = &END_DONE;
            let guard = done.lock().unwrap_or_else(|e| e.into_inner());
            let _ = ended.wait_timeout_while(guard, END_WAIT, |done| !*done);
            return 0;
        }
        _ => {}
    }
    // SAFETY: the arguments are the ones Windows passed in.
    unsafe { DefWindowProcW(window, msg, wparam, lparam) }
}

/// Whether another launch asked this Vento to open since the last call.
pub(super) fn asked_to_open() -> bool {
    OPEN_ASKED.swap(false, Ordering::SeqCst)
}

/// Whether the session is ending.
pub(super) fn asked_to_end() -> bool {
    END_ASKED.load(Ordering::SeqCst)
}

/// Lets the session end: vento has tidied up.
pub(super) fn ended() {
    let (done, ended) = &END_DONE;
    *done.lock().unwrap_or_else(|e| e.into_inner()) = true;
    ended.notify_all();
}
