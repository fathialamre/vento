//! Batch rename: names, comparison keys and natural ordering (no OS-specific code).

mod name;

pub use name::{NameParts, Os, join_name, name_key, natural_cmp, nfc, split_name};
