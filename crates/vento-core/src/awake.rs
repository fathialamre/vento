//! Keep Awake: what a chip of the Quick panel asks for, the session it
//! starts and how the panel words it. Pure: callers pass the time and the
//! time zone.

use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::i18n::dates::{day_month, time_hm};
use crate::i18n::{fill, tr};

/// The longest the app waits between two looks at a running session. The
/// app's timer runs on a clock that stops while the computer sleeps, so a
/// session that ended during a sleep ends at most this long after waking.
pub const MAX_CHECK: Duration = Duration::from_secs(60);

/// The timed chips the panel shows until the user picks others, in
/// minutes.
pub const DEFAULT_TIMES: [u32; 6] = [15, 30, 60, 2 * 60, 4 * 60, 8 * 60];

/// The most timed chips the panel shows: one row of them.
pub const MAX_TIMES: usize = 6;

/// The shortest and longest time a chip can hold, in minutes.
pub const MIN_TIME: u32 = 5;
pub const MAX_TIME: u32 = 24 * 60;

/// `times` as the panel shows them: each within [`MIN_TIME`] and
/// [`MAX_TIME`], once, shortest first, at most [`MAX_TIMES`] (the
/// shortest kept).
pub fn tidy_times(times: &[u32]) -> Vec<u32> {
    let mut tidy: Vec<u32> = times
        .iter()
        .copied()
        .filter(|m| (MIN_TIME..=MAX_TIME).contains(m))
        .collect();
    tidy.sort_unstable();
    tidy.dedup();
    tidy.truncate(MAX_TIMES);
    tidy
}

/// The time next to `minutes` on the Settings window's stepper, up or
/// down: every 5 minutes under an hour, every 15 under two hours, every 30
/// under six and every hour above, within [`MIN_TIME`] and [`MAX_TIME`].
pub fn step_time(minutes: u32, up: bool) -> u32 {
    let ladder = || {
        (MIN_TIME..60)
            .step_by(5)
            .chain((60..120).step_by(15))
            .chain((120..360).step_by(30))
            .chain((360..=MAX_TIME).step_by(60))
    };
    let next = if up {
        ladder().find(|&m| m > minutes)
    } else {
        ladder().filter(|&m| m < minutes).last()
    };
    next.unwrap_or(minutes.clamp(MIN_TIME, MAX_TIME))
}

