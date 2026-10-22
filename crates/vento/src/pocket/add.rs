//! How a drop turns into Pocket items, and storing what vento saves or
//! copies for them.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vento_core::fileops::copy_into;
use vento_core::pocket::ItemSource;
use vento_core::pocket_content::{save_image, save_text, save_url, webloc_bytes};
use vento_platform::Dropped;

use super::display_name;

/// How a drop turns into Pocket items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AddPlan {
    /// Add the files where they are.
    References(Vec<PathBuf>),
    /// Copy the files into `<pocket_dir>/kept` and add the copies.
    Copies(Vec<PathBuf>),
    /// Save into `<pocket_dir>/saved` and add the new file.
    SaveText(String),
    SaveUrl(String),
    SaveImage(Vec<u8>, &'static str),
}

/// What to do with `d`: files are referenced, or copied when the Pocket keeps
/// copies; text, links and images are always saved as files of their own.
pub(super) fn plan_add(d: &Dropped, keep_copies: bool) -> AddPlan {
    match d {
        Dropped::Files(paths) if keep_copies => AddPlan::Copies(paths.clone()),
        Dropped::Files(paths) => AddPlan::References(paths.clone()),
        Dropped::Text(text) => AddPlan::SaveText(text.clone()),
        Dropped::Url(url) => AddPlan::SaveUrl(url.clone()),
        Dropped::Image { bytes, ext } => AddPlan::SaveImage(bytes.clone(), ext),
    }
}

/// [`plan_add`] for `d`, leaving out files the Pocket already holds
/// (`held`): as items, as the originals of kept copies, or because they are
/// vento's own copies. Nothing is copied twice.
pub(super) fn plan_new(d: &Dropped, keep_copies: bool, held: impl Fn(&Path) -> bool) -> AddPlan {
    match plan_add(d, keep_copies) {
        AddPlan::References(paths) => {
            AddPlan::References(paths.into_iter().filter(|p| !held(p)).collect())
        }
        AddPlan::Copies(paths) => AddPlan::Copies(paths.into_iter().filter(|p| !held(p)).collect()),
        other => other,
    }
}

/// The bytes [`store`] would write for saved content (text, a link or an
/// image); `None` for files, which are compared by path instead.
pub(super) fn saved_content(plan: &AddPlan) -> Option<Cow<'_, [u8]>> {
    match plan {
        AddPlan::SaveText(text) => Some(Cow::Borrowed(text.as_bytes())),
        AddPlan::SaveUrl(url) => Some(Cow::Owned(webloc_bytes(url))),
        AddPlan::SaveImage(bytes, _) => Some(Cow::Borrowed(bytes)),
        AddPlan::References(_) | AddPlan::Copies(_) => None,
    }
}

/// Whether one of the `saved` files holds exactly `bytes`, so the same
/// text, link or image is not saved into the Pocket twice. Only files of
/// the same size are read; one that can't be read counts as different.
pub(super) fn holds_content<'a>(saved: impl IntoIterator<Item = &'a Path>, bytes: &[u8]) -> bool {
    let len = bytes.len() as u64;
    saved.into_iter().any(|path| {
        fs::metadata(path).is_ok_and(|m| m.len() == len)
            && fs::read(path).is_ok_and(|held| held == bytes)
    })
}

/// A file to add, who owns it, and (for a kept copy) where it came from.
pub(super) type Entry = (PathBuf, ItemSource, Option<PathBuf>);

