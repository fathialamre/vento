//! The tray icon on Linux: a StatusNotifierItem, which Ubuntu's AppIndicator
//! extension shows in GNOME's top bar. A left click opens the panel; the
//! menu has Settings (for hosts where a left click opens the menu too),
//! then the items macOS has.

use std::cell::RefCell;
use std::rc::Rc;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{Icon, MenuItem, ToolTip, Tray};
use log::warn;
use vento_core::geometry::{Point, Rect};
use vento_core::settings::{Settings, settings_path};
use vento_platform::Platform;

use crate::exec::{Executor, UndoKind, undo_menu_text};
use crate::pause::PauseSwitch;

/// The white mark, for GNOME's dark top bar, at two sizes; the host shows
/// the nearer one.
const MARKS: [&[u8]; 2] = [
    include_bytes!("../assets/tray-mark-32.png"),
    include_bytes!("../assets/tray-mark-64.png"),
];

/// Width given to the icon when the panel is placed under it.
const ICON_W: f64 = 24.0;

/// What a click asks for, sent from the tray's thread to the main thread.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    OpenPanel,
    CopyFromScreen,
    Undo,
    Pause(bool),
    OpenSettingsFile,
    Quit,
}

/// Acts on a click.
type Dispatch = Box<dyn Fn(Action)>;

thread_local! {
    /// Acts on a click; lives on the main thread, which owns the non-`Send`
    /// executor and panel.
    static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
}

/// Hands `action` to the main thread. Called by ksni for a click, when the
/// tray host may have given an activation token for it: vento's windows
/// cannot use it, so it is ended at once, or GNOME's pointer stays busy
/// until the token times out.
fn send(action: Action) {
    if let Some(token) = ksni::current_xdg_activation_token() {
        let spawned = std::thread::Builder::new()
            .name("vento-end-startup".into())
            .spawn(move || vento_platform::end_startup(&token));
        if let Err(e) = spawned {
            warn!("cannot end the tray click's startup: {e}");
        }
    }
    if let Err(e) = slint::invoke_from_event_loop(move || {
        DISPATCH.with(|slot| {
            if let Some(dispatch) = slot.borrow().as_ref() {
                dispatch(action);
            }
        });
    }) {
        warn!("cannot deliver tray click: {e}");
    }
}

/// A PNG as the ARGB32 (network byte order) icon the protocol wants.
fn to_icon(png: &[u8]) -> Option<Icon> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .ok()?;
    let mut rgba = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let data = rgba[..info.buffer_size()]
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect();
    Some(Icon {
        width: i32::try_from(info.width).ok()?,
        height: i32::try_from(info.height).ok()?,
        data,
    })
}

/// Where the icon is when it was clicked at `cursor`: a slot of the top bar
/// (the `bar_h` above the first screen's work area) around it.
fn icon_frame(cursor: Point, screen_top: f64, bar_h: f64) -> Rect {
    Rect {
        x: cursor.x - ICON_W / 2.0,
        y: screen_top,
        w: ICON_W,
        h: bar_h,
    }
}

/// The tray's state, kept on its thread.
struct VentoTray {
    icons: Vec<Icon>,
    copy_from_screen: bool,
    undo: Option<UndoKind>,
    paused: bool,
}

impl Tray for VentoTray {
    fn id(&self) -> String {
        "vento".into()
    }

