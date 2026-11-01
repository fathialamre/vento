//! Batch rename: names, comparison keys, natural ordering and rename rules (no OS-specific code).

mod items;
mod name;
mod preview;
mod rules;

pub use items::{Item, MAX_ITEMS, Order, OrderBy, load_items, sort_items};
pub use name::{NameParts, Os, join_name, name_key, natural_cmp, nfc, split_name};
pub use preview::{Batch, Preview, Problem, Row, check_name, preview};
pub use rules::{CaseTo, Ctx, ExtTo, Rule, RuleHistory, apply_rules, unknown_tags};
