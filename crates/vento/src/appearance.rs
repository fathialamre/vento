//! The Appearance and Theme settings as the windows read them: set from
//! the settings hub, applied to each window's `Theme` whenever the window is
//! made or shown.
//!
//! Every Slint window has a `Theme` of its own, so the setting has to be
//! given to each one. `dark` itself is never written: it stays a binding in
//! `theme.slint`, so System follows a system switch again after Light or
//! Dark.
//!
//! The interface language's direction (`Theme.rtl`) is given the same way,
//! to every window: each one [`follow`]s it from when it is made, and
//! [`apply_language`] gives the new direction to every window that exists.

use std::cell::{Cell, RefCell};

use slint::winit_030::winit::window::Theme as WindowTheme;
use vento_core::i18n;
use vento_core::settings::{Appearance, ThemeName};
use vento_platform::Platform;

use slint::ComponentHandle;

use crate::ui::Theme;

/// Gives a window's `Theme` what it follows again; false once the window is
/// gone.
type Follower = Box<dyn Fn() -> bool>;

thread_local! {
    // All the UI runs on the main thread.
    static CURRENT: Cell<Appearance> = const { Cell::new(Appearance::System) };
    static CURRENT_THEME: Cell<ThemeName> = const { Cell::new(ThemeName::Vento) };
    static FOLLOWERS: RefCell<Vec<Follower>> = const { RefCell::new(Vec::new()) };
}

/// What a window takes from [`follow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// The Appearance and Theme settings and the direction ([`apply`]).
    Full,
    /// Only the direction ([`apply_direction`]): the window keeps its own
    /// colours (the Pocket island, the wheel, the toast).
    Direction,
}

/// Makes `a` the setting the windows get from [`apply`].
pub fn set_current(a: Appearance) {
    CURRENT.with(|current| current.set(a));
}

/// The setting as last given to [`set_current`]; System until then.
pub fn current() -> Appearance {
    CURRENT.with(Cell::get)
}

/// Makes `t` the theme the windows get from [`apply`].
pub fn set_current_theme(t: ThemeName) {
    CURRENT_THEME.with(|current| current.set(t));
}

