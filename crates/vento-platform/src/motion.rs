//! The parts of "what the system says" about motion, quiet time and battery
//! saver that need no system call, so every target compiles and tests them.

/// The value `gsettings get` printed for a boolean key: `true` or `false`
/// and a newline. Anything else (a string, empty output) is `None`.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn parse_gsettings_bool(out: &str) -> Option<bool> {
    match out.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

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
    fn gsettings_bool_parses() {
        assert_eq!(parse_gsettings_bool("true\n"), Some(true));
        assert_eq!(parse_gsettings_bool("false"), Some(false));
        assert_eq!(parse_gsettings_bool(""), None);
        assert_eq!(parse_gsettings_bool("'x'"), None);
    }

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
