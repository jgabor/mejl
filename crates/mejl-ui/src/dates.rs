//! Relative dates for the list and the reader.
//!
//! Everything is computed against an explicit "now" in an explicit time
//! zone, both taken from the injected [`Clock`](crate::Clock) and the UI
//! config, so output never depends on the machine running the tests.
//!
//! Weeks start on Monday. The list uses a fixed six cell date column, so
//! every format here fits in six cells: `14:02` today, `Thu` this week and
//! yesterday, `Oct 17` this year, and `Oct'24` for older years.

use jiff::Timestamp;
use jiff::Zoned;
use jiff::civil::Date;

/// Width of the list's date column in cells.
pub const LIST_DATE_WIDTH: usize = 6;

/// The heading a list row is grouped under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DateGroup {
    /// Same calendar day as now, or in the future (clock skew).
    Today,
    /// The calendar day before today.
    Yesterday,
    /// Earlier in the current Monday-based week.
    ThisWeek,
    /// Anything older.
    Earlier,
}

impl DateGroup {
    /// The heading text.
    pub fn label(self) -> &'static str {
        match self {
            DateGroup::Today => "Today",
            DateGroup::Yesterday => "Yesterday",
            DateGroup::ThisWeek => "This week",
            DateGroup::Earlier => "Earlier",
        }
    }
}

fn local_date(ts: Timestamp, now: &Zoned) -> Date {
    ts.to_zoned(now.time_zone().clone()).date()
}

fn week_start(today: Date) -> Date {
    let back = i64::from(today.weekday().to_monday_zero_offset());
    today
        .checked_sub(jiff::Span::new().days(back))
        .unwrap_or(today)
}

fn is_yesterday(d: Date, today: Date) -> bool {
    today.yesterday().is_ok_and(|y| y == d)
}

/// The group heading for a message received at `ts`.
pub fn group(ts: Timestamp, now: &Zoned) -> DateGroup {
    let today = now.date();
    let d = local_date(ts, now);
    if d >= today {
        DateGroup::Today
    } else if is_yesterday(d, today) {
        DateGroup::Yesterday
    } else if d >= week_start(today) {
        DateGroup::ThisWeek
    } else {
        DateGroup::Earlier
    }
}

/// The list's date column text, at most [`LIST_DATE_WIDTH`] cells.
pub fn list_date(ts: Timestamp, now: &Zoned) -> String {
    let z = ts.to_zoned(now.time_zone().clone());
    let today = now.date();
    let d = z.date();
    if d >= today {
        z.strftime("%H:%M").to_string()
    } else if is_yesterday(d, today) || d >= week_start(today) {
        z.strftime("%a").to_string()
    } else if d.year() == today.year() {
        z.strftime("%b %-d").to_string()
    } else {
        z.strftime("%b'%y").to_string()
    }
}

/// A compact date for a collapsed message in the reader pane: `14:02`
/// today, `Mon 10:14` this week, `Oct 17` this year, `2024-10-03` older.
pub fn pane_date(ts: Timestamp, now: &Zoned) -> String {
    let z = ts.to_zoned(now.time_zone().clone());
    let today = now.date();
    let d = z.date();
    if d >= today {
        z.strftime("%H:%M").to_string()
    } else if is_yesterday(d, today) || d >= week_start(today) {
        z.strftime("%a %H:%M").to_string()
    } else if d.year() == today.year() {
        z.strftime("%b %-d").to_string()
    } else {
        z.strftime("%Y-%m-%d").to_string()
    }
}

/// Date and time for a collapsed message in the full reader:
/// `Oct 20 16:05`, or `2024-10-03` for older years.
pub fn full_date(ts: Timestamp, now: &Zoned) -> String {
    let z = ts.to_zoned(now.time_zone().clone());
    if z.date().year() == now.date().year() {
        z.strftime("%b %-d %H:%M").to_string()
    } else {
        z.strftime("%Y-%m-%d").to_string()
    }
}

