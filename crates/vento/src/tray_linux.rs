//! The tray icon on Linux: a StatusNotifierItem, which Ubuntu's AppIndicator
//! extension shows in GNOME's top bar. A left click opens the Quick panel;
//! the menu offers the same quick actions and Settings…, as on macOS.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{Icon, MenuItem, ToolTip, Tray};
use log::warn;
use vento_core::geometry::{Point, Rect};
use vento_core::i18n::tr;
use vento_core::settings::{Settings, settings_path};
use vento_platform::Platform;

use crate::awake::{KeepAwake, keep_awake_menu_text};
use crate::exec::{Executor, UndoKind, undo_menu_text};
use crate::pause::PauseSwitch;
use crate::pdf::PdfController;
use crate::quit;
use crate::settings_hub::SettingsHub;

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
    OpenSettings,
    CopyFromScreen,
    OpenClipboard,
    OpenPocket,
    ToggleKeepAwake,
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

/// The tray's StatusNotifierItem id, by which the shell finds its icon.
const TRAY_ID: &str = "vento";

/// Where the icon is when it was clicked at `cursor`, when the shell cannot
/// say: a slot of the top bar (the `bar_h` above the first screen's work
/// area) around it. A click on the menu is below the icon and often to one
/// side, so this is only a guess.
fn icon_frame(cursor: Point, screen_top: f64, bar_h: f64) -> Rect {
    Rect {
        x: cursor.x - ICON_W / 2.0,
        y: screen_top,
        w: ICON_W,
        h: bar_h,
    }
}

/// Has the shell show Keep Awake's cup in the top bar while something is
/// held, and opens the Quick panel when it is clicked. Needs no tray
/// host: the Vento extension draws the cup.
fn show_awake_cup(
    platform: &Rc<dyn Platform>,
    keep_awake: &Rc<KeepAwake>,
    on_open: Box<dyn Fn(Rect)>,
) {
    platform.on_awake_indicator(on_open);
    let shown = Cell::new(false);
    let keep = Rc::downgrade(keep_awake);
    let platform = Rc::downgrade(platform);
    keep_awake.on_change(Box::new(move || {
        let on = keep.upgrade().is_some_and(|k| k.session().is_some());
        if shown.replace(on) != on
            && let Some(platform) = platform.upgrade()
        {
            platform.show_awake_indicator(on);
        }
    }));
}

/// The tray's state, kept on its thread.
struct VentoTray {
    icons: Vec<Icon>,
    copy_from_screen: bool,
    undo: Option<UndoKind>,
    paused: bool,
    /// Whether Keep Awake holds the computer awake, for the item's text.
    keep_awake_on: bool,
    /// Whether the clipboard history is on, which the clipboard item needs.
    clip_history: bool,
}

