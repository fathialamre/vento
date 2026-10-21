//! Menu-bar icon: a left click opens the panel, a right click shows the menu
//! with undo, pause, settings file access and quit.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;

use log::warn;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use vento_core::geometry::{Point, Rect};
use vento_core::settings::{Settings, settings_path};
use vento_platform::Platform;

use crate::exec::{Executor, UndoKind};
use crate::pause::PauseSwitch;

/// Side length of the menu-bar icon in pixels.
const ICON_SIZE: u32 = 18;
/// Circumradius of the hexagon in pixels.
const HEX_RADIUS: f32 = 8.0;

/// Acts on a clicked menu item.
type Dispatch = Box<dyn Fn(&MenuId)>;

/// Opens the panel for a left click on the icon, given the icon's frame in
/// physical pixels.
type PanelDispatch = Box<dyn Fn(tray_icon::Rect)>;

thread_local! {
    /// Acts on a clicked menu item; lives on the main thread, which owns the
    /// non-`Send` wheel and executor.
    static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
    /// Acts on a left click on the icon; lives on the main thread too.
    static PANEL_DISPATCH: RefCell<Option<PanelDispatch>> = const { RefCell::new(None) };
}

/// How far above a screen's work area (which starts below the menu bar) the
/// icon's centre may lie and still count as on that screen.
const MENU_BAR_SLACK: f64 = 80.0;

/// How far outside the icon's frame the cursor may be and still count as on
/// it, in points.
const CURSOR_SLACK: f64 = 2.0;

/// The icon's frame in global logical points, from its frame in physical
/// pixels.
///
/// The pixels are the icon's logical frame times the backing scale of the
/// screen it is on. The cursor is on the icon when it is clicked, so the
/// scale at the cursor (`scale_at`, given a global logical point) is tried
/// first; it is believed when the frame it gives holds the cursor. That
/// settles screens of different scales, where one pixel value can be a valid
/// position on two of them. Otherwise each screen is tried with its own scale
/// and the first one the frame lands on wins. Work areas start below the menu
/// bar, where the icon is, so the icon may lie above the work area by
/// [`MENU_BAR_SLACK`]. With no match the first screen's scale is used.
fn icon_frame(
    physical: &tray_icon::Rect,
    cursor: Point,
    screens: &[Rect],
    scale_at: &dyn Fn(Point) -> f64,
) -> Rect {
    let scaled = |scale: f64| Rect {
        x: physical.position.x / scale,
        y: physical.position.y / scale,
        w: f64::from(physical.size.width) / scale,
        h: f64::from(physical.size.height) / scale,
    };
    let centre_of = |r: &Rect| Point {
        x: r.x + r.w / 2.0,
        y: r.y + r.h / 2.0,
    };
    let at_cursor = scaled(scale_at(cursor));
    if cursor.x >= at_cursor.x - CURSOR_SLACK
        && cursor.x <= at_cursor.x + at_cursor.w + CURSOR_SLACK
        && cursor.y >= at_cursor.y - CURSOR_SLACK
        && cursor.y <= at_cursor.y + at_cursor.h + CURSOR_SLACK
    {
        return at_cursor;
    }
    for screen in screens {
        let frame = scaled(scale_at(centre_of(screen)));
        let c = centre_of(&frame);
        if c.x >= screen.x
            && c.x < screen.x + screen.w
            && c.y >= screen.y - MENU_BAR_SLACK
            && c.y < screen.y + screen.h
        {
            return frame;
        }
    }
    screens
        .first()
        .map_or_else(|| scaled(1.0), |s| scaled(scale_at(centre_of(s))))
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

/// The Undo item's text: names what would be undone, and reads "move" when
/// there is nothing to undo.
fn undo_menu_text(kind: Option<UndoKind>) -> &'static str {
    match kind {
        Some(UndoKind::Copy) => "Undo last copy",
        Some(UndoKind::Archive) => "Undo last archive",
        Some(UndoKind::Move) | None => "Undo last move",
    }
}

/// What the tray needs to act on clicks.
pub struct TrayDeps {
    pub exec: Rc<Executor>,
    /// Places the icon on its screen.
    pub platform: Rc<dyn Platform>,
    /// Opens the panel under the icon; given the icon's frame in global
    /// logical points.
    pub on_open_panel: Box<dyn Fn(Rect)>,
    /// Whether vento is paused, shared with the panel's Pause.
    pub pause: Rc<PauseSwitch>,
}

