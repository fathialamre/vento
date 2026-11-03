//! vento: a radial file-actions wheel that opens under the cursor while files
//! are dragged from Finder and Ctrl is pressed.

// No console window behind vento on Windows; debug builds keep it for the
// log.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod appearance;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod archive_ui;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod awake;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod exec;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[cfg_attr(target_os = "linux", path = "hotkey_linux.rs")]
mod hotkey;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod info;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod lazy_window;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod notice;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod panel;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod pause;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod pdf;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod pocket;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod quit;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod rename;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod screen_text;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod settings_hub;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod settings_window;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod toast;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[cfg_attr(target_os = "linux", path = "tray_linux.rs")]
mod tray;
#[cfg(target_os = "macos")]
mod updater;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod welcome_window;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod whats_new_window;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod wheel;

/// The `drag` crate needs GTK on Linux; the Pocket uses this stand-in.
#[cfg(target_os = "linux")]
#[path = "drag_linux.rs"]
mod drag;

/// Slint components generated from `ui/app.slint`.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod ui {
    slint::include_modules!();
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn main() -> Result<(), slint::PlatformError> {
    use std::cell::RefCell;
    use std::rc::{Rc, Weak};
    use std::time::Duration;

    use vento_core::actions::ActionId;
    use vento_core::destinations::{RecentDestinations, recent_path};
    use vento_core::i18n::{system_languages, tr};
    use vento_core::settings::{Settings, settings_path};
    use vento_core::update::changelog::{bundled_notes, digest};
    use vento_core::welcome;

    /// How long the island stays open to confirm an add from the wheel.
    const POCKET_CONFIRM: Duration = Duration::from_millis(1500);

    // Warnings show without RUST_LOG: they are what explains a failure
    // after the fact (a hold Keep Awake could not release, say).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    // Run as the update watcher (the old binary, started by the copy that
    // quits for an update): no windows and no event loop. After the logger,
    // so its warnings show when it is run by hand.
    // A watcher line that does not read starts nothing: the app here would
    // be a second copy.
    #[cfg(target_os = "macos")]
    match updater::watch::dispatch(&std::env::args().collect::<Vec<_>>()) {
        updater::watch::Dispatch::Watch(args) => {
            let _ = updater::watch::run(args);
            return Ok(());
        }
        updater::watch::Dispatch::Malformed => {
            log::warn!(
                "unreadable {} command line; not starting",
                updater::watch::FLAG
            );
            return Ok(());
        }
        updater::watch::Dispatch::Normal => {}
    }
    // A second launch (the app grid) opens the running Vento's settings.
    #[cfg(any(target_os = "linux", windows))]
    if !vento_platform::claim_instance() {
        return Ok(());
    }
    select_backend()?;
    // Before the hub reads the settings and anything writes to Vento's
    // folders: a fresh install saves the settings it starts with, the
    // Welcome setup still to do. A fresh install shows no What's new.
    let fresh = match welcome_window::first_launch(
        &settings_path(),
        welcome::data_dir().as_deref(),
        &system_languages(),
    ) {
        Ok(fresh) => fresh,
        Err(e) => {
            log::warn!("cannot save the first settings: {e}");
            false
        }
    };
    let hub = settings_hub::SettingsHub::new(settings_path());
    if let Some(reason) = hub.load_error() {
        log::warn!("settings file not loaded, using the defaults: {reason}");
    }
    appearance::set_current(hub.current().appearance);
    appearance::set_current_theme(hub.current().theme);
    hub.on_change(Box::new(|old, new| {
        appearance::set_current(new.appearance);
        appearance::set_current_theme(new.theme);
        // This listener comes first, so the windows' own listeners (the
        // Settings window re-applies the look) see the new language. Any
        // window exists by now, as `apply_language` needs.
        if old.language != new.language {
            appearance::apply_language(language_code(&new.language));
        }
    }));
    // The copy the executor and the Pocket read; the wheel keeps its own
    // through its constructor and its hub listener.
    let shared = hub.shared();
    let platform = vento_platform::current();
    let toaster = toast::Toaster::new(platform.clone());
    // Folders recently moved or copied to: the executor adds to them, the
    // wheel's ring reads them.
    let recent = Rc::new(RefCell::new(RecentDestinations::load(&recent_path())));
    let executor = exec::Executor::new(
        platform.clone(),
        shared.clone(),
        toaster.clone(),
        recent.clone(),
        archive_ui::ProgressView::new(platform.clone()),
        archive_ui::PasswordView::new(platform.clone()),
    );
    let pocket = pocket::PocketController::new(platform.clone(), shared.clone(), toaster.clone());
    let sink: Rc<dyn notice::NoticeSink> = pocket.clone();
    toaster.set_sink(Rc::downgrade(&sink));
    // Activity offers the newest Undo while it still undoes the last
    // operation.
    {
        let executor = Rc::downgrade(&executor);
        pocket.set_last_serial(Box::new(move || {
            executor.upgrade().and_then(|e| e.last_serial())
        }));
    }
    // Slint can switch its translation only once a component exists: the
    // Pocket's window is the first. Text made before this is English.
    appearance::apply_language(language_code(&hub.current().language));
    let info = info::InfoController::new(platform.clone(), toaster.clone());
    let pdf = pdf::PdfController::new(
        platform.clone(),
        toaster.clone(),
        executor.clone(),
        hub.clone(),
    );
    let rename = rename::RenameController::new(
        platform.clone(),
        toaster.clone(),
        executor.clone(),
        hub.clone(),
    );
    let drop_executor = executor.clone();
    let drop_pocket = pocket.clone();
    let drop_info = info.clone();
    let drop_pdf = pdf.clone();
    let drop_rename = rename.clone();
    // The Welcome window, once it exists: a drop of its sample file while it
    // is open is practice, and goes no further.
    let welcome_slot: Rc<RefCell<Weak<welcome_window::WelcomeController>>> =
        Rc::new(RefCell::new(Weak::new()));
    let drop_welcome = welcome_slot.clone();
    let controller = wheel::WheelController::new(
        platform.clone(),
        hub.current(),
        recent.clone(),
        Box::new(move |drop: wheel::WheelDrop| {
            welcome_window::route_drop(
                drop,
                |drop| {
                    drop_welcome
                        .borrow()
                        .upgrade()
                        .is_some_and(|w| w.catch_practice_drop(drop))
                },
                |drop| match drop.action {
                    ActionId::Pocket => drop_pocket.add(drop.paths, POCKET_CONFIRM),
                    ActionId::Info => drop_info.open(drop.paths, drop.at),
                    ActionId::Pdf => drop_pdf.open(drop.paths, drop.at),
                    ActionId::Rename => drop_rename.open(drop.paths, drop.at),
                    _ => drop_executor.run(drop),
                },
            );
        }),
    );
    controller.start();
    let wheel = controller.clone();
    hub.on_change(Box::new(move |_old, new| wheel.apply_settings(new.clone())));

    // Pausing, from the tray or the panel, pauses the wheel and the Pocket.
    let pause = pause::PauseSwitch::new();
    {
        let wheel = controller.clone();
        let pocket = Rc::downgrade(&pocket);
        pause.on_change(Box::new(move |paused| {
            wheel.set_paused(paused);
            if let Some(pocket) = pocket.upgrade() {
                pocket.set_paused(paused);
            }
        }));
    }
    // The virtual island's paused pill resumes: while paused, the wheel's
    // poll keeps sampling for it.
    {
        let wheel = Rc::downgrade(&controller);
        pocket.set_on_virtual(Box::new(move |on| {
            if let Some(wheel) = wheel.upgrade() {
                wheel.set_island_virtual(on);
            }
        }));
    }
    {
        let pause = Rc::downgrade(&pause);
        pocket.set_on_resume(Box::new(move || {
            if let Some(pause) = pause.upgrade() {
                pause.set(false);
            }
        }));
    }
    // The Pocket shortcut opens the island on the Pocket, the Clipboard
    // shortcut on the history, and the Copy from screen shortcut starts
    // that. Each is registered at startup (the Clipboard one only while the
    // history is on, the Copy from screen one only on macOS 14 or later)
    // and again whenever it changes, and tried by the Settings window when
    // one is recorded.
    let open_on = |view: pocket::View| -> Rc<dyn Fn()> {
        let pocket = Rc::downgrade(&pocket);
        Rc::new(move || {
            if let Some(pocket) = pocket.upgrade() {
                pocket.open_view(view);
            }
        })
    };
    let pocket_slot = Rc::new(ShortcutSlot::new(
        || tr("Open the Pocket"),
        open_on(pocket::View::Pocket),
    ));
    let clip_slot = Rc::new(ShortcutSlot::new(
        || tr("Open the clipboard history"),
        open_on(pocket::View::Clipboard),
    ));
    // Copy from screen needs macOS 14: on macOS 13 it has no shortcut, no
    // menu item and no settings field.
    let os = vento_platform::screen_text();
    let supported = os.supported();
    // The Screen Recording window; What's new shows it too, when an update
    // cost the grant.
    let screen_access =
        supported.then(|| screen_text::ScreenAccessView::new(platform.clone(), os.clone()));
    let screen_text = screen_access.clone().map(|access| {
        let hider = access.clone();
        let (pause, copier, toaster) = (pause.clone(), platform.clone(), toaster.clone());
        screen_text::ScreenTextController::new(screen_text::ScreenTextDeps {
            os,
            is_paused: Box::new(move || pause.is_paused()),
            read: Box::new(screen_text::read_on_worker),
            copy: Box::new(move |text| copier.copy_text(text)),
            notify: Box::new(move |notice, at| toaster.notify(notice, at)),
            ask_access: Box::new(move || access.show()),
            hide_access: Box::new(move || hider.hide_if_shown()),
        })
    });
    let screen_slot = screen_text.clone().map(|controller| {
        Rc::new(ShortcutSlot::new(
            || tr("Copy from screen"),
            Rc::new(move || controller.start()),
        ))
    });
    let settings = {
        let (pocket_slot, clip_slot) = (pocket_slot.clone(), clip_slot.clone());
        let screen_slot = screen_slot.clone();
        let clearer = Rc::downgrade(&pocket);
        let (pocket_access, pocket_touch) = (Rc::downgrade(&pocket), Rc::downgrade(&pocket));
        settings_window::SettingsWindowController::new(
            platform.clone(),
            hub.clone(),
            recent.clone(),
            supported,
            // The window only logs why a key could not be registered, so the
            // reason is worded in English.
            Box::new(move |which, spec| match which {
                settings_window::ShortcutFor::Pocket => {
                    pocket_slot.register(spec).map_err(in_english_text)
                }
                settings_window::ShortcutFor::Clipboard => {
                    clip_slot.register(spec).map_err(in_english_text)
                }
                settings_window::ShortcutFor::ScreenText => screen_slot.as_ref().map_or_else(
                    || Err("Copy from screen needs macOS 14 or later".into()),
                    |slot| slot.register(spec).map_err(in_english_text),
                ),
            }),
            Box::new(move || {
                if let Some(pocket) = clearer.upgrade() {
                    pocket.clear_clip_history();
                }
            }),
            settings_window::ShotsLink {
                access: Box::new(move || {
                    pocket_access.upgrade().map_or_else(
                        || (pocket::Access::Off, std::path::PathBuf::new()),
                        |pocket| pocket.shots_access(),
                    )
                }),
                touch: Box::new(move || {
                    if let Some(pocket) = pocket_touch.upgrade() {
                        pocket.shots_touch();
                    }
                }),
            },
        )
    };
    // A listing that changes the access to the screenshot folder shows on
    // the Settings page.
    {
        let settings = Rc::downgrade(&settings);
        pocket.set_on_shots_changed(Box::new(move || {
            if let Some(settings) = settings.upgrade() {
                settings.show_shots();
            }
        }));
    }
    // The first-launch setup; Open Settings on its last screen ends it and
    // opens the Settings window on General.
    let welcome = welcome_window::WelcomeController::new(
        platform.clone(),
        hub.clone(),
        pause.clone(),
        supported,
    );
    *welcome_slot.borrow_mut() = Rc::downgrade(&welcome);
    {
        let settings = Rc::downgrade(&settings);
        welcome.set_on_open_settings(Box::new(move || {
            if let Some(settings) = settings.upgrade() {
                settings.open_on(settings_window::Page::General);
            }
        }));
    }
    {
        let welcome = Rc::downgrade(&welcome);
        settings.set_on_open_welcome(Box::new(move || {
            if let Some(welcome) = welcome.upgrade() {
                welcome.open();
            }
        }));
    }
    // What changed, after an update, from About's View and before an
    // update from See what's new. Open Settings opens a feature's page; the
    // notice's button shows the Screen Recording window.
    let whats_new = whats_new_window::WhatsNewController::new(platform.clone());
    {
        let settings = Rc::downgrade(&settings);
        whats_new.set_on_open_settings(Box::new(move |key| {
            if let (Some(settings), Some(page)) =
                (settings.upgrade(), settings_window::Page::from_key(key))
            {
                settings.open_on(page);
            }
        }));
    }
    if let Some(access) = screen_access {
        whats_new.set_on_notice(Box::new(move || access.show()));
    }
    {
        let whats_new = Rc::downgrade(&whats_new);
        settings.set_on_open_whats_new(Box::new(move || {
            let (Some(whats_new), Some(running)) = (whats_new.upgrade(), running_version()) else {
                return;
            };
            let notes = bundled_notes(vento_core::i18n::current().code);
            match digest(&notes, None, running) {
                Some(d) => whats_new.open(d, None),
                None => log::warn!("no notes for {running} in the changelog"),
            }
        }));
    }
    let panel = {
        let pause = pause.clone();
        panel::PanelController::new(
            platform.clone(),
            hub.clone(),
            Rc::downgrade(&pocket),
            executor.clone(),
            pdf.clone(),
            supported,
            Box::new(move |paused| pause.set(paused)),
        )
    };
    // Keep Awake in the virtual island: the resting pill shows a cup and
    // the time left, and the island says when a session ends soon (with
    // +1 h) and when the computer may sleep again.
    {
        let keep = panel.keep_awake();
        let (watched, island) = (Rc::downgrade(&keep), Rc::downgrade(&pocket));
        let watch = RefCell::new(awake::AwakeWatch::default());
        keep.on_change(Box::new(move || {
            let (Some(keep), Some(pocket)) = (watched.upgrade(), island.upgrade()) else {
                return;
            };
            let (session, now) = (keep.session(), jiff::Timestamp::now());
            let tz = jiff::tz::TimeZone::system();
            pocket.set_awake(
                session.map(|s| awake::rest_status(s, now, &tz)),
                session.is_some_and(|s| awake::ends_soon(s, now)),
                session.and_then(|s| awake::left_fraction(s, now)),
            );
            let news = watch.borrow_mut().changed(session, now);
            match news {
                Some(awake::AwakeNews::EndsSoon) => {
                    let keep = Rc::downgrade(&keep);
                    pocket.notify(
                        notice::Notice::busy(tr("Awake ends in 5 min"))
                            .with_button(notice::ButtonLabel::AddHour, move || {
                                if let Some(keep) = keep.upgrade() {
                                    keep.add_hour();
                                }
                            })
                            .island_only(),
                    );
                }
                Some(awake::AwakeNews::Stopped) => {
                    pocket.notify(notice::Notice::info(tr("Sleep allowed again")).island_only());
                }
                None => {}
            }
        }));
    }
    // The menu-bar icon needs the running event loop, and lives as long as the app.
    let tray_handle: Rc<RefCell<Option<tray::TrayHandle>>> = Rc::new(RefCell::new(None));
    // Updates (macOS): the daily check, the download and the restart. Its
    // quit asks the PDF windows with unsaved edits first, as any quit does.
    #[cfg(target_os = "macos")]
    let updates = match welcome::data_dir() {
        Some(data_dir) => {
            let (exec, pdf) = (Rc::downgrade(&executor), Rc::downgrade(&pdf));
            Some(updater::UpdateController::new(
                hub.clone(),
                data_dir,
                Box::new(
                    move |swap, on_cancel, on_fail| match (exec.upgrade(), pdf.upgrade()) {
                        (Some(exec), Some(pdf)) => {
                            quit::quit_for_update(&exec, &pdf, swap, on_cancel, on_fail);
                        }
                        _ => on_cancel(),
                    },
                ),
                Box::new(|| vento_platform::screen_text().has_access()),
                {
                    let exec = Rc::downgrade(&executor);
                    Box::new(move || exec.upgrade().is_some_and(|exec| exec.is_busy()))
                },
            ))
        }
        None => {
            log::warn!("no data folder: updates are off");
            None
        }
    };
    // The panel's update card and About's update row follow the controller,
    // and their buttons drive it. Both are written again when the language
    // changes.
    #[cfg(target_os = "macos")]
    if let (Some(updates), Some(running)) = (&updates, running_version()) {
        let show = {
            let (panel, settings, updates) = (
                Rc::downgrade(&panel),
                Rc::downgrade(&settings),
                Rc::downgrade(updates),
            );
            move || {
                if let (Some(panel), Some(settings), Some(updates)) =
                    (panel.upgrade(), settings.upgrade(), updates.upgrade())
                {
                    let state = updates.state();
                    panel.set_update(panel::update_view(&state, running));
                    settings.set_update(settings_window::about_update_view(
                        &state,
                        running,
                        updater::unix_now(),
                    ));
                }
            }
        };
        let on_state = show.clone();
        let show_now = show.clone();
        // The tray's dot follows the state too; the tray may not exist yet,
        // and is given the state once it does.
        let (dot_tray, dot_updates) = (tray_handle.clone(), Rc::downgrade(updates));
        updates.on_change(Box::new(move |_| {
            on_state();
            if let (Some(tray), Some(updates)) = (dot_tray.borrow().as_ref(), dot_updates.upgrade())
            {
                tray.set_dot(updates.state().shows_dot());
            }
        }));
        hub.on_change(Box::new(move |old, new| {
            if old.language != new.language {
                show();
            }
        }));
        // The panel's card and About's row do the same things.
        let actions = |updates: &Rc<updater::UpdateController>| {
            let (update, cancel, notes, retry) = (
                Rc::downgrade(updates),
                Rc::downgrade(updates),
                Rc::downgrade(updates),
                Rc::downgrade(updates),
            );
            let preview = Rc::downgrade(&whats_new);
            (
                // Restart once the update is downloaded, Update before.
                Box::new(move || {
                    if let Some(updates) = update.upgrade() {
                        if matches!(
                            updates.state(),
                            vento_core::update::state::UpdateState::Ready(_)
                        ) {
                            updates.restart();
                        } else {
                            updates.update();
                        }
                    }
                }) as Box<dyn Fn()>,
                Box::new(move || {
                    if let Some(updates) = cancel.upgrade() {
                        updates.cancel();
                    }
                }) as Box<dyn Fn()>,
                // See what's new: the coming version's notes, from its
                // release on GitHub (English only).
                Box::new(move || {
                    let (Some(updates), Some(whats_new)) = (notes.upgrade(), preview.upgrade())
                    else {
                        return;
                    };
                    if let Some((version, body)) = updates.pending_notes() {
                        let notes = [vento_core::update::changelog::parse_release_body(
                            version, &body,
                        )];
                        if let Some(d) = digest(&notes, None, version) {
                            whats_new.open(d, None);
                        }
                    }
                }) as Box<dyn Fn()>,
                // Try again: the failed update, or a new check when it is
                // not known.
                Box::new(move || {
                    if let Some(updates) = retry.upgrade() {
                        updates.try_again();
                    }
                }) as Box<dyn Fn()>,
            )
        };
        let (update, cancel, notes, retry) = actions(updates);
        panel.set_on_update(panel::UpdateActions {
            update,
            cancel,
            notes,
            retry,
        });
        let (update, cancel, notes, retry) = actions(updates);
        let check_now = Rc::downgrade(updates);
        settings.set_on_update(settings_window::AboutUpdateActions {
            update,
            cancel,
            notes,
            retry,
            check_now: Box::new(move || {
                if let Some(updates) = check_now.upgrade() {
                    updates.check_now();
                }
            }),
        });
        // The row shows the state at once, not only after the first change.
        show_now();
    }
    // The update in the virtual island: a new version, its download as a
    // job with Cancel, and the update ready to restart. Get it and Restart
    // do what the panel's card does.
    #[cfg(target_os = "macos")]
    if let Some(updates) = &updates {
        use vento_core::i18n::fill;

        let (watched, island, jobs) = (
            Rc::downgrade(updates),
            Rc::downgrade(&pocket),
            toaster.clone(),
        );
        let watch = RefCell::new(updater::island::UpdateWatch::default());
        let job: RefCell<Option<(u64, std::sync::Arc<vento_core::archive::Progress>)>> =
            RefCell::new(None);
        updates.on_change(Box::new(move |state| {
            let Some(pocket) = island.upgrade() else {
                return;
            };
            let steps = watch.borrow_mut().changed(state);
            for step in steps {
                match step {
                    updater::island::IslandStep::Out(version) => {
                        let updates = watched.clone();
                        pocket.notify(
                            notice::Notice::info(fill(
                                &tr("Vento {version} is out"),
                                &[("version", &version.to_string())],
                            ))
                            .with_button(notice::ButtonLabel::GetIt, move || {
                                if let Some(updates) = updates.upgrade() {
                                    updates.update();
                                }
                            })
                            .island_only(),
                        );
                    }
                    updater::island::IslandStep::DownloadStarted(version) => {
                        let (id, progress) =
                            (notice::next_job_id(), updater::island::download_progress());
                        let updates = watched.clone();
                        jobs.job_started(notice::JobNotice {
                            id,
                            icon: None,
                            text: fill(
                                &tr("Downloading {v}\u{2026}"),
                                &[("v", &version.to_string())],
                            ),
                            progress: Some(progress.clone()),
                            cancel: Some(Rc::new(move || {
                                if let Some(updates) = updates.upgrade() {
                                    updates.cancel();
                                }
                            })),
                        });
                        *job.borrow_mut() = Some((id, progress));
                    }
                    updater::island::IslandStep::Downloaded(fraction) => {
                        if let Some((_, progress)) = job.borrow().as_ref() {
                            updater::island::set_downloaded(progress, fraction);
                        }
                    }
                    updater::island::IslandStep::DownloadEnded => {
                        let ended = job.borrow_mut().take();
                        if let Some((id, _)) = ended {
                            jobs.job_ended(id);
                        }
                    }
                    updater::island::IslandStep::Ready => {
                        let updates = watched.clone();
                        pocket.notify(
                            notice::Notice::done(tr("Update ready"))
                                .with_button(notice::ButtonLabel::Restart, move || {
                                    if let Some(updates) = updates.upgrade() {
                                        updates.restart();
                                    }
                                })
                                .sticky()
                                .island_only(),
                        );
                    }
                }
            }
        }));
    }
    if let Some(controller) = screen_text.clone() {
        panel.set_on_copy_from_screen(Box::new(move || controller.start()));
    }
    {
        let settings = Rc::downgrade(&settings);
        panel.set_on_open_settings(Box::new(move || {
            if let Some(settings) = settings.upgrade() {
                settings.open();
            }
        }));
    }
    // The Info window and the panel never show together: whichever opens
    // closes the other, keeping the focus for itself. The Settings window
    // and the Welcome window are ordinary windows and stay; turning to one
    // closes both, and each leaves the focus with either when it closes.
    {
        let panel = Rc::downgrade(&panel);
        info.set_before_open(Box::new(move || {
            if let Some(panel) = panel.upgrade() {
                panel.close_for_other_window();
            }
        }));
    }
    {
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        settings.set_on_activated(Box::new(move || {
            if let Some(panel) = panel.upgrade() {
                panel.close_for_other_window();
            }
            if let Some(info) = info.upgrade() {
                info.dismiss();
            }
        }));
    }
    // So does turning to the Welcome window or the What's new window,
    // ordinary windows as well.
    {
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        welcome.set_on_activated(Box::new(move || {
            if let Some(panel) = panel.upgrade() {
                panel.close_for_other_window();
            }
            if let Some(info) = info.upgrade() {
                info.dismiss();
            }
        }));
    }
    {
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        whats_new.set_on_activated(Box::new(move || {
            if let Some(panel) = panel.upgrade() {
                panel.close_for_other_window();
            }
            if let Some(info) = info.upgrade() {
                info.dismiss();
            }
        }));
    }
    {
        let rename = Rc::downgrade(&rename);
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        let (pdf, welcome) = (Rc::downgrade(&pdf), Rc::downgrade(&welcome));
        let whats_new = Rc::downgrade(&whats_new);
        settings.set_other_window_open(Box::new(move || {
            panel.upgrade().is_some_and(|p| p.is_open())
                || info.upgrade().is_some_and(|i| i.is_open())
                || pdf.upgrade().is_some_and(|p| p.has_windows())
                || rename.upgrade().is_some_and(|r| r.is_open())
                || welcome.upgrade().is_some_and(|w| w.is_open())
                || whats_new.upgrade().is_some_and(|w| w.is_open())
        }));
    }
    // The Welcome window, closing, leaves the focus with any other vento
    // window open, the Settings window its Open Settings opens included.
    {
        let rename = Rc::downgrade(&rename);
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        let (pdf, settings) = (Rc::downgrade(&pdf), Rc::downgrade(&settings));
        let whats_new = Rc::downgrade(&whats_new);
        welcome.set_other_window_open(Box::new(move || {
            panel.upgrade().is_some_and(|p| p.is_open())
                || info.upgrade().is_some_and(|i| i.is_open())
                || pdf.upgrade().is_some_and(|p| p.has_windows())
                || rename.upgrade().is_some_and(|r| r.is_open())
                || settings.upgrade().is_some_and(|s| s.is_open())
                || whats_new.upgrade().is_some_and(|w| w.is_open())
        }));
    }
    // So does the What's new window.
    {
        let rename = Rc::downgrade(&rename);
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        let (pdf, settings) = (Rc::downgrade(&pdf), Rc::downgrade(&settings));
        let welcome = Rc::downgrade(&welcome);
        whats_new.set_other_window_open(Box::new(move || {
            panel.upgrade().is_some_and(|p| p.is_open())
                || info.upgrade().is_some_and(|i| i.is_open())
                || pdf.upgrade().is_some_and(|p| p.has_windows())
                || rename.upgrade().is_some_and(|r| r.is_open())
                || settings.upgrade().is_some_and(|s| s.is_open())
                || welcome.upgrade().is_some_and(|w| w.is_open())
        }));
    }
    // The last PDF window to close leaves the focus with another vento
    // window that is open.
    {
        let rename = Rc::downgrade(&rename);
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        let (settings, welcome) = (Rc::downgrade(&settings), Rc::downgrade(&welcome));
        let whats_new = Rc::downgrade(&whats_new);
        pdf.set_other_window_open(Box::new(move || {
            panel.upgrade().is_some_and(|p| p.is_open())
                || info.upgrade().is_some_and(|i| i.is_open())
                || rename.upgrade().is_some_and(|r| r.is_open())
                || settings.upgrade().is_some_and(|s| s.is_open())
                || welcome.upgrade().is_some_and(|w| w.is_open())
                || whats_new.upgrade().is_some_and(|w| w.is_open())
        }));
    }
    // So does the Rename window.
    {
        let (panel, info) = (Rc::downgrade(&panel), Rc::downgrade(&info));
        let (pdf, settings) = (Rc::downgrade(&pdf), Rc::downgrade(&settings));
        let (welcome, whats_new) = (Rc::downgrade(&welcome), Rc::downgrade(&whats_new));
        rename.set_other_window_open(Box::new(move || {
            panel.upgrade().is_some_and(|p| p.is_open())
                || info.upgrade().is_some_and(|i| i.is_open())
                || pdf.upgrade().is_some_and(|p| p.has_windows())
                || settings.upgrade().is_some_and(|s| s.is_open())
                || welcome.upgrade().is_some_and(|w| w.is_open())
                || whats_new.upgrade().is_some_and(|w| w.is_open())
        }));
    }
    // The panel opens above the tray icon, where the island lives on
    // Windows: the island gives way to it.
    {
        let panel = Rc::downgrade(&panel);
        pocket.set_corner_taken(Box::new(move || {
            panel.upgrade().is_some_and(|p| p.is_open())
        }));
    }
    {
        let panel = Rc::downgrade(&panel);
        pause.on_change(Box::new(move |paused| {
            if let Some(panel) = panel.upgrade() {
                panel.set_paused(paused);
            }
        }));
    }

    pocket.start();
    // A setup not finished (a fresh install, or Vento quit during it) opens
    // once the event loop runs.
    if !hub.current().welcome_done {
        let welcome = welcome.clone();
        slint::Timer::single_shot(Duration::ZERO, move || welcome.open());
    }
    // Files waiting for the Trash go now when vento quits, also when the
    // system ends it (log out, shut down); a running file job is stopped so
    // it leaves no partial output behind.
    let flusher = Rc::downgrade(&pocket);
    let stopper = Rc::downgrade(&executor);
    platform.on_terminate(Box::new(move || {
        if let Some(executor) = stopper.upgrade() {
            executor.shutdown();
        }
        if let Some(pocket) = flusher.upgrade() {
            pocket.flush_pending();
        }
    }));
    // Quit from the app menu (⌘Q in a vento window) goes the tray's way:
    // PDF windows with unsaved edits ask first.
    {
        let (exec, pdf) = (Rc::downgrade(&executor), Rc::downgrade(&pdf));
        platform.on_quit_request(Box::new(move || {
            if let (Some(exec), Some(pdf)) = (exec.upgrade(), pdf.upgrade()) {
                quit::quit(&exec, &pdf);
            }
        }));
    }
    // Launched again while running: the Settings window.
    let reopened = Rc::downgrade(&settings);
    platform.on_reopen(Box::new(move || {
        if let Some(settings) = reopened.upgrade() {
            log::info!("launched again: opening the settings");
            settings.open();
        }
    }));
    let observer = pocket.clone();
    controller.set_sample_listener(Box::new(move |sample, dragging| {
        observer.observe(sample, dragging);
    }));

    // Registers a shortcut, saying in a toast why one cannot be. `name` is
    // the feature's name for the log, `shown` the same name in the interface
    // language for the toast.
    let try_slot = {
        let toaster = toaster.clone();
        let platform = platform.clone();
        let settings = Rc::downgrade(&settings);
        move |slot: &ShortcutSlot, name: &str, shown: String, spec: &str| {
            if let Err(reason) = slot.register(spec) {
                log::warn!(
                    "{name} shortcut {spec:?} unavailable: {}",
                    in_english_text(reason.clone())
                );
                let at = platform.sample_input().cursor;
                let settings = settings.clone();
                toaster.notify(
                    shortcut_unavailable_notice(&shown, &reason.to_string(), move || {
                        if let Some(settings) = settings.upgrade() {
                            settings.open();
                        }
                    }),
                    at,
                );
            }
        }
    };
    let apply_pocket_shortcut = Rc::new({
        let (try_slot, slot) = (try_slot.clone(), pocket_slot);
        move |s: &Settings| try_slot(&slot, "Pocket", tr("Pocket"), &s.pocket_shortcut)
    });
    let apply_screen_shortcut = Rc::new({
        let (try_slot, slot) = (try_slot.clone(), screen_slot);
        move |s: &Settings| {
            if let Some(slot) = &slot {
                try_slot(
                    slot,
                    "Copy from screen",
                    tr("Copy from screen"),
                    &s.screen_text_shortcut,
                );
            }
        }
    });
    // The Clipboard shortcut is free while the history is off.
    let apply_clip_shortcut = Rc::new({
        let slot = clip_slot;
        move |s: &Settings| {
            if s.clipboard_history {
                try_slot(&slot, "Clipboard", tr("Clipboard"), &s.clipboard_shortcut);
            } else {
                slot.unregister();
            }
        }
    });
    {
        let apply_pocket_shortcut = apply_pocket_shortcut.clone();
        let apply_clip_shortcut = apply_clip_shortcut.clone();
        let apply_screen_shortcut = apply_screen_shortcut.clone();
        let pocket = Rc::downgrade(&pocket);
        hub.on_change(Box::new(move |old, new| {
            if old.pocket_shortcut != new.pocket_shortcut {
                apply_pocket_shortcut(new);
            }
            if old.screen_text_shortcut != new.screen_text_shortcut {
                apply_screen_shortcut(new);
            }
            if old.clipboard_history != new.clipboard_history
                || old.clipboard_shortcut != new.clipboard_shortcut
            {
                apply_clip_shortcut(new);
            }
            if let Some(pocket) = pocket.upgrade() {
                pocket.apply_clip_settings(new);
                pocket.apply_shot_settings(new);
                pocket.apply_pocket_settings(new);
                if old.island_mode != new.island_mode
                    || old.island_place != new.island_place
                    || old.island_shape != new.island_shape
                    || old.island_glass != new.island_glass
                    || old.island_shimmer != new.island_shimmer
                    || old.island_shimmer_every != new.island_shimmer_every
                    || old.island_shimmer_needs_items != new.island_shimmer_needs_items
                    || old.island_reduce_motion != new.island_reduce_motion
                {
                    pocket.island_settings_changed();
                }
                // The first listener has switched the language by now.
                if old.language != new.language {
                    pocket.apply_language();
                }
            }
        }));
    }

    // The menu is texted when it is made; a language change writes it
    // again. This listener comes after the first, which switches the
    // language.
    {
        let tray = tray_handle.clone();
        hub.on_change(Box::new(move |old, new| {
            if old.language != new.language
                && let Some(tray) = tray.borrow().as_ref()
            {
                tray.retext();
            }
        }));
    }
    let keep = tray_handle.clone();
    let tray_info = info.clone();
    let tray_settings = settings.clone();
    let tray_pdf = pdf.clone();
    let (open_clipboard, open_pocket, open_screenshots) = (
        open_on(pocket::View::Clipboard),
        open_on(pocket::View::Pocket),
        open_on(pocket::View::Screenshots),
    );
    #[cfg(target_os = "macos")]
    let start_updates = updates.clone();
    let start_whats_new = whats_new.clone();
    let (hint_hub, hint_platform, hint_toaster) = (hub.clone(), platform.clone(), toaster.clone());
    slint::Timer::single_shot(Duration::ZERO, move || {
        let settings = hub.current();
        apply_pocket_shortcut(&settings);
        apply_clip_shortcut(&settings);
        apply_screen_shortcut(&settings);
        *keep.borrow_mut() = Some(tray::install_tray(tray::TrayDeps {
            exec: executor,
            pdf: tray_pdf,
            platform,
            keep_awake: panel.keep_awake(),
            on_open_panel: Box::new(move |icon| {
                tray_info.dismiss();
                panel.toggle_at(icon);
            }),
            on_open_clipboard: Box::new(move || open_clipboard()),
            on_open_pocket: Box::new(move || open_pocket()),
            on_open_screenshots: Box::new(move || open_screenshots()),
            on_open_settings: Box::new(move || tray_settings.open()),
            pause,
            hub,
            on_copy_from_screen: screen_text
                .map(|controller| Box::new(move || controller.start()) as Box<dyn Fn()>),
        }));
        // Started by the update watcher: the tray and shortcuts are up, so
        // tell it this copy works. A minute later, with the watcher long
        // gone, the old copy it kept is deleted. Any other start after a
        // finished update deletes an old copy left behind the same way.
        #[cfg(target_os = "macos")]
        if std::env::args().any(|a| a == updater::watch::AFTER_UPDATE) {
            if let (Some(data_dir), Some(running)) = (welcome::data_dir(), running_version()) {
                match updater::watch::write_marker(&data_dir, running) {
                    Ok(()) => {
                        if let Some(bundle) = updater::swap::running_bundle() {
                            remove_old_copy_later(updater::swap::old_path(&bundle));
                        }
                    }
                    Err(e) => log::warn!("cannot write the update marker: {e}"),
                }
            } else {
                log::warn!("cannot write the update marker: no data folder");
            }
        } else if let (Some(data_dir), Some(running), Some(bundle)) = (
            welcome::data_dir(),
            running_version(),
            updater::swap::running_bundle(),
        ) && let Some(old) = updater::left_old_copy(&data_dir, &bundle, running)
        {
            remove_old_copy_later(old);
        }
        show_whats_new_on_start(&start_whats_new, fresh);
        show_island_hint_once(&hint_hub, hint_platform.as_ref(), &hint_toaster);
        #[cfg(target_os = "macos")]
        if let Some(updates) = start_updates {
            if let Some(tray) = keep.borrow().as_ref() {
                tray.set_dot(updates.state().shows_dot());
            }
            updates.start();
        }
    });
    let result = slint::run_event_loop_until_quit();
    // Quitting stops a running count explicitly.
    info.dismiss();
    pocket.flush_pending();
    result
}