impl Tray for VentoTray {
    fn id(&self) -> String {
        TRAY_ID.into()
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
        let item = |label: String, enabled: bool, action: Action| {
            MenuItem::Standard(StandardItem {
                label,
                enabled,
                activate: Box::new(move |_: &mut Self| send(action)),
                ..Default::default()
            })
        };
        // While paused, the items that do nothing meanwhile are greyed out.
        let mut menu = Vec::new();
        if self.copy_from_screen {
            menu.push(item(
                tr("Copy from screen"),
                !self.paused,
                Action::CopyFromScreen,
            ));
        }
        menu.extend([
            item(
                tr("Open clipboard history"),
                !self.paused && self.clip_history,
                Action::OpenClipboard,
            ),
            item(tr("Open the Pocket"), !self.paused, Action::OpenPocket),
            MenuItem::Separator,
            item(
                keep_awake_menu_text(self.keep_awake_on),
                true,
                Action::ToggleKeepAwake,
            ),
            item(undo_menu_text(self.undo), self.undo.is_some(), Action::Undo),
            MenuItem::Checkmark(CheckmarkItem {
                label: tr("Pause Vento"),
                checked: self.paused,
                activate: Box::new(|tray: &mut Self| {
                    tray.paused = !tray.paused;
                    send(Action::Pause(tray.paused));
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            item(tr("Settings…"), true, Action::OpenSettings),
            item(tr("Open settings file"), true, Action::OpenSettingsFile),
            MenuItem::Separator,
            item(tr("Quit Vento"), true, Action::Quit),
        ]);
        menu
    }
}

/// Keeps the tray alive; dropping it removes the icon.
pub struct TrayHandle {
    handle: Option<ksni::blocking::Handle<VentoTray>>,
}

impl TrayHandle {
    /// Writes the menu's text again in the language now in use: call after
    /// the language changes. An update with nothing changed makes ksni build
    /// the menu again through `menu()`, which translates it, and send the
    /// host the items whose text differs.
    pub fn retext(&self) {
        if let Some(handle) = &self.handle {
            handle.update(|_| {});
        }
    }
}

/// What the tray needs to act on clicks; the same as on macOS.
pub struct TrayDeps {
    pub exec: Rc<Executor>,
    /// The PDF windows, which Quit asks about when they hold unsaved edits.
    pub pdf: Rc<PdfController>,
    /// Places the panel under the icon.
    pub platform: Rc<dyn Platform>,
    /// Shows its cup in the top bar while it holds the computer awake.
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
    /// Starts Copy from screen; `None` leaves the item out.
    pub on_copy_from_screen: Option<Box<dyn Fn()>>,
}

/// Shows the tray icon. Call on the main thread once the event loop is
/// running. Without a tray host (GNOME without the AppIndicator extension)
/// nothing shows, and the handle keeps nothing.
pub fn install_tray(deps: TrayDeps) -> TrayHandle {
    let TrayDeps {
        exec,
        pdf,
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
    let on_open_panel: Rc<dyn Fn(Rect)> = Rc::from(on_open_panel);
    let on_open_cup = on_open_panel.clone();
    show_awake_cup(
        &platform,
        &keep_awake,
        Box::new(move |cup| on_open_cup(cup)),
    );
    let tray = VentoTray {
        icons: MARKS.iter().filter_map(|png| to_icon(png)).collect(),
        copy_from_screen: on_copy_from_screen.is_some(),
        undo: exec.has_undo(),
        paused: pause.is_paused(),
        keep_awake_on: keep_awake.session().is_some(),
        clip_history: hub.current().clipboard_history,
    };
    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(e) => {
            warn!("no tray icon: {e}");
            return TrayHandle { handle: None };
        }
    };

    // Keep the Undo item, the Pause check, the Keep Awake item and the
    // clipboard item in step with the app.
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

    {
        let handle = handle.clone();
        let keep = Rc::downgrade(&keep_awake);
        keep_awake.on_change(Box::new(move || {
            if let Some(keep) = keep.upgrade() {
                let on = keep.session().is_some();
                handle.update(|tray| tray.keep_awake_on = on);
            }
        }));
    }
    {
        let handle = handle.clone();
        hub.on_change(Box::new(move |_old, new| {
            let history = new.clipboard_history;
            handle.update(|tray| tray.clip_history = history);
        }));
    }

    DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |action| match action {
            Action::OpenPanel => {
                let cursor = platform.sample_input().cursor;
                let (top, bar_h) = platform
                    .pocket_home()
                    .map_or((0.0, 32.0), |home| (home.screen.y, home.menu_bar_h));
                let icon = vento_platform::tray_icon_frame(TRAY_ID)
                    .unwrap_or_else(|| icon_frame(cursor, top, bar_h));
                on_open_panel(icon);
            }
            Action::OpenSettings => on_open_settings(),
            Action::OpenClipboard => on_open_clipboard(),
            Action::OpenPocket => on_open_pocket(),
            Action::ToggleKeepAwake => {
                keep_awake.toggle_start();
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
                quit::quit(&exec, &pdf);
            }
        }));
    });

    TrayHandle {
        handle: Some(handle),
    }
}

/// Opens the settings file with its default app, writing the defaults
/// first if the file does not exist.
pub(crate) fn open_settings_file(platform: &dyn Platform) {
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
