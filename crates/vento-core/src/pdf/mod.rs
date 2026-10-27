//! PDF pages: the page list a PDF window edits and the selection over it.
//! Both are platform-free; reading and writing PDFs lives elsewhere.

mod grid;
mod pages;
mod selection;
mod thumbs;

pub use grid::GridMetrics;
pub use pages::{PageList, PageRef};
pub use selection::Selection;
pub use thumbs::ThumbQueue;
