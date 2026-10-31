//! The update file (`update.json`), the "what's new" rule, and the state
//! machine behind the banner, the About rows and the tray dot.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::release::Update;
use super::version::Version;

/// Seconds in a day: how often Vento checks for a new version.
pub const DAY: i64 = 86_400;

/// Whether a daily check is due: never checked, a day has passed since
/// `last_check`, or the clock went back past it (Unix seconds).
pub fn check_due(last_check: Option<i64>, now: i64) -> bool {
    match last_check {
        None => true,
        Some(at) => now < at || now - at >= DAY,
    }
}

/// An update that failed to install and was rolled back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedUpdate {
    /// The version that failed.
    pub version: Version,
    /// A short reason for the About row.
    pub reason: String,
}

/// What Vento remembers between runs about updating, kept in
/// `<data_dir>/update.json` (never in the user's settings file).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateFile {
    /// The version that ran last, for What's new.
    pub last_run_version: Option<Version>,
    /// When the last check finished (Unix seconds).
    pub last_check: Option<i64>,
    /// The update that failed and was rolled back, if any.
    pub failed: Option<FailedUpdate>,
    /// Whether Screen Recording was granted before, so the new copy can
    /// tell when an update cost the user the grant.
    pub had_screen_access: bool,
}

/// On-disk shape: versions as strings; unknown fields are ignored and a
/// version that does not parse reads as missing.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct RawUpdateFile {
    last_run_version: Option<String>,
    last_check: Option<i64>,
    failed: Option<RawFailed>,
    had_screen_access: bool,
}

#[derive(Serialize, Deserialize)]
struct RawFailed {
    version: String,
    reason: String,
}

impl From<&UpdateFile> for RawUpdateFile {
    fn from(f: &UpdateFile) -> Self {
        RawUpdateFile {
            last_run_version: f.last_run_version.map(|v| v.to_string()),
            last_check: f.last_check,
            failed: f.failed.as_ref().map(|x| RawFailed {
                version: x.version.to_string(),
                reason: x.reason.clone(),
            }),
            had_screen_access: f.had_screen_access,
        }
    }
}

impl From<RawUpdateFile> for UpdateFile {
    fn from(raw: RawUpdateFile) -> Self {
        UpdateFile {
            last_run_version: raw.last_run_version.as_deref().and_then(Version::parse),
            last_check: raw.last_check,
            failed: raw.failed.and_then(|x| {
                Some(FailedUpdate {
                    version: Version::parse(&x.version)?,
                    reason: x.reason,
                })
            }),
            had_screen_access: raw.had_screen_access,
        }
    }
}

impl UpdateFile {
    /// Where the file lives inside Vento's data folder.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("update.json")
    }

    /// Reads the file; a missing or broken one gives the default.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RawUpdateFile>(&bytes).ok())
            .map(UpdateFile::from)
            .unwrap_or_default()
    }

    /// Writes the file to a temp file in the same folder, then renames it
    /// over `path`, so a crash never leaves half a file. Creates the folder
    /// if it is missing.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
        if let Some(dir) = dir {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(&RawUpdateFile::from(self))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }
}

/// Whether the What's new window opens at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhatsNew {
    /// Open it with the changes after this version (`None`: all of them).
    Show { after: Option<Version> },
    /// Do not open it.
    Skip,
}

/// What to do at start: a fresh install sees the Welcome setup instead; an
/// older or unknown last run sees what changed; the same or a newer last
/// run (a downgrade) sees nothing.
pub fn whats_new_on_start(
    last_run: Option<Version>,
    running: Version,
    fresh_install: bool,
) -> WhatsNew {
    if fresh_install {
        return WhatsNew::Skip;
    }
    match last_run {
        Some(last) if last >= running => WhatsNew::Skip,
        after => WhatsNew::Show { after },
    }
}

/// Where the update flow stands.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    /// Nothing newer; `checked_at` is when we last looked (Unix seconds).
    UpToDate { checked_at: Option<i64> },
    /// A check is running.
    Checking,
    /// The last check could not reach GitHub or read its answer.
    CheckFailed { reason: String },
    /// A newer release is out.
    Available(Update),
    /// Downloading and preparing it; `progress` is 0.0 to 1.0.
    Downloading { update: Update, progress: f32 },
    /// Downloaded and verified; waiting for the user to restart.
    Ready(Update),
    /// Quitting so the new copy can start.
    Restarting(Version),
    /// The install failed; `update` is kept (when known) for Try again.
    Failed {
        update: Option<Update>,
        version: Version,
        reason: String,
    },
}

