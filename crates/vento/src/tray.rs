//! Menu-bar icon: a left click opens the Quick panel, a right click shows
//! the menu, which offers the panel's quick actions (Copy from screen, the
//! clipboard history, the Pocket, Keep Awake, undo, pause) and Settings….
//! While Keep Awake holds the Mac awake, a cup shows beside it; a click on
//! the cup opens the Quick panel under it.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;

use log::warn;
use tray_icon::menu::{
    CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use vento_core::awake::Chip;
use vento_core::geometry::{Point, Rect};
use vento_core::i18n::{current, tr};
use vento_core::settings::{Settings, settings_path};
use vento_platform::Platform;

use crate::awake::{KeepAwake, keep_awake_menu_text};
use crate::exec::{Executor, undo_menu_text};
use crate::pause::PauseSwitch;
use crate::settings_hub::SettingsHub;

/// The flame mark as a black template image, 36 px: an 18 pt glyph at 2x.
const FLAME_PNG: &[u8] = include_bytes!("../assets/menu-bar-flame@2x.png");
/// The Keep Awake cup, the same way.
const CUP_PNG: &[u8] = include_bytes!("../assets/menu-bar-cup@2x.png");
/// Side length of the flame and the cup in pixels.
const FLAME_SIZE: u32 = 36;
/// The cup's tray icon id, which tells its clicks from Vento's.
const CUP_ID: &str = "vento-keep-awake";
/// Side length of the menu-bar icon in pixels. tray-icon shows an image
/// taller than 22 pt at 22 pt, so the 44 px icon lands at 22 pt and the
/// flame centred in it at 18 pt, sharp on Retina screens.
const ICON_SIZE: u32 = 44;

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
    /// Acts on a left click on the Keep Awake cup; main thread too.
    static CUP_DISPATCH: RefCell<Option<PanelDispatch>> = const { RefCell::new(None) };
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
    /// Sets the menu's text and direction again from the current language.
    retext: Box<dyn Fn()>,
}

impl TrayHandle {
    /// Writes every item's text again in the language now in use, and lays
    /// the menu out in its direction: call after the language changes.
    pub fn retext(&self) {
        (self.retext)();
    }
}

/// The menu's items, for writing their text again.
struct Texted {
    copy_from_screen: Option<MenuItem>,
    clipboard: MenuItem,
    pocket: MenuItem,
    keep_awake: MenuItem,
    undo: MenuItem,
    pause: CheckMenuItem,
    settings: MenuItem,
    open_settings: MenuItem,
    quit: MenuItem,
    exec: Rc<Executor>,
}

/// Lays the tray's menu out right to left in a right-to-left language and
/// left to right otherwise. AppKit reads the direction when the menu is
/// shown, so this goes with every change of the text.
fn set_menu_direction(menu: &Menu) {
    // SAFETY: `ns_menu` is the live NSMenu muda made for `menu`, which the
    // caller holds; the pointer is used at once on the main thread.
    unsafe { vento_platform::set_ns_menu_direction(menu.ns_menu(), current().rtl) };
}

/// A 44x44 RGBA image of the bundled 36x36 `mark` (the flame or the cup)
/// centred on a transparent margin: black with an alpha mask, so macOS can
/// tint it as a template image.
///
/// # Panics
/// If `mark` is not a 36x36 RGBA PNG.
fn tray_icon_rgba(mark: &[u8]) -> Vec<u8> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(mark))
        .read_info()
        .expect("the bundled flame is a PNG");
    let mut flame = vec![0u8; reader.output_buffer_size().expect("flame fits in memory")];
    let info = reader
        .next_frame(&mut flame)
        .expect("the bundled flame decodes");
    assert_eq!(
        (info.width, info.height, info.color_type, info.bit_depth),
        (
            FLAME_SIZE,
            FLAME_SIZE,
            png::ColorType::Rgba,
            png::BitDepth::Eight
        ),
        "the bundled flame is 36x36 RGBA8"
    );
    let size = ICON_SIZE as usize;
    let flame_size = FLAME_SIZE as usize;
    let margin = (size - flame_size) / 2;
    let mut rgba = vec![0u8; size * size * 4];
    for (y, row) in flame.chunks_exact(flame_size * 4).enumerate() {
        let start = ((y + margin) * size + margin) * 4;
        rgba[start..start + row.len()].copy_from_slice(row);
    }
    rgba
}

