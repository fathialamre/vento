//! The What's new window: what changed in Vento, feature by feature, from
//! the changelogs bundled with the app (or, before an update, from the
//! release's notes). It opens at the first start after an update (see
//! [`digest_on_start`]), from Settings → About, and from the update
//! banner's See what's new.
//!
//! A sidebar lists the features that changed under New, Improved and
//! Everywhere, each with a dot until it is opened; beside it the overview of
//! the version, or the chosen feature's changes. Like the Settings window,
//! it is created on the first open and dropped once it has been closed for
//! [`crate::lazy_window::IDLE`].

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use log::{debug, warn};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalPosition, ModelRc, Timer, VecModel,
};
use vento_core::geometry::screen_for;
use vento_core::i18n::{fill, tr};
use vento_core::update::changelog::{Digest, FeatureGroup, VersionNotes, bundled_notes, digest};
use vento_core::update::features::{Feature, FeatureIcon};
use vento_core::update::state::{UpdateFile, WhatsNew, whats_new_on_start};
use vento_core::update::version::Version;
use vento_platform::Platform;

use crate::appearance::{self, show_menu_colors};
use crate::lazy_window::LazyWindow;
use crate::panel::set_rows;
use crate::settings_window::{Page, Turn, centred_on, hands_focus_back, show_title_bar, turn};
use crate::ui::{Icons, PanelIcons, Theme, WhatsNewChange, WhatsNewItem, WhatsNewWindow};

/// The window's size, in logical points (matches `whats_new.slint`).
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 430.0;

/// The sidebar's sections (`WhatsNewItem.section`).
const SECTION_NEW: i32 = 0;
const SECTION_IMPROVED: i32 = 1;
const SECTION_EVERYWHERE: i32 = 2;

