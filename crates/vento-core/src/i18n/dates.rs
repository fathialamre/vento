//! Times and dates in words of the interface language: month names and
//! the order of day, month and time come from the catalog, so each language
//! writes them its own way. Digits stay Western.

use jiff::civil::{Date, DateTime, Time, Weekday};

use super::{fill, trc};

/// The name of month `month` (1 to 12): English writes it short, as
/// `"Nov"`, and a language may write it out in full. Anything else is `""`.
fn month_name(month: i8) -> String {
    match month {
        1 => trc("month", "Jan"),
        2 => trc("month", "Feb"),
        3 => trc("month", "Mar"),
        4 => trc("month", "Apr"),
        5 => trc("month", "May"),
        6 => trc("month", "Jun"),
        7 => trc("month", "Jul"),
        8 => trc("month", "Aug"),
        9 => trc("month", "Sep"),
        10 => trc("month", "Oct"),
        11 => trc("month", "Nov"),
        12 => trc("month", "Dec"),
        _ => String::new(),
    }
}

/// The day and month of `date`: `"5 Nov"`, `"5 نوفمبر"`.
pub fn day_month(date: Date) -> String {
    fill(
        &trc("date", "{day} {month}"),
        &[
            ("day", &date.day().to_string()),
            ("month", &month_name(date.month())),
        ],
    )
}

/// The time of day on a 24-hour clock, hour and minute: `"14:30"`.
pub fn time_hm(time: Time) -> String {
    fill(
        &trc("time", "{hour}:{minute}"),
        &[
            ("hour", &format!("{:02}", time.hour())),
            ("minute", &format!("{:02}", time.minute())),
        ],
    )
}

/// The name of the day of the week `date` falls on: English writes it short,
/// as `"Wed"`, and a language may write it out in full (`"الأربعاء"`).
pub fn weekday_short(date: Date) -> String {
    match date.weekday() {
        Weekday::Sunday => trc("weekday", "Sun"),
        Weekday::Monday => trc("weekday", "Mon"),
        Weekday::Tuesday => trc("weekday", "Tue"),
        Weekday::Wednesday => trc("weekday", "Wed"),
        Weekday::Thursday => trc("weekday", "Thu"),
        Weekday::Friday => trc("weekday", "Fri"),
        Weekday::Saturday => trc("weekday", "Sat"),
    }
}

/// The time of day on a 12-hour clock with the period: `"3:20 PM"`,
/// `"3:20 م"`. Midnight is `"12:00 AM"`, noon `"12:00 PM"`.
fn time_12h(time: Time) -> String {
    let hour = match time.hour() % 12 {
        0 => 12,
        h => h,
    };
    let period = if time.hour() < 12 {
        trc("time", "AM")
    } else {
        trc("time", "PM")
    };
    fill(
        &trc("time", "{hour}:{minute} {period}"),
        &[
            ("hour", &hour.to_string()),
            ("minute", &format!("{:02}", time.minute())),
            ("period", &period),
        ],
    )
}

