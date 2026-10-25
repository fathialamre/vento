//! Keep Awake: what a chip of the settings panel asks for, the session it
//! starts and how the panel words it. Pure: callers pass the time and the
//! time zone.

use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

/// The longest the app waits between two looks at a running session. The
/// app's timer runs on a clock that stops while the computer sleeps, so a
/// session that ended during a sleep ends at most this long after waking.
pub const MAX_CHECK: Duration = Duration::from_secs(60);

/// The timed chips, in the order shown.
pub const TIMED_CHIPS: [Duration; 6] = [
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(2 * 60 * 60),
    Duration::from_secs(4 * 60 * 60),
    Duration::from_secs(8 * 60 * 60),
];

/// What a chip asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    /// A while, from the click.
    For(Duration),
    /// Until this time of day: today, or tomorrow when that is past.
    Until { hour: i8, minute: i8 },
    /// Until stopped.
    Forever,
}

/// A running Keep Awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    /// The chip that started it.
    pub chip: Chip,
    /// When it ends; `None` for [`Chip::Forever`].
    pub ends: Option<Timestamp>,
}

impl Session {
    /// The session `chip` starts at `now`, an [`Chip::Until`] time read in
    /// `tz`.
    pub fn start(chip: Chip, now: Timestamp, tz: &TimeZone) -> Self {
        let ends = match chip {
            Chip::For(length) => Some(now.saturating_add(length).unwrap_or(Timestamp::MAX)),
            Chip::Until { hour, minute } => Some(next_time_of_day(hour, minute, now, tz)),
            Chip::Forever => None,
        };
        Session { chip, ends }
    }

    /// Whether it has ended by `now`.
    pub fn is_over(&self, now: Timestamp) -> bool {
        self.ends.is_some_and(|ends| ends <= now)
    }

    /// What the panel says while it runs: `"Awake for 1 h 23 min more"`,
    /// `"Awake until 18:30"` (`"… tomorrow"` when the end is not today in
    /// `tz`) or `"Awake until you stop it"`.
    pub fn status(&self, now: Timestamp, tz: &TimeZone) -> String {
        let Some(ends) = self.ends else {
            return "Awake until you stop it".into();
        };
        match self.chip {
            Chip::For(_) => {
                let left = left_until(ends, now);
                if left < Duration::from_secs(60) {
                    "Awake for less than a minute more".into()
                } else {
                    format!("Awake for {} more", minutes_text(whole_minutes(left)))
                }
            }
            Chip::Until { .. } | Chip::Forever => {
                let end = ends.to_zoned(tz.clone());
                let today = now.to_zoned(tz.clone()).date();
                let time = end.strftime("%H:%M");
                match day_word(end.date(), today) {
                    Some(day) => format!("Awake until {time} {day}"),
                    None => format!("Awake until {time}"),
                }
            }
        }
    }

    /// How long until [`Session::status`] or [`Session::is_over`] may next
    /// change, at most [`MAX_CHECK`]; `None` when neither will by itself.
    pub fn next_check(&self, now: Timestamp) -> Option<Duration> {
        let ends = self.ends?;
        let left = left_until(ends, now);
        let change = match self.chip {
            // The minutes shown are rounded up, so they change as `left`
            // crosses each whole minute.
            Chip::For(_) if left >= Duration::from_secs(60) => {
                left - Duration::from_secs((whole_minutes(left) - 1) * 60)
            }
            _ => left,
        };
        Some(change.min(MAX_CHECK))
    }
}

/// When the Until… popup opens: the next whole hour at least 30 minutes
/// after `now` in `tz`, as (hour, minute).
pub fn default_until(now: Timestamp, tz: &TimeZone) -> (i8, i8) {
    let here = now.to_zoned(tz.clone());
    let mut hour = (here.hour() + 1) % 24;
    if here.minute() >= 30 {
        hour = (hour + 1) % 24;
    }
    (hour, 0)
}