/// Creates the menu-bar icon. Call on the main thread once the event loop is
/// running.
///
/// # Panics
/// If the icon or the menu cannot be created.
pub fn install_tray(deps: TrayDeps) -> TrayHandle {
    let TrayDeps {
        exec,
        platform,
        on_open_panel,
        pause,
    } = deps;
    let undo = MenuItem::new(
        undo_menu_text(exec.has_undo()),
        exec.has_undo().is_some(),
        None,
    );
    let pause_item = CheckMenuItem::new("Pause vento", true, pause.is_paused(), None);
    let open_settings = MenuItem::new("Open settings file", true, None);
    let quit = MenuItem::new("Quit vento", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &undo,
        &pause_item,
        &PredefinedMenuItem::separator(),
        &open_settings,
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
        .with_menu_on_left_click(false)
        .build()
        .expect("cannot create the menu-bar icon");

    // Keep the Undo item in step with the executor, instead of polling it.
    {
        let undo = undo.clone();
        exec.on_undo_changed(Box::new(move |kind| {
            undo.set_text(undo_menu_text(kind));
            undo.set_enabled(kind.is_some());
        }));
    }

    // Keep the Pause item in step with the panel's Pause.
    {
        let pause_item = pause_item.clone();
        pause.on_change(Box::new(move |paused| pause_item.set_checked(paused)));
    }

    // Menu clicks arrive via a handler that may run on any thread: forward
    // only the (Send) item id to the main thread and act on it there.
    DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |id: &MenuId| {
            if id == undo.id() {
                exec.undo_last();
            } else if id == pause_item.id() {
                pause.set(pause_item.is_checked());
            } else if id == open_settings.id() {
                open_settings_file();
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

    // A left click on the icon opens the panel. Same forwarding as the menu:
    // only the (Send) frame crosses to the main thread.
    PANEL_DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |physical: tray_icon::Rect| {
            let cursor = platform.sample_input().cursor;
            let frame = icon_frame(&physical, cursor, &platform.screens(), &|p| {
                platform.scale_factor_at(p)
            });
            on_open_panel(frame);
        }));
    });
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        let TrayIconEvent::Click {
            rect,
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        else {
            return;
        };
        if let Err(e) = slint::invoke_from_event_loop(move || {
            PANEL_DISPATCH.with(|slot| {
                if let Some(dispatch) = slot.borrow().as_ref() {
                    dispatch(rect);
                }
            });
        }) {
            warn!("cannot deliver tray click: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn physical(x: f64, y: f64, w: u32, h: u32) -> tray_icon::Rect {
        tray_icon::Rect {
            position: tray_icon::dpi::PhysicalPosition::new(x, y),
            size: tray_icon::dpi::PhysicalSize::new(w, h),
        }
    }

    /// A cursor that is nowhere near any icon, so the screen search decides.
    const FAR: Point = Point {
        x: -5000.0,
        y: -5000.0,
    };

    fn screen(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn icon_frame_divides_by_the_screens_scale() {
        // A retina screen with a 25 pt menu bar; the icon is 36 px wide.
        let screens = [screen(0.0, 25.0, 1440.0, 875.0)];
        let f = icon_frame(&physical(2800.0, 0.0, 36, 48), FAR, &screens, &|_| 2.0);
        assert_eq!((f.x, f.y, f.w, f.h), (1400.0, 0.0, 18.0, 24.0));
    }

    #[test]
    fn icon_frame_uses_the_scale_of_the_screen_it_lands_on() {
        // A retina primary and a 1x screen to its right.
        let screens = [
            screen(0.0, 25.0, 1440.0, 875.0),
            screen(1440.0, 25.0, 1920.0, 1055.0),
        ];
        let scale = |p: Point| if p.x < 1440.0 { 2.0 } else { 1.0 };
        // Divided by 2 this would be at 1500 pt: off the primary screen.
        let f = icon_frame(&physical(3000.0, 0.0, 24, 24), FAR, &screens, &scale);
        assert_eq!((f.x, f.y, f.w, f.h), (3000.0, 0.0, 24.0, 24.0));
        // On the primary screen the first match wins.
        let f = icon_frame(&physical(2000.0, 0.0, 36, 48), FAR, &screens, &scale);
        assert_eq!((f.x, f.w), (1000.0, 18.0));
    }

    #[test]
    fn icon_frame_trusts_the_cursor_in_the_ambiguous_range() {
        // A 2x primary (1440 pt) beside a 1x screen. The pixel position 2000
        // is valid on both: 1000 pt on the primary, 2000 pt on the 1x screen.
        let screens = [
            screen(0.0, 25.0, 1440.0, 875.0),
            screen(1440.0, 25.0, 1920.0, 1055.0),
        ];
        let scale = |p: Point| if p.x < 1440.0 { 2.0 } else { 1.0 };
        let icon = physical(2000.0, 0.0, 24, 24);
        // The click was on the 1x screen's menu bar.
        let on_second = Point { x: 2010.0, y: 12.0 };
        let f = icon_frame(&icon, on_second, &screens, &scale);
        assert_eq!((f.x, f.w), (2000.0, 24.0));
        // The click was on the primary's menu bar.
        let on_first = Point { x: 1005.0, y: 6.0 };
        let f = icon_frame(&physical(2000.0, 0.0, 36, 48), on_first, &screens, &scale);
        assert_eq!((f.x, f.w), (1000.0, 18.0));
    }

    #[test]
    fn icon_frame_ignores_a_cursor_that_is_not_on_the_icon() {
        let screens = [
            screen(0.0, 25.0, 1440.0, 875.0),
            screen(1440.0, 25.0, 1920.0, 1055.0),
        ];
        let scale = |p: Point| if p.x < 1440.0 { 2.0 } else { 1.0 };
        // The cursor moved away: the screen search still finds the icon.
        let away = Point { x: 700.0, y: 400.0 };
        let f = icon_frame(&physical(3000.0, 0.0, 24, 24), away, &screens, &scale);
        assert_eq!((f.x, f.w), (3000.0, 24.0));
    }

    #[test]
    fn icon_frame_without_a_match_uses_the_first_screens_scale() {
        let screens = [screen(0.0, 25.0, 1440.0, 875.0)];
        let f = icon_frame(&physical(9000.0, 0.0, 36, 48), FAR, &screens, &|_| 2.0);
        assert_eq!((f.x, f.w), (4500.0, 18.0));
        let f = icon_frame(&physical(9000.0, 0.0, 36, 48), FAR, &[], &|_| 2.0);
        assert_eq!((f.x, f.w), (9000.0, 36.0));
    }

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

    #[test]
    fn undo_item_names_what_it_would_undo() {
        assert_eq!(undo_menu_text(Some(UndoKind::Move)), "Undo last move");
        assert_eq!(undo_menu_text(Some(UndoKind::Copy)), "Undo last copy");
        assert_eq!(undo_menu_text(Some(UndoKind::Archive)), "Undo last archive");
        assert_eq!(undo_menu_text(None), "Undo last move");
    }
}
