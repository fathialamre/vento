//! Keeps the Mac awake with an IOKit power assertion. The kernel drops the
//! assertions of a process that exits, so one never outlives vento.

use std::cell::Cell;
use std::ffi::c_void;

use objc2_foundation::NSString;

use crate::{AwakeHold, PlatformError};

/// `kIOPMAssertionLevelOn`.
const LEVEL_ON: u32 = 255;
/// `kIOReturnSuccess`.
const SUCCESS: i32 = 0;
/// Why, as `pmset -g assertions` shows it.
const REASON: &str = "Vento: Keep Awake";

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    // See `IOKit/pwr_mgt/IOPMLib.h`. The strings are CFStrings, which an
    // NSString is (toll-free bridged).
    fn IOPMAssertionCreateWithName(
        assertion_type: *const c_void,
        level: u32,
        name: *const c_void,
        id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// The assertion vento holds, if any.
#[derive(Debug, Default)]
pub(super) struct Assertion {
    id: Cell<Option<u32>>,
}

impl Assertion {
    /// Holds `hold`, or releases with `None`. A new assertion is taken
    /// before the old one goes, so the Mac is never left without one in
    /// between.
    pub(super) fn set(&self, hold: Option<AwakeHold>) -> Result<(), PlatformError> {
        let new = match hold {
            Some(hold) => Some(create(hold)?),
            None => None,
        };
        if let Some(old) = self.id.replace(new) {
            // SAFETY: `old` came from `IOPMAssertionCreateWithName` and is
            // released once, here.
            let status = unsafe { IOPMAssertionRelease(old) };
            if status != SUCCESS {
                log::warn!("cannot release the Keep Awake assertion {old}: {status:#x}");
            }
        }
        Ok(())
    }
}

fn create(hold: AwakeHold) -> Result<u32, PlatformError> {
    // The display one keeps the system awake too.
    let kind = NSString::from_str(if hold.screen {
        "PreventUserIdleDisplaySleep"
    } else {
        "PreventUserIdleSystemSleep"
    });
    let reason = NSString::from_str(REASON);
    let mut id = 0u32;
    // SAFETY: both strings outlive the call, and `id` is an out-pointer.
    let status = unsafe {
        IOPMAssertionCreateWithName(
            objc2::rc::Retained::as_ptr(&kind).cast(),
            LEVEL_ON,
            objc2::rc::Retained::as_ptr(&reason).cast(),
            &mut id,
        )
    };
    if status == SUCCESS {
        Ok(id)
    } else {
        Err(PlatformError::Failed(format!(
            "Keep Awake: IOPMAssertionCreateWithName failed: {status:#x}"
        )))
    }
}
