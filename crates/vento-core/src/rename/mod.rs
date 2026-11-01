//! Batch rename: names, comparison keys, natural ordering, rename rules and the
//! two-phase rename itself (no OS-specific code).

mod items;
mod name;
mod preview;
mod rules;
mod run;

pub use items::{Item, MAX_ITEMS, Order, OrderBy, load_items, sort_items};
pub use name::{NameParts, Os, join_name, name_key, natural_cmp, nfc, split_name};
pub use preview::{Batch, Preview, Problem, Row, check_name, preview, read_taken};
pub use rules::{CaseTo, Ctx, ExtTo, Rule, RuleHistory, apply_rules, unknown_tags};
pub use run::{
    Disk, ItemPrint, RealDisk, RenameOutcome, RenameRecord, Step, UndoOutcome, execute, steps, undo,
};
