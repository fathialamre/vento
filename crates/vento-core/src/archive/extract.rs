//! Extracting several archives in a row, asking for passwords as needed.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zeroize::Zeroizing;

use super::detect::{Format, detect};
use super::entries::extract_one;
use super::job::{ArchiveError, Made, Progress};

/// A password held in memory that is zeroed when dropped.
type Password = Zeroizing<Vec<u8>>;

/// Why a password is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordAsk {
    /// The archive is encrypted and nothing on hand opens it.
    Needed,
    /// The password just given does not open the archive.
    Wrong,
}

/// What an [`extract_batch`] did.
#[derive(Debug, Default)]
pub struct BatchOutcome {
    /// The archives extracted, in order.
    pub made: Vec<Made>,
    /// The archives that failed, with why.
    pub failed: Vec<(PathBuf, ArchiveError)>,
    /// The archives left alone because no password was given.
    pub skipped: Vec<PathBuf>,
    /// Whether the batch was cancelled; the archives after the one in
    /// progress were not tried.
    pub cancelled: bool,
}

/// Extracts each of `archives` beside itself, one after the other.
///
/// An encrypted archive is first tried with the password that last worked in
/// this batch, so a set of archives sharing a password asks once. When that
/// is not enough, `ask` is called with [`PasswordAsk::Needed`], or with
/// [`PasswordAsk::Wrong`] after a password the user gave was refused, and the
/// archive is retried with the answer. `ask` returning `None` leaves the
/// archive in `skipped` and the batch goes on. A refusal of the remembered
/// password counts as [`PasswordAsk::Needed`]: the user has not yet been
/// asked about this archive. `ask` is never called once `progress` is
/// cancelled. The remembered password is zeroed when the batch ends.
///
/// `mark` is called with the archive and each item created for it, as in
/// [`extract_one`]. Cancelling stops the batch at once and keeps what was
/// finished; any other error goes to `failed` and the batch continues. An
/// archive that no longer detects as one fails as unsupported.
/// `progress` is given the name of each archive as it starts.
pub fn extract_batch(
    archives: &[PathBuf],
    progress: &Arc<Progress>,
    mark: &dyn Fn(&Path, &Path) -> io::Result<()>,
    ask: &mut dyn FnMut(&Path, PasswordAsk) -> Option<Password>,
) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();
    let mut remembered: Option<Password> = None;
    for archive in archives {
        if progress.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        progress.set_current(&archive.file_name().unwrap_or_default().to_string_lossy());
        let step = match detect(archive) {
            Some(format) => {
                let mark = |entry: &Path| mark(archive, entry);
                let job = Job {
                    archive,
                    format,
                    progress,
                    mark: &mark,
                };
                job.run(&mut remembered, ask)
            }
            None => Step::Failed(ArchiveError::Unsupported(
                "not an archive any more".to_owned(),
            )),
        };
        match step {
            Step::Made(made) => outcome.made.push(made),
            Step::Failed(error) => {
                log::warn!("could not extract {}: {error}", archive.display());
                outcome.failed.push((archive.clone(), error));
            }
            Step::Skipped => outcome.skipped.push(archive.clone()),
            Step::Cancelled => {
                outcome.cancelled = true;
                break;
            }
        }
    }
    outcome
}

/// How one archive of a batch ended.
enum Step {
    Made(Made),
    Failed(ArchiveError),
    Skipped,
    Cancelled,
}

/// One archive of a batch, with what extracting it needs.
struct Job<'a> {
    archive: &'a Path,
    format: Format,
    progress: &'a Arc<Progress>,
    mark: &'a dyn Fn(&Path) -> io::Result<()>,
}