/// The value of `Theme.palette` for `t`: its place in [`ThemeName::ALL`],
/// the order of the colour lists in `theme.slint`.
pub fn theme_index(t: ThemeName) -> i32 {
    ThemeName::ALL
        .iter()
        .position(|&each| each == t)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

/// The value of `Theme.appearance` for `a`: 0 System, 1 Light, 2 Dark.
pub fn index(a: Appearance) -> i32 {
    match a {
        Appearance::System => 0,
        Appearance::Light => 1,
        Appearance::Dark => 2,
    }
}

/// The theme for the native title bar of a framed window (the Settings
/// window) under `a`, whose content is dark when `content_dark`. Light and
/// Dark force it. Under System a Mac's follows the system (`None`); X11 has
/// no such choice (winit asks for the dark variant for `None`), so there it
/// matches the content, which follows the desktop.
pub fn title_bar_theme(a: Appearance, content_dark: bool) -> Option<WindowTheme> {
    match a {
        Appearance::Light => Some(WindowTheme::Light),
        Appearance::Dark => Some(WindowTheme::Dark),
        Appearance::System if cfg!(target_os = "macos") => None,
        Appearance::System if content_dark => Some(WindowTheme::Dark),
        Appearance::System => Some(WindowTheme::Light),
    }
}

/// Gives a window's `Theme` the current settings. Only the windows that
/// set `clean` (the Quick panel and the Settings window) show the theme.
pub fn apply(theme: &Theme<'_>) {
    theme.set_appearance(index(current()));
    theme.set_palette(theme_index(CURRENT_THEME.with(Cell::get)));
    theme.set_rtl(i18n::current().rtl);
}

/// Gives a window's `Theme` the interface language's direction alone.
pub fn apply_direction(theme: &Theme<'_>) {
    theme.set_rtl(i18n::current().rtl);
}

/// Gives `window` its `look` now, and again after every language change
/// for as long as the window exists (the registry keeps only a weak
/// reference, so a dropped window is forgotten).
pub fn follow<T>(window: &T, look: Look)
where
    T: ComponentHandle + 'static,
    for<'a> Theme<'a>: slint::Global<'a, T>,
{
    let weak = window.as_weak();
    let refresh = move || {
        let Some(window) = weak.upgrade() else {
            return false;
        };
        let theme = window.global::<Theme>();
        match look {
            Look::Full => apply(&theme),
            Look::Direction => apply_direction(&theme),
        }
        true
    };
    refresh();
    FOLLOWERS.with(|followers| followers.borrow_mut().push(Box::new(refresh)));
}

/// Gives every window that [`follow`]s its look again, and forgets the
/// windows that are gone.
fn refresh_followers() {
    // Taken out while they run, so a window made meanwhile can still follow.
    let followers = FOLLOWERS.with(|followers| std::mem::take(&mut *followers.borrow_mut()));
    let mut kept: Vec<Follower> = followers.into_iter().filter(|refresh| refresh()).collect();
    FOLLOWERS.with(|followers| {
        let mut followers = followers.borrow_mut();
        kept.append(&mut followers);
        *followers = kept;
    });
}

/// Makes `code` the language of the interface: the text Rust makes and the
/// text in `.slint` files, from the bundled translations. Slint wants a
/// component to exist before it can switch, so this comes after the first
/// window is made. Every window that [`follow`]s the language then gets the
/// new direction at once.
pub fn apply_language(code: &str) {
    i18n::set_language(code);
    // English is the source text: no translation is selected for it.
    let selected = if code == "en" { "" } else { code };
    if let Err(e) = slint::select_bundled_translation(selected) {
        log::error!("cannot select the {code:?} translation: {e}");
    }
    refresh_followers();
}

/// Matches the desktop's own menus where the platform gives their colours,
/// as they are now (a theme change shows at the next open), and gives
/// `theme` the Appearance setting, which decides whether those colours
/// apply.
pub fn show_menu_colors(platform: &dyn Platform, theme: &Theme<'_>) {
    apply(theme);
    let Some(colors) = platform.menu_colors() else {
        theme.set_system_colors(false);
        return;
    };
    let color = |[r, g, b, a]: [u8; 4]| slint::Color::from_argb_u8(a, r, g, b);
    theme.set_system_bg(color(colors.background));
    theme.set_system_border(color(colors.border));
    theme.set_system_text(color(colors.text));
    theme.set_system_dark(colors.is_dark());
    theme.set_system_colors(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_indices_match_theme_slint() {
        assert_eq!(index(Appearance::System), 0);
        assert_eq!(index(Appearance::Light), 1);
        assert_eq!(index(Appearance::Dark), 2);
    }

    #[test]
    fn the_title_bar_is_forced_light_or_dark_and_follows_the_system_on_a_mac() {
        assert_eq!(
            title_bar_theme(Appearance::Light, true),
            Some(WindowTheme::Light)
        );
        assert_eq!(
            title_bar_theme(Appearance::Dark, false),
            Some(WindowTheme::Dark)
        );
        if cfg!(target_os = "macos") {
            assert_eq!(title_bar_theme(Appearance::System, true), None);
            assert_eq!(title_bar_theme(Appearance::System, false), None);
        } else {
            assert_eq!(
                title_bar_theme(Appearance::System, true),
                Some(WindowTheme::Dark)
            );
            assert_eq!(
                title_bar_theme(Appearance::System, false),
                Some(WindowTheme::Light)
            );
        }
    }

    #[test]
    fn theme_indices_follow_the_list_theme_slint_holds() {
        for (i, t) in ThemeName::ALL.into_iter().enumerate() {
            assert_eq!(theme_index(t), i32::try_from(i).unwrap());
        }
        assert_eq!(theme_index(ThemeName::Vento), 0);
        assert_eq!(theme_index(ThemeName::Nord), 6);
    }

    #[test]
    fn followers_run_again_until_their_window_is_gone() {
        use std::rc::Rc;
        let (kept, gone) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let (k, g) = (kept.clone(), gone.clone());
        FOLLOWERS.with(|followers| {
            let mut followers = followers.borrow_mut();
            followers.push(Box::new(move || {
                k.set(k.get() + 1);
                true
            }));
            followers.push(Box::new(move || {
                g.set(g.get() + 1);
                false
            }));
        });
        refresh_followers();
        refresh_followers();
        assert_eq!(kept.get(), 2);
        // Asked once, said its window is gone, and was forgotten.
        assert_eq!(gone.get(), 1);
        assert_eq!(FOLLOWERS.with(|followers| followers.borrow().len()), 1);
        FOLLOWERS.with(|followers| followers.borrow_mut().clear());
    }

    #[test]
    fn the_current_appearance_is_the_last_one_set() {
        assert_eq!(current(), Appearance::System);
        set_current(Appearance::Dark);
        assert_eq!(current(), Appearance::Dark);
        set_current(Appearance::System);
        assert_eq!(current(), Appearance::System);
    }
}
