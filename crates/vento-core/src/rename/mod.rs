//! Batch rename: names, comparison keys, natural ordering and rename rules (no OS-specific code).

mod name;
mod rules;

pub use name::{NameParts, Os, join_name, name_key, natural_cmp, nfc, split_name};
pub use rules::{CaseTo, Ctx, ExtTo, Rule, RuleHistory, apply_rules, unknown_tags};
