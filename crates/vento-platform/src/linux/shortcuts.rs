//! Global shortcuts through the desktop portal's GlobalShortcuts, which
//! GNOME implements on Wayland (where no app can grab a key itself).
//!
//! GNOME delivers shortcuts only to an app it knows: vento registers its app
//! id first ([`register_app`]), which needs a `.desktop` file of that name.
//!
//! GNOME keeps one list of shortcuts per app, which each binding replaces,
//! and asks the user only for shortcuts not already in it. So all of vento's
//! shortcuts are bound together, in one session: any change rebinds the
//! whole list, once the changes made together have all come in.
//!
//! The portal answers with signals, so a thread of its own runs a glib loop.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gio::glib;
use gio::prelude::*;
use vento_core::settings::parse_shortcut;

use super::portal::{PORTAL, PORTAL_PATH, next_token, request_path};

/// vento's app id: the name of its `.desktop` file.
pub const APP_ID: &str = "io.github.fathialamre.Vento";

const GLOBAL_SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";

/// How long to wait for more changes before rebinding: vento registers its
/// shortcuts one by one at startup, and they should go in one binding.
const SETTLE: Duration = Duration::from_millis(100);

/// Tells the portal which app this is, so GNOME delivers its shortcuts.
/// Comes before any other portal call on the session bus, which is when the
/// portal looks the app up.
pub(super) fn register_app() {
    let bus = match super::session_bus() {
        Ok(bus) => bus,
        Err(e) => return log::warn!("no session bus to register the app: {e}"),
    };
    let args =
        glib::Variant::tuple_from_iter([APP_ID.to_variant(), glib::VariantDict::new(None).end()]);
    if let Err(e) = bus.call_sync(
        Some(PORTAL),
        PORTAL_PATH,
        "org.freedesktop.host.portal.Registry",
        "Register",
        Some(&args),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
    ) {
        log::warn!(
            "cannot register {APP_ID} with the portal (global shortcuts need its .desktop file): {}",
            e.message()
        );
    }
}

/// `spec` (`"ctrl+alt+p"`) as the portal's trigger (`"CTRL+ALT+p"`).
fn trigger(spec: &str) -> Option<String> {
    let (m, key) = parse_shortcut(spec)?;
    let mut parts: Vec<String> = [
        (m.ctrl, "CTRL"),
        (m.alt, "ALT"),
        (m.shift, "SHIFT"),
        (m.meta, "LOGO"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, name)| name.to_owned())
    .collect();
    parts.push(key.to_string());
    Some(parts.join("+"))
}

/// A shortcut vento wants bound, on the shortcuts thread.
struct Wanted {
    spec: String,
    trigger: String,
    description: String,
    on_press: Box<dyn Fn() + Send>,
}

/// Called when a queued job is over.
type Done = Box<dyn FnOnce()>;
/// A step that talks to the portal, told how to say it is over.
type Job = Box<dyn FnOnce(Done)>;

thread_local! {
    /// The shortcuts vento wants, by id; only the shortcuts thread uses it.
    static WANTED: RefCell<BTreeMap<u64, Wanted>> = const { RefCell::new(BTreeMap::new()) };
    /// The session the shortcuts are bound in.
    static SESSION: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Whether a rebind is already on its way.
    static PENDING: Cell<bool> = const { Cell::new(false) };
    /// Listens for presses for as long as the thread runs.
    static ACTIVATED: RefCell<Option<gio::SignalSubscription>> = const { RefCell::new(None) };
    /// Jobs waiting for the one in flight. GNOME answers neither of two
    /// binds asked for at once (it shows both dialogs, then drops the
    /// answers), so one bind waits for the last to be answered.
    static QUEUE: RefCell<VecDeque<Job>> = RefCell::new(VecDeque::new());
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// Runs `job` once the jobs before it are over.
fn enqueue(job: Job) {
    QUEUE.with(|queue| queue.borrow_mut().push_back(job));
    run_next();
}

fn run_next() {
    if BUSY.get() {
        return;
    }
    let Some(job) = QUEUE.with(|queue| queue.borrow_mut().pop_front()) else {
        return;
    };
    BUSY.set(true);
    job(Box::new(|| {
        BUSY.set(false);
        run_next();
    }));
}

/// The shortcuts thread's context, started on first use.
fn context() -> &'static glib::MainContext {
    static CONTEXT: OnceLock<glib::MainContext> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        let context = glib::MainContext::new();
        let own = context.clone();
        std::thread::Builder::new()
            .name("vento-shortcuts".into())
            .spawn(move || {
                let main_loop = glib::MainLoop::new(Some(&own), false);
                if let Err(e) = own.with_thread_default(|| main_loop.run()) {
                    log::warn!("shortcuts thread stopped: {e}");
                }
            })
            .expect("spawn the shortcuts thread");
        context
    })
}

