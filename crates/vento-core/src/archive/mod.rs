//! Archive support: telling archives from other files and deciding whether a
//! selection is compressed or extracted.

mod compress;
mod detect;
mod job;
mod staging;

pub use compress::compress;
pub use detect::{
    Format, Plan, archive_stem, detect, plan, rar_first_volume, rar_volumes, zip_name,
};
pub use job::{ArchiveError, Made, Progress, ProgressSnapshot};