/// What the tray needs to act on clicks.
pub struct TrayDeps {
    pub exec: Rc<Executor>,
    /// Places the icon on its screen.
    pub platform: Rc<dyn Platform>,
    /// Shows its cup beside the icon while it holds the Mac awake.
    pub keep_awake: Rc<KeepAwake>,
    /// Opens the Quick panel under the icon or the cup; given the frame of
    /// the one clicked in global logical points.
    pub on_open_panel: Box<dyn Fn(Rect)>,
    /// Opens the island on the clipboard history.
    pub on_open_clipboard: Box<dyn Fn()>,
    /// Opens the island on the Pocket.
    pub on_open_pocket: Box<dyn Fn()>,
    /// Opens the Settings window.
    pub on_open_settings: Box<dyn Fn()>,
    /// Whether vento is paused, shared with the panel's Pause.
    pub pause: Rc<PauseSwitch>,
    /// The settings, for whether the clipboard history is on.
    pub hub: Rc<SettingsHub>,
    /// Starts Copy from screen; `None` leaves the item out (macOS 13).
    pub on_copy_from_screen: Option<Box<dyn Fn()>>,
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
        keep_awake,
        on_open_panel,
        on_open_clipboard,
        on_open_pocket,
        on_open_settings,
        pause,
        hub,
        on_copy_from_screen,
    } = deps;
    let paused = pause.is_paused();
    let copy_from_screen = on_copy_from_screen
        .map(|start| (MenuItem::new(tr("Copy from screen"), !paused, None), start));
    let clipboard = MenuItem::new(
        tr("Open clipboard history"),
        !paused && hub.current().clipboard_history,
        None,
    );
    let pocket = MenuItem::new(tr("Open the Pocket"), !paused, None);
    let keep_awake_item = MenuItem::new(
        keep_awake_menu_text(keep_awake.session().is_some()),
        true,
        None,
    );
    let undo = MenuItem::new(
        undo_menu_text(exec.has_undo()),
        exec.has_undo().is_some(),
        None,
    );
    let pause_item = CheckMenuItem::new(tr("Pause Vento"), true, paused, None);
    let settings_item = MenuItem::new(tr("Settings…"), true, None);
    let open_settings = MenuItem::new(tr("Open settings file"), true, None);
    let quit = MenuItem::new(tr("Quit Vento"), true, None);

    let menu = Menu::new();
    if let Some((item, _)) = &copy_from_screen {
        menu.append(item).expect("cannot build the tray menu");
    }
    menu.append_items(&[
        &clipboard,
        &pocket,
        &PredefinedMenuItem::separator(),
        &keep_awake_item,
        &undo,
        &pause_item,
        &PredefinedMenuItem::separator(),
        &settings_item,
        &open_settings,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .expect("cannot build the tray menu");
    set_menu_direction(&menu);

    let icon = Icon::from_rgba(tray_icon_rgba(FLAME_PNG), ICON_SIZE, ICON_SIZE)
        .expect("tray icon has a valid size");
    let tray = TrayIconBuilder::new()
        .with_icon_templated(icon)
        .with_tooltip("Vento")
        .with_menu(Box::new(menu.clone()))
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

    // Pausing greys out the items that do nothing meanwhile, and the
    // clipboard item is greyed too while the history is off. Both the
    // Pause and the setting move, so each listener refreshes all three.
    let sync_gated = {
        let items = (
            copy_from_screen.as_ref().map(|(item, _)| item.clone()),
            clipboard.clone(),
            pocket.clone(),
        );
        Rc::new(move |paused: bool, history: bool| {
            let (copy, clipboard, pocket) = &items;
            if let Some(copy) = copy {
                copy.set_enabled(!paused);
            }
            clipboard.set_enabled(!paused && history);
            pocket.set_enabled(!paused);
        })
    };

    // Keep the Pause item in step with the panel's Pause.
    {
        let pause_item = pause_item.clone();
        let (sync_gated, hub) = (sync_gated.clone(), hub.clone());
        pause.on_change(Box::new(move |paused| {
            pause_item.set_checked(paused);
            sync_gated(paused, hub.current().clipboard_history);
        }));
    }
    {
        let pause = Rc::downgrade(&pause);
        hub.on_change(Box::new(move |_old, new| {
            if let Some(pause) = pause.upgrade() {
                sync_gated(pause.is_paused(), new.clipboard_history);
            }
        }));
    }

    // Keep the Keep Awake item saying what a click does.
    {
        let item = keep_awake_item.clone();
        let keep = Rc::downgrade(&keep_awake);
        keep_awake.on_change(Box::new(move || {
            if let Some(keep) = keep.upgrade() {
                item.set_text(keep_awake_menu_text(keep.session().is_some()));
            }
        }));
    }

    // What retexting the menu sets again; the clicks below take the items.
    let texted = Texted {
        copy_from_screen: copy_from_screen.as_ref().map(|(item, _)| item.clone()),
        clipboard: clipboard.clone(),
        pocket: pocket.clone(),
        keep_awake: keep_awake_item.clone(),
        undo: undo.clone(),
        pause: pause_item.clone(),
        settings: settings_item.clone(),
        open_settings: open_settings.clone(),
        quit: quit.clone(),
        exec: exec.clone(),
    };

    // Menu clicks arrive via a handler that may run on any thread: forward
    // only the (Send) item id to the main thread and act on it there.
    let opener = platform.clone();
    let menu_keep = keep_awake.clone();
    DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |id: &MenuId| {
            if let Some((item, start)) = &copy_from_screen
                && id == item.id()
            {
                start();
            } else if id == clipboard.id() {
                on_open_clipboard();
            } else if id == pocket.id() {
                on_open_pocket();
            } else if id == keep_awake_item.id() {
                if menu_keep.session().is_some() {
                    menu_keep.stop();
                } else {
                    menu_keep.start(Chip::Forever);
                }
            } else if id == undo.id() {
                exec.undo_last();
            } else if id == pause_item.id() {
                pause.set(pause_item.is_checked());
            } else if id == settings_item.id() {
                on_open_settings();
            } else if id == open_settings.id() {
                open_settings_file(&*opener);
            } else if id == quit.id() {
                // A job still running would leave its partial output behind.
                exec.shutdown();
                if let Err(e) = slint::quit_event_loop() {
                    warn!("cannot quit: {e}");
                }
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

    // The cup comes and goes with Keep Awake.
    let cup: Rc<RefCell<Option<TrayIcon>>> = Rc::new(RefCell::new(None));
    {
        let cup = cup.clone();
        let shown = Cell::new(false);
        let keep = Rc::downgrade(&keep_awake);
        keep_awake.on_change(Box::new(move || {
            let on = keep.upgrade().is_some_and(|k| k.session().is_some());
            if shown.replace(on) == on {
                return;
            }
            *cup.borrow_mut() = if on { build_cup() } else { None };
        }));
    }

    // A left click on the icon or on the cup opens the panel under it. Same
    // forwarding as the menu: only the (Send) frame crosses to the main
    // thread.
    let frame_of = {
        let platform = platform.clone();
        move |physical: &tray_icon::Rect| {
            let cursor = platform.sample_input().cursor;
            icon_frame(physical, cursor, &platform.screens(), &|p| {
                platform.scale_factor_at(p)
            })
        }
    };
    let cup_frame_of = frame_of.clone();
    let on_open_panel: Rc<dyn Fn(Rect)> = Rc::from(on_open_panel);
    let on_open_cup = on_open_panel.clone();
    PANEL_DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |physical: tray_icon::Rect| {
            on_open_panel(frame_of(&physical));
        }));
    });
    CUP_DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |physical: tray_icon::Rect| {
            on_open_cup(cup_frame_of(&physical));
        }));
    });
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        let TrayIconEvent::Click {
            id,
            rect,
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        else {
            return;
        };
        let on_cup = id == CUP_ID;
        if let Err(e) = slint::invoke_from_event_loop(move || {
            let slot = if on_cup {
                &CUP_DISPATCH
            } else {
                &PANEL_DISPATCH
            };
            slot.with(|slot| {
                if let Some(dispatch) = slot.borrow().as_ref() {
                    dispatch(rect);
                }
            });
        }) {
            warn!("cannot deliver tray click: {e}");
        }
    }));

    let retext = {
        let keep = Rc::downgrade(&keep_awake);
        move || {
            if let Some(copy) = &texted.copy_from_screen {
                copy.set_text(tr("Copy from screen"));
            }
            texted.clipboard.set_text(tr("Open clipboard history"));
            texted.pocket.set_text(tr("Open the Pocket"));
            if let Some(keep) = keep.upgrade() {
                texted
                    .keep_awake
                    .set_text(keep_awake_menu_text(keep.session().is_some()));
            }
            texted.undo.set_text(undo_menu_text(texted.exec.has_undo()));
            texted.pause.set_text(tr("Pause Vento"));
            texted.settings.set_text(tr("Settings…"));
            texted.open_settings.set_text(tr("Open settings file"));
            texted.quit.set_text(tr("Quit Vento"));
            if let Some(cup) = &*cup.borrow()
                && let Err(e) = cup.set_tooltip(Some(cup_tooltip()))
            {
                warn!("cannot retext the Keep Awake cup: {e}");
            }
            set_menu_direction(&menu);
        }
    };
    TrayHandle {
        _icon: tray,
        retext: Box::new(retext),
    }
}

