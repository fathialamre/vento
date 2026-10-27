//! File names vento writes for PDF outputs, the page-range text inside them,
//! and how a split is cut into pieces.
//!
//! These are file names, so they stay English in every language. Work stays
//! on `OsStr`/`OsString` so a name that is not valid Unicode survives intact.

use std::ffi::{OsStr, OsString};
use std::ops::Range;
use std::path::Path;

/// The file name without a trailing `.pdf` (in any case); nothing else is
/// stripped, so `a.pdf.txt` and a bare `.pdf` come back unchanged.
pub fn stem(file_name: &OsStr) -> OsString {
    let path = Path::new(file_name);
    match path.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("pdf") => path
            .file_stem()
            .map_or_else(|| file_name.to_os_string(), OsStr::to_os_string),
        _ => file_name.to_os_string(),
    }
}

/// Zero-based page positions as 1-based range text: sorted, duplicates
/// dropped, runs joined with an en dash and parts with ", " (`"1, 3–5"`).
pub fn page_ranges(positions: &[usize]) -> String {
    let mut sorted = positions.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let first = sorted[i];
        let mut last = first;
        while i + 1 < sorted.len() && sorted[i + 1] == last + 1 {
            i += 1;
            last = sorted[i];
        }
        parts.push(if first == last {
            (first + 1).to_string()
        } else {
            format!("{}\u{2013}{}", first + 1, last + 1)
        });
        i += 1;
    }
    parts.join(", ")
}

/// Joins name pieces into one `OsString`.
fn join(parts: &[&OsStr]) -> OsString {
    let mut out = OsString::new();
    for part in parts {
        out.push(part);
    }
    out
}

/// The name a saved document gets, from the stems of the sources that still
/// have pages in the grid (one per source position, in order of first
/// appearance; the same file opened twice counts twice): one source reads
/// `a (edited).pdf`, two `a + b.pdf`, more `a + N more.pdf`.
pub fn save_name(source_stems: &[OsString]) -> OsString {
    let Some(first) = source_stems.first() else {
        return OsString::from("PDF (edited).pdf");
    };
    match source_stems.len() {
        1 => join(&[first, OsStr::new(" (edited).pdf")]),
        2 => join(&[
            first,
            OsStr::new(" + "),
            &source_stems[1],
            OsStr::new(".pdf"),
        ]),
        n => {
            let more = format!(" + {} more.pdf", n - 1);
            join(&[first, OsStr::new(&more)])
        }
    }
}

/// The name of an extraction: `a (pages 3–5).pdf`, or `a (page 4).pdf` when
/// exactly one page is selected.
pub fn extract_name(anchor: &OsStr, positions: &[usize]) -> OsString {
    let mut sorted = positions.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let label = if sorted.len() == 1 {
        " (page "
    } else {
        " (pages "
    };
    let tail = format!("{}).pdf", page_ranges(&sorted));
    join(&[anchor, OsStr::new(label), OsStr::new(&tail)])
}

/// How a document is cut into pieces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitMode {
    /// One piece per page.
    EveryPage,
    /// Pieces of this many pages (the last may be shorter); 0 counts as 1.
    Every(usize),
    /// Cut before each listed position: a cut `c` falls between position
    /// `c - 1` and `c`.
    AtCuts(Vec<usize>),
}

/// The page ranges a split produces for a document of `count` pages. Empty
/// pieces never appear; cuts at 0, at or past `count`, and duplicates are
/// ignored.
pub fn split_plan(count: usize, mode: &SplitMode) -> Vec<Range<usize>> {
    if count == 0 {
        return Vec::new();
    }
    match mode {
        SplitMode::EveryPage => split_plan(count, &SplitMode::Every(1)),
        SplitMode::Every(n) => {
            let n = (*n).max(1);
            (0..count)
                .step_by(n)
                .map(|start| start..(start + n).min(count))
                .collect()
        }
        SplitMode::AtCuts(cuts) => {
            let mut cuts: Vec<usize> = cuts
                .iter()
                .copied()
                .filter(|&c| c > 0 && c < count)
                .collect();
            cuts.sort_unstable();
            cuts.dedup();
            let mut plan = Vec::with_capacity(cuts.len() + 1);
            let mut start = 0;
            for cut in cuts {
                plan.push(start..cut);
                start = cut;
            }
            plan.push(start..count);
            plan
        }
    }
}

/// The folder a split writes into: `a split`.
pub fn split_folder_name(anchor: &OsStr) -> OsString {
    join(&[anchor, OsStr::new(" split")])
}

/// The name of piece `k` (1-based) of a split: `a 2 (3–5).pdf`.
pub fn split_piece_name(anchor: &OsStr, k: usize, range: &Range<usize>) -> OsString {
    let positions: Vec<usize> = range.clone().collect();
    let tail = format!(" {k} ({}).pdf", page_ranges(&positions));
    join(&[anchor, OsStr::new(&tail)])
}