/// The date with its year and the time of day on a 12-hour clock:
/// `"5 Nov 2026, 3:20 PM"`, `"5 نوفمبر 2026، 3:20 م"`.
pub fn date_time(dt: DateTime) -> String {
    fill(
        &trc("date", "{day} {month} {year}, {time}"),
        &[
            ("day", &dt.day().to_string()),
            ("month", &month_name(dt.month())),
            ("year", &dt.year().to_string()),
            ("time", &time_12h(dt.time())),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_language;

    fn date(month: i8, day: i8) -> Date {
        jiff::civil::date(2026, month, day)
    }

    #[test]
    fn a_day_and_month_read_in_english() {
        assert_eq!(day_month(date(11, 5)), "5 Nov");
        assert_eq!(day_month(date(1, 31)), "31 Jan");
        assert_eq!(day_month(date(5, 1)), "1 May");
    }

    #[test]
    fn a_day_and_month_read_in_arabic_with_the_full_month_name() {
        with_language("ar", || {
            assert_eq!(day_month(date(11, 5)), "5 نوفمبر");
            let months = [
                "يناير",
                "فبراير",
                "مارس",
                "أبريل",
                "مايو",
                "يونيو",
                "يوليو",
                "أغسطس",
                "سبتمبر",
                "أكتوبر",
                "نوفمبر",
                "ديسمبر",
            ];
            for (i, name) in months.into_iter().enumerate() {
                let month = i8::try_from(i + 1).unwrap();
                assert_eq!(day_month(date(month, 1)), format!("1 {name}"));
            }
        });
    }

    fn at(year: i16, month: i8, day: i8, hour: i8, minute: i8) -> DateTime {
        jiff::civil::date(year, month, day).at(hour, minute, 0, 0)
    }

    #[test]
    fn a_date_and_time_read_in_english() {
        assert_eq!(date_time(at(2026, 11, 5, 15, 20)), "5 Nov 2026, 3:20 PM");
        assert_eq!(date_time(at(2026, 11, 5, 9, 5)), "5 Nov 2026, 9:05 AM");
        assert_eq!(date_time(at(2026, 11, 5, 0, 0)), "5 Nov 2026, 12:00 AM");
        assert_eq!(date_time(at(2026, 11, 5, 12, 0)), "5 Nov 2026, 12:00 PM");
        assert_eq!(date_time(at(2026, 1, 31, 23, 59)), "31 Jan 2026, 11:59 PM");
    }

    #[test]
    fn a_date_and_time_read_in_arabic() {
        with_language("ar", || {
            assert_eq!(date_time(at(2026, 11, 5, 15, 20)), "5 نوفمبر 2026، 3:20 م");
            assert_eq!(date_time(at(2026, 11, 5, 9, 5)), "5 نوفمبر 2026، 9:05 ص");
            assert_eq!(date_time(at(2026, 11, 5, 0, 0)), "5 نوفمبر 2026، 12:00 ص");
            assert_eq!(date_time(at(2026, 11, 5, 12, 0)), "5 نوفمبر 2026، 12:00 م");
        });
    }

    #[test]
    fn the_english_date_and_time_match_strftime_everywhere() {
        for month in 1..=12 {
            for (day, hour, minute) in
                [(1, 0, 0), (9, 1, 7), (15, 11, 59), (28, 12, 0), (28, 23, 5)]
            {
                let dt = at(2026, month, day, hour, minute);
                assert_eq!(
                    date_time(dt),
                    dt.strftime("%-d %b %Y, %-I:%M %p").to_string(),
                    "{dt}"
                );
            }
        }
    }

    #[test]
    fn a_weekday_reads_short_in_english_and_in_full_in_arabic() {
        // 2026-11-01 is a Sunday.
        let english = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let arabic = [
            "الأحد",
            "الاثنين",
            "الثلاثاء",
            "الأربعاء",
            "الخميس",
            "الجمعة",
            "السبت",
        ];
        for day in 1..=7 {
            let d = date(11, day);
            let i = usize::try_from(day - 1).unwrap();
            assert_eq!(weekday_short(d), english[i]);
            assert_eq!(weekday_short(d), d.strftime("%a").to_string());
            with_language("ar", || assert_eq!(weekday_short(d), arabic[i]));
        }
    }

    #[test]
    fn every_month_has_a_distinct_english_name() {
        let names: std::collections::BTreeSet<_> =
            (1..=12).map(|m| day_month(date(m, 1))).collect();
        assert_eq!(names.len(), 12);
    }

    #[test]
    fn the_time_is_two_digits_each_in_both_languages() {
        let t = |h, m| jiff::civil::time(h, m, 0, 0);
        assert_eq!(time_hm(t(14, 30)), "14:30");
        assert_eq!(time_hm(t(7, 5)), "07:05");
        assert_eq!(time_hm(t(0, 0)), "00:00");
        with_language("ar", || {
            assert_eq!(time_hm(t(14, 30)), "14:30");
            assert_eq!(time_hm(t(7, 5)), "07:05");
        });
    }
}
