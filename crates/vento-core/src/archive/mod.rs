//! Archive support: telling archives from other files, deciding whether a
//! selection is compressed or extracted, and doing either safely.

mod compress;
mod detect;
mod entries;
mod job;
mod rar_read;
mod sevenz_read;
mod staging;
#[cfg(test)]
mod test_support;
mod zip_read;

pub use compress::compress;
pub use detect::{
    Format, Plan, archive_stem, detect, plan, rar_first_volume, rar_volumes, zip_name,
};
pub use entries::extract_one;
pub use job::{ArchiveError, Made, Progress, ProgressSnapshot};
