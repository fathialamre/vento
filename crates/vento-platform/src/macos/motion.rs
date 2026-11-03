//! What the Mac says about motion and power: Reduce motion and Low Power
//! Mode. There is no public API for Focus, so quiet time is never reported.

use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSProcessInfo;

/// Whether System Settings > Accessibility > Display > Reduce motion is on.
pub(super) fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// Whether Low Power Mode is on.
pub(super) fn power_saving() -> bool {
    NSProcessInfo::processInfo().isLowPowerModeEnabled()
}
