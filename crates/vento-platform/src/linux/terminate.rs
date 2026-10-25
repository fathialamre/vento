//! [`Platform::on_terminate`](crate::Platform::on_terminate) on Linux: GNOME
//! ends a session with SIGTERM, a terminal with SIGINT or SIGHUP. A thread
//! hears them and only raises a flag; the main thread runs the handler at
//! its next input sample and exits. If it never gets there (a menu waiting
//! for an answer), the thread ends vento itself after a grace period.

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// Set when a signal asked vento to quit.
static ASKED: AtomicBool = AtomicBool::new(false);
static LISTEN: Once = Once::new();

/// How long the main thread has to tidy up before vento ends anyway.
const GRACE: Duration = Duration::from_secs(3);

/// Starts listening for the signals that end vento, once.
pub(super) fn listen() {
    LISTEN.call_once(|| {
        let mut signals = match Signals::new([SIGTERM, SIGINT, SIGHUP]) {
            Ok(signals) => signals,
            Err(e) => return log::warn!("cannot listen for SIGTERM: {e}"),
        };
        let spawned = std::thread::Builder::new()
            .name("vento-signals".into())
            .spawn(move || {
                if let Some(signal) = signals.forever().next() {
                    log::info!("signal {signal}: quitting");
                    ASKED.store(true, Ordering::SeqCst);
                    std::thread::sleep(GRACE);
                    log::warn!("the main thread did not tidy up in time; quitting anyway");
                    std::process::exit(0);
                }
            });
        if let Err(e) = spawned {
            log::warn!("cannot listen for SIGTERM: {e}");
        }
    });
}

/// Whether a signal asked vento to quit since the last call.
pub(super) fn asked() -> bool {
    ASKED.swap(false, Ordering::SeqCst)
}

/// What runs before vento quits on a signal.
pub(super) struct Handler(pub Box<dyn Fn()>);

impl std::fmt::Debug for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handler")
    }
}
