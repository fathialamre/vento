//! When the virtual island may play its shimmer: the pure rules.

use std::time::Duration;

use crate::settings::ShimmerStyle;

/// How long without a key or a click counts as being away from the computer.
pub const AWAY_AFTER: Duration = Duration::from_secs(300);

/// Everything the rules look at, read when a moment is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MomentConditions {
    /// The chosen shimmer style.
    pub style: ShimmerStyle,
    /// The island is the virtual one, collapsed, and not over the lip or the
    /// half-moon.
    pub virtual_resting: bool,
    /// A news item or a job is on the island.
    pub news_or_job: bool,
    /// Vento is paused.
    pub paused: bool,
    /// How many items the Pocket holds.
    pub pocket_items: usize,
    /// Whether a moment needs the Pocket to hold items.
    pub needs_items: bool,
    /// Whether to follow the system's Reduce motion.
    pub follow_reduce: bool,
    /// Whether the system asks for less motion.
    pub system_reduce: bool,
    /// Do Not Disturb or a Focus is on.
    pub quiet_time: bool,
    /// The computer is saving power (battery saver).
    pub power_saving: bool,
    /// How long since the last key or click.
    pub idle_for: Duration,
}

/// Whether a moment may play now: false if any rule says no.
pub fn moment_allowed(c: &MomentConditions) -> bool {
    c.style != ShimmerStyle::Off
        && c.virtual_resting
        && !c.news_or_job
        && !c.paused
        && !(c.needs_items && c.pocket_items == 0)
        && !motion_reduced(c.follow_reduce, c.system_reduce)
        && !c.quiet_time
        && !c.power_saving
        && c.idle_for < AWAY_AFTER
}

/// Whether motion is reduced: only when Vento follows Reduce motion and the
/// system asks for it.
pub fn motion_reduced(follow_reduce: bool, system_reduce: bool) -> bool {
    follow_reduce && system_reduce
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear() -> MomentConditions {
        MomentConditions {
            style: ShimmerStyle::Sweep,
            virtual_resting: true,
            news_or_job: false,
            paused: false,
            pocket_items: 3,
            needs_items: true,
            follow_reduce: true,
            system_reduce: false,
            quiet_time: false,
            power_saving: false,
            idle_for: Duration::ZERO,
        }
    }

    #[test]
    fn allowed_when_all_clear() {
        assert!(moment_allowed(&clear()));
    }

    #[test]
    fn off_and_reduce_never_allow() {
        let off = MomentConditions {
            style: ShimmerStyle::Off,
            ..clear()
        };
        assert!(!moment_allowed(&off));
        let reduced = MomentConditions {
            follow_reduce: true,
            system_reduce: true,
            ..clear()
        };
        assert!(!moment_allowed(&reduced));
        let not_followed = MomentConditions {
            follow_reduce: false,
            system_reduce: true,
            ..clear()
        };
        assert!(moment_allowed(&not_followed));
        assert!(motion_reduced(true, true));
        assert!(!motion_reduced(false, true));
        assert!(!motion_reduced(true, false));
    }

    #[test]
    fn busy_open_paused_quiet_saver_block() {
        let blocked = [
            MomentConditions {
                news_or_job: true,
                ..clear()
            },
            MomentConditions {
                virtual_resting: false,
                ..clear()
            },
            MomentConditions {
                paused: true,
                ..clear()
            },
            MomentConditions {
                quiet_time: true,
                ..clear()
            },
            MomentConditions {
                power_saving: true,
                ..clear()
            },
        ];
        for c in &blocked {
            assert!(!moment_allowed(c), "{c:?}");
        }
    }

    #[test]
    fn needs_items_blocks_empty_pocket() {
        let empty = MomentConditions {
            pocket_items: 0,
            ..clear()
        };
        assert!(!moment_allowed(&empty));
        let empty_ok = MomentConditions {
            needs_items: false,
            ..empty
        };
        assert!(moment_allowed(&empty_ok));
        let three = MomentConditions {
            pocket_items: 3,
            needs_items: true,
            ..clear()
        };
        assert!(moment_allowed(&three));
    }

    #[test]
    fn away_skips_and_waits_a_period() {
        let near = MomentConditions {
            idle_for: Duration::from_secs(299),
            ..clear()
        };
        assert!(moment_allowed(&near));
        let away = MomentConditions {
            idle_for: Duration::from_secs(300),
            ..clear()
        };
        assert!(!moment_allowed(&away));
    }
}