/// Opens the Settings window on the page with this key.
type OpenSettings = Box<dyn Fn(&'static str)>;

/// What the window shows: one digest, and how far the user has read it.
struct Shown {
    digest: Digest,
    groups: Vec<FeatureGroup>,
    /// By group: the user has opened its page.
    read: Vec<bool>,
    /// The card over the overview, if any, in the language of the open.
    notice: Option<String>,
    /// The group shown; `None` for the overview.
    selected: Option<usize>,
}

/// Opens, fills and closes the What's new window.
pub struct WhatsNewController {
    platform: Rc<dyn Platform>,
    /// The window while it is open, and for a while after it closes.
    window: LazyWindow<WhatsNewWindow>,
    shown: RefCell<Option<Shown>>,
    /// The rows behind the sidebar and the feature page.
    items: Rc<VecModel<WhatsNewItem>>,
    changes: Rc<VecModel<WhatsNewChange>>,
    /// The window has been placed since it was made (a new window opens
    /// centred; one shown again stays where the user left it).
    placed: Cell<bool>,
    open: Cell<bool>,
    /// Open Settings on a feature's page, with that page's key; main opens
    /// the Settings window on it.
    on_open_settings: RefCell<Option<OpenSettings>>,
    /// The notice's button; main shows the Screen Recording window.
    on_notice: RefCell<Option<Box<dyn Fn()>>>,
    /// Whether another vento window is open; main sets it. That window
    /// keeps the focus when this one closes.
    other_window_open: RefCell<Option<Box<dyn Fn() -> bool>>>,
    /// Runs when the user turns to the window; main closes the panel and
    /// the Info window then.
    on_activated: RefCell<Option<Box<dyn Fn()>>>,
    this: Weak<Self>,
}

impl WhatsNewController {
    /// Creates the (closed) window controller; the window comes with the
    /// first open.
    pub fn new(platform: Rc<dyn Platform>) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            window: LazyWindow::new(),
            shown: RefCell::new(None),
            items: Rc::new(VecModel::default()),
            changes: Rc::new(VecModel::default()),
            placed: Cell::new(false),
            open: Cell::new(false),
            on_open_settings: RefCell::new(None),
            on_notice: RefCell::new(None),
            other_window_open: RefCell::new(None),
            on_activated: RefCell::new(None),
            this: this.clone(),
        });
        // The text Rust writes (names, headings, the version line) in the
        // new language. The changes themselves stay in the language they
        // were read in until the window opens again.
        appearance::follow_language(Rc::downgrade(&controller), Self::show);
        controller
    }

    /// Has Open Settings call `f` with the key of the feature's Settings
    /// page ([`Page::from_key`] reads it).
    pub fn set_on_open_settings(&self, f: OpenSettings) {
        *self.on_open_settings.borrow_mut() = Some(f);
    }

    /// Has the notice's button call `f`.
    pub fn set_on_notice(&self, f: Box<dyn Fn()>) {
        *self.on_notice.borrow_mut() = Some(f);
    }

    /// Has the window call `f` when the user turns to it (a click in it, or
    /// it becoming the key window).
    pub fn set_on_activated(&self, f: Box<dyn Fn()>) {
        *self.on_activated.borrow_mut() = Some(f);
    }

    /// Has the window ask `f` whether another vento window is open as it
    /// closes: focus is then left with that window instead of handed back
    /// to the app that had it.
    pub fn set_other_window_open(&self, f: Box<dyn Fn() -> bool>) {
        *self.other_window_open.borrow_mut() = Some(f);
    }

    /// The window, created and connected if there is none.
    ///
    /// # Panics
    /// If the Slint window cannot be created.
    fn window(&self) -> WhatsNewWindow {
        self.window.get_or_create(|| {
            let window = WhatsNewWindow::new().expect("cannot create the What's new window");
            // A Mac-window page, as the Settings window has.
            window.global::<Theme>().set_clean(true);
            appearance::follow(&window, appearance::Look::Full);
            window.set_items(ModelRc::from(self.items.clone()));
            window.set_changes(ModelRc::from(self.changes.clone()));
            // A new window opens centred.
            self.placed.set(false);
            self.connect(&window);
            window
        })
    }

    /// The window, if it has one now.
    fn existing(&self) -> Option<WhatsNewWindow> {
        self.window.get()
    }

    fn connect(&self, window: &WhatsNewWindow) {
        let this = self.this.clone();
        window.window().on_close_requested(move || {
            if let Some(controller) = this.upgrade() {
                controller.close();
            }
            CloseRequestResponse::KeepWindowShown
        });
        let this = self.this.clone();
        window.window().on_winit_window_event(move |_, event| {
            if let Some(controller) = this.upgrade() {
                controller.turned_to(turn(event));
            }
            EventResult::Propagate
        });
        let this = self.this.clone();
        window.on_done(move || {
            if let Some(controller) = this.upgrade() {
                controller.close();
            }
        });
        let this = self.this.clone();
        window.on_select(move |i| {
            if let Some(controller) = this.upgrade() {
                controller.select(usize::try_from(i).ok());
            }
        });
        let this = self.this.clone();
        window.on_open_settings(move || {
            let Some(controller) = this.upgrade() else {
                return;
            };
            let page = controller
                .shown
                .borrow()
                .as_ref()
                .and_then(|s| s.selected.and_then(|i| s.groups.get(i)))
                .and_then(settings_page);
            if let (Some(page), Some(f)) = (page, controller.on_open_settings.borrow().as_ref()) {
                f(page);
            }
        });
        let this = self.this.clone();
        window.on_notice_clicked(move || {
            if let Some(controller) = this.upgrade()
                && let Some(f) = controller.on_notice.borrow().as_ref()
            {
                f();
            }
        });
    }

    /// Shows the overview (`None`) or the page of group `group`, which
    /// counts as read from then on. A group out of range shows the
    /// overview.
    fn select(&self, group: Option<usize>) {
        if let Some(shown) = self.shown.borrow_mut().as_mut() {
            shown.selected = group.filter(|&i| i < shown.groups.len());
            if let Some(i) = shown.selected {
                shown.read[i] = true;
            }
        }
        self.show();
    }

    /// Sets every property from what is shown, in the interface language.
    fn show(&self) {
        let Some(window) = self.existing() else {
            return;
        };
        let shown = self.shown.borrow();
        let Some(shown) = shown.as_ref() else { return };
        let d = &shown.digest;
        window.set_version_title(version_title(d.version).into());
        window.set_from_text(d.previous.map(from_text).unwrap_or_default().into());
        let counts: Vec<i32> = d
            .counts()
            .iter()
            .map(|&n| i32::try_from(n).unwrap_or(i32::MAX))
            .collect();
        window.set_counts(ModelRc::from(counts.as_slice()));
        window.set_summary(d.summary.clone().unwrap_or_default().into());
        window.set_notice(shown.notice.clone().unwrap_or_default().into());
        let items = shown
            .groups
            .iter()
            .zip(&shown.read)
            .map(|(g, &read)| WhatsNewItem {
                icon: icon(group_icon(g), &window),
                label: group_label(g).into(),
                section: section(g),
                unread: !read,
                has_settings: settings_page(g).is_some(),
            })
            .collect();
        set_rows(&self.items, items);
        let group = shown.selected.and_then(|i| shown.groups.get(i));
        window.set_selected(
            shown
                .selected
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(-1),
        );
        window.set_detail_label(group.map(group_label).unwrap_or_default().into());
        window.set_detail_description(group.map(group_description).unwrap_or_default().into());
        window.set_detail_has_settings(group.and_then(settings_page).is_some());
        let changes = group
            .map(|g| {
                g.changes
                    .iter()
                    .map(|c| WhatsNewChange {
                        kind: c.kind as i32,
                        text: c.text.as_str().into(),
                        path: c.settings_path.as_deref().unwrap_or_default().into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        set_rows(&self.changes, changes);
    }

    /// Opens the window on the overview of `digest`, with `notice` over it
    /// when given, or shows them in the open window (brought forward). The
    /// window then has the keyboard. A new window opens centred on the
    /// screen under the pointer.
    pub fn open(&self, digest: Digest, notice: Option<String>) {
        let groups = digest.groups();
        *self.shown.borrow_mut() = Some(Shown {
            read: vec![false; groups.len()],
            groups,
            digest,
            notice,
            selected: None,
        });
        let window = self.window();
        self.open.set(true);
        self.show();
        show_menu_colors(&*self.platform, &window.global::<Theme>());
        if !self.placed.replace(true) {
            let screens = self.platform.screens();
            if let Some(screen) = screen_for(self.platform.sample_input().cursor, &screens) {
                let (x, y) = centred_on(screen, WIDTH, HEIGHT);
                window
                    .window()
                    .set_position(LogicalPosition::new(x as f32, y as f32));
            }
        }
        // vento runs as an accessory app: bring it forward so the window
        // comes to the front and takes the keyboard.
        self.platform.activate_app();
        if let Err(e) = window.show() {
            warn!("cannot show the What's new window: {e}");
            self.open.set(false);
            self.platform.deactivate_app();
            self.window.release_later();
            return;
        }
        // A minimised window comes back too.
        window.window().with_winit_window(|w| {
            w.set_minimized(false);
            w.focus_window();
        });
        // A new window gets its native window, and so its title bar, at the
        // end of this turn of the event loop.
        let this = self.this.clone();
        self.window.when_native(move || {
            if let Some(window) = this.upgrade().and_then(|c| c.existing()) {
                show_title_bar(&window);
            }
        });
        debug!("What's new window open");
    }

    /// Hides the window and hands focus back to the app that had it on the
    /// next turn of the event loop, unless the window is open again by then
    /// or another vento window is open. Closing a closed window does nothing
    /// (Esc repeats while held).
    pub fn close(&self) {
        if !self.open.replace(false) {
            return;
        }
        if let Some(window) = self.existing()
            && let Err(e) = window.hide()
        {
            warn!("cannot hide the What's new window: {e}");
        }
        self.window.release_later();
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(controller) = this.upgrade()
                && hands_focus_back(controller.is_open(), controller.other_window_open())
            {
                controller.platform.deactivate_app();
            }
        });
        debug!("What's new window closed");
    }

    /// Whether the window is open (shown, not yet closed).
    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Tells main the user turned to the window, at once for `Turn::Now`,
    /// and for `Turn::IfStillKey` on the next turn of the event loop if the
    /// window is still key then.
    fn turned_to(&self, turn: Turn) {
        match turn {
            Turn::No => {}
            Turn::Now => self.activated(),
            Turn::IfStillKey => {
                let this = self.this.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(controller) = this.upgrade()
                        && controller.is_open()
                        && controller.has_focus()
                    {
                        controller.activated();
                    }
                });
            }
        }
    }

    fn activated(&self) {
        if let Some(f) = self.on_activated.borrow().as_ref() {
            f();
        }
    }

    /// Whether the window is the key window (it takes the keyboard).
    fn has_focus(&self) -> bool {
        self.existing()
            .and_then(|window| window.window().with_winit_window(|w| w.has_focus()))
            .unwrap_or(false)
    }

    /// Whether another vento window is open, as main says.
    fn other_window_open(&self) -> bool {
        self.other_window_open
            .borrow()
            .as_ref()
            .is_some_and(|f| f())
    }
}