/// Something that happened to the update flow.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateEvent {
    CheckStarted,
    Checked {
        found: Option<Update>,
        at: i64,
    },
    CheckFailed(String),
    DownloadStarted,
    Progress(f32),
    /// The user cancelled the download.
    Cancelled,
    /// Something kept the app from quitting (a Quit that was cancelled).
    QuitCancelled,
    Restarting,
    Failed(String),
}

/// The next state after `event`; a pair that is not listed leaves `state`.
pub fn reduce(state: &UpdateState, event: UpdateEvent) -> UpdateState {
    use UpdateEvent as E;
    use UpdateState as S;
    match (state, event) {
        (S::UpToDate { .. } | S::CheckFailed { .. } | S::Failed { .. }, E::CheckStarted) => {
            S::Checking
        }
        (S::Checking, E::Checked { found: None, at }) => S::UpToDate {
            checked_at: Some(at),
        },
        (S::Checking, E::Checked { found: Some(u), .. }) => S::Available(u),
        (S::Checking, E::CheckFailed(reason)) => S::CheckFailed { reason },
        (
            S::Available(u)
            | S::Failed {
                update: Some(u), ..
            },
            E::DownloadStarted,
        ) => S::Downloading {
            update: u.clone(),
            progress: 0.0,
        },
        (S::Downloading { update, .. }, E::Progress(p)) => S::Downloading {
            update: update.clone(),
            progress: if p.is_nan() { 0.0 } else { p.clamp(0.0, 1.0) },
        },
        (S::Downloading { update, .. }, E::Cancelled) => S::Available(update.clone()),
        (S::Downloading { update, .. } | S::Ready(update), E::QuitCancelled) => {
            S::Ready(update.clone())
        }
        (S::Downloading { update, .. } | S::Ready(update), E::Restarting) => {
            S::Restarting(update.version)
        }
        (S::Downloading { update, .. } | S::Ready(update), E::Failed(reason)) => S::Failed {
            update: Some(update.clone()),
            version: update.version,
            reason,
        },
        (S::Restarting(version), E::Failed(reason)) => S::Failed {
            update: None,
            version: *version,
            reason,
        },
        _ => state.clone(),
    }
}

impl UpdateState {
    /// Whether the banner shows.
    pub fn shows_banner(&self) -> bool {
        matches!(
            self,
            UpdateState::Available(_)
                | UpdateState::Downloading { .. }
                | UpdateState::Ready(_)
                | UpdateState::Restarting(_)
                | UpdateState::Failed { .. }
        )
    }

    /// Whether the tray icon carries the update dot.
    pub fn shows_dot(&self) -> bool {
        self.shows_banner()
    }

