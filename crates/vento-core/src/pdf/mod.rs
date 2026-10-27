//! PDF pages: the page list a PDF window edits and the selection over it.
//! Both are platform-free; reading and writing PDFs lives elsewhere.

mod grid;
mod jobs;
mod names;
mod pages;
mod selection;
mod thumbs;

pub use grid::GridMetrics;
pub use jobs::{
    ImageOptions, JobKind, PdfBackend, PdfError, PdfJob, PdfOutcome, Source, open_sources, run_job,
};
pub use names::{
    ImageKind, SplitMode, extract_name, image_name, images_folder_name, page_ranges, save_name,
    split_folder_name, split_piece_name, split_plan, stem,
};
pub use pages::{PageList, PageRef};
pub use selection::Selection;
pub use thumbs::ThumbQueue;