/// What the window shows at start, from the changelogs bundled in `lang`
/// (`i18n::current().code`): the changes since the version that ran last,
/// or `None` when nothing is to be shown (see [`whats_new_on_start`]).
pub fn digest_on_start(
    file: &UpdateFile,
    running: Version,
    fresh: bool,
    lang: &str,
) -> Option<Digest> {
    digest_on_start_from(&bundled_notes(lang), file, running, fresh)
}

/// [`digest_on_start`] from the changelog `notes`.
fn digest_on_start_from(
    notes: &[VersionNotes],
    file: &UpdateFile,
    running: Version,
    fresh: bool,
) -> Option<Digest> {
    match whats_new_on_start(file.last_run_version, running, fresh) {
        WhatsNew::Show { after } => digest(notes, after, running),
        WhatsNew::Skip => None,
    }
}

/// The overview's title, "Vento 0.3.0".
fn version_title(version: Version) -> String {
    fill(&tr("Vento {v}"), &[("v", &version.to_string())])
}

/// The line under the title, "Updated from 0.2.0".
fn from_text(previous: Version) -> String {
    fill(&tr("Updated from {v}"), &[("v", &previous.to_string())])
}

/// The sidebar section a group sits under.
fn section(group: &FeatureGroup) -> i32 {
    match group.feature {
        None => SECTION_EVERYWHERE,
        Some(_) if group.is_new => SECTION_NEW,
        Some(_) => SECTION_IMPROVED,
    }
}

