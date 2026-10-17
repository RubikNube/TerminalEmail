use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Offset, TimeZone};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Period {
    Future,
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Older,
    Unknown,
}

impl Period {
    pub const ALL: [Self; 7] = [
        Self::Future,
        Self::Today,
        Self::Yesterday,
        Self::ThisWeek,
        Self::ThisMonth,
        Self::Older,
        Self::Unknown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Future => "Future",
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This week",
            Self::ThisMonth => "This month",
            Self::Older => "Older",
            Self::Unknown => "Unknown date",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Heading(Period, usize),
    Conversation(usize),
}

pub fn classify<T: TimeZone>(timestamp: i64, today: NaiveDate, zone: &T) -> Period {
    if timestamp == 0 {
        return Period::Unknown;
    }
    let Some(date) = DateTime::from_timestamp(timestamp, 0)
        .and_then(|date| {
            date.naive_utc()
                .checked_add_signed(Duration::seconds(i64::from(
                    zone.offset_from_utc_datetime(&date.naive_utc())
                        .fix()
                        .local_minus_utc(),
                )))
        })
        .map(|date| date.date())
    else {
        return Period::Unknown;
    };
    classify_date(date, today)
}

fn classify_date(date: NaiveDate, today: NaiveDate) -> Period {
    if date > today {
        Period::Future
    } else if date == today {
        Period::Today
    } else if today.signed_duration_since(date).num_days() == 1 {
        Period::Yesterday
    } else if today.signed_duration_since(date).num_days()
        <= i64::from(today.weekday().num_days_from_monday())
    {
        Period::ThisWeek
    } else if date.year() == today.year() && date.month() == today.month() {
        Period::ThisMonth
    } else {
        Period::Older
    }
}

pub fn local_period(timestamp: i64, today: NaiveDate) -> Period {
    classify(timestamp, today, &Local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn periods_use_calendar_boundaries_and_first_matching_section() {
        let today = date("2026-10-17");
        for (value, expected) in [
            ("2026-10-18", Period::Future),
            ("2026-10-17", Period::Today),
            ("2026-10-16", Period::Yesterday),
            ("2026-10-12", Period::ThisWeek),
            ("2026-10-11", Period::ThisMonth),
            ("2026-09-30", Period::Older),
        ] {
            let timestamp = date(value)
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp();
            assert_eq!(classify(timestamp, today, &Utc), expected, "{value}");
        }
        assert_eq!(classify(0, today, &Utc), Period::Unknown);
        assert_eq!(classify(i64::MAX, today, &Utc), Period::Unknown);
        assert_eq!(classify(i64::MIN, today, &Utc), Period::Unknown);
        assert_eq!(
            classify_date(date("2026-10-10"), date("2026-10-12")),
            Period::ThisMonth
        );
        assert_eq!(
            classify_date(date("2026-10-11"), date("2026-10-12")),
            Period::Yesterday
        );
        assert_eq!(
            classify_date(date("2025-12-31"), date("2026-01-02")),
            Period::ThisWeek
        );
        assert_eq!(
            classify_date(date("2025-12-31"), date("2026-01-01")),
            Period::Yesterday
        );
        assert_eq!(
            classify_date(date("2026-02-28"), date("2026-03-02")),
            Period::Older
        );
    }

    #[test]
    fn timestamps_use_local_date_instead_of_utc_date() {
        let today = date("2026-10-17");
        let east = FixedOffset::east_opt(7200).unwrap();
        let west = FixedOffset::west_opt(7200).unwrap();
        let before_midnight = Utc
            .with_ymd_and_hms(2026, 10, 16, 23, 30, 0)
            .unwrap()
            .timestamp();
        assert_eq!(classify(before_midnight, today, &east), Period::Today);
        assert_eq!(classify(before_midnight, today, &west), Period::Yesterday);
        let after_midnight = Utc
            .with_ymd_and_hms(2026, 10, 17, 1, 30, 0)
            .unwrap()
            .timestamp();
        assert_eq!(classify(after_midnight, today, &west), Period::Yesterday);
    }
}
