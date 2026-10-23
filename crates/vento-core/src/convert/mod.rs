//! Image conversion: which formats the ring offers and the batch that
//! writes the converted copies.

mod batch;
mod format;

pub use batch::{ConvertError, ConvertOutcome, ImageCodec, convert_batch, output_name};
pub use format::{ImageFormat, looks_like_image, ring_formats};