/// A group's name: its feature's, or Vento as a whole for the changes that
/// apply everywhere.
fn group_label(group: &FeatureGroup) -> String {
    group
        .feature
        .map_or_else(|| tr("Vento as a whole"), |f| (f.name)())
}

/// The line under a group's name on its page.
fn group_description(group: &FeatureGroup) -> String {
    group.feature.map_or_else(
        || tr("Changes that touch all of Vento"),
        feature_description,
    )
}

/// A group's icon.
fn group_icon(group: &FeatureGroup) -> FeatureIcon {
    group.feature.map_or(FeatureIcon::General, |f| f.icon)
}

/// The key of the Settings page that Open Settings opens for a group, if
/// its feature has one the Settings window knows.
fn settings_page(group: &FeatureGroup) -> Option<&'static str> {
    group
        .feature
        .and_then(|f| f.settings_page)
        .filter(|key| Page::from_key(key).is_some())
}

/// What a feature is, in one line, for the header of its page.
pub fn feature_description(f: &Feature) -> String {
    match f.key {
        "Wheel" => tr("Quick actions under the file you are dragging"),
        "Move" => tr("Move or copy files to your pinned and recent folders"),
        "Pocket" => tr("Keep files at hand while you move between apps"),
        "Clipboard" => tr("Bring back anything you copied earlier"),
        "Copy from screen" => tr("Copy the text from any part of the screen"),
        "Convert" => tr("Turn images into another format"),
        "PDF Pages" => tr("Reorder, rotate and cut the pages of a PDF"),
        "Archive" => tr("Compress files into an archive, or extract one"),
        "Info" => tr("See the details of files and folders"),
        "Share" => tr("Send files to people and apps"),
        "Keep Awake" => tr("Stop the computer from going to sleep for a while"),
        "Updates" => tr("Keep Vento up to date"),
        "Settings" => tr("Choose how Vento looks and works"),
        "Welcome" => tr("The short setup on the first launch"),
        _ => String::new(),
    }
}

