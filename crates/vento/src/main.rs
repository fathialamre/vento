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
mod lazy_window;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(target_os = "macos")]
mod pause;
#[cfg(target_os = "macos")]
mod pocket;
#[cfg(target_os = "macos")]
mod screen_text;
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
    use vento_core::settings::{Settings, settings_path};

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
    // The Pocket shortcut opens the island on the Pocket, the Clipboard
    // shortcut on the history, and the Copy from screen shortcut starts
    // that. Each is registered at startup (the Clipboard one only while the
    // history is on, the Copy from screen one only on macOS 14 or later)
    // and again whenever it changes, and tried by the panel when one is
    // recorded.
    let open_on = |view: pocket::View| -> Rc<dyn Fn()> {
        let pocket = Rc::downgrade(&pocket);
        Rc::new(move || {
            if let Some(pocket) = pocket.upgrade() {
                pocket.open_view(view);
            }
        })
    };
    let pocket_slot = Rc::new(ShortcutSlot::new(open_on(pocket::View::Pocket)));
    let clip_slot = Rc::new(ShortcutSlot::new(open_on(pocket::View::Clipboard)));
    // Copy from screen needs macOS 14: on macOS 13 it has no shortcut, no
    // menu item and no settings field.
    let os = vento_platform::screen_text();
    let supported = os.supported();
    let screen_text = supported.then(|| {
        let access = screen_text::ScreenAccessView::new(platform.clone(), os.clone());
        let hider = access.clone();
        let (pause, copier, toaster) = (pause.clone(), platform.clone(), toaster.clone());
        screen_text::ScreenTextController::new(screen_text::ScreenTextDeps {
            os,
            is_paused: Box::new(move || pause.is_paused()),
            read: Box::new(screen_text::read_on_worker),
            copy: Box::new(move |text| copier.copy_text(text)),
            notify: Box::new(move |text, at| toaster.show(text, None, at)),
            ask_access: Box::new(move || access.show()),
            hide_access: Box::new(move || hider.hide_if_shown()),
        })
    });
    let screen_slot = screen_text
        .clone()
        .map(|controller| Rc::new(ShortcutSlot::new(Rc::new(move || controller.start()))));
    let panel = {
        let pause = pause.clone();
        let (pocket_slot, clip_slot) = (pocket_slot.clone(), clip_slot.clone());
        let screen_slot = screen_slot.clone();
        let clearer = Rc::downgrade(&pocket);
        panel::PanelController::new(
            platform.clone(),
            hub.clone(),
            recent.clone(),
            supported,
            Box::new(move |which, spec| match which {
                panel::ShortcutFor::Pocket => pocket_slot.register(spec),
                panel::ShortcutFor::Clipboard => clip_slot.register(spec),
                panel::ShortcutFor::ScreenText => screen_slot.as_ref().map_or_else(
                    || Err("Copy from screen needs macOS 14 or later".into()),
                    |slot| slot.register(spec),
                ),
            }),
            Box::new(move || {
                if let Some(pocket) = clearer.upgrade() {
                    pocket.clear_clip_history();
                }
            }),
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

    // Registers a shortcut, saying in a toast why one cannot be.
    let try_slot = {
        let toaster = toaster.clone();
        let platform = platform.clone();
        move |slot: &ShortcutSlot, name: &str, spec: &str| {
            if let Err(reason) = slot.register(spec) {
                log::warn!("{name} shortcut {spec:?} unavailable: {reason}");
                let at = platform.sample_input().cursor;
                toaster.show(&format!("{name} shortcut unavailable: {reason}"), None, at);
            }
        }
    };
    let apply_pocket_shortcut = Rc::new({
        let (try_slot, slot) = (try_slot.clone(), pocket_slot);
        move |s: &Settings| try_slot(&slot, "Pocket", &s.pocket_shortcut)
    });
    let apply_screen_shortcut = Rc::new({
        let (try_slot, slot) = (try_slot.clone(), screen_slot);
        move |s: &Settings| {
            if let Some(slot) = &slot {
                try_slot(slot, "Copy from screen", &s.screen_text_shortcut);
            }
        }
    });
    // The Clipboard shortcut is free while the history is off.
    let apply_clip_shortcut = Rc::new({
        let slot = clip_slot;
        move |s: &Settings| {
            if s.clipboard_history {
                try_slot(&slot, "Clipboard", &s.clipboard_shortcut);
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
            }
        }));
    }

    // The menu-bar icon needs the running event loop, and lives as long as the app.
    let tray_handle = Rc::new(RefCell::new(None));
    let keep = tray_handle.clone();
    let tray_info = info.clone();
    slint::Timer::single_shot(Duration::ZERO, move || {
        let settings = hub.current();
        apply_pocket_shortcut(&settings);
        apply_clip_shortcut(&settings);
        apply_screen_shortcut(&settings);
        *keep.borrow_mut() = Some(tray::install_tray(tray::TrayDeps {
            exec: executor,
            platform,
            on_open_panel: Box::new(move |icon| {
                tray_info.dismiss();
                panel.toggle_at(icon);
            }),
            pause,
            on_copy_from_screen: screen_text
                .map(|controller| Box::new(move || controller.start()) as Box<dyn Fn()>),
        }));
    });
    let result = slint::run_event_loop_until_quit();
    // Quitting stops a running count explicitly.
    info.dismiss();
    pocket.flush_pending();
    result
}

/// One global shortcut and what it does: holds the key registered for it,
/// if any.
#[cfg(target_os = "macos")]
struct ShortcutSlot {
    action: std::rc::Rc<dyn Fn()>,
    held: std::cell::RefCell<Option<(String, hotkey::HotkeyHandle)>>,
}

#[cfg(target_os = "macos")]
impl ShortcutSlot {
    /// A slot with no key registered yet, calling `action` on each press.
    fn new(action: std::rc::Rc<dyn Fn()>) -> Self {
        Self {
            action,
            held: std::cell::RefCell::new(None),
        }
    }

    /// Registers `spec` in place of the key this slot holds. Registering the
    /// spec already registered does nothing, so a shortcut the panel has
    /// registered is not registered a second time when its change is saved.
    fn register(&self, spec: &str) -> Result<(), String> {
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
        let handle = hotkey::install_hotkey(spec, Box::new(move || action()))?;
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

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("Vento: this platform is not supported yet");
}
