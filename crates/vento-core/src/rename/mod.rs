//! Batch rename: names, comparison keys, natural ordering, rename rules and the
//! two-phase rename itself (no OS-specific code).

mod items;
mod labels;
mod name;
mod preview;
mod rules;
mod run;

pub use items::{Item, MAX_ITEMS, Order, OrderBy, load_items, sort_items};
pub use labels::{
    changed_text, chip_summary, chip_title, done_text, failed_text, footer_text, order_label,
    problem_text, stuck_text, title_text, too_many_text, undo_text,
};
pub use name::{NameParts, Os, join_name, name_key, natural_cmp, nfc, split_name};
pub use preview::{Batch, Preview, Problem, Row, check_name, preview, read_taken};
pub use rules::{CaseTo, Ctx, ExtTo, Rule, RuleHistory, apply_rules, unknown_tags};
pub use run::{
    Disk, ItemPrint, RealDisk, RenameOutcome, RenameRecord, Step, UndoOutcome, execute, steps, undo,
};

/// What the Rename window remembers between uses: the rules last applied and
/// the list order last chosen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenameMemory {
    /// The rules, top to bottom.
    pub rules: Vec<Rule>,
    /// The Order menu's choice.
    pub order: Order,
}
