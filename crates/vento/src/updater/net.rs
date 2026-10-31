//! The app's only networking: one request for the latest release and the
//! downloads of the assets it names.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vento_core::i18n::{fill, tr};

/// Where the latest published release is described.
pub const API_URL: &str = "https://api.github.com/repos/fathialamre/vento/releases/latest";

/// Name of the environment variable that replaces [`API_URL`], for hand checks.
const API_ENV: &str = "VENTO_UPDATE_API";

/// How long the release lookup, and the start of a download, may take.
const TIMEOUT: Duration = Duration::from_secs(15);

/// A download is read and checked for cancel in pieces of this size.
const CHUNK: usize = 64 * 1024;

/// The release URL to ask: `VENTO_UPDATE_API` when it is set, else [`API_URL`].
pub fn api_url() -> String {
    match std::env::var(API_ENV) {
        Ok(url) if !url.is_empty() => url,
        _ => API_URL.to_string(),
    }
}

/// Why a request or download did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// The server could not be reached (no network, DNS, refused, timeout).
    Offline(String),
    /// The server answered with this error status.
    Http(u16),
    /// The caller asked to stop.
    Cancelled,
    /// The download could not be written to disk.
    Io(String),
}

impl std::fmt::Display for NetError {
    /// A short English reason, for the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::Offline(why) => write!(f, "offline: {why}"),
            NetError::Http(code) => write!(f, "http status {code}"),
            NetError::Cancelled => write!(f, "cancelled"),
            NetError::Io(why) => write!(f, "io: {why}"),
        }
    }
}

impl NetError {
    /// The reason in the user's language, for the About rows and the banner.
    pub fn user_text(&self) -> String {
        match self {
            NetError::Offline(_) => tr("You're offline"),
            NetError::Http(code) => fill(
                &tr("GitHub answered {code}"),
                &[("code", &code.to_string())],
            ),
            NetError::Cancelled => tr("Cancelled"),
            NetError::Io(_) => tr("Couldn't save the download"),
        }
    }
}

fn user_agent() -> String {
    format!("Vento/{}", env!("CARGO_PKG_VERSION"))
}

/// Maps a ureq failure to ours: an error status is `Http`, anything else
/// (connection, DNS, TLS, timeout) means the server was not reached.
fn map_err(e: ureq::Error) -> NetError {
    match e {
        ureq::Error::StatusCode(code) => NetError::Http(code),
        other => NetError::Offline(other.to_string()),
    }
}

/// Asks `url` for the latest release and returns the JSON body.
pub fn fetch_latest(url: &str) -> Result<String, NetError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("User-Agent", user_agent())
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(map_err)?;
    response.body_mut().read_to_string().map_err(map_err)
}

/// Downloads `url` to `to`, calling `progress(done, total)` after each chunk
/// (`total` is 0 when the server did not say). The bytes go to
/// `to.with_extension("part")`, which becomes `to` on success and is removed
/// on any error or cancel. `cancel` is checked between chunks.
pub fn download(
    url: &str,
    to: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), NetError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(NetError::Cancelled);
    }
    let part = to.with_extension("part");
    let result = fetch_to(url, &part, cancel, progress);
    match result {
        Ok(()) => fs::rename(&part, to).map_err(|e| {
            let _ = fs::remove_file(&part);
            NetError::Io(e.to_string())
        }),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn fetch_to(
    url: &str,
    part: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), NetError> {
    // No global timeout here: it would cut off a slow but healthy download.
    // Only the connection and the wait for the response headers are bounded.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(TIMEOUT))
        .timeout_recv_response(Some(TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("User-Agent", user_agent())
        .call()
        .map_err(map_err)?;
    let total = response
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let mut file = File::create(part).map_err(|e| NetError::Io(e.to_string()))?;
    let mut reader = response.body_mut().as_reader();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(NetError::Cancelled);
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| NetError::Offline(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| NetError::Io(e.to_string()))?;
        done += n as u64;
        progress(done, total);
    }
    file.flush().map_err(|e| NetError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Serves one request on a local port with `status` and `body`, and
    /// returns the URL.
    fn serve_once(status: u16, body: &[u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_vec();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        format!("http://127.0.0.1:{port}/")
    }

    #[test]
    fn fetch_returns_the_body() {
        let url = serve_once(200, b"{\"tag_name\":\"v1.0.0\"}");
        assert!(fetch_latest(&url).unwrap().contains("v1.0.0"));
    }

    #[test]
    fn fetch_maps_http_errors() {
        let url = serve_once(403, b"rate limited");
        assert!(matches!(fetch_latest(&url), Err(NetError::Http(403))));
    }

    #[test]
    fn download_writes_file_and_reports_progress() {
        let body: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
        let url = serve_once(200, &body);
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.zip");
        let mut last = (0, 0);
        let mut calls = 0;
        download(&url, &to, &AtomicBool::new(false), &mut |d, t| {
            last = (d, t);
            calls += 1;
        })
        .unwrap();
        assert_eq!(fs::read(&to).unwrap(), body);
        assert_eq!(last, (body.len() as u64, body.len() as u64));
        assert!(calls > 2, "progress came in pieces");
        assert!(!to.with_extension("part").exists());
    }

    #[test]
    fn cancelled_download_leaves_nothing() {
        let url = serve_once(200, &[7u8; 1024]);
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.zip");
        let r = download(&url, &to, &AtomicBool::new(true), &mut |_, _| {});
        assert_eq!(r, Err(NetError::Cancelled));
        assert!(!to.exists());
        assert!(!to.with_extension("part").exists());
    }

    #[test]
    fn cancel_midway_removes_the_part_file() {
        let url = serve_once(200, &vec![7u8; 300 * 1024]);
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.zip");
        let cancel = AtomicBool::new(false);
        let r = download(&url, &to, &cancel, &mut |d, _| {
            if d > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(r, Err(NetError::Cancelled));
        assert!(!to.exists());
        assert!(!to.with_extension("part").exists());
    }

    #[test]
    fn http_error_on_download_leaves_nothing() {
        let url = serve_once(404, b"nope");
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.zip");
        let r = download(&url, &to, &AtomicBool::new(false), &mut |_, _| {});
        assert_eq!(r, Err(NetError::Http(404)));
        assert!(!to.with_extension("part").exists());
    }

    #[test]
    fn unreachable_host_is_offline() {
        assert!(matches!(
            fetch_latest("http://127.0.0.1:9/"),
            Err(NetError::Offline(_))
        ));
    }
}