/// The version running now: this crate's, the app's.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn running_version() -> Option<vento_core::update::version::Version> {
    vento_core::update::version::Version::parse(env!("CARGO_PKG_VERSION"))
}

/// At start, after an update: opens What's new on what changed since the
/// version that ran last (nothing on a `fresh` install), with a notice when
/// the update cost the Screen Recording grant (macOS). Then records this
/// version as the last that ran, so it opens once.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn show_whats_new_on_start(whats_new: &whats_new_window::WhatsNewController, fresh: bool) {
    use vento_core::update::state::UpdateFile;

    let Some(running) = running_version() else {
        log::warn!("unreadable version {}", env!("CARGO_PKG_VERSION"));
        return;
    };
    let Some(data_dir) = vento_core::welcome::data_dir() else {
        log::warn!("no data folder: What's new is off");
        return;
    };
    let path = UpdateFile::path(&data_dir);
    let mut file = UpdateFile::load(&path);
    let lang = vento_core::i18n::current().code;
    if let Some(d) = whats_new_window::digest_on_start(&file, running, fresh, lang) {
        let lost_access = cfg!(target_os = "macos")
            && file.had_screen_access
            && !vento_platform::screen_text().has_access();
        let notice = lost_access
            .then(|| vento_core::i18n::tr("Switch Screen Recording on again for Copy from screen"));
        whats_new.open(d, notice);
    }
    let before = file.clone();
    file.last_run_version = Some(running);
    file.had_screen_access = false;
    if file != before
        && let Err(e) = file.save(&path)
    {
        log::warn!("cannot save {}: {e}", path.display());
    }
}