/// Carries out `plan` under `dir` (the Pocket's content folder): copies go to
/// `kept/`, saved content to `saved/`. Returns the files to add with their
/// source (and origin), and a reason for each part that failed. Copies can
/// take long: run them off the main thread.
pub(super) fn store(plan: AddPlan, dir: &Path) -> (Vec<Entry>, Vec<String>) {
    let saved = dir.join("saved");
    let now = SystemTime::now();
    let saved_one = |result: std::io::Result<PathBuf>| match result {
        Ok(path) => (vec![(path, ItemSource::Saved, None)], Vec::new()),
        Err(e) => (Vec::new(), vec![e.to_string()]),
    };
    match plan {
        AddPlan::References(paths) => (
            paths
                .into_iter()
                .map(|p| (p, ItemSource::Reference, None))
                .collect(),
            Vec::new(),
        ),
        AddPlan::Copies(paths) => {
            let kept = dir.join("kept");
            // `copy_into` needs the folder to exist.
            if let Err(e) = fs::create_dir_all(&kept) {
                return (Vec::new(), vec![e.to_string()]);
            }
            let mut entries = Vec::new();
            let mut errors = Vec::new();
            for path in paths {
                match copy_into(&path, &kept) {
                    Ok(copy) => entries.push((copy, ItemSource::Kept, Some(path))),
                    Err(e) => errors.push(format!("{}: {e}", display_name(&path))),
                }
            }
            (entries, errors)
        }
        AddPlan::SaveText(text) => saved_one(save_text(&saved, &text, now)),
        AddPlan::SaveUrl(url) => saved_one(save_url(&saved, &url, now)),
        AddPlan::SaveImage(bytes, ext) => saved_one(save_image(&saved, &bytes, ext, now)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_references_by_default() {
        let paths = vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b")];
        assert_eq!(
            plan_add(&Dropped::Files(paths.clone()), false),
            AddPlan::References(paths)
        );
    }

    #[test]
    fn files_are_copies_with_keep_copies() {
        let paths = vec![PathBuf::from("/tmp/a.txt")];
        assert_eq!(
            plan_add(&Dropped::Files(paths.clone()), true),
            AddPlan::Copies(paths)
        );
    }

    #[test]
    fn text_url_image_are_saved() {
        for keep in [false, true] {
            assert_eq!(
                plan_add(&Dropped::Text("hello".into()), keep),
                AddPlan::SaveText("hello".into())
            );
            assert_eq!(
                plan_add(&Dropped::Url("https://example.com/".into()), keep),
                AddPlan::SaveUrl("https://example.com/".into())
            );
            let image = Dropped::Image {
                bytes: vec![1, 2, 3],
                ext: "png",
            };
            assert_eq!(
                plan_add(&image, keep),
                AddPlan::SaveImage(vec![1, 2, 3], "png")
            );
        }
    }

    #[test]
    fn already_held_files_are_left_out_of_the_plan() {
        let (a, b) = (PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt"));
        let files = Dropped::Files(vec![a.clone(), b.clone()]);
        let held = |p: &Path| p == a;
        assert_eq!(
            plan_new(&files, false, held),
            AddPlan::References(vec![b.clone()])
        );
        assert_eq!(plan_new(&files, true, held), AddPlan::Copies(vec![b]));
        assert_eq!(
            plan_new(&files, true, |_| true),
            AddPlan::Copies(Vec::new())
        );
        // Saved content is always new.
        assert_eq!(
            plan_new(&Dropped::Text("hi".into()), true, |_| true),
            AddPlan::SaveText("hi".into())
        );
    }

    #[test]
    fn saved_content_is_what_store_writes() {
        let dir = tempfile::tempdir().unwrap();
        let plans = [
            AddPlan::SaveText("hello".into()),
            AddPlan::SaveUrl(" https://example.com/a?b=1&c=2 ".into()),
            AddPlan::SaveImage(vec![1, 2, 3], "png"),
        ];
        for plan in plans {
            let bytes = saved_content(&plan).unwrap().into_owned();
            let (entries, errors) = store(plan, dir.path());
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(std::fs::read(&entries[0].0).unwrap(), bytes);
        }
        assert_eq!(saved_content(&AddPlan::References(Vec::new())), None);
        assert_eq!(saved_content(&AddPlan::Copies(Vec::new())), None);
    }

    #[test]
    fn holds_content_compares_whole_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "hello").unwrap();
        std::fs::write(&b, "world").unwrap();
        let gone = dir.path().join("gone.txt");
        let saved = [gone.as_path(), b.as_path(), a.as_path()];
        assert!(holds_content(saved, b"hello"));
        // Same length, other bytes; a prefix; nothing at all.
        assert!(!holds_content(saved, b"hellp"));
        assert!(!holds_content(saved, b"hell"));
        assert!(!holds_content([gone.as_path()], b"hello"));
    }

    #[test]
    fn store_creates_kept_and_saved_folders() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        std::fs::write(&src, "hi").unwrap();
        let pocket = dir.path().join("pocket");

        let (entries, errors) = store(AddPlan::Copies(vec![src.clone()]), &pocket);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            entries,
            vec![(
                pocket.join("kept/a.txt"),
                ItemSource::Kept,
                Some(src.clone())
            )]
        );
        assert_eq!(std::fs::read_to_string(&entries[0].0).unwrap(), "hi");

        let (entries, errors) = store(AddPlan::SaveText("hello".into()), &pocket);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(entries[0].1, ItemSource::Saved);
        assert!(entries[0].0.starts_with(pocket.join("saved")));

        let (entries, errors) = store(AddPlan::References(vec![src.clone()]), &pocket);
        assert!(errors.is_empty());
        assert_eq!(entries, vec![(src, ItemSource::Reference, None)]);
    }

    #[test]
    fn store_reports_each_failed_copy() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone.txt");
        let (entries, errors) = store(AddPlan::Copies(vec![gone]), dir.path());
        assert!(entries.is_empty());
        assert_eq!(errors, vec!["gone.txt: source not found".to_owned()]);
    }
}
