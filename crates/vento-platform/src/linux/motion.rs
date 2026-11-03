//! What GNOME says about animations and notification banners, read with
//! `gsettings`. A missing binary, schema or key, or a slow answer, is false.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::motion::parse_gsettings_bool;

/// How long `gsettings` gets to answer before it is given up on.
const TIMEOUT: Duration = Duration::from_millis(500);

/// Whether animations are turned off.
pub(super) fn reduce_motion() -> bool {
    gsettings_bool("org.gnome.desktop.interface", "enable-animations") == Some(false)
}

/// Whether notification banners are turned off (Do Not Disturb).
pub(super) fn quiet_time() -> bool {
    gsettings_bool("org.gnome.desktop.notifications", "show-banners") == Some(false)
}

/// `gsettings get schema key` as a bool, `None` if it fails, times out or
/// prints anything else.
fn gsettings_bool(schema: &str, key: &str) -> Option<bool> {
    let mut child = Command::new("gsettings")
        .args(["get", schema, key])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_gsettings_bool(&out)
}