/// Calls the request-style portal method `method` with the args `args`
/// builds from a request token, then `done` with the response code and
/// results. Runs on the shortcuts thread.
fn request(
    bus: &gio::DBusConnection,
    method: &str,
    args: impl FnOnce(&str) -> glib::Variant,
    done: impl FnOnce(u32, glib::VariantDict) + 'static,
) {
    let Some(sender) = bus.unique_name() else {
        return log::warn!("no unique name on the session bus");
    };
    let token = next_token();
    let handle = request_path(&sender, &token);
    let done = std::rc::Rc::new(RefCell::new(Some(done)));
    let done_on_error = done.clone();
    let subscription: std::rc::Rc<RefCell<Option<gio::SignalSubscription>>> = Default::default();
    let keep = subscription.clone();
    *subscription.borrow_mut() = Some(bus.subscribe_to_signal(
        Some(PORTAL),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&handle),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            let code = signal
                .parameters
                .try_child_value(0)
                .and_then(|v| v.get::<u32>())
                .unwrap_or(2);
            let results = glib::VariantDict::new(signal.parameters.try_child_value(1).as_ref());
            if let Some(done) = done.borrow_mut().take() {
                done(code, results);
            }
            // One answer per request.
            keep.borrow_mut().take();
        },
    ));
    if let Err(e) = bus.call_sync(
        Some(PORTAL),
        PORTAL_PATH,
        GLOBAL_SHORTCUTS,
        method,
        Some(&args(&token)),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
    ) {
        log::warn!("{method} failed: {}", e.message());
        subscription.borrow_mut().take();
        // As the portal's "other error", so the caller always hears back.
        if let Some(done) = done_on_error.borrow_mut().take() {
            done(2, glib::VariantDict::new(None));
        }
    }
}

/// Calls the action of the shortcut `shortcut_id` pressed in `session`, if
/// that is the current session. Runs on the shortcuts thread.
fn pressed(session: &str, shortcut_id: &str) {
    if SESSION.with(|s| s.borrow().as_deref() != Some(session)) {
        return;
    }
    WANTED.with(|wanted| {
        if let Some(w) = wanted.borrow().values().find(|w| w.spec == shortcut_id) {
            (w.on_press)();
        }
    });
}

