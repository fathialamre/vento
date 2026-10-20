//! vento: a radial file-actions wheel that opens under the cursor while files
//! are dragged from Finder and Ctrl is pressed.

#[cfg(target_os = "macos")]
mod exec;
#[cfg(target_os = "macos")]
mod hotkey;
#[cfg(target_os = "macos")]
mod pocket;
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
    use vento_core::settings::{Settings, settings_path};

    /// How long the island stays open to confirm an add from the wheel.
    const POCKET_CONFIRM: Duration = Duration::from_millis(1500);

    env_logger::init();
    select_backend()?;
    let settings = Settings::load(&settings_path());
    // One copy for the executor; the wheel keeps its own through its constructor.
    let shared = Rc::new(RefCell::new(settings.clone()));
    let platform = vento_platform::current();
    let toaster = toast::Toaster::new(platform.clone());
    let executor = exec::Executor::new(platform.clone(), shared.clone(), toaster.clone());
    let pocket = pocket::PocketController::new(platform.clone(), shared.clone(), toaster.clone());
    let drop_executor = executor.clone();
    let drop_pocket = pocket.clone();
    let controller = wheel::WheelController::new(
        platform.clone(),
        settings,
        Box::new(move |action, paths, at| match action {
            ActionId::Pocket => drop_pocket.add(paths, POCKET_CONFIRM),
            _ => drop_executor.run(action, paths, at),
        }),
    );
    controller.start();

    pocket.start();
    // Files waiting for the Trash go now when vento quits, also when the
    // system ends it (log out, shut down).
    let flusher = Rc::downgrade(&pocket);
    platform.on_terminate(Box::new(move || {
        if let Some(pocket) = flusher.upgrade() {
            pocket.flush_pending();
        }
    }));
    let observer = pocket.clone();
    controller.set_sample_listener(Box::new(move |sample, dragging| {
        observer.observe(sample, dragging);
    }));

    // The Pocket shortcut, registered now and again on every settings reload.
    let shortcut = Rc::new(RefCell::new(None));
    let register_shortcut = {
        let pocket = Rc::downgrade(&pocket);
        let toaster = toaster.clone();
        let platform = platform.clone();
        move |spec: &str| {
            // Free the old key first, so the same key can be registered again.
            shortcut.borrow_mut().take();
            let pocket = pocket.clone();
            let on_press = Box::new(move || {
                if let Some(pocket) = pocket.upgrade() {
                    pocket.toggle_pinned();
                }
            });
            match hotkey::install_hotkey(spec, on_press) {
                Ok(handle) => *shortcut.borrow_mut() = Some(handle),
                Err(reason) => {
                    log::warn!("pocket shortcut {spec:?} unavailable: {reason}");
                    let at = platform.sample_input().cursor;
                    toaster.show(&format!("Pocket shortcut unavailable: {reason}"), None, at);
                }
            }
        }
    };

    // The menu-bar icon needs the running event loop, and lives as long as the app.
    let tray_handle = Rc::new(RefCell::new(None));
    let keep = tray_handle.clone();
    let paused_pocket = Rc::downgrade(&pocket);
    slint::Timer::single_shot(Duration::ZERO, move || {
        let spec = shared.borrow().pocket_shortcut.clone();
        register_shortcut(&spec);
        *keep.borrow_mut() = Some(tray::install_tray(tray::TrayDeps {
            ctrl: controller,
            exec: executor,
            shared,
            toast: toaster,
            platform,
            on_reload: Box::new(move |settings| register_shortcut(&settings.pocket_shortcut)),
            on_pause: Box::new(move |paused| {
                if let Some(pocket) = paused_pocket.upgrade() {
                    pocket.set_paused(paused);
                }
            }),
        }));
    });
    let result = slint::run_event_loop_until_quit();
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
    println!("vento: this platform is not supported yet");
}