/// The next time `hour:minute` comes round in `tz` after `now`.
fn next_time_of_day(hour: i8, minute: i8, now: Timestamp, tz: &TimeZone) -> Timestamp {
    let today = now.to_zoned(tz.clone()).date();
    let at = |date: Date| {
        date.at(hour.clamp(0, 23), minute.clamp(0, 59), 0, 0)
            .to_zoned(tz.clone())
            .map(|z| z.timestamp())
    };
    match at(today) {
        Ok(t) if t > now => t,
        _ => today
            .tomorrow()
            .ok()
            .and_then(|d| at(d).ok())
            .unwrap_or(Timestamp::MAX),
    }
}

/// How long from `now` to `ends`; zero once past.
fn left_until(ends: Timestamp, now: Timestamp) -> Duration {
    Duration::try_from(ends.duration_since(now)).unwrap_or(Duration::ZERO)
}

/// `left` in minutes, rounded up.
fn whole_minutes(left: Duration) -> u64 {
    u64::try_from(left.as_nanos().div_ceil(60_000_000_000)).unwrap_or(u64::MAX)
}

/// `"1 h 23 min"`, `"45 min"`, `"2 h"`.
fn minutes_text(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// `None` for `today`, `"tomorrow"` for the day after, else the date.
fn day_word(day: Date, today: Date) -> Option<String> {
    if day == today {
        None
    } else if today.tomorrow().ok() == Some(day) {
        Some("tomorrow".into())
    } else {
        Some(day.strftime("on %-d %b").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    /// 2026-10-25 at `hh:mm:ss` UTC.
    fn at(hh: i8, mm: i8, ss: i8) -> Timestamp {
        jiff::civil::date(2026, 10, 25)
            .at(hh, mm, ss, 0)
            .to_zoned(utc())
            .unwrap()
            .timestamp()
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn a_timed_chip_ends_that_long_after_the_click() {
        let s = Session::start(Chip::For(minutes(60)), at(10, 0, 0), &utc());
        assert_eq!(s.ends, Some(at(11, 0, 0)));
        assert!(!s.is_over(at(10, 59, 59)));
        assert!(s.is_over(at(11, 0, 0)));
    }

    #[test]
    fn forever_never_ends() {
        let s = Session::start(Chip::Forever, at(10, 0, 0), &utc());
        assert_eq!(s.ends, None);
        assert!(!s.is_over(Timestamp::MAX));
        assert_eq!(s.status(at(10, 0, 0), &utc()), "Awake until you stop it");
        assert_eq!(s.next_check(at(10, 0, 0)), None);
    }

    #[test]
    fn until_is_today_or_else_tomorrow() {
        let later = Session::start(
            Chip::Until {
                hour: 18,
                minute: 30,
            },
            at(10, 0, 0),
            &utc(),
        );
        assert_eq!(later.ends, Some(at(18, 30, 0)));
        assert_eq!(later.status(at(10, 0, 0), &utc()), "Awake until 18:30");

        let past = Session::start(Chip::Until { hour: 7, minute: 0 }, at(10, 0, 0), &utc());
        let tomorrow_7 = at(7, 0, 0).saturating_add(minutes(24 * 60)).unwrap();
        assert_eq!(past.ends, Some(tomorrow_7));
        assert_eq!(
            past.status(at(10, 0, 0), &utc()),
            "Awake until 07:00 tomorrow"
        );
        // After midnight the same end is today.
        let after_midnight = at(0, 30, 0).saturating_add(minutes(24 * 60)).unwrap();
        assert_eq!(past.status(after_midnight, &utc()), "Awake until 07:00");

        // Now itself is past: tomorrow.
        let now = Session::start(
            Chip::Until {
                hour: 10,
                minute: 0,
            },
            at(10, 0, 0),
            &utc(),
        );
        assert_eq!(now.ends, at(10, 0, 0).saturating_add(minutes(24 * 60)).ok());
    }

    #[test]
    fn until_reads_the_time_in_the_given_zone() {
        let tz = TimeZone::fixed(jiff::tz::offset(2));
        // 10:00 UTC is 12:00 at +02: 18:30 there is 16:30 UTC.
        let s = Session::start(
            Chip::Until {
                hour: 18,
                minute: 30,
            },
            at(10, 0, 0),
            &tz,
        );
        assert_eq!(s.ends, Some(at(16, 30, 0)));
        assert_eq!(s.status(at(10, 0, 0), &tz), "Awake until 18:30");
    }

    #[test]
    fn until_across_a_clock_change_lands_on_the_wall_time() {
        let tz = TimeZone::get("Europe/Berlin").unwrap();
        // 2026-10-25 03:00 summer time becomes 02:00 winter time; from
        // 00:30 local (22:30 UTC the day before), 08:00 local is 07:00 UTC.
        let now = jiff::civil::date(2026, 10, 25)
            .at(0, 30, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp();
        let s = Session::start(Chip::Until { hour: 8, minute: 0 }, now, &tz);
        assert_eq!(s.ends, Some(at(7, 0, 0)));
        assert_eq!(s.status(now, &tz), "Awake until 08:00");
    }

    #[test]
    fn timed_status_counts_down_in_whole_minutes() {
        let s = Session::start(Chip::For(minutes(83)), at(10, 0, 0), &utc());
        let status = |t| s.status(t, &utc());
        assert_eq!(status(at(10, 0, 0)), "Awake for 1 h 23 min more");
        assert_eq!(status(at(10, 0, 1)), "Awake for 1 h 23 min more");
        assert_eq!(status(at(10, 23, 0)), "Awake for 1 h more");
        // 59:59 left rounds up to an hour.
        assert_eq!(status(at(10, 23, 1)), "Awake for 1 h more");
        assert_eq!(status(at(10, 38, 0)), "Awake for 45 min more");
        assert_eq!(status(at(11, 22, 0)), "Awake for 1 min more");
        assert_eq!(status(at(11, 22, 1)), "Awake for less than a minute more");
        let long = Session::start(Chip::For(minutes(8 * 60)), at(10, 0, 0), &utc());
        assert_eq!(long.status(at(10, 0, 0), &utc()), "Awake for 8 h more");
    }

    #[test]
    fn next_check_comes_when_the_minutes_shown_change() {
        let s = Session::start(Chip::For(minutes(3)), at(10, 0, 0), &utc());
        // 3:00 left shows "3 min" until 2:00 left.
        assert_eq!(s.next_check(at(10, 0, 0)), Some(minutes(1)));
        // 2:30 left shows "3 min" until 2:00 left.
        assert_eq!(s.next_check(at(10, 0, 30)), Some(Duration::from_secs(30)));
        // Under a minute left: at the end.
        assert_eq!(s.next_check(at(10, 2, 20)), Some(Duration::from_secs(40)));
        assert_eq!(s.next_check(at(10, 5, 0)), Some(Duration::ZERO));
    }

    #[test]
    fn next_check_is_at_most_a_minute_so_a_sleep_past_the_end_ends_it() {
        let s = Session::start(
            Chip::Until {
                hour: 18,
                minute: 0,
            },
            at(10, 0, 0),
            &utc(),
        );
        assert_eq!(s.next_check(at(10, 0, 0)), Some(MAX_CHECK));
        assert_eq!(s.next_check(at(17, 59, 50)), Some(Duration::from_secs(10)));
        // Woken after the end: over at the next look.
        assert!(s.is_over(at(19, 0, 0)));
    }

    #[test]
    fn the_popup_opens_at_the_next_hour_at_least_half_an_hour_away() {
        assert_eq!(default_until(at(10, 0, 0), &utc()), (11, 0));
        assert_eq!(default_until(at(10, 29, 59), &utc()), (11, 0));
        assert_eq!(default_until(at(10, 30, 0), &utc()), (12, 0));
        assert_eq!(default_until(at(23, 10, 0), &utc()), (0, 0));
        assert_eq!(default_until(at(23, 45, 0), &utc()), (1, 0));
    }
}