/// Listens for presses, once. Runs on the shortcuts thread.
fn listen(bus: &gio::DBusConnection) {
    if ACTIVATED.with(|a| a.borrow().is_some()) {
        return;
    }
    let subscription = bus.subscribe_to_signal(
        Some(PORTAL),
        Some(GLOBAL_SHORTCUTS),
        Some("Activated"),
        Some(PORTAL_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        |signal| {
            let p = signal.parameters;
            let session = p
                .try_child_value(0)
                .and_then(|v| v.str().map(str::to_owned));
            let id = p
                .try_child_value(1)
                .and_then(|v| v.str().map(str::to_owned));
            if let (Some(session), Some(id)) = (session, id) {
                pressed(&session, &id);
            }
        },
    );
    ACTIVATED.with(|a| *a.borrow_mut() = Some(subscription));
}

/// Rebinds the shortcuts once the changes made together have come in.
/// Runs on the shortcuts thread.
fn schedule_rebind() {
    if PENDING.replace(true) {
        return;
    }
    // On this thread's own context: `timeout_add_local` would use the
    // default one, which nothing runs in vento.
    let timer = glib::timeout_source_new(SETTLE, None, glib::Priority::DEFAULT, || {
        enqueue(Box::new(|done| {
            PENDING.set(false);
            rebind(done);
        }));
        glib::ControlFlow::Break
    });
    timer.attach(Some(context()));
}

/// Closes the current session and binds every wanted shortcut in a new one,
/// then calls `done`. Runs on the shortcuts thread, as a queued job.
fn rebind(done: Done) {
    let bus = match super::session_bus() {
        Ok(bus) => bus,
        Err(e) => {
            log::warn!("no session bus for the shortcuts: {e}");
            return done();
        }
    };
    listen(&bus);
    if let Some(old) = SESSION.with(|s| s.borrow_mut().take()) {
        close_session(&bus, &old);
    }
    let shortcuts: Vec<(String, String, String)> = WANTED.with(|wanted| {
        wanted
            .borrow()
            .values()
            .map(|w| (w.spec.clone(), w.trigger.clone(), w.description.clone()))
            .collect()
    });
    if shortcuts.is_empty() {
        return done();
    }
    let created_bus = bus.clone();
    request(
        &bus,
        "CreateSession",
        |token| {
            let options = glib::VariantDict::new(None);
            options.insert("handle_token", token);
            options.insert("session_handle_token", format!("{token}_s"));
            glib::Variant::tuple_from_iter([options.end()])
        },
        move |code, results| {
            let session: Option<String> = results.lookup("session_handle").ok().flatten();
            let Some(session) = session.filter(|_| code == 0) else {
                log::warn!("shortcuts: no portal session (response {code})");
                return done();
            };
            SESSION.with(|s| *s.borrow_mut() = Some(session.clone()));
            bind(&created_bus, session, shortcuts, done);
        },
    );
}

/// The portal's `a(sa{sv})` for `shortcuts` (spec, trigger, description):
/// each spec is its own id.
fn shortcuts_variant(shortcuts: &[(String, String, String)]) -> glib::Variant {
    glib::Variant::array_from_iter_with_type(
        glib::VariantTy::new("(sa{sv})").expect("a valid type"),
        shortcuts.iter().map(|(spec, trigger, description)| {
            let props = glib::VariantDict::new(None);
            props.insert("description", description);
            props.insert("preferred_trigger", trigger);
            glib::Variant::tuple_from_iter([spec.to_variant(), props.end()])
        }),
    )
}

/// Binds `shortcuts` in `session`, then calls `done`. Runs on the shortcuts
/// thread.
fn bind(
    bus: &gio::DBusConnection,
    session: String,
    shortcuts: Vec<(String, String, String)>,
    done: Done,
) {
    let specs: Vec<String> = shortcuts.iter().map(|(spec, _, _)| spec.clone()).collect();
    request(
        bus,
        "BindShortcuts",
        move |token| {
            let options = glib::VariantDict::new(None);
            options.insert("handle_token", token);
            glib::Variant::tuple_from_iter([
                glib::variant::ObjectPath::try_from(session)
                    .expect("the portal gives object paths")
                    .to_variant(),
                shortcuts_variant(&shortcuts),
                "".to_variant(),
                options.end(),
            ])
        },
        move |code, _results| {
            if code == 0 {
                log::info!("shortcuts bound: {}", specs.join(", "));
            } else {
                log::warn!("shortcuts {} not bound (response {code})", specs.join(", "));
            }
            done();
        },
    );
}

fn close_session(bus: &gio::DBusConnection, session: &str) {
    if let Err(e) = bus.call_sync(
        Some(PORTAL),
        session,
        "org.freedesktop.portal.Session",
        "Close",
        None,
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
    ) {
        log::debug!("closing {session}: {}", e.message());
    }
}

/// A global shortcut; dropping it unbinds it.
#[derive(Debug)]
pub struct GlobalShortcut {
    id: u64,
}

impl Drop for GlobalShortcut {
    fn drop(&mut self) {
        let id = self.id;
        context().invoke(move || {
            WANTED.with(|wanted| wanted.borrow_mut().remove(&id));
            schedule_rebind();
        });
    }
}

/// Binds `spec` (such as `"ctrl+alt+p"`) as a global shortcut, shown to the
/// user as `description`, calling `on_press` (on another thread) each time
/// it is pressed. The binding finishes in the background, together with the
/// other shortcuts bound at the same time: the first time, GNOME asks the
/// user to allow them; a binding that fails is logged.
///
/// # Errors
/// If `spec` is not a valid shortcut.
pub fn bind_global_shortcut(
    spec: &str,
    description: &str,
    on_press: Box<dyn Fn() + Send>,
) -> Result<GlobalShortcut, String> {
    let trigger = trigger(spec).ok_or_else(|| format!("invalid shortcut \"{spec}\""))?;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let wanted = Wanted {
        spec: spec.to_owned(),
        trigger,
        description: description.to_owned(),
        on_press,
    };
    context().invoke(move || {
        WANTED.with(|w| w.borrow_mut().insert(id, wanted));
        schedule_rebind();
    });
    Ok(GlobalShortcut { id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_become_portal_triggers() {
        assert_eq!(trigger("ctrl+alt+p").as_deref(), Some("CTRL+ALT+p"));
        assert_eq!(trigger("shift+meta+7").as_deref(), Some("SHIFT+LOGO+7"));
        assert_eq!(trigger("alt+ctrl+V").as_deref(), Some("CTRL+ALT+v"));
        assert_eq!(trigger("p"), None, "a bare key is no shortcut");
    }

    #[test]
    fn queued_jobs_wait_for_the_one_in_flight() {
        let log = std::rc::Rc::new(RefCell::new(Vec::new()));
        let first_done: std::rc::Rc<RefCell<Option<Done>>> = Default::default();
        {
            let (log, first_done) = (log.clone(), first_done.clone());
            enqueue(Box::new(move |done| {
                log.borrow_mut().push("first");
                *first_done.borrow_mut() = Some(done);
            }));
        }
        {
            let log = log.clone();
            enqueue(Box::new(move |done| {
                log.borrow_mut().push("second");
                done();
            }));
        }
        assert_eq!(*log.borrow(), ["first"], "the second waits");
        let done = first_done.borrow_mut().take().unwrap();
        done();
        assert_eq!(*log.borrow(), ["first", "second"]);
        assert!(!BUSY.get(), "nothing left in flight");
    }

    #[test]
    fn all_shortcuts_go_in_one_list_keyed_by_spec() {
        let v = shortcuts_variant(&[
            (
                "ctrl+alt+p".into(),
                "CTRL+ALT+p".into(),
                "Open the Pocket".into(),
            ),
            (
                "ctrl+alt+v".into(),
                "CTRL+ALT+v".into(),
                "Open the clipboard history".into(),
            ),
        ]);
        assert_eq!(v.type_().as_str(), "a(sa{sv})");
        assert_eq!(v.n_children(), 2);
        let second = v.child_value(1);
        assert_eq!(second.child_value(0).str(), Some("ctrl+alt+v"));
        let props = glib::VariantDict::new(Some(&second.child_value(1)));
        let trigger: Option<String> = props.lookup("preferred_trigger").unwrap();
        assert_eq!(trigger.as_deref(), Some("CTRL+ALT+v"));
    }

    #[test]
    fn invalid_specs_are_refused_at_once() {
        assert!(bind_global_shortcut("nonsense", "x", Box::new(|| {})).is_err());
    }
}
