//! The right-click menu on a Pocket cell: which items it acts on, its
//! entries, and what the chosen entry does.

use std::path::PathBuf;

use vento_core::i18n::{fill, tr};
use vento_platform::{AppInfo, MenuEntry};

/// What a chosen menu entry does to the menu's targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MenuAction {
    /// Open each target with its default app.
    Open,
    /// Open every target with this app bundle.
    OpenWith(PathBuf),
    /// Show the targets in Finder, selected.
    Reveal,
    /// Put the targets' files on the clipboard.
    Copy,
    /// Take the targets out of the Pocket, with Undo.
    Remove,
}

/// The items a right click on `clicked` acts on, in Pocket order (`all`):
/// the whole selection when `clicked` is part of a selection of more than
/// one item, otherwise `clicked` alone. Missing items are kept: Remove
/// takes them too.
pub(super) fn menu_targets(clicked: u64, all: &[u64], selected: &[u64]) -> Vec<u64> {
    if selected.len() > 1 && selected.contains(&clicked) {
        all.iter()
            .copied()
            .filter(|id| selected.contains(id))
            .collect()
    } else {
        vec![clicked]
    }
}

/// The item whose apps Open With lists: the clicked one, or, when its
/// file is gone, the first target (in order) whose file is still there.
pub(super) fn apps_source(clicked: u64, targets: &[u64], missing: &[u64]) -> Option<u64> {
    if !missing.contains(&clicked) {
        return Some(clicked);
    }
    targets.iter().copied().find(|id| !missing.contains(id))
}

/// Whether a press opens the menu instead of pressing the item: a right
/// press, or a left one with Control held (Finder's secondary click).
pub(super) fn opens_menu(left: bool, ctrl: bool) -> bool {
    !left || ctrl
}

const OPEN: u32 = 1;
const REVEAL: u32 = 2;
const COPY: u32 = 3;
const REMOVE: u32 = 4;
/// Open With's apps take ids from here on, in the order they are listed.
const FIRST_APP: u32 = 100;

/// The file manager's name in "Show in …".
#[cfg(not(any(target_os = "linux", windows)))]
fn reveal_title() -> String {
    tr("Show in Finder")
}
#[cfg(target_os = "linux")]
fn reveal_title() -> String {
    tr("Show in Files")
}
#[cfg(windows)]
fn reveal_title() -> String {
    tr("Show in Explorer")
}

/// A cell's context menu: its entries, and the action behind each id.
pub(super) struct ItemMenu {
    pub(super) entries: Vec<MenuEntry>,
    /// Open With's apps, in menu order: the one at `i` has id
    /// `FIRST_APP + i`.
    apps: Vec<PathBuf>,
}

impl ItemMenu {
    /// The menu for `targets` items, `existing` of which still have their
    /// file, with `apps` (the clicked item's) under Open With: the default
    /// app first, then the others by name, as Finder lists them. With no
    /// file left, only Remove from Pocket is enabled; with no target at
    /// all, the menu is empty.
    pub(super) fn build(targets: usize, existing: usize, apps: &[AppInfo]) -> Self {
        if targets == 0 {
            return Self {
                entries: Vec::new(),
                apps: Vec::new(),
            };
        }
        let any = existing > 0;
        let (defaults, mut others): (Vec<&AppInfo>, Vec<&AppInfo>) =
            apps.iter().partition(|a| a.is_default);
        others.sort_by_cached_key(|a| (a.name.to_lowercase(), a.path.clone()));
        let mut children = Vec::new();
        let mut paths = Vec::new();
        let mut push = |app: &AppInfo, title: String, children: &mut Vec<MenuEntry>| {
            let id = FIRST_APP.saturating_add(u32::try_from(paths.len()).unwrap_or(u32::MAX));
            children.push(MenuEntry::Item {
                id,
                title,
                enabled: true,
                icon: Some(app.path.clone()),
            });
            paths.push(app.path.clone());
        };
        for app in &defaults {
            push(
                app,
                fill(&tr("{app} (default)"), &[("app", &app.name)]),
                &mut children,
            );
        }
        if !defaults.is_empty() && !others.is_empty() {
            children.push(MenuEntry::Separator);
        }
        for app in &others {
            push(app, app.name.clone(), &mut children);
        }
        let item = |id, title: &str, enabled| MenuEntry::Item {
            id,
            title: title.into(),
            enabled,
            icon: None,
        };
        let entries = vec![
            item(OPEN, &tr("Open"), any),
            MenuEntry::Submenu {
                title: tr("Open With"),
                enabled: any && !paths.is_empty(),
                children,
            },
            item(REVEAL, &reveal_title(), any),
            MenuEntry::Separator,
            item(COPY, &tr("Copy"), any),
            item(REMOVE, &tr("Remove from Pocket"), true),
        ];
        Self {
            entries,
            apps: paths,
        }
    }

