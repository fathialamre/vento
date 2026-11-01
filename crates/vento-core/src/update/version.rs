//! The `major.minor.patch` version of a release.

use std::fmt;

/// A release version such as `0.3.0`, ordered numerically field by field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// The first number; a breaking change.
    pub major: u32,
    /// The second number; a new feature.
    pub minor: u32,
    /// The third number; a fix.
    pub patch: u32,
}

impl Version {
    /// Reads `0.3.0` or `v0.3.0`. Anything else (a suffix such as `-beta`,
    /// fewer or more than three numbers, signs, spaces) gives `None`.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let major = number(parts.next()?)?;
        let minor = number(parts.next()?)?;
        let patch = number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

/// One dotted field: ASCII digits only, so `+1`, `-1` and spaces are refused.
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_and_without_v() {
        assert_eq!(Version::parse("v0.3.0"), Version::parse("0.3.0"));
        assert!(Version::parse("0.3.0").is_some());
    }

    #[test]
    fn rejects_suffixes_and_junk() {
        for s in ["0.4.0-beta", "0.3", "x.y.z", "", "0.3.0.1", "v", "0.3.+1"] {
            assert_eq!(Version::parse(s), None, "{s}");
        }
    }

    #[test]
    fn orders_numerically() {
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
    }

    #[test]
    fn displays_as_dotted_triple() {
        assert_eq!(Version::parse("v0.3.0").unwrap().to_string(), "0.3.0");
    }
}