/// At start, tells someone on the classic island, where the virtual one can
/// be chosen, about it: one toast, then `island_hint_shown` is saved so it
/// never comes again. Nothing happens while the settings file is broken, as
/// saving would write the defaults over it. The toast waits for the save,
/// so a settings file that cannot be written does not bring it back at
/// every start.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn show_island_hint_once(
    hub: &settings_hub::SettingsHub,
    platform: &dyn vento_platform::Platform,
    toaster: &toast::Toaster,
) {
    let offered = platform
        .pocket_home()
        .is_some_and(|home| pocket::virtual_offered(&home, platform.virtual_island_supported()));
    let loaded = hub.load_error().is_none();
    if !vento_core::settings::should_show_island_hint(&hub.current(), offered, loaded) {
        return;
    }
    if let Err(e) = hub.update(|s| s.island_hint_shown = true) {
        log::warn!("cannot save that the island hint was shown: {e}");
        return;
    }
    toaster.notify(
        notice::Notice::info(vento_core::i18n::tr(
            "New: try the Virtual island in Settings › Island",
        )),
        platform.sample_input().cursor,
    );
}

/// Deletes `old`, the old copy an update left beside the bundle
/// (`Vento.app.old`), 60 seconds from now and off the main thread.
#[cfg(target_os = "macos")]
fn remove_old_copy_later(old: std::path::PathBuf) {
    slint::Timer::single_shot(std::time::Duration::from_secs(60), move || {
        let removed = std::thread::Builder::new()
            .name("vento-remove-old".into())
            .spawn(move || match std::fs::remove_dir_all(&old) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    log::warn!("cannot remove {}: {e}", old.display());
                }
                _ => {}
            });
        if let Err(e) = removed {
            log::warn!("cannot remove the old copy: {e}");
        }
    });
}