impl Job<'_> {
    fn try_with(&self, password: Option<&[u8]>) -> Result<Made, ArchiveError> {
        extract_one(
            self.archive,
            self.format,
            password,
            self.progress,
            self.mark,
        )
    }

    /// Extracts the archive: with no password, then with `remembered`, then
    /// with whatever `ask` supplies until it opens or `ask` gives up. A
    /// password that opens the archive replaces `remembered`.
    fn run(
        &self,
        remembered: &mut Option<Password>,
        ask: &mut dyn FnMut(&Path, PasswordAsk) -> Option<Password>,
    ) -> Step {
        let mut result = self.try_with(None);
        let mut tried_remembered = false;
        let mut typed: Option<Password> = None;
        loop {
            let why = match result {
                Ok(made) => {
                    if typed.is_some() {
                        *remembered = typed;
                    }
                    return Step::Made(made);
                }
                Err(ArchiveError::Cancelled) => return Step::Cancelled,
                Err(ArchiveError::NeedPassword) if !tried_remembered && remembered.is_some() => {
                    tried_remembered = true;
                    result = self.try_with(remembered.as_ref().map(|p| p.as_slice()));
                    continue;
                }
                Err(ArchiveError::NeedPassword) => PasswordAsk::Needed,
                // Nothing was typed for this archive yet when the remembered
                // password is refused, so it is not a wrong guess.
                Err(ArchiveError::WrongPassword) if typed.is_none() && tried_remembered => {
                    PasswordAsk::Needed
                }
                Err(ArchiveError::WrongPassword) => PasswordAsk::Wrong,
                Err(error) => return Step::Failed(error),
            };
            if self.progress.is_cancelled() {
                return Step::Cancelled;
            }
            let Some(password) = ask(self.archive, why) else {
                return Step::Skipped;
            };
            result = self.try_with(Some(password.as_slice()));
            typed = Some(password);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::test_support::{
        ZipEntry as E, listing, make_encrypted, make_encrypted_with, make_zip,
    };
    use std::cell::RefCell;
    use std::fs;

    fn password(text: &str) -> Option<Password> {
        Some(Zeroizing::new(text.as_bytes().to_vec()))
    }

    fn no_mark(_: &Path, _: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Makes plain zips named `names` in `dir`, each holding `<name>.txt`.
    fn plain_zips(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| {
                let path = dir.join(format!("{name}.zip"));
                let file = format!("{name}.txt");
                make_zip(&path, &[E::File(&file, name.as_bytes())]);
                path
            })
            .collect()
    }

    #[test]
    fn batch_extracts_each_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let archives = plain_zips(dir.path(), &["a", "b", "c"]);
        let progress = Progress::new();
        let mut ask = |_: &Path, _: PasswordAsk| panic!("nothing is encrypted");

        let outcome = extract_batch(&archives, &progress, &no_mark, &mut ask);

        assert!(outcome.failed.is_empty() && outcome.skipped.is_empty() && !outcome.cancelled);
        let sources: Vec<&PathBuf> = outcome.made.iter().map(|m| &m.source).collect();
        assert_eq!(sources, archives.iter().collect::<Vec<_>>());
        let outputs: Vec<&PathBuf> = outcome.made.iter().map(|m| &m.output).collect();
        assert_eq!(
            outputs,
            [
                dir.path().join("a.txt"),
                dir.path().join("b.txt"),
                dir.path().join("c.txt")
            ]
            .iter()
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mark_is_given_the_archive_and_each_item() {
        let dir = tempfile::tempdir().unwrap();
        let archives = plain_zips(dir.path(), &["a", "b"]);
        let seen = RefCell::new(Vec::new());
        let mark = |archive: &Path, entry: &Path| {
            seen.borrow_mut()
                .push((archive.to_path_buf(), entry.to_path_buf()));
            Ok(())
        };
        let mut ask = |_: &Path, _: PasswordAsk| None;

        extract_batch(&archives, &Progress::new(), &mark, &mut ask);

        let seen = seen.into_inner();
        assert!(!seen.is_empty());
        assert!(seen.iter().any(|(archive, _)| *archive == archives[0]));
        assert!(seen.iter().any(|(archive, _)| *archive == archives[1]));
        assert!(
            seen.iter()
                .all(|(archive, entry)| entry.starts_with(archive.parent().unwrap()))
        );
    }

    #[test]
    fn last_good_password_is_tried_before_asking() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("One.zip");
        let second = dir.path().join("Two.zip");
        make_encrypted(&first);
        make_encrypted(&second);
        let asked = RefCell::new(Vec::new());
        let mut ask = |archive: &Path, why: PasswordAsk| {
            asked.borrow_mut().push((archive.to_path_buf(), why));
            password("hunter2")
        };

        let outcome = extract_batch(
            &[first.clone(), second],
            &Progress::new(),
            &no_mark,
            &mut ask,
        );

        assert_eq!(outcome.made.len(), 2);
        assert_eq!(asked.into_inner(), [(first, PasswordAsk::Needed)]);
    }

    #[test]
    fn wrong_password_asks_again_with_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Secret.zip");
        make_encrypted(&archive);
        let mut answers = vec![password("hunter2"), password("nope")];
        let asked = RefCell::new(Vec::new());
        let mut ask = |_: &Path, why: PasswordAsk| {
            asked.borrow_mut().push(why);
            answers.pop().unwrap()
        };

        let outcome = extract_batch(&[archive], &Progress::new(), &no_mark, &mut ask);

        assert_eq!(outcome.made.len(), 1);
        assert_eq!(
            asked.into_inner(),
            [PasswordAsk::Needed, PasswordAsk::Wrong]
        );
    }

    #[test]
    fn refused_remembered_password_asks_needed() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("One.zip");
        let second = dir.path().join("Two.zip");
        make_encrypted_with(&first, "hunter2");
        make_encrypted_with(&second, "other");
        let mut answers = vec![password("hunter2"), password("other")];
        let asked = RefCell::new(Vec::new());
        let mut ask = |_: &Path, why: PasswordAsk| {
            asked.borrow_mut().push(why);
            answers.remove(0)
        };

        let outcome = extract_batch(&[first, second], &Progress::new(), &no_mark, &mut ask);

        assert_eq!(outcome.made.len(), 2);
        assert_eq!(
            asked.into_inner(),
            [PasswordAsk::Needed, PasswordAsk::Needed]
        );
    }

    #[test]
    fn refused_password_is_not_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("One.zip");
        let second = dir.path().join("Two.zip");
        make_encrypted(&first);
        make_encrypted(&second);
        let mut answers = vec![password("nope"), None, None];
        let asked = RefCell::new(Vec::new());
        let mut ask = |_: &Path, why: PasswordAsk| {
            asked.borrow_mut().push(why);
            answers.remove(0)
        };

        let outcome = extract_batch(
            &[first.clone(), second.clone()],
            &Progress::new(),
            &no_mark,
            &mut ask,
        );

        assert!(outcome.made.is_empty());
        assert_eq!(outcome.skipped, [first, second]);
        assert_eq!(
            asked.into_inner(),
            [PasswordAsk::Needed, PasswordAsk::Wrong, PasswordAsk::Needed]
        );
    }

    #[test]
    fn cancelled_password_skips_only_that_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archives = vec![
            plain_zips(dir.path(), &["a"]).remove(0),
            dir.path().join("Secret.zip"),
            plain_zips(dir.path(), &["c"]).remove(0),
        ];
        make_encrypted(&archives[1]);
        let mut ask = |_: &Path, _: PasswordAsk| None;

        let outcome = extract_batch(&archives, &Progress::new(), &no_mark, &mut ask);

        assert_eq!(outcome.made.len(), 2);
        assert_eq!(outcome.skipped, [archives[1].clone()]);
        assert!(outcome.failed.is_empty() && !outcome.cancelled);
        assert_eq!(
            listing(dir.path()),
            ["Secret.zip", "a.txt", "a.zip", "c.txt", "c.zip"]
        );
    }

    #[test]
    fn cancel_keeps_finished_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let archives = plain_zips(dir.path(), &["a", "b", "c"]);
        let progress = Progress::new();
        let mark = |archive: &Path, _: &Path| {
            if archive == archives[1] {
                progress.cancel();
            }
            Ok(())
        };
        let mut ask = |_: &Path, _: PasswordAsk| None;

        let outcome = extract_batch(&archives, &progress, &mark, &mut ask);

        assert!(outcome.cancelled);
        assert_eq!(outcome.made.len(), 1);
        assert_eq!(outcome.made[0].source, archives[0]);
        assert!(outcome.failed.is_empty() && outcome.skipped.is_empty());
        assert_eq!(listing(dir.path()), ["a.txt", "a.zip", "b.zip", "c.zip"]);
    }

    #[test]
    fn cancelled_before_start_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archives = plain_zips(dir.path(), &["a"]);
        let progress = Progress::new();
        progress.cancel();
        let mut ask = |_: &Path, _: PasswordAsk| panic!("cancelled");

        let outcome = extract_batch(&archives, &progress, &no_mark, &mut ask);

        assert!(outcome.cancelled && outcome.made.is_empty());
        assert_eq!(listing(dir.path()), ["a.zip"]);
    }

    #[test]
    fn cancel_while_asking_stops_without_asking_again() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("One.zip");
        let second = dir.path().join("Two.zip");
        make_encrypted(&first);
        make_encrypted(&second);
        let progress = Progress::new();
        let calls = RefCell::new(0);
        let mut ask = |_: &Path, _: PasswordAsk| {
            *calls.borrow_mut() += 1;
            progress.cancel();
            password("nope")
        };

        let outcome = extract_batch(&[first, second], &progress, &no_mark, &mut ask);

        assert!(outcome.cancelled);
        assert_eq!(*calls.borrow(), 1);
        assert!(outcome.made.is_empty() && outcome.skipped.is_empty());
        assert_eq!(listing(dir.path()), ["One.zip", "Two.zip"]);
    }

    #[test]
    fn one_damaged_archive_does_not_stop_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mut archives = plain_zips(dir.path(), &["a"]);
        let damaged = dir.path().join("Broken.zip");
        fs::write(&damaged, b"PK\x03\x04 this is not a zip").unwrap();
        archives.push(damaged.clone());
        archives.extend(plain_zips(dir.path(), &["c"]));
        let mut ask = |_: &Path, _: PasswordAsk| None;

        let outcome = extract_batch(&archives, &Progress::new(), &no_mark, &mut ask);

        assert_eq!(outcome.made.len(), 2);
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].0, damaged);
        assert!(!outcome.cancelled);
        assert_eq!(
            listing(dir.path()),
            ["Broken.zip", "a.txt", "a.zip", "c.txt", "c.zip"]
        );
    }

    #[test]
    fn archive_that_is_gone_or_changed_fails_as_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let mut archives = plain_zips(dir.path(), &["a"]);
        let missing = dir.path().join("Gone.zip");
        let renamed = dir.path().join("Notes.txt");
        fs::write(&renamed, b"plain text").unwrap();
        archives.push(missing.clone());
        archives.push(renamed.clone());
        let mut ask = |_: &Path, _: PasswordAsk| None;

        let progress = Progress::new();
        let outcome = extract_batch(&archives, &progress, &no_mark, &mut ask);
        assert_eq!(progress.snapshot().current, "Notes.txt");

        assert_eq!(outcome.made.len(), 1);
        let failed: Vec<&PathBuf> = outcome.failed.iter().map(|(path, _)| path).collect();
        assert_eq!(failed, [&missing, &renamed]);
        assert!(
            outcome
                .failed
                .iter()
                .all(|(_, error)| matches!(error, ArchiveError::Unsupported(_)))
        );
    }
}
