//! The Appearance setting as the windows read it: set from the settings
//! hub, applied to each window's `Theme` whenever the window is made or
//! shown.
//!
//! Every Slint window has a `Theme` of its own, so the setting has to be
//! given to each one. `dark` itself is never written: it stays a binding in
//! `theme.slint`, so System follows a system switch again after Light or
//! Dark.

use std::cell::Cell;

use vento_core::settings::Appearance;

use crate::ui::Theme;

thread_local! {
    // All the UI runs on the main thread.
    static CURRENT: Cell<Appearance> = const { Cell::new(Appearance::System) };
}

/// Makes `a` the setting the windows get from [`apply`].
pub fn set_current(a: Appearance) {
    CURRENT.with(|current| current.set(a));
}

/// The setting as last given to [`set_current`]; System until then.
pub fn current() -> Appearance {
    CURRENT.with(Cell::get)
}

/// The value of `Theme.appearance` for `a`: 0 System, 1 Light, 2 Dark.
pub fn index(a: Appearance) -> i32 {
    match a {
        Appearance::System => 0,
        Appearance::Light => 1,
        Appearance::Dark => 2,
    }
}

/// Gives a window's `Theme` the current setting.
pub fn apply(theme: &Theme<'_>) {
    theme.set_appearance(index(current()));
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
    fn the_current_appearance_is_the_last_one_set() {
        assert_eq!(current(), Appearance::System);
        set_current(Appearance::Dark);
        assert_eq!(current(), Appearance::Dark);
        set_current(Appearance::System);
        assert_eq!(current(), Appearance::System);
    }
}
