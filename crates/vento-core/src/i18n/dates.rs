//! Times and dates in words of the interface language: month names and
//! the order of day, month and time come from the catalog, so each language
//! writes them its own way. Digits stay Western.

use jiff::civil::{Date, Time};

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