    /// Whether something is running that a second request must not start.
    pub fn busy(&self) -> bool {
        matches!(
            self,
            UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Restarting(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::release::{Asset, Update};
    use crate::update::version::Version;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_owned(),
            url: format!("https://example.com/{name}"),
            size: 10,
        }
    }

    fn update(ver: &str) -> Update {
        Update {
            version: v(ver),
            notes: "notes".to_owned(),
            archive: asset("vento.zip"),
            sums: asset("SHA256SUMS.txt"),
        }
    }

    #[test]
    fn check_is_due_after_a_day() {
        assert!(check_due(None, 0));
        assert!(!check_due(Some(1000), 1000 + DAY - 1));
        assert!(check_due(Some(1000), 1000 + DAY));
    }

    #[test]
    fn check_is_due_when_the_clock_went_back() {
        assert!(check_due(Some(1000), 10));
    }

    #[test]
    fn whats_new_rules() {
        assert_eq!(whats_new_on_start(None, v("0.3.0"), true), WhatsNew::Skip);
        assert_eq!(
            whats_new_on_start(None, v("0.3.0"), false),
            WhatsNew::Show { after: None }
        );
        assert_eq!(
            whats_new_on_start(Some(v("0.3.0")), v("0.5.0"), false),
            WhatsNew::Show {
                after: Some(v("0.3.0"))
            }
        );
        assert_eq!(
            whats_new_on_start(Some(v("0.5.0")), v("0.5.0"), false),
            WhatsNew::Skip
        );
        assert_eq!(
            whats_new_on_start(Some(v("0.6.0")), v("0.5.0"), false),
            WhatsNew::Skip
        );
    }

    #[test]
    fn file_path_is_update_json() {
        assert_eq!(
            UpdateFile::path(Path::new("/data")),
            PathBuf::from("/data/update.json")
        );
    }

    #[test]
    fn file_round_trips_and_broken_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("update.json");
        let f = UpdateFile {
            last_run_version: Some(v("0.3.0")),
            last_check: Some(1234),
            failed: Some(FailedUpdate {
                version: v("0.4.0"),
                reason: "no space".to_owned(),
            }),
            had_screen_access: true,
        };
        f.save(&p).unwrap();
        assert_eq!(UpdateFile::load(&p), f);
        std::fs::write(&p, "{").unwrap();
        assert_eq!(UpdateFile::load(&p), UpdateFile::default());
        assert_eq!(
            UpdateFile::load(&dir.path().join("missing.json")),
            UpdateFile::default()
        );
    }