    fn title(&self) -> String {
        "Vento".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Vento".into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        send(Action::OpenPanel);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: &str, enabled: bool, action: Action| {
            MenuItem::Standard(StandardItem {
                label: label.into(),
                enabled,
                activate: Box::new(move |_: &mut Self| send(action)),
                ..Default::default()
            })
        };
        let mut menu = vec![
            item("Settings…", true, Action::OpenPanel),
            MenuItem::Separator,
        ];
        if self.copy_from_screen {
            menu.push(item("Copy from screen", true, Action::CopyFromScreen));
            menu.push(MenuItem::Separator);
        }
        menu.extend([
            item(undo_menu_text(self.undo), self.undo.is_some(), Action::Undo),
            MenuItem::Checkmark(CheckmarkItem {
                label: "Pause Vento".into(),
                checked: self.paused,
                activate: Box::new(|tray: &mut Self| {
                    tray.paused = !tray.paused;
                    send(Action::Pause(tray.paused));
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            item("Open settings file", true, Action::OpenSettingsFile),
            MenuItem::Separator,
            item("Quit Vento", true, Action::Quit),
        ]);
        menu
    }
}

/// Keeps the tray alive; dropping it removes the icon.
pub struct TrayHandle {
    _handle: Option<ksni::blocking::Handle<VentoTray>>,
}

/// What the tray needs to act on clicks; the same as on macOS.
pub struct TrayDeps {
    pub exec: Rc<Executor>,
    /// Places the panel under the icon.
    pub platform: Rc<dyn Platform>,
    /// Opens the panel under the icon; given the icon's frame in global
    /// logical points.
    pub on_open_panel: Box<dyn Fn(Rect)>,
    /// Whether vento is paused, shared with the panel's Pause.
    pub pause: Rc<PauseSwitch>,
    /// Starts Copy from screen; `None` leaves the item out.
    pub on_copy_from_screen: Option<Box<dyn Fn()>>,
}

/// Shows the tray icon. Call on the main thread once the event loop is
/// running. Without a tray host (GNOME without the AppIndicator extension)
/// nothing shows, and the handle keeps nothing.
pub fn install_tray(deps: TrayDeps) -> TrayHandle {
    let TrayDeps {
        exec,
        platform,
        on_open_panel,
        pause,
        on_copy_from_screen,
    } = deps;
    let tray = VentoTray {
        icons: MARKS.iter().filter_map(|png| to_icon(png)).collect(),
        copy_from_screen: on_copy_from_screen.is_some(),
        undo: exec.has_undo(),
        paused: pause.is_paused(),
    };
    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(e) => {
            warn!("no tray icon: {e}");
            return TrayHandle { _handle: None };
        }
    };

    // Keep the Undo item and the Pause check in step with the app.
    {
        let handle = handle.clone();
        exec.on_undo_changed(Box::new(move |kind| {
            handle.update(|tray| tray.undo = kind);
        }));
    }
    {
        let handle = handle.clone();
        pause.on_change(Box::new(move |paused| {
            handle.update(|tray| tray.paused = paused);
        }));
    }

    DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |action| match action {
            Action::OpenPanel => {
                let cursor = platform.sample_input().cursor;
                let (top, bar_h) = platform
                    .pocket_home()
                    .map_or((0.0, 32.0), |home| (home.screen.y, home.menu_bar_h));
                on_open_panel(icon_frame(cursor, top, bar_h));
            }
            Action::CopyFromScreen => {
                if let Some(start) = &on_copy_from_screen {
                    start();
                }
            }
            Action::Undo => {
                exec.undo_last();
            }
            Action::Pause(paused) => pause.set(paused),
            Action::OpenSettingsFile => open_settings_file(&*platform),
            Action::Quit => {
                // A job still running would leave its partial output behind.
                exec.shutdown();
                if let Err(e) = slint::quit_event_loop() {
                    warn!("cannot quit: {e}");
                }
            }
        }));
    });

    TrayHandle {
        _handle: Some(handle),
    }
}

/// Opens the settings file with its default app, writing the defaults
/// first if the file does not exist.
fn open_settings_file(platform: &dyn Platform) {
    let path = settings_path();
    if !path.exists()
        && let Err(e) = Settings::default().save(&path)
    {
        warn!("cannot create {}: {e}", path.display());
        return;
    }
    if let Err(e) = platform.open_paths(std::slice::from_ref(&path)) {
        warn!("cannot open {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_marks_become_argb_icons() {
        let icons: Vec<Icon> = MARKS.iter().filter_map(|png| to_icon(png)).collect();
        assert_eq!(icons.iter().map(|i| i.width).collect::<Vec<_>>(), [32, 64]);
        for icon in &icons {
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
            // White where drawn, clear elsewhere: alpha first, then white.
            assert!(
                icon.data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[0] == 0 || p[1..] == [255, 255, 255])
            );
            assert!(icon.data.as_chunks::<4>().0.iter().any(|p| p[0] == 255));
        }
    }

    #[test]
    fn a_png_that_is_not_rgba8_is_refused() {
        assert!(to_icon(b"not a png").is_none());
    }

    #[test]
    fn the_panel_opens_under_the_clicked_slot_of_the_top_bar() {
        let frame = icon_frame(Point { x: 1900.0, y: 14.0 }, 0.0, 32.0);
        assert_eq!(
            frame,
            Rect {
                x: 1888.0,
                y: 0.0,
                w: 24.0,
                h: 32.0
            }
        );
    }
}