/// The picture for `icon`.
fn icon(icon: FeatureIcon, window: &WhatsNewWindow) -> Image {
    let icons = window.global::<Icons>();
    let panel = window.global::<PanelIcons>();
    match icon {
        FeatureIcon::Wheel => panel.get_wheel(),
        FeatureIcon::Move => icons.get_move(),
        FeatureIcon::Pocket => icons.get_pocket(),
        FeatureIcon::Clipboard => icons.get_clipboard(),
        FeatureIcon::ScanText => panel.get_scan_text(),
        FeatureIcon::Convert => icons.get_convert(),
        FeatureIcon::Pdf => icons.get_pdf(),
        FeatureIcon::Archive => icons.get_archive(),
        FeatureIcon::Info => icons.get_info(),
        FeatureIcon::Share => icons.get_share(),
        FeatureIcon::Awake => panel.get_awake(),
        FeatureIcon::Drop => icons.get_drop(),
        FeatureIcon::General => panel.get_general(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vento_core::i18n::with_language;
    use vento_core::update::changelog::parse;
    use vento_core::update::features::{FEATURES, find};

    const LOG: &str = "## [0.5.0]\n### Summary\nFive.\n### Added\n- Pocket: Five.\n\n## [0.4.0]\n### Fixed\n- Wheel: Four.\n\n## [0.3.0]\n### Changed\n- Clipboard: Three.\n";

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn file(last: Option<&str>) -> UpdateFile {
        UpdateFile {
            last_run_version: last.map(v),
            ..UpdateFile::default()
        }
    }

    fn texts(d: &Digest) -> Vec<&str> {
        d.changes.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn a_fresh_install_shows_nothing() {
        assert!(digest_on_start_from(&parse(LOG), &file(None), v("0.5.0"), true).is_none());
    }

    #[test]
    fn no_last_run_shows_only_the_running_version() {
        let d = digest_on_start_from(&parse(LOG), &file(None), v("0.5.0"), false).unwrap();
        assert_eq!(texts(&d), ["Five."]);
        assert_eq!(d.previous, None);
    }

    #[test]
    fn a_skipped_version_is_merged() {
        let d = digest_on_start_from(&parse(LOG), &file(Some("0.3.0")), v("0.5.0"), false).unwrap();
        assert_eq!(texts(&d), ["Five.", "Four."]);
        assert_eq!(d.previous, Some(v("0.3.0")));
        assert_eq!(d.summary.as_deref(), Some("Five."));
    }

    #[test]
    fn the_same_or_a_newer_last_run_shows_nothing() {
        let notes = parse(LOG);
        assert!(digest_on_start_from(&notes, &file(Some("0.5.0")), v("0.5.0"), false).is_none());
        assert!(digest_on_start_from(&notes, &file(Some("0.6.0")), v("0.5.0"), false).is_none());
    }

    #[test]
    fn the_bundled_notes_cover_the_running_version() {
        let running = v(env!("CARGO_PKG_VERSION"));
        for lang in ["en", "ar"] {
            assert!(
                digest_on_start(&file(None), running, false, lang).is_some(),
                "{lang}"
            );
        }
    }

    #[test]
    fn every_feature_settings_page_is_a_settings_window_page() {
        for f in FEATURES {
            if let Some(key) = f.settings_page {
                assert!(Page::from_key(key).is_some(), "{} -> {key}", f.key);
            }
        }
    }

    #[test]
    fn every_feature_has_a_description_in_each_language() {
        for lang in ["en", "ar"] {
            with_language(lang, || {
                for f in FEATURES {
                    assert!(!feature_description(f).is_empty(), "{lang} {}", f.key);
                }
            });
        }
    }

    #[test]
    fn groups_go_under_new_improved_and_everywhere() {
        let notes = parse(
            "## [1.0.0]\n### Added\n- Updates: a.\n- Something global.\n### Fixed\n- PDF Pages: b.\n",
        );
        let groups = digest(&notes, None, v("1.0.0")).unwrap().groups();
        let sections: Vec<_> = groups.iter().map(section).collect();
        assert_eq!(
            sections,
            [SECTION_NEW, SECTION_IMPROVED, SECTION_EVERYWHERE]
        );
        let pages: Vec<_> = groups.iter().map(settings_page).collect();
        assert_eq!(pages, [Some("about"), None, None]);
        assert_eq!(group_icon(&groups[2]), FeatureIcon::General);
        assert_eq!(group_label(&groups[0]), (find("Updates").unwrap().name)());
    }

    #[test]
    fn the_title_and_the_previous_version_read_in_each_language() {
        assert_eq!(version_title(v("0.3.0")), "Vento 0.3.0");
        assert_eq!(from_text(v("0.2.0")), "Updated from 0.2.0");
        with_language("ar", || {
            assert!(version_title(v("0.3.0")).starts_with('\u{200F}'));
            assert!(from_text(v("0.2.0")).contains("\u{200E}0.2.0"));
        });
    }
}