/// The raster format of an exported page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    /// Lossless PNG.
    Png,
    /// JPEG on a white background.
    Jpeg,
}

impl ImageKind {
    /// The file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ImageKind::Png => "png",
            ImageKind::Jpeg => "jpg",
        }
    }
}

/// The name of one exported page: `a p1.png` (`position` is zero-based).
pub fn image_name(anchor: &OsStr, position: usize, kind: ImageKind) -> OsString {
    let tail = format!(" p{}.{}", position + 1, kind.extension());
    join(&[anchor, OsStr::new(&tail)])
}

/// The folder exported pages go into: `a images`.
pub fn images_folder_name(anchor: &OsStr) -> OsString {
    join(&[anchor, OsStr::new(" images")])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_handles_case_and_dots() {
        assert_eq!(stem("report.final.PDF".as_ref()), "report.final");
        assert_eq!(stem("notes".as_ref()), "notes");
        assert_eq!(stem("تقرير.pdf".as_ref()), "تقرير");
        assert_eq!(stem("a.pdf.txt".as_ref()), "a.pdf.txt");
        assert_eq!(stem(".pdf".as_ref()), ".pdf");
    }

    #[test]
    fn ranges_join() {
        assert_eq!(page_ranges(&[0, 2, 3, 4]), "1, 3–5");
        assert_eq!(page_ranges(&[6]), "7");
        assert_eq!(page_ranges(&[4, 0, 1]), "1–2, 5");
        assert_eq!(page_ranges(&[1, 1, 2]), "2–3");
        assert_eq!(page_ranges(&[]), "");
    }

    #[test]
    fn save_names() {
        let s = |v: &[&str]| save_name(&v.iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(s(&["report"]), "report (edited).pdf");
        assert_eq!(s(&["report", "scan"]), "report + scan.pdf");
        assert_eq!(s(&["report", "scan", "memo"]), "report + 2 more.pdf");
        assert_eq!(s(&["a", "b", "c", "d"]), "a + 3 more.pdf");
    }

    #[test]
    fn save_name_same_file_twice() {
        assert_eq!(
            save_name(&["report".into(), "report".into()]),
            "report + report.pdf"
        );
    }

    #[test]
    fn extract_names() {
        assert_eq!(
            extract_name("report".as_ref(), &[2, 3, 4]),
            "report (pages 3–5).pdf"
        );
        assert_eq!(extract_name("report".as_ref(), &[3]), "report (page 4).pdf");
        assert_eq!(
            extract_name("report".as_ref(), &[0, 2]),
            "report (pages 1, 3).pdf"
        );
    }

    #[test]
    fn split_plans() {
        assert_eq!(split_plan(8, &SplitMode::EveryPage).len(), 8);
        assert_eq!(split_plan(5, &SplitMode::Every(2)), [0..2, 2..4, 4..5]);
        assert_eq!(
            split_plan(8, &SplitMode::AtCuts(vec![2, 5])),
            [0..2, 2..5, 5..8]
        );
        assert_eq!(
            split_plan(8, &SplitMode::AtCuts(vec![0, 8, 5, 5])),
            [0..5, 5..8]
        );
        assert_eq!(split_plan(3, &SplitMode::Every(0)), [0..1, 1..2, 2..3]);
        assert_eq!(split_plan(3, &SplitMode::Every(10)), vec![0..3]);
        assert_eq!(split_plan(4, &SplitMode::AtCuts(vec![])), vec![0..4]);
        assert!(split_plan(0, &SplitMode::EveryPage).is_empty());
        assert!(split_plan(0, &SplitMode::AtCuts(vec![1])).is_empty());
    }

    #[test]
    fn piece_and_image_names() {
        assert_eq!(
            split_piece_name("report".as_ref(), 2, &(2..5)),
            "report 2 (3–5).pdf"
        );
        assert_eq!(
            split_piece_name("report".as_ref(), 1, &(0..1)),
            "report 1 (1).pdf"
        );
        assert_eq!(split_folder_name("report".as_ref()), "report split");
        assert_eq!(
            image_name("report".as_ref(), 0, ImageKind::Png),
            "report p1.png"
        );
        assert_eq!(
            image_name("report".as_ref(), 2, ImageKind::Jpeg),
            "report p3.jpg"
        );
        assert_eq!(images_folder_name("report".as_ref()), "report images");
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_names_survive() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let raw = OsString::from_vec(b"re\xFFport.pdf".to_vec());
        let s = stem(&raw);
        assert_eq!(s.as_bytes(), b"re\xFFport");
        assert_eq!(save_name(&[s]).as_bytes(), b"re\xFFport (edited).pdf");
    }
}
