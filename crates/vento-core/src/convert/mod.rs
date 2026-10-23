//! Image conversion: which formats the ring offers and, later, the batch
//! that writes the converted copies.

mod format;

pub use format::{ImageFormat, looks_like_image, ring_formats};
