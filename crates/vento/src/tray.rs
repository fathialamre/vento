//! Menu-bar icon: undo, pause, settings file access and quit.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;

use log::{debug, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use vento_core::settings::{Settings, settings_path};
use vento_platform::Platform;

use crate::exec::Executor;
use crate::toast::Toaster;
use crate::wheel::WheelController;

/// Side length of the menu-bar icon in pixels.
const ICON_SIZE: u32 = 18;
/// Circumradius of the hexagon in pixels.
const HEX_RADIUS: f32 = 8.0;

/// Acts on a clicked menu item.
type Dispatch = Box<dyn Fn(&MenuId)>;

thread_local! {
    /// Acts on a clicked menu item; lives on the main thread, which owns the
    /// non-`Send` wheel and executor.
    static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
}

/// Keeps the menu-bar icon alive.
pub struct TrayHandle {
    _icon: TrayIcon,
}

/// An 18x18 RGBA image of a filled flat-top hexagon: black with an alpha
/// mask, so macOS can tint it as a template image.
fn tray_icon_rgba() -> Vec<u8> {
    let size = ICON_SIZE as usize;
    let centre = (ICON_SIZE as f32) / 2.0;
    let mut rgba = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            // Sample at the pixel centre.
            let dx = (x as f32 + 0.5 - centre).abs();
            let dy = (y as f32 + 0.5 - centre).abs();
            // Flat-top regular hexagon: flat edges up and down, points left
            // and right.
            let inside =
                dy <= HEX_RADIUS * 3f32.sqrt() / 2.0 && dx + dy / 3f32.sqrt() <= HEX_RADIUS;
            if inside {
                rgba[(y * size + x) * 4 + 3] = 255;
            }
        }
    }
    rgba
}

/// What the tray needs to act on menu clicks.
pub struct TrayDeps {
    pub ctrl: Rc<WheelController>,
    pub exec: Rc<Executor>,
    /// The settings copy the executor reads; a reload writes into it.
    pub shared: Rc<RefCell<Settings>>,
    /// Shows reload errors.
    pub toast: Rc<Toaster>,
    pub platform: Rc<dyn Platform>,
    /// Applies reloaded settings to what the tray doesn't own (the Pocket
    /// shortcut).
    pub on_reload: Box<dyn Fn(&Settings)>,
    /// Tells what the tray doesn't own (the Pocket) that vento was paused
    /// or unpaused.
    pub on_pause: Box<dyn Fn(bool)>,
}

/// Creates the menu-bar icon. Call on the main thread once the event loop is
/// running.
///
/// # Panics
/// If the icon or the menu cannot be created.
pub fn install_tray(deps: TrayDeps) -> TrayHandle {
    let TrayDeps {
        ctrl,
        exec,
        shared,
        toast,
        platform,
        on_reload,
        on_pause,
    } = deps;
    let undo = MenuItem::new("Undo last move", exec.has_undo(), None);
    let pause = CheckMenuItem::new("Pause vento", true, false, None);
    let open_settings = MenuItem::new("Open settings file", true, None);
    let reload = MenuItem::new("Reload settings", true, None);
    let quit = MenuItem::new("Quit vento", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &undo,
        &pause,
        &PredefinedMenuItem::separator(),
        &open_settings,
        &reload,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .expect("cannot build the tray menu");

    let icon = Icon::from_rgba(tray_icon_rgba(), ICON_SIZE, ICON_SIZE)
        .expect("tray icon has a valid size");
    let tray = TrayIconBuilder::new()
        .with_icon_templated(icon)
        .with_tooltip("vento")
        .with_menu(Box::new(menu))
        .build()
        .expect("cannot create the menu-bar icon");

    // Keep the Undo item in step with the executor, instead of polling it.
    {
        let undo = undo.clone();
        exec.on_undo_changed(Box::new(move |available| undo.set_enabled(available)));
    }

    // Menu clicks arrive via a handler that may run on any thread: forward
    // only the (Send) item id to the main thread and act on it there.
    DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |id: &MenuId| {
            if id == undo.id() {
                exec.undo_last();
            } else if id == pause.id() {
                ctrl.set_paused(pause.is_checked());
                on_pause(pause.is_checked());
            } else if id == open_settings.id() {
                open_settings_file();
            } else if id == reload.id() {
                reload_settings(&ctrl, &shared, &toast, platform.as_ref(), &*on_reload);
            } else if id == quit.id()
                && let Err(e) = slint::quit_event_loop()
            {
                warn!("cannot quit: {e}");
            }
        }));
    });
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        let id = event.id;
        if let Err(e) = slint::invoke_from_event_loop(move || {
            DISPATCH.with(|slot| {
                if let Some(dispatch) = slot.borrow().as_ref() {
                    dispatch(&id);
                }
            });
        }) {
            warn!("cannot deliver menu event: {e}");
        }
    }));

    TrayHandle { _icon: tray }
}

/// Opens the settings file in the default text editor, writing the defaults
/// first if the file does not exist.
fn open_settings_file() {
    let path = settings_path();
    if !path.exists()
        && let Err(e) = Settings::default().save(&path)
    {
        warn!("cannot create {}: {e}", path.display());
        return;
    }
    if let Err(e) = Command::new("open").arg("-t").arg(&path).spawn() {
        warn!("cannot open {}: {e}", path.display());
    }
}

/// Re-reads the settings file and applies it to the wheel, the executor and
/// (through `on_reload`) the Pocket shortcut.
/// An unreadable or invalid file keeps the current settings and says why in a
/// toast near the cursor (the menu bar).
fn reload_settings(
    ctrl: &WheelController,
    shared: &RefCell<Settings>,
    toast: &Toaster,
    platform: &dyn Platform,
    on_reload: &dyn Fn(&Settings),
) {
    let path = settings_path();
    match Settings::try_load(&path) {
        Ok(loaded) => {
            debug!("settings reloaded");
            *shared.borrow_mut() = loaded.clone();
            on_reload(&loaded);
            ctrl.apply_settings(loaded);
        }
        Err(e) => {
            warn!("settings not reloaded from {}: {e}", path.display());
            let at = platform.sample_input().cursor;
            toast.show(&format!("Settings not reloaded: {e}"), None, at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(rgba: &[u8], x: usize, y: usize) -> u8 {
        rgba[(y * ICON_SIZE as usize + x) * 4 + 3]
    }

    #[test]
    fn icon_has_the_right_length() {
        assert_eq!(tray_icon_rgba().len(), 18 * 18 * 4);
    }

    #[test]
    fn icon_centre_is_opaque() {
        assert_eq!(alpha(&tray_icon_rgba(), 9, 9), 255);
        assert_eq!(alpha(&tray_icon_rgba(), 8, 8), 255);
    }

    #[test]
    fn icon_corners_are_transparent() {
        let rgba = tray_icon_rgba();
        for (x, y) in [(0, 0), (17, 0), (0, 17), (17, 17)] {
            assert_eq!(alpha(&rgba, x, y), 0, "corner ({x}, {y})");
        }
    }

    #[test]
    fn icon_is_black_and_flat_top() {
        let rgba = tray_icon_rgba();
        assert!(rgba.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]));
        // Flat top: the topmost opaque row spans several pixels.
        let top_row = (0..18).find(|&y| (0..18).any(|x| alpha(&rgba, x, y) != 0));
        let width = (0..18)
            .filter(|&x| alpha(&rgba, x, top_row.unwrap()) != 0)
            .count();
        assert!(width >= 6, "top edge is {width} px wide");
    }
}