/// The code of the language the `language` setting asks for now: the
/// system's preferred languages decide for "auto".
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn language_code(setting: &str) -> &'static str {
    vento_core::i18n::resolve(setting, &vento_core::i18n::system_languages()).code
}

/// The toast for a shortcut of the feature `name` that cannot be registered,
/// for `reason`.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn shortcut_unavailable_text(name: &str, reason: &str) -> String {
    vento_core::i18n::fill(
        &vento_core::i18n::tr("{name} shortcut unavailable: {reason}"),
        &[("name", name), ("reason", reason)],
    )
}

/// The notice for a shortcut of the feature `name` that cannot be registered,
/// for `reason`; its Change button runs `change`, which opens the settings.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn shortcut_unavailable_notice(
    name: &str,
    reason: &str,
    change: impl Fn() + 'static,
) -> notice::Notice {
    notice::Notice::error(shortcut_unavailable_text(name, reason))
        .with_button(notice::ButtonLabel::Change, change)
}

/// Why a shortcut failed, in English: for log lines.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn in_english_text(error: vento_core::settings::ShortcutError) -> String {
    vento_core::i18n::with_language("en", || error.to_string())
}

/// One global shortcut and what it does: holds the key registered for it,
/// if any.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
struct ShortcutSlot {
    /// What the shortcut does, where the system names it (GNOME asks the
    /// user to allow it under this name), in the interface language as of
    /// the registration: GNOME keeps the name it was first given until the
    /// next start.
    description: fn() -> String,
    action: std::rc::Rc<dyn Fn()>,
    held: std::cell::RefCell<Option<(String, hotkey::HotkeyHandle)>>,
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
impl ShortcutSlot {
    /// A slot with no key registered yet, calling `action` on each press.
    fn new(description: fn() -> String, action: std::rc::Rc<dyn Fn()>) -> Self {
        Self {
            description,
            action,
            held: std::cell::RefCell::new(None),
        }
    }

