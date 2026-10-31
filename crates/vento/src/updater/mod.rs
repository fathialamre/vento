//! Self-update for the macOS build: asks GitHub for the latest release,
//! downloads it and swaps the app in place. Later tasks add the swap, the
//! watcher and the controller here.

pub mod net;
