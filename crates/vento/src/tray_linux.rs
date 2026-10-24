//! Linux stand-in for the menu-bar icon until the StatusNotifierItem tray
//! lands: nothing is shown, so the settings and Quit are out of reach for
//! now (stop vento with Ctrl+C).

use std::rc::Rc;

use vento_core::geometry::Rect;
use vento_platform::Platform;

use crate::exec::Executor;
use crate::pause::PauseSwitch;

/// Keeps nothing alive.
pub struct TrayHandle;

/// What the tray needs to act on clicks; the same as on macOS, so `main`
/// builds it the same way. Nothing reads it until the tray lands.
#[allow(dead_code)]
pub struct TrayDeps {
    pub exec: Rc<Executor>,
    pub platform: Rc<dyn Platform>,
    pub on_open_panel: Box<dyn Fn(Rect)>,
    pub pause: Rc<PauseSwitch>,
    pub on_copy_from_screen: Option<Box<dyn Fn()>>,
}

/// Shows no icon yet.
pub fn install_tray(deps: TrayDeps) -> TrayHandle {
    drop(deps);
    log::info!("no tray icon on Linux yet: stop vento with Ctrl+C");
    TrayHandle
}