    /// Registers `spec` in place of the key this slot holds. Registering the
    /// spec already registered does nothing, so a shortcut the panel has
    /// registered is not registered a second time when its change is saved.
    fn register(&self, spec: &str) -> Result<(), vento_core::settings::ShortcutError> {
        if self
            .held
            .borrow()
            .as_ref()
            .is_some_and(|(registered, _)| registered == spec)
        {
            return Ok(());
        }
        // Free the old key first, so the same key can be registered again.
        self.unregister();
        let action = self.action.clone();
        let handle =
            hotkey::install_hotkey(spec, &(self.description)(), Box::new(move || action()))?;
        *self.held.borrow_mut() = Some((spec.to_owned(), handle));
        Ok(())
    }

    /// Frees the key this slot holds, if any.
    fn unregister(&self) {
        if let Some((spec, _)) = self.held.borrow_mut().take() {
            log::debug!("shortcut {spec} unregistered");
        }
    }
}

/// Uses winit with an Accessory app that never activates itself at launch,
/// so vento has no Dock icon and never takes focus from the frontmost app,
/// even before the first window is shown. Windows draw with the software
/// renderer (see the slint entry in Cargo.toml for why).
#[cfg(target_os = "macos")]
fn select_backend() -> Result<(), slint::PlatformError> {
    use slint::winit_030::winit::event_loop::EventLoop;
    use slint::winit_030::winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

    let mut event_loop = EventLoop::with_user_event();
    event_loop
        .with_activation_policy(ActivationPolicy::Accessory)
        .with_activate_ignoring_other_apps(false);
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_event_loop_builder(event_loop)
        .select()
}

/// Runs vento's windows as X11 clients under XWayland: GNOME's Wayland
/// lets no app place a window or keep it above the others. The windows that
/// must sit exactly where vento puts them (the wheel, the Pocket island,
/// toasts) are made override-redirect once they exist, through
/// `Platform::make_overlay` and `make_non_key`.
#[cfg(target_os = "linux")]
fn select_backend() -> Result<(), slint::PlatformError> {
    use slint::winit_030::winit::event_loop::EventLoop;
    use slint::winit_030::winit::platform::x11::EventLoopBuilderExtX11;

    let mut event_loop = EventLoop::with_user_event();
    event_loop.with_x11();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_event_loop_builder(event_loop)
        .select()
}

/// Uses winit with the software renderer, as on the other two. Windows
/// keeps vento's windows off the taskbar and above the others once they
/// exist, through `Platform::make_overlay` and `make_non_key`.
#[cfg(windows)]
fn select_backend() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn main() {
    println!("Vento: this platform is not supported yet");
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux", windows)))]
mod tests {
    use super::*;
    use vento_core::i18n::{tr, with_language};

    #[test]
    fn a_shortcut_that_cannot_be_registered_is_told_in_the_language() {
        assert_eq!(
            shortcut_unavailable_text("Pocket", "in use"),
            "Pocket shortcut unavailable: in use"
        );
        with_language("ar", || {
            assert_eq!(
                shortcut_unavailable_text(&tr("Pocket"), "in use"),
                "اختصار الجيب غير متاح: in use"
            );
            assert_eq!(
                shortcut_unavailable_text(&tr("Copy from screen"), "in use"),
                "اختصار النسخ من الشاشة غير متاح: in use"
            );
        });
    }

    #[test]
    fn an_invalid_shortcut_is_toasted_in_the_language_and_logged_in_english() {
        use vento_core::settings::ShortcutError;
        let error = ShortcutError::Invalid("p".into());
        with_language("ar", || {
            assert_eq!(
                shortcut_unavailable_text(&tr("Pocket"), &error.to_string()),
                "اختصار الجيب غير متاح: اختصار غير صالح \"p\""
            );
            assert_eq!(in_english_text(error.clone()), "invalid shortcut \"p\"");
        });
        assert_eq!(
            shortcut_unavailable_text("Pocket", &error.to_string()),
            "Pocket shortcut unavailable: invalid shortcut \"p\""
        );
    }

    #[test]
    fn the_names_gnome_shows_for_the_shortcuts_are_translated() {
        with_language("ar", || {
            assert_eq!(tr("Open the Pocket"), "فتح الجيب");
            assert_eq!(tr("Open the clipboard history"), "فتح سجل الحافظة");
        });
    }
}
