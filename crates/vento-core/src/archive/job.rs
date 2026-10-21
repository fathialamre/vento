//! What an archive job shares with its caller: progress, errors and the
//! result of a finished compress.

use std::io::{self, Write};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::fileops::Fingerprint;

/// Message carried by the `io::Error` that stops a write after Cancel.
pub(crate) const CANCELLED: &str = "cancelled";

/// Most bytes handed to the inner writer at once, so that Cancel is noticed
/// quickly even when the inner writer is slow.
const CHUNK: usize = 1024 * 1024;

/// Why an archive job failed.
#[derive(thiserror::Error, Debug)]
pub enum ArchiveError {
    /// An underlying filesystem call failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The destination volume has too little free space.
    #[error("not enough disk space")]
    NotEnoughSpace,
    /// An entry would be written outside the destination folder.
    #[error("unsafe path in archive: {0}")]
    UnsafeEntry(String),
    /// The archive is corrupt or cut short.
    #[error("damaged archive: {0}")]
    Damaged(String),
    /// The archive uses a feature vento cannot read.
    #[error("unsupported archive: {0}")]
    Unsupported(String),
    /// There is nothing to extract or compress.
    #[error("archive is empty")]
    Empty,
    /// The archive is encrypted and no password was given.
    #[error("password needed")]
    NeedPassword,
    /// The given password does not open the archive.
    #[error("wrong password")]
    WrongPassword,
    /// The user cancelled the job.
    #[error("cancelled")]
    Cancelled,
}

impl ArchiveError {
    /// Maps an error from a [`Counting`] write back to [`ArchiveError::Cancelled`];
    /// any other error stays an [`ArchiveError::Io`].
    pub(crate) fn from_io(error: io::Error) -> Self {
        let cancelled = error
            .get_ref()
            .is_some_and(|inner| inner.to_string() == CANCELLED);
        if cancelled {
            ArchiveError::Cancelled
        } else {
            ArchiveError::Io(error)
        }
    }

    /// Maps an error from reading an archive: data that does not decode or
    /// ends early, or a failed checksum, is [`ArchiveError::Damaged`]; any
    /// other error stays an [`ArchiveError::Io`].
    pub(crate) fn from_read(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
                ArchiveError::Damaged(error.to_string())
            }
            _ => ArchiveError::from_io(error),
        }
    }
}

/// How far a job has got, shared between the worker and the progress window.
#[derive(Debug)]
pub struct Progress {
    cancelled: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    current: Mutex<String>,
}

/// A point-in-time copy of a [`Progress`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressSnapshot {
    /// Bytes processed so far.
    pub done: u64,
    /// Bytes to process in all; 0 until known.
    pub total: u64,
    /// Name of the item being processed.
    pub current: String,
}

impl Progress {
    /// A fresh counter with nothing done, to share between threads.
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress {
            cancelled: AtomicBool::new(false),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            current: Mutex::new(String::new()),
        })
    }

    /// Asks the job to stop as soon as it can.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether [`Progress::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Sets the number of bytes the job will process.
    pub fn set_total(&self, bytes: u64) {
        self.total.store(bytes, Ordering::Relaxed);
    }

    /// Records `bytes` more as processed.
    pub fn add_done(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Sets the name of the item being processed.
    pub fn set_current(&self, name: &str) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        current.clear();
        current.push_str(name);
    }

    /// The current state.
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            current: self
                .current
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }
}

/// A writer that counts what passes through it into a [`Progress`] and stops
/// with a cancellation error once the job is cancelled. `P` is how it reaches
/// the progress: a borrow, or an `Arc` when the writer must own it.
pub(crate) struct Counting<P, W> {
    inner: W,
    progress: P,
}

impl<P, W> Counting<P, W> {
    /// Wraps `inner`, counting into `progress`.
    pub(crate) fn new(inner: W, progress: P) -> Self {
        Counting { inner, progress }
    }
}

impl<P: Deref<Target = Progress>, W: Write> Write for Counting<P, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.progress.is_cancelled() {
            return Err(io::Error::other(CANCELLED));
        }
        let end = buf.len().min(CHUNK);
        let written = self.inner.write(&buf[..end])?;
        self.progress.add_done(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A finished compress or extract: what it read and what it created.
#[derive(Debug)]
pub struct Made {
    /// The first input of a compress, or the archive extracted.
    pub source: PathBuf,
    /// The zip, or the extracted file or folder, that was created.
    pub output: PathBuf,
    /// The output as it was when created, for undo to recognise it.
    pub fingerprint: Fingerprint,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_tracks_values() {
        let p = Progress::new();
        p.set_total(10);
        p.add_done(3);
        p.add_done(4);
        p.set_current("a.txt");
        assert_eq!(
            p.snapshot(),
            ProgressSnapshot {
                done: 7,
                total: 10,
                current: "a.txt".into()
            }
        );
        assert!(!p.is_cancelled());
        p.cancel();
        assert!(p.is_cancelled());
    }

    #[test]
    fn counting_counts_and_chunks() {
        let p = Progress::new();
        let mut out = Counting::new(Vec::new(), Arc::clone(&p));
        let data = vec![1u8; CHUNK + 5];
        assert_eq!(out.write(&data).unwrap(), CHUNK);
        out.write_all(&data[CHUNK..]).unwrap();
        assert_eq!(p.snapshot().done, data.len() as u64);
    }

    #[test]
    fn counting_stops_after_cancel() {
        let p = Progress::new();
        let mut out = Counting::new(Vec::new(), Arc::clone(&p));
        out.write_all(b"abc").unwrap();
        p.cancel();
        let error = out.write(b"def").unwrap_err();
        assert!(matches!(
            ArchiveError::from_io(error),
            ArchiveError::Cancelled
        ));
        assert_eq!(p.snapshot().done, 3);
    }

    #[test]
    fn bad_data_while_reading_is_damage() {
        let bad = io::Error::new(io::ErrorKind::InvalidData, "Invalid checksum");
        assert!(matches!(
            ArchiveError::from_read(bad),
            ArchiveError::Damaged(_)
        ));
        let short = io::Error::from(io::ErrorKind::UnexpectedEof);
        assert!(matches!(
            ArchiveError::from_read(short),
            ArchiveError::Damaged(_)
        ));
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(matches!(
            ArchiveError::from_read(denied),
            ArchiveError::Io(_)
        ));
    }

    #[test]
    fn other_io_errors_stay_io() {
        let error = io::Error::other("disk on fire");
        assert!(matches!(ArchiveError::from_io(error), ArchiveError::Io(_)));
    }
}
