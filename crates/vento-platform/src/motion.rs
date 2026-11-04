//! The part of "what the system says" about quiet time that needs no system
//! call, so every target compiles and tests it.

/// Whether a `QUERY_USER_NOTIFICATION_STATE` is one where Windows holds
/// notifications back: busy (2), a full-screen game (3), a presentation (4)
/// or quiet time (6). Not asleep (1), not the accepting state (5), and not
/// the Metro app state (7).
#[cfg(any(test, windows))]
pub(crate) fn quiet_from_state(state: i32) -> bool {
    matches!(state, 2..=4 | 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_states_map() {
        for quiet in [2, 3, 4, 6] {
            assert!(quiet_from_state(quiet), "state {quiet}");
        }
        for open in [1, 5, 7] {
            assert!(!quiet_from_state(open), "state {open}");
        }
    }
}