/// The absolute timestamp for a message header: `Fri 23 Oct 09:12`, with
/// the year added when it is not the current one.
pub fn header_date(ts: Timestamp, now: &Zoned) -> String {
    let z = ts.to_zoned(now.time_zone().clone());
    if z.date().year() == now.date().year() {
        z.strftime("%a %-d %b %H:%M").to_string()
    } else {
        z.strftime("%a %-d %b %Y %H:%M").to_string()
    }
}

/// The status line clock: `14:02:31`.
pub fn clock(now: &Zoned) -> String {
    now.strftime("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn now() -> Zoned {
        let ts: Timestamp = "2026-10-23T14:02:31Z".parse().unwrap();
        ts.to_zoned(TimeZone::UTC)
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn groups_follow_monday_weeks() {
        let now = now();
        assert_eq!(group(at("2026-10-23T07:30:00Z"), &now), DateGroup::Today);
        assert_eq!(group(at("2026-10-24T07:30:00Z"), &now), DateGroup::Today);
        assert_eq!(
            group(at("2026-10-22T07:30:00Z"), &now),
            DateGroup::Yesterday
        );
        assert_eq!(group(at("2026-10-19T00:00:00Z"), &now), DateGroup::ThisWeek);
        assert_eq!(group(at("2026-10-18T23:59:59Z"), &now), DateGroup::Earlier);
        assert_eq!(group(at("2026-10-17T10:00:00Z"), &now), DateGroup::Earlier);
    }

    #[test]
    fn yesterday_on_a_monday_is_still_yesterday() {
        let monday = at("2026-10-19T09:00:00Z").to_zoned(TimeZone::UTC);
        let sunday = at("2026-10-18T09:00:00Z");
        assert_eq!(group(sunday, &monday), DateGroup::Yesterday);
        assert_eq!(list_date(sunday, &monday), "Sun");
    }

    #[test]
    fn list_dates_fit_the_column() {
        let now = now();
        let cases = [
            ("2026-10-23T14:02:00Z", "14:02"),
            ("2026-10-22T10:00:00Z", "Thu"),
            ("2026-10-19T10:00:00Z", "Mon"),
            ("2026-10-17T10:00:00Z", "Oct 17"),
            ("2026-10-03T10:00:00Z", "Oct 3"),
            ("2024-10-03T10:00:00Z", "Oct'24"),
        ];
        for (ts, want) in cases {
            let got = list_date(at(ts), &now);
            assert_eq!(got, want);
            assert!(crate::text::width(&got) <= LIST_DATE_WIDTH);
        }
    }

    #[test]
    fn reader_dates() {
        let now = now();
        assert_eq!(pane_date(at("2026-10-19T10:14:00Z"), &now), "Mon 10:14");
        assert_eq!(pane_date(at("2026-10-23T09:12:00Z"), &now), "09:12");
        assert_eq!(full_date(at("2026-10-20T16:05:00Z"), &now), "Oct 20 16:05");
        assert_eq!(full_date(at("2025-10-20T16:05:00Z"), &now), "2025-10-20");
        assert_eq!(
            header_date(at("2026-10-23T09:12:00Z"), &now),
            "Fri 23 Oct 09:12"
        );
        assert_eq!(
            header_date(at("2025-03-14T09:12:00Z"), &now),
            "Fri 14 Mar 2025 09:12"
        );
        assert_eq!(clock(&now), "14:02:31");
    }

    #[test]
    fn time_zone_changes_the_day() {
        let tz = TimeZone::fixed(jiff::tz::offset(-10));
        let now = at("2026-10-23T14:02:31Z").to_zoned(tz);
        // 07:30 UTC is 21:30 the day before at UTC-10.
        assert_eq!(
            group(at("2026-10-23T07:30:00Z"), &now),
            DateGroup::Yesterday
        );
    }
}
