//! vento: a radial file-actions wheel that opens under the cursor while files
//! are dragged from Finder and Ctrl is pressed.

#[cfg(target_os = "macos")]
mod archive_ui;
#[cfg(target_os = "macos")]
mod exec;
#[cfg(target_os = "macos")]
mod hotkey;
#[cfg(target_os = "macos")]
mod info;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(target_os = "macos")]
mod pause;
#[cfg(target_os = "macos")]
mod pocket;
#[cfg(target_os = "macos")]
mod settings_hub;
#[cfg(target_os = "macos")]
mod toast;
#[cfg(target_os = "macos")]
mod tray;
#[cfg(target_os = "macos")]
mod wheel;

/// Slint components generated from `ui/app.slint`.
#[cfg(target_os = "macos")]
mod ui {
    slint::include_modules!();
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), slint::PlatformError> {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use vento_core::actions::ActionId;
    use vento_core::destinations::{RecentDestinations, recent_path};
    use vento_core::settings::settings_path;

    /// How long the island stays open to confirm an add from the wheel.
    const POCKET_CONFIRM: Duration = Duration::from_millis(1500);

    env_logger::init();
    select_backend()?;
    let hub = settings_hub::SettingsHub::new(settings_path());
    if let Some(reason) = hub.load_error() {
        log::warn!("settings file not loaded, using the defaults: {reason}");
    }
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
    let info = info::InfoController::new(platform.clone(), toaster.clone());
    let drop_executor = executor.clone();
    let drop_pocket = pocket.clone();
    let drop_info = info.clone();
    let controller = wheel::WheelController::new(
        platform.clone(),
        hub.current(),
        recent.clone(),
        Box::new(move |drop: wheel::WheelDrop| match drop.action {
            ActionId::Pocket => drop_pocket.add(drop.paths, POCKET_CONFIRM),
            ActionId::Info => drop_info.open(drop.paths, drop.at),
            _ => drop_executor.run(drop),
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
    // The Pocket shortcut, registered at startup and again whenever it
    // changes, and tried by the panel when one is recorded. Registering the
    // spec already registered does nothing, so a shortcut the panel has
    // registered is not registered a second time when its change is saved.
    let shortcut: Rc<RefCell<Option<(String, hotkey::HotkeyHandle)>>> = Rc::new(RefCell::new(None));
    let register_shortcut = Rc::new({
        let pocket = Rc::downgrade(&pocket);
        move |spec: &str| -> Result<(), String> {
            if shortcut
                .borrow()
                .as_ref()
                .is_some_and(|(registered, _)| registered == spec)
            {
                return Ok(());
            }
            // Free the old key first, so the same key can be registered again.
            shortcut.borrow_mut().take();
            let pocket = pocket.clone();
            let on_press = Box::new(move || {
                if let Some(pocket) = pocket.upgrade() {
                    pocket.toggle_pinned();
                }
            });
            let handle = hotkey::install_hotkey(spec, on_press)?;
            *shortcut.borrow_mut() = Some((spec.to_owned(), handle));
            Ok(())
        }
    });
    let panel = {
        let pause = pause.clone();
        let register_shortcut = register_shortcut.clone();
        panel::PanelController::new(
            platform.clone(),
            hub.clone(),
            recent.clone(),
            Box::new(move |spec| register_shortcut(spec)),
            Box::new(move |paused| pause.set(paused)),
        )
    };
    // The Info window and the panel never show together: whichever opens
    // closes the other, keeping the focus for itself.
    {
        let panel = Rc::downgrade(&panel);
        info.set_before_open(Box::new(move || {
            if let Some(panel) = panel.upgrade() {
                panel.close_for_other_window();
            }
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
    let observer = pocket.clone();
    controller.set_sample_listener(Box::new(move |sample, dragging| {
        observer.observe(sample, dragging);
    }));

    let apply_shortcut = Rc::new({
        let toaster = toaster.clone();
        let platform = platform.clone();
        move |spec: &str| {
            if let Err(reason) = register_shortcut(spec) {
                log::warn!("pocket shortcut {spec:?} unavailable: {reason}");
                let at = platform.sample_input().cursor;
                toaster.show(&format!("Pocket shortcut unavailable: {reason}"), None, at);
            }
        }
    });
    {
        let apply_shortcut = apply_shortcut.clone();
        hub.on_change(Box::new(move |old, new| {
            if old.pocket_shortcut != new.pocket_shortcut {
                apply_shortcut(&new.pocket_shortcut);
            }
        }));
    }

    // The menu-bar icon needs the running event loop, and lives as long as the app.
    let tray_handle = Rc::new(RefCell::new(None));
    let keep = tray_handle.clone();
    let tray_info = info.clone();
    slint::Timer::single_shot(Duration::ZERO, move || {
        apply_shortcut(&hub.current().pocket_shortcut);
        *keep.borrow_mut() = Some(tray::install_tray(tray::TrayDeps {
            exec: executor,
            platform,
            on_open_panel: Box::new(move |icon| {
                tray_info.dismiss();
                panel.toggle_at(icon);
            }),
            pause,
        }));
    });
    let result = slint::run_event_loop_until_quit();
    // Quitting stops a running count explicitly.
    info.dismiss();
    pocket.flush_pending();
    result
}

/// Uses winit with an Accessory app that never activates itself at launch,
/// so vento has no Dock icon and never takes focus from the frontmost app,
/// even before the first window is shown.
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
        .with_winit_event_loop_builder(event_loop)
        .select()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("Vento: this platform is not supported yet");
}