/// The Keep Awake cup's tooltip.
fn cup_tooltip() -> String {
    tr("Keep Awake")
}

/// The Keep Awake cup's menu-bar icon, with no menu: a click opens the
/// panel. `None`, logged, when macOS will not make it.
fn build_cup() -> Option<TrayIcon> {
    let icon = Icon::from_rgba(tray_icon_rgba(CUP_PNG), ICON_SIZE, ICON_SIZE)
        .expect("the cup has a valid size");
    TrayIconBuilder::new()
        .with_id(CUP_ID)
        .with_icon_templated(icon)
        .with_tooltip(cup_tooltip())
        .build()
        .inspect_err(|e| warn!("cannot show the Keep Awake cup: {e}"))
        .ok()
}

/// Opens the settings file in the default text editor, writing the defaults
/// first if the file does not exist. `open -t` needs no platform; it is
/// taken to match Linux, where the platform opens the file.
pub(crate) fn open_settings_file(_platform: &dyn Platform) {
    let path = settings_path();
    if !path.exists()
        && let Err(e) = Settings::default().save(&path)
    {
        warn!("cannot create {}: {e}", path.display());
        return;
    }
    // Waited for on a thread of its own: unwaited, `open` would stay a
    // zombie, and waiting here would stall the menu.
    let opener = std::thread::Builder::new()
        .name("open-settings".into())
        .spawn(
            move || match Command::new("open").arg("-t").arg(&path).status() {
                Ok(status) if status.success() => {}
                Ok(status) => warn!("open -t {} failed: {status}", path.display()),
                Err(e) => warn!("cannot open {}: {e}", path.display()),
            },
        );
    if let Err(e) = opener {
        warn!("cannot start opening the settings file: {e}");
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
        assert_eq!(tray_icon_rgba(FLAME_PNG).len(), 44 * 44 * 4);
        assert_eq!(tray_icon_rgba(CUP_PNG).len(), 44 * 44 * 4);
    }

    #[test]
    fn icon_margin_is_transparent() {
        let rgba = tray_icon_rgba(FLAME_PNG);
        for y in 0..44 {
            for x in 0..44 {
                if !(4..40).contains(&x) || !(4..40).contains(&y) {
                    assert_eq!(alpha(&rgba, x, y), 0, "margin pixel ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn icon_holds_the_flame() {
        let rgba = tray_icon_rgba(FLAME_PNG);
        let opaque = (0..44 * 44)
            .filter(|i| alpha(&rgba, i % 44, i / 44) == 255)
            .count();
        assert!(opaque > 100, "{opaque} opaque pixels");
    }

    #[test]
    fn icon_is_black() {
        let rgba = tray_icon_rgba(FLAME_PNG);
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 0 || p[..3] == [0, 0, 0])
        );
    }
}
