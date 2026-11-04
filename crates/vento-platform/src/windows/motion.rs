//! What Windows says about motion, notifications and power: animations in
//! Settings, the notification state (Focus assist, a game, a presentation)
//! and Battery Saver.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::S_OK;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::UI::Shell::SHQueryUserNotificationState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
};

use crate::motion::quiet_from_state;

/// Whether animations are off (Settings > Accessibility > Visual effects).
/// A failed call is not asked-for reduction.
pub(super) fn reduce_motion() -> bool {
    let mut animations: i32 = 1; // a BOOL
    // SAFETY: `animations` outlives the call and is the BOOL this action
    // writes.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&raw mut animations).cast::<c_void>(),
            0,
        )
    };
    ok != 0 && animations == 0
}

/// Whether Windows is holding notifications back.
pub(super) fn quiet_time() -> bool {
    let mut state = 0;
    // SAFETY: `state` outlives the call.
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    hr == S_OK && quiet_from_state(state)
}

/// Whether Battery Saver is on.
pub(super) fn power_saving() -> bool {
    // SAFETY: an all-zero status is a valid value to fill in.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: `status` outlives the call.
    let ok = unsafe { GetSystemPowerStatus(&mut status) };
    ok != 0 && status.SystemStatusFlag == 1
}
