//! The open panel, for a folder or for PDFs, shown modeless with
//! `beginWithCompletionHandler:`. A modal panel (`runModal`) runs its own
//! loop inside winit's event handler: AppKit then hands winit events from
//! that loop (a drag while the panel is up), winit finds its handler busy
//! and aborts the app.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSOpenPanel};
use objc2_foundation::{NSArray, NSString, NSURL};

/// What the panel asks for.
pub(super) enum Ask {
    /// One folder; new folders can be made in the panel.
    Folder,
    /// PDF files, several at once.
    Pdfs,
}

thread_local! {
    /// The panel on screen: one at a time.
    static SHOWN: RefCell<Option<Retained<NSOpenPanel>>> = const { RefCell::new(None) };
}

/// Shows the panel, starting in `start`, and returns at once. `done` gets
/// the chosen paths when the panel closes, empty when cancelled. While a
/// panel is already up, it is brought forward and `done` is dropped.
pub(super) fn open(
    mtm: MainThreadMarker,
    ask: Ask,
    start: Option<&Path>,
    done: impl FnOnce(Vec<PathBuf>) + 'static,
) {
    super::screens::activate_app(mtm);
    if let Some(panel) = SHOWN.with_borrow(Clone::clone) {
        panel.makeKeyAndOrderFront(None);
        return;
    }
    let panel = NSOpenPanel::openPanel(mtm);
    match ask {
        Ask::Folder => {
            panel.setCanChooseFiles(false);
            panel.setCanChooseDirectories(true);
            panel.setCanCreateDirectories(true);
        }
        Ask::Pdfs => {
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(true);
            // `allowedContentTypes` needs UniformTypeIdentifiers; the old
            // extension list still works.
            #[allow(deprecated)]
            panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str(
                "pdf",
            )])));
        }
    }
    if let Some(url) = start.and_then(NSURL::from_directory_path) {
        panel.setDirectoryURL(Some(&url));
    }

    // The handler takes the panel back from `SHOWN` rather than holding it,
    // so the panel and its handler don't keep each other alive.
    let done = Cell::new(Some(done));
    let handler = RcBlock::new(move |response: NSModalResponse| {
        let Some(panel) = SHOWN.take() else {
            return;
        };
        let paths = if response == NSModalResponseOK {
            panel
                .URLs()
                .iter()
                .filter_map(|url| url.to_file_path())
                .collect()
        } else {
            Vec::new()
        };
        if let Some(done) = done.take() {
            done(paths);
        }
    });
    SHOWN.set(Some(panel.clone()));
    panel.beginWithCompletionHandler(&handler);
}