/// What the Keep Awake tile and the menu's Start Keep Awake start: a timed
/// chip for `minutes`, or [`Chip::Forever`] for 0.
pub fn start_chip(minutes: u32) -> Chip {
    if minutes == 0 {
        Chip::Forever
    } else {
        Chip::For(Duration::from_secs(u64::from(minutes) * 60))
    }
}

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

    /// What the panel says while it runs, in the interface language:
    /// `"Awake for 1 h 23 min more"`, `"Awake until 18:30"` (`"… tomorrow"`
    /// or `"… on 5 Nov"` when the end is not today in `tz`) or
    /// `"Awake until you stop it"`.
    pub fn status(&self, now: Timestamp, tz: &TimeZone) -> String {
        let Some(ends) = self.ends else {
            return tr("Awake until you stop it");
        };
        match self.chip {
            Chip::For(_) => {
                let left = left_until(ends, now);
                if left < Duration::from_secs(60) {
                    tr("Awake for less than a minute more")
                } else {
                    fill(
                        &tr("Awake for {duration} more"),
                        &[("duration", &duration_text(whole_minutes(left)))],
                    )
                }
            }
            Chip::Until { .. } | Chip::Forever => {
                let end = ends.to_zoned(tz.clone());
                let today = now.to_zoned(tz.clone()).date();
                let time = time_hm(end.time());
                let day = end.date();
                if day == today {
                    fill(&tr("Awake until {time}"), &[("time", &time)])
                } else if today.tomorrow().ok() == Some(day) {
                    fill(&tr("Awake until {time} tomorrow"), &[("time", &time)])
                } else {
                    fill(
                        &tr("Awake until {time} on {date}"),
                        &[("time", &time), ("date", &day_month(day))],
                    )
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

/// `minutes` as hours and minutes, in the interface language: `"1 h 23 min"`,
/// `"45 min"`, `"2 h"`.
pub fn duration_text(minutes: u64) -> String {
    let (h, m) = ((minutes / 60).to_string(), (minutes % 60).to_string());
    match (minutes / 60, minutes % 60) {
        (0, _) => fill(&tr("{m} min"), &[("m", &m)]),
        (_, 0) => fill(&tr("{h} h"), &[("h", &h)]),
        _ => fill(&tr("{h} h {m} min"), &[("h", &h), ("m", &m)]),
    }
}

/// `minutes` on a chip of the panel, where six share a row: as
/// [`duration_text`], but `"1 h 30"` for hours and minutes.
pub fn chip_text(minutes: u32) -> String {
    if minutes >= 60 && !minutes.is_multiple_of(60) {
        let (h, m) = ((minutes / 60).to_string(), (minutes % 60).to_string());
        fill(&tr("{h} h {m}"), &[("h", &h), ("m", &m)])
    } else {
        duration_text(u64::from(minutes))
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
        // Berlin's rule, given whole: Windows has no zone database to look
        // the name up in.
        let tz = TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
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

    #[test]
    fn status_reads_in_arabic() {
        use crate::i18n::with_language;
        with_language("ar", || {
            let forever = Session::start(Chip::Forever, at(10, 0, 0), &utc());
            assert_eq!(forever.status(at(10, 0, 0), &utc()), "مستيقظ حتى توقفه");

            let timed = Session::start(Chip::For(minutes(83)), at(10, 0, 0), &utc());
            assert_eq!(
                timed.status(at(10, 0, 0), &utc()),
                "مستيقظ لمدة 1 س 23 د أخرى"
            );
            assert_eq!(timed.status(at(11, 22, 1), &utc()), "مستيقظ لأقل من دقيقة");

            let until = |hour, minute| Chip::Until { hour, minute };
            let today = Session::start(until(18, 30), at(10, 0, 0), &utc());
            assert_eq!(today.status(at(10, 0, 0), &utc()), "مستيقظ حتى 18:30");
            let tomorrow = Session::start(until(7, 0), at(10, 0, 0), &utc());
            assert_eq!(
                tomorrow.status(at(10, 0, 0), &utc()),
                "مستيقظ حتى 07:00 غداً"
            );
        });
    }

    #[test]
    fn an_end_beyond_tomorrow_names_its_date() {
        // Seen from the 25th, an end on the 28th is named by its date.
        let ends = at(9, 5, 0).saturating_add(minutes(3 * 24 * 60)).unwrap();
        let session = Session {
            chip: Chip::Until { hour: 9, minute: 5 },
            ends: Some(ends),
        };
        assert_eq!(
            session.status(at(10, 0, 0), &utc()),
            "Awake until 09:05 on 28 Oct"
        );
        crate::i18n::with_language("ar", || {
            assert_eq!(
                session.status(at(10, 0, 0), &utc()),
                "مستيقظ حتى 09:05 يوم 28 أكتوبر"
            );
        });
    }

    #[test]
    fn durations_read_in_both_languages() {
        assert_eq!(duration_text(45), "45 min");
        assert_eq!(duration_text(120), "2 h");
        assert_eq!(duration_text(90), "1 h 30 min");
        crate::i18n::with_language("ar", || {
            assert_eq!(duration_text(45), "45 د");
            assert_eq!(duration_text(120), "2 س");
            assert_eq!(duration_text(90), "1 س 30 د");
        });
    }

    #[test]
    fn chips_shorten_hours_and_minutes() {
        assert_eq!(chip_text(45), "45 min");
        assert_eq!(chip_text(180), "3 h");
        assert_eq!(chip_text(90), "1 h 30");
        crate::i18n::with_language("ar", || {
            assert_eq!(chip_text(45), "45 د");
            assert_eq!(chip_text(90), "1 س 30");
        });
    }

    #[test]
    fn times_are_tidied_into_one_short_sorted_row() {
        assert_eq!(tidy_times(&DEFAULT_TIMES), DEFAULT_TIMES);
        assert_eq!(tidy_times(&[60, 15, 60, 0, 2, 2000, 45]), [15, 45, 60]);
        assert_eq!(
            tidy_times(&[1, 5, 10, 15, 20, 25, 30, 35]),
            [5, 10, 15, 20, 25, 30]
        );
        assert!(tidy_times(&[]).is_empty());
    }

    #[test]
    fn the_stepper_moves_in_steps_that_grow_with_the_time() {
        let up = |m| step_time(m, true);
        let down = |m| step_time(m, false);
        assert_eq!(up(45), 50);
        assert_eq!(up(55), 60);
        assert_eq!(up(60), 75);
        assert_eq!(up(105), 120);
        assert_eq!(up(120), 150);
        assert_eq!(up(330), 360);
        assert_eq!(up(360), 420);
        assert_eq!(up(MAX_TIME), MAX_TIME);
        assert_eq!(down(60), 55);
        assert_eq!(down(150), 120);
        assert_eq!(down(MIN_TIME), MIN_TIME);
        // A time off the ladder (a hand-edited file) steps onto it.
        assert_eq!(up(47), 50);
        assert_eq!(down(47), 45);
    }

    #[test]
    fn the_tile_starts_until_stopped_or_a_timed_chip() {
        assert_eq!(start_chip(0), Chip::Forever);
        assert_eq!(start_chip(90), Chip::For(minutes(90)));
    }
}
