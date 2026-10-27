//! PDF pages: the page list a PDF window edits and the selection over it.
//! Both are platform-free; reading and writing PDFs lives elsewhere.

mod pages;
mod selection;

pub use pages::{PageList, PageRef};
pub use selection::Selection;