    /// What the entry with `id` does; `None` for an id the menu doesn't
    /// have.
    pub(super) fn action(&self, id: u32) -> Option<MenuAction> {
        if self.entries.is_empty() {
            return None;
        }
        match id {
            OPEN => Some(MenuAction::Open),
            REVEAL => Some(MenuAction::Reveal),
            COPY => Some(MenuAction::Copy),
            REMOVE => Some(MenuAction::Remove),
            id => {
                let i = usize::try_from(id.checked_sub(FIRST_APP)?).ok()?;
                self.apps.get(i).cloned().map(MenuAction::OpenWith)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, is_default: bool) -> AppInfo {
        AppInfo {
            path: PathBuf::from(format!("/Applications/{name}.app")),
            name: name.into(),
            is_default,
        }
    }

    /// Titles and enabled states, one level deep; submenus as
    /// "title >", separators as "-".
    fn outline(entries: &[MenuEntry]) -> Vec<(String, bool)> {
        entries
            .iter()
            .map(|e| match e {
                MenuEntry::Item { title, enabled, .. } => (title.clone(), *enabled),
                MenuEntry::Submenu { title, enabled, .. } => (format!("{title} >"), *enabled),
                MenuEntry::Separator => ("-".into(), false),
            })
            .collect()
    }

    fn submenu(menu: &ItemMenu) -> &[MenuEntry] {
        menu.entries
            .iter()
            .find_map(|e| match e {
                MenuEntry::Submenu { children, .. } => Some(children.as_slice()),
                _ => None,
            })
            .expect("the menu has Open With")
    }

    fn id_of(entries: &[MenuEntry], wanted: &str) -> u32 {
        entries
            .iter()
            .find_map(|e| match e {
                MenuEntry::Item { id, title, .. } if title == wanted => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no entry {wanted}"))
    }

    #[test]
    fn targets_are_the_selection_only_when_the_clicked_item_is_in_a_bigger_one() {
        let all = [1, 2, 3, 4];
        // Part of a selection of several: all of it, in Pocket order.
        assert_eq!(menu_targets(3, &all, &[4, 1, 3]), vec![1, 3, 4]);
        // Not selected: just the clicked item.
        assert_eq!(menu_targets(2, &all, &[4, 1, 3]), vec![2]);
        // The only selected item: just it.
        assert_eq!(menu_targets(3, &all, &[3]), vec![3]);
        assert_eq!(menu_targets(3, &all, &[]), vec![3]);
    }

    #[test]
    fn open_with_follows_the_first_target_still_there_when_the_clicked_one_is_gone() {
        assert_eq!(apps_source(2, &[1, 2, 3], &[]), Some(2));
        assert_eq!(apps_source(2, &[1, 2, 3], &[1, 2]), Some(3));
        assert_eq!(apps_source(2, &[1, 2, 3], &[2]), Some(1));
        assert_eq!(apps_source(2, &[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(apps_source(2, &[2], &[2]), None);
    }

    #[test]
    fn right_presses_and_control_clicks_open_the_menu() {
        assert!(opens_menu(false, false));
        assert!(opens_menu(false, true));
        assert!(opens_menu(true, true));
        assert!(!opens_menu(true, false));
    }

    #[test]
    fn entries_come_in_finder_order() {
        let menu = ItemMenu::build(1, 1, &[app("Preview", true)]);
        assert_eq!(
            outline(&menu.entries),
            vec![
                ("Open".into(), true),
                ("Open With >".into(), true),
                (reveal_title(), true),
                ("-".into(), false),
                ("Copy".into(), true),
                ("Remove from Pocket".into(), true),
            ]
        );
    }

    #[test]
    fn open_with_lists_the_default_first_then_the_rest_by_name() {
        let apps = [
            app("zed", false),
            app("TextEdit", true),
            app("BBEdit", false),
            app("atom", false),
        ];
        let menu = ItemMenu::build(1, 1, &apps);
        let children = submenu(&menu);
        assert_eq!(
            outline(children),
            vec![
                ("TextEdit (default)".into(), true),
                ("-".into(), false),
                ("atom".into(), true),
                ("BBEdit".into(), true),
                ("zed".into(), true),
            ]
        );
        // Each app shows its own icon.
        for e in children {
            if let MenuEntry::Item { title, icon, .. } = e {
                let name = title.trim_end_matches(" (default)");
                assert_eq!(
                    icon.as_deref(),
                    Some(PathBuf::from(format!("/Applications/{name}.app")).as_path())
                );
            }
        }
    }

    #[test]
    fn open_with_has_no_separator_without_a_default_or_without_others() {
        let menu = ItemMenu::build(1, 1, &[app("b", false), app("A", false)]);
        assert_eq!(
            outline(submenu(&menu)),
            vec![("A".into(), true), ("b".into(), true)]
        );
        let menu = ItemMenu::build(1, 1, &[app("Preview", true)]);
        assert_eq!(
            outline(submenu(&menu)),
            vec![("Preview (default)".into(), true)]
        );
    }

    #[test]
    fn an_empty_app_list_still_shows_open_with_disabled() {
        let menu = ItemMenu::build(1, 1, &[]);
        assert_eq!(outline(&menu.entries)[1], ("Open With >".into(), false));
        assert!(submenu(&menu).is_empty());
        assert_eq!(outline(&menu.entries)[0], ("Open".into(), true));
    }

    #[test]
    fn all_missing_leaves_only_remove_enabled() {
        let menu = ItemMenu::build(3, 0, &[]);
        assert_eq!(
            outline(&menu.entries),
            vec![
                ("Open".into(), false),
                ("Open With >".into(), false),
                (reveal_title(), false),
                ("-".into(), false),
                ("Copy".into(), false),
                ("Remove from Pocket".into(), true),
            ]
        );
    }

    #[test]
    fn some_missing_keeps_everything_enabled() {
        let menu = ItemMenu::build(3, 1, &[app("Preview", true)]);
        assert!(outline(&menu.entries).iter().all(|(t, on)| *on || t == "-"));
    }

    #[test]
    fn ids_map_to_their_actions() {
        let apps = [
            app("zed", false),
            app("TextEdit", true),
            app("BBEdit", false),
        ];
        let menu = ItemMenu::build(2, 2, &apps);
        let e = &menu.entries;
        assert_eq!(menu.action(id_of(e, "Open")), Some(MenuAction::Open));
        assert_eq!(
            menu.action(id_of(e, &reveal_title())),
            Some(MenuAction::Reveal)
        );
        assert_eq!(menu.action(id_of(e, "Copy")), Some(MenuAction::Copy));
        assert_eq!(
            menu.action(id_of(e, "Remove from Pocket")),
            Some(MenuAction::Remove)
        );
        let children = submenu(&menu);
        for (title, name) in [
            ("TextEdit (default)", "TextEdit"),
            ("BBEdit", "BBEdit"),
            ("zed", "zed"),
        ] {
            assert_eq!(
                menu.action(id_of(children, title)),
                Some(MenuAction::OpenWith(PathBuf::from(format!(
                    "/Applications/{name}.app"
                ))))
            );
        }
    }

    #[test]
    fn ids_are_unique_and_unknown_ids_do_nothing() {
        let apps = [app("a", true), app("b", false), app("c", false)];
        let menu = ItemMenu::build(1, 1, &apps);
        let mut ids = Vec::new();
        fn collect(entries: &[MenuEntry], ids: &mut Vec<u32>) {
            for e in entries {
                match e {
                    MenuEntry::Item { id, .. } => ids.push(*id),
                    MenuEntry::Submenu { children, .. } => collect(children, ids),
                    MenuEntry::Separator => {}
                }
            }
        }
        collect(&menu.entries, &mut ids);
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
        assert_eq!(count, 4 + apps.len());
        assert_eq!(menu.action(0), None);
        assert_eq!(menu.action(u32::MAX), None);
    }
}
