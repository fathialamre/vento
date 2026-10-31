//! Self-update for the macOS build: asks GitHub for the latest release,
//! downloads it and swaps the app in place, with a watcher process that
//! starts the new copy and rolls back when it does not start. A later task
//! adds the controller here.

pub mod net;
pub mod swap;
pub mod watch;
