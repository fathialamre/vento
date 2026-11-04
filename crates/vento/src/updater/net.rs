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

/// The most a whole download may take once it has started. ureq 3 has no
/// idle-read timeout, so without this cap a connection that goes silent would
/// block the read forever and Cancel could not interrupt it. Generous on
/// purpose: the archive is about 20 MB. The controller treats Cancel as
/// immediate in the UI and drops a late result from the thread.
const BODY_TIMEOUT: Duration = Duration::from_secs(600);

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
    /// The download could not be written to disk, or ended early.
    Io(String),
    /// The address is not https (and not a local test server).
    Insecure,
}

impl std::fmt::Display for NetError {
    /// A short English reason, for the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::Offline(why) => write!(f, "offline: {why}"),
            NetError::Http(code) => write!(f, "http status {code}"),
            NetError::Cancelled => write!(f, "cancelled"),
            NetError::Io(why) => write!(f, "io: {why}"),
            NetError::Insecure => write!(f, "refused a non-https address"),
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
            NetError::Insecure => tr("The update address isn't secure"),
        }
    }
}

/// Whether `url` may be requested: https, or http to this machine (the tests
/// and `VENTO_UPDATE_API` hand checks).
fn allowed(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    // Host part: after any user info, before the port, path, query or fragment.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if host_port.starts_with('[') {
        host_port
            .split_once(']')
            .map_or("", |(h, _)| &h[1..])
            .to_string()
    } else {
        host_port.split(':').next().unwrap_or("").to_string()
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// Whether `url` is https, so redirects away from https can be refused.
fn url_is_https(url: &str) -> bool {
    url.get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
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

/// The settings every request starts from: on Windows, Windows' own TLS
/// and certificate store.
fn config() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    let builder = ureq::Agent::config_builder();
    #[cfg(windows)]
    let builder = {
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

        builder.tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
    };
    builder
}

/// Asks `url` for the latest release and returns the JSON body.
pub fn fetch_latest(url: &str) -> Result<String, NetError> {
    if !allowed(url) {
        return Err(NetError::Insecure);
    }
    let agent: ureq::Agent = config()
        .timeout_global(Some(TIMEOUT))
        .https_only(url_is_https(url))
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
    download_within(url, to, cancel, progress, BODY_TIMEOUT)
}

/// [`download`] with the whole-body time limit given (the tests shorten it).
fn download_within(
    url: &str,
    to: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
    body_timeout: Duration,
) -> Result<(), NetError> {
    if !allowed(url) {
        return Err(NetError::Insecure);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(NetError::Cancelled);
    }
    let part = to.with_extension("part");
    let result = fetch_to(url, &part, cancel, progress, body_timeout);
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
    body_timeout: Duration,
) -> Result<(), NetError> {
    // No short global timeout here: it would cut off a slow but healthy
    // download. The connection and the response headers get 15 s; the body
    // gets `body_timeout` in all.
    let agent: ureq::Agent = config()
        .timeout_connect(Some(TIMEOUT))
        .timeout_recv_response(Some(TIMEOUT))
        .timeout_recv_body(Some(body_timeout))
        .https_only(url_is_https(url))
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
        let n = reader.read(&mut buf).map_err(|e| {
            let text = e.to_string();
            if e.kind() == std::io::ErrorKind::TimedOut || text.contains("timeout") {
                NetError::Offline(format!("download timed out: {text}"))
            } else {
                NetError::Offline(text)
            }
        })?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| NetError::Io(e.to_string()))?;
        done += n as u64;
        progress(done, total);
    }
    if total > 0 && done != total {
        return Err(NetError::Io("download ended early".into()));
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
        serve(status, body.len(), body, None).0
    }

    /// Like [`serve_once`], but the header may promise `declared` bytes (more
    /// than `body` holds), the connection may then stay silent for `stall`,
    /// and the request head comes back through the receiver.
    fn serve(
        status: u16,
        declared: usize,
        body: &[u8],
        stall: Option<Duration>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_vec();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            let _ = tx.send(request);
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                declared
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            if let Some(stall) = stall {
                let _ = stream.flush();
                std::thread::sleep(stall);
            }
        });
        (format!("http://127.0.0.1:{port}/"), rx)
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
    fn fetch_sends_the_required_headers() {
        let (url, head) = serve(200, 2, b"{}", None);
        fetch_latest(&url).unwrap();
        let head = head
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .to_lowercase();
        let ua = format!("user-agent: vento/{}", env!("CARGO_PKG_VERSION"));
        assert!(head.contains(&ua), "{head}");
        assert!(
            head.contains("accept: application/vnd.github+json"),
            "{head}"
        );
    }

    #[test]
    fn http_to_a_remote_host_is_refused() {
        assert_eq!(
            fetch_latest("http://example.com/x"),
            Err(NetError::Insecure)
        );
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.zip");
        let r = download(
            "http://example.com/x",
            &to,
            &AtomicBool::new(false),
            &mut |_, _| {},
        );
        assert_eq!(r, Err(NetError::Insecure));
        assert!(!to.exists());
        assert!(!to.with_extension("part").exists());
    }

    #[test]
    fn https_url_is_allowed_by_the_check() {
        assert!(allowed("https://api.github.com/repos/x"));
        assert!(allowed("http://127.0.0.1:8080/x"));
        assert!(allowed("http://localhost/x"));
        assert!(allowed("http://[::1]:9/x"));
        assert!(!allowed("http://example.com/x"));
        assert!(!allowed("http://127.0.0.1.example.com/x"));
        assert!(!allowed("http://localhost@example.com/x"));
        assert!(!allowed("ftp://127.0.0.1/x"));
        assert!(!allowed("file:///etc/passwd"));
        assert!(!allowed("127.0.0.1/x"));
    }

    #[test]
    fn a_short_download_is_an_error() {
        let (url, _) = serve(200, 1000, &[1u8; 100], None);
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.zip");
        let r = download(&url, &to, &AtomicBool::new(false), &mut |_, _| {});
        // ureq itself notices the short body ("Peer disconnected"); the
        // length check in `fetch_to` is the net under it.
        assert!(
            matches!(r, Err(NetError::Io(_)) | Err(NetError::Offline(_))),
            "{r:?}"
        );
        assert!(!to.exists());
        assert!(!to.with_extension("part").exists());
    }

    #[test]
    fn a_stalled_body_times_out() {
        let (url, _) = serve(200, 1000, &[1u8; 100], Some(Duration::from_secs(5)));
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.zip");
        let started = std::time::Instant::now();
        let r = download_within(
            &url,
            &to,
            &AtomicBool::new(false),
            &mut |_, _| {},
            Duration::from_millis(400),
        );
        assert!(started.elapsed() < Duration::from_secs(4));
        match r {
            Err(NetError::Offline(why)) => assert!(why.contains("timed out"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(!to.exists());
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
