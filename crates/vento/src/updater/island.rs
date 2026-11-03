//! The update in the virtual island: a new version, said once; its download,
//! as a job; and the update ready to restart, which waits to be seen.

use std::sync::Arc;

use vento_core::archive::Progress;
use vento_core::update::state::UpdateState;
use vento_core::update::version::Version;

/// What a download's job counts to: its share done, in thousandths.
const PROGRESS_TOTAL: u64 = 1000;

/// What the island is to say as the update moves on.
#[derive(Clone, Debug, PartialEq)]
pub enum IslandStep {
    /// This version is out.
    Out(Version),
    /// The download of this version began.
    DownloadStarted(Version),
    /// The download has got this far, 0 to 1.
    Downloaded(f32),
    /// The download is over: done, cancelled or failed.
    DownloadEnded,
    /// The update is downloaded: a restart finishes it.
    Ready,
}

/// Follows the update's state for the island: a version is said to be out
/// once, and to be ready once.
#[derive(Default)]
pub struct UpdateWatch {
    out: Option<Version>,
    downloading: bool,
    ready: Option<Version>,
}

impl UpdateWatch {
    /// The update's state is now `state`: what the island is to say, in
    /// order.
    pub fn changed(&mut self, state: &UpdateState) -> Vec<IslandStep> {
        let mut steps = Vec::new();
        let downloading = matches!(state, UpdateState::Downloading { .. });
        if self.downloading && !downloading {
            steps.push(IslandStep::DownloadEnded);
        }
        match state {
            UpdateState::Available(update) if self.out != Some(update.version) => {
                self.out = Some(update.version);
                steps.push(IslandStep::Out(update.version));
            }
            UpdateState::Downloading { update, progress } => {
                if !self.downloading {
                    steps.push(IslandStep::DownloadStarted(update.version));
                }
                steps.push(IslandStep::Downloaded(*progress));
            }
            UpdateState::Ready(update) if self.ready != Some(update.version) => {
                self.ready = Some(update.version);
                steps.push(IslandStep::Ready);
            }
            _ => {}
        }
        self.downloading = downloading;
        steps
    }
}

/// The progress of a download's job, with nothing done yet.
pub fn download_progress() -> Arc<Progress> {
    let progress = Progress::new();
    progress.start(PROGRESS_TOTAL);
    progress
}

/// Sets `progress` (from [`download_progress`]) to `fraction` done.
pub fn set_downloaded(progress: &Progress, fraction: f32) {
    progress.start(PROGRESS_TOTAL);
    progress.add_done((f64::from(fraction.clamp(0.0, 1.0)) * PROGRESS_TOTAL as f64).round() as u64);
}

#[cfg(test)]
mod tests {
    use vento_core::update::release::{Asset, Update};

    use super::*;

    fn update(ver: &str) -> Update {
        let asset = |name: &str| Asset {
            name: name.to_owned(),
            url: format!("https://example.com/{name}"),
            size: 10,
        };
        Update {
            version: Version::parse(ver).unwrap(),
            notes: String::new(),
            archive: asset("vento.zip"),
            sums: asset("SHA256SUMS.txt"),
        }
    }

    fn downloading(ver: &str, progress: f32) -> UpdateState {
        UpdateState::Downloading {
            update: update(ver),
            progress,
        }
    }

    #[test]
    fn a_version_is_said_to_be_out_once() {
        let mut watch = UpdateWatch::default();
        let v = Version::parse("0.4.0").unwrap();
        let out = UpdateState::Available(update("0.4.0"));
        assert_eq!(watch.changed(&out), [IslandStep::Out(v)]);
        assert!(watch.changed(&UpdateState::Checking).is_empty());
        assert!(watch.changed(&out).is_empty());
        // A newer one is news again.
        let newer = UpdateState::Available(update("0.5.0"));
        assert_eq!(
            watch.changed(&newer),
            [IslandStep::Out(Version::parse("0.5.0").unwrap())]
        );
    }

    #[test]
    fn a_download_is_a_job_that_ends() {
        let mut watch = UpdateWatch::default();
        let v = Version::parse("0.4.0").unwrap();
        watch.changed(&UpdateState::Available(update("0.4.0")));
        assert_eq!(
            watch.changed(&downloading("0.4.0", 0.0)),
            [IslandStep::DownloadStarted(v), IslandStep::Downloaded(0.0)]
        );
        assert_eq!(
            watch.changed(&downloading("0.4.0", 0.5)),
            [IslandStep::Downloaded(0.5)]
        );
        // Cancelled: back to the update on offer, already said.
        assert_eq!(
            watch.changed(&UpdateState::Available(update("0.4.0"))),
            [IslandStep::DownloadEnded]
        );
        watch.changed(&downloading("0.4.0", 0.9));
        assert_eq!(
            watch.changed(&UpdateState::Ready(update("0.4.0"))),
            [IslandStep::DownloadEnded, IslandStep::Ready]
        );
        // A restart that was not agreed to leaves it ready, said once.
        assert!(
            watch
                .changed(&UpdateState::Restarting(update("0.4.0")))
                .is_empty()
        );
        assert!(
            watch
                .changed(&UpdateState::Ready(update("0.4.0")))
                .is_empty()
        );
    }

    #[test]
    fn the_jobs_progress_follows_the_download() {
        let progress = download_progress();
        assert_eq!(
            (progress.snapshot().done, progress.snapshot().total),
            (0, 1000)
        );
        set_downloaded(&progress, 0.42);
        assert_eq!(progress.snapshot().done, 420);
        set_downloaded(&progress, 1.5);
        assert_eq!(progress.snapshot().done, 1000);
    }
}