    #[test]
    fn unknown_fields_and_bad_versions_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("update.json");
        std::fs::write(
            &p,
            r#"{"last_run_version":"0.3.0","future":1,"last_check":5}"#,
        )
        .unwrap();
        let f = UpdateFile::load(&p);
        assert_eq!(f.last_run_version, Some(v("0.3.0")));
        assert_eq!(f.last_check, Some(5));
        std::fs::write(&p, r#"{"last_run_version":"x"}"#).unwrap();
        assert_eq!(UpdateFile::load(&p).last_run_version, None);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let p = UpdateFile::path(dir.path());
        UpdateFile::default().save(&p).unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("update.json")]);
    }

    #[test]
    fn check_failure_shows_no_banner() {
        let s = reduce(
            &UpdateState::Checking,
            UpdateEvent::CheckFailed("offline".into()),
        );
        assert!(!s.shows_banner());
        assert!(!s.shows_dot());
    }

    #[test]
    fn check_finds_nothing_or_something() {
        let s = reduce(
            &UpdateState::Checking,
            UpdateEvent::Checked {
                found: None,
                at: 50,
            },
        );
        assert_eq!(
            s,
            UpdateState::UpToDate {
                checked_at: Some(50)
            }
        );
        let s = reduce(
            &UpdateState::Checking,
            UpdateEvent::Checked {
                found: Some(update("0.4.0")),
                at: 50,
            },
        );
        assert_eq!(s, UpdateState::Available(update("0.4.0")));
        assert!(s.shows_banner() && s.shows_dot());
    }

    #[test]
    fn check_started_from_idle_states() {
        for from in [
            UpdateState::UpToDate { checked_at: None },
            UpdateState::CheckFailed { reason: "x".into() },
            UpdateState::Failed {
                update: None,
                version: v("0.4.0"),
                reason: "x".into(),
            },
        ] {
            assert_eq!(
                reduce(&from, UpdateEvent::CheckStarted),
                UpdateState::Checking
            );
        }
        for same in [
            UpdateState::Available(update("0.4.0")),
            UpdateState::Ready(update("0.4.0")),
        ] {
            assert_eq!(reduce(&same, UpdateEvent::CheckStarted), same);
        }
    }

    #[test]
    fn second_download_and_check_are_ignored_while_downloading() {
        let d = UpdateState::Downloading {
            update: update("0.4.0"),
            progress: 0.5,
        };
        assert_eq!(reduce(&d, UpdateEvent::DownloadStarted), d);
        assert_eq!(reduce(&d, UpdateEvent::CheckStarted), d);
    }

    #[test]
    fn download_starts_from_available() {
        let s = reduce(
            &UpdateState::Available(update("0.4.0")),
            UpdateEvent::DownloadStarted,
        );
        assert_eq!(
            s,
            UpdateState::Downloading {
                update: update("0.4.0"),
                progress: 0.0
            }
        );
    }

    #[test]
    fn cancel_returns_to_available() {
        let d = UpdateState::Downloading {
            update: update("0.4.0"),
            progress: 0.5,
        };
        assert_eq!(
            reduce(&d, UpdateEvent::Cancelled),
            UpdateState::Available(update("0.4.0"))
        );
    }

    #[test]
    fn cancelled_quit_is_ready() {
        let d = UpdateState::Downloading {
            update: update("0.4.0"),
            progress: 1.0,
        };
        assert_eq!(
            reduce(&d, UpdateEvent::QuitCancelled),
            UpdateState::Ready(update("0.4.0"))
        );
        let r = UpdateState::Ready(update("0.4.0"));
        assert_eq!(reduce(&r, UpdateEvent::QuitCancelled), r);
        assert_eq!(reduce(&r, UpdateEvent::DownloadStarted), r);
    }

    #[test]
    fn progress_is_clamped() {
        let d = UpdateState::Downloading {
            update: update("0.4.0"),
            progress: 0.0,
        };
        for (p, want) in [(0.4, 0.4), (-1.0, 0.0), (7.0, 1.0)] {
            assert_eq!(
                reduce(&d, UpdateEvent::Progress(p)),
                UpdateState::Downloading {
                    update: update("0.4.0"),
                    progress: want
                }
            );
        }
        let a = UpdateState::Available(update("0.4.0"));
        assert_eq!(reduce(&a, UpdateEvent::Progress(0.5)), a);
    }

    #[test]
    fn restarting_and_its_failure() {
        let r = UpdateState::Ready(update("0.4.0"));
        let s = reduce(&r, UpdateEvent::Restarting);
        assert_eq!(s, UpdateState::Restarting(v("0.4.0")));
        assert!(s.busy() && s.shows_banner());
        assert_eq!(
            reduce(&s, UpdateEvent::Failed("boom".into())),
            UpdateState::Failed {
                update: None,
                version: v("0.4.0"),
                reason: "boom".into()
            }
        );
    }

    #[test]
    fn failure_keeps_the_update_for_try_again() {
        let d = UpdateState::Downloading {
            update: update("0.4.0"),
            progress: 0.3,
        };
        let f = reduce(&d, UpdateEvent::Failed("net".into()));
        assert_eq!(
            f,
            UpdateState::Failed {
                update: Some(update("0.4.0")),
                version: v("0.4.0"),
                reason: "net".into()
            }
        );
        assert_eq!(
            reduce(&f, UpdateEvent::DownloadStarted),
            UpdateState::Downloading {
                update: update("0.4.0"),
                progress: 0.0
            }
        );
        let r = reduce(
            &UpdateState::Ready(update("0.4.0")),
            UpdateEvent::Failed("x".into()),
        );
        assert!(matches!(
            r,
            UpdateState::Failed {
                update: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn unlisted_pairs_leave_the_state() {
        let u = UpdateState::UpToDate { checked_at: None };
        assert_eq!(reduce(&u, UpdateEvent::DownloadStarted), u);
        assert_eq!(reduce(&u, UpdateEvent::Failed("x".into())), u);
        let c = UpdateState::Checking;
        assert_eq!(reduce(&c, UpdateEvent::Cancelled), c);
    }

    #[test]
    fn banner_dot_and_busy_flags() {
        let a = update("0.4.0");
        let table = [
            (UpdateState::UpToDate { checked_at: None }, false, false),
            (UpdateState::Checking, false, true),
            (
                UpdateState::CheckFailed { reason: "x".into() },
                false,
                false,
            ),
            (UpdateState::Available(a.clone()), true, false),
            (
                UpdateState::Downloading {
                    update: a.clone(),
                    progress: 0.1,
                },
                true,
                true,
            ),
            (UpdateState::Ready(a.clone()), true, false),
            (UpdateState::Restarting(a.version), true, true),
            (
                UpdateState::Failed {
                    update: Some(a.clone()),
                    version: a.version,
                    reason: "x".into(),
                },
                true,
                false,
            ),
        ];
        for (s, shown, busy) in table {
            assert_eq!(s.shows_banner(), shown, "{s:?}");
            assert_eq!(s.shows_dot(), shown, "{s:?}");
            assert_eq!(s.busy(), busy, "{s:?}");
        }
    }
}
