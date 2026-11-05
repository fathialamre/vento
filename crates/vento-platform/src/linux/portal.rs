//! The folder and PDF pickers, through the desktop portal's FileChooser
//! (the dialog GNOME shows for sandboxed apps too).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::glib;
use gio::prelude::*;
use vento_core::i18n::tr;

pub(super) const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub(super) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// The object path the portal will answer a request on, made from the
/// caller's unique bus name (`:1.42` → `1_42`) and the request's token.
pub(super) fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// A token not used before by this process.
pub(super) fn next_token() -> String {
    thread_local!(static COUNT: Cell<u32> = const { Cell::new(0) });
    let n = COUNT.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    format!("vento_{}_{n}", std::process::id())
}

/// The picked local files or folders from a `Response` signal's
/// `(ua{sv})`: code 0 and its `uris` entries; empty otherwise.
fn picked(response: &glib::Variant) -> Vec<PathBuf> {
    let uris = || -> Option<Vec<String>> {
        let code: u32 = response.try_child_value(0)?.get()?;
        if code != 0 {
            return None;
        }
        let results = glib::VariantDict::new(Some(&response.try_child_value(1)?));
        results.lookup("uris").ok()?
    };
    uris()
        .unwrap_or_default()
        .iter()
        .filter_map(|uri| gio::File::for_uri(uri).path())
        .collect()
}

/// Show the portal's folder picker, starting in `start`, and wait for it.
/// `None` when cancelled or when there is no portal.
pub(super) fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let options = glib::VariantDict::new(None);
    options.insert("directory", true);
    choose("Choose Folder", options, start).into_iter().next()
}

/// Show the portal's file picker for PDFs, starting in `start`, and wait
/// for it; several can be chosen. Empty when cancelled or when there is no
/// portal.
pub(super) fn pick_pdfs(start: Option<&Path>) -> Vec<PathBuf> {
    let options = glib::VariantDict::new(None);
    options.insert("multiple", true);
    // By name and, for any case of the extension, by type.
    let patterns = [(0u32, "*.pdf"), (1, "application/pdf")];
    let patterns: Vec<(u32, String)> = patterns.map(|(k, p)| (k, p.to_owned())).to_vec();
    let pdf = (tr("PDF"), patterns);
    options.insert("filters", vec![pdf.clone()]);
    options.insert("current_filter", pdf);
    choose(&tr("Add PDF"), options, start)
}

/// Show the portal's FileChooser titled `title` with `options` (to which
/// the request's own are added), starting in `start`, and wait for it.
fn choose(title: &str, options: glib::VariantDict, start: Option<&Path>) -> Vec<PathBuf> {
    let bus = match super::session_bus() {
        Ok(bus) => bus,
        Err(e) => {
            log::warn!("no session bus for the file picker: {e}");
            return Vec::new();
        }
    };
    let token = next_token();
    let Some(unique_name) = bus.unique_name() else {
        return Vec::new();
    };
    let handle = request_path(unique_name.as_str(), &token);

    // Listen before asking, so a fast answer is not missed.
    let main_loop = glib::MainLoop::new(None, false);
    let answer: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let _subscription = {
        let main_loop = main_loop.clone();
        let answer = answer.clone();
        bus.subscribe_to_signal(
            Some(PORTAL),
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&handle),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                *answer.borrow_mut() = picked(signal.parameters);
                main_loop.quit();
            },
        )
    };

    options.insert("handle_token", &token);
    options.insert("modal", true);
    if let Some(start) = start {
        let mut bytes = start.as_os_str().as_encoded_bytes().to_vec();
        bytes.push(0);
        options.insert_value(
            "current_folder",
            &glib::Variant::array_from_fixed_array(&bytes),
        );
    }
    let args = glib::Variant::tuple_from_iter(["".to_variant(), title.to_variant(), options.end()]);
    if let Err(e) = bus.call_sync(
        Some(PORTAL),
        PORTAL_PATH,
        "org.freedesktop.portal.FileChooser",
        "OpenFile",
        Some(&args),
        Some(glib::VariantTy::new("(o)").unwrap()),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
    ) {
        log::warn!("file picker unavailable: {}", e.message());
        return Vec::new();
    }
    main_loop.run();
    answer.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(code: u32, uris: Option<&[&str]>) -> glib::Variant {
        let results = glib::VariantDict::new(None);
        if let Some(uris) = uris {
            results.insert("uris", uris.to_vec());
        }
        glib::Variant::tuple_from_iter([code.to_variant(), results.end()])
    }

    #[test]
    fn request_paths_follow_the_portal_rule() {
        assert_eq!(
            request_path(":1.42", "vento_7_1"),
            "/org/freedesktop/portal/desktop/request/1_42/vento_7_1"
        );
    }

    #[test]
    fn tokens_are_never_reused() {
        assert_ne!(next_token(), next_token());
    }

    #[test]
    fn a_picked_folder_is_read_from_the_response() {
        let r = response(0, Some(&["file:///home/me/My%20Files"]));
        assert_eq!(picked(&r), [PathBuf::from("/home/me/My Files")]);
    }

    #[test]
    fn every_picked_local_file_is_read_from_the_response() {
        let r = response(
            0,
            Some(&["file:///home/me/a.pdf", "file:///home/me/c%20d.pdf"]),
        );
        assert_eq!(
            picked(&r),
            [
                PathBuf::from("/home/me/a.pdf"),
                PathBuf::from("/home/me/c d.pdf")
            ]
        );
    }

    #[test]
    fn cancelled_or_empty_responses_pick_nothing() {
        assert!(picked(&response(1, Some(&["file:///home"]))).is_empty());
        assert!(picked(&response(0, Some(&[]))).is_empty());
        assert!(picked(&response(0, None)).is_empty());
    }
}
