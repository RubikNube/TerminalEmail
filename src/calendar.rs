use chrono::{DateTime, Datelike, Duration, Local, Months, NaiveDate};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DateRange {
    pub fn new(a: NaiveDate, b: NaiveDate) -> Self {
        Self {
            start: a.min(b),
            end: a.max(b),
        }
    }

    pub fn contains(self, timestamp: i64) -> bool {
        local_date(timestamp).is_some_and(|date| date >= self.start && date <= self.end)
    }
}

impl std::fmt::Display for DateRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.start == self.end {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}..{}", self.start, self.end)
        }
    }
}

pub fn local_date(timestamp: i64) -> Option<NaiveDate> {
    if timestamp == 0 {
        return None;
    }
    DateTime::from_timestamp(timestamp, 0).map(|date| date.with_timezone(&Local).date_naive())
}

pub struct Calendar {
    pub cursor: NaiveDate,
    pub anchor: Option<NaiveDate>,
}

impl Calendar {
    pub fn new(cursor: NaiveDate) -> Self {
        Self {
            cursor,
            anchor: None,
        }
    }

    pub fn move_days(&mut self, days: i64) {
        if let Some(date) = self.cursor.checked_add_signed(Duration::days(days)) {
            self.cursor = date;
        }
    }

    pub fn move_months(&mut self, months: i32) {
        let amount = Months::new(months.unsigned_abs());
        let date = if months < 0 {
            self.cursor.checked_sub_months(amount)
        } else {
            self.cursor.checked_add_months(amount)
        };
        if let Some(date) = date {
            self.cursor = date;
        }
    }

    pub fn range(&self) -> DateRange {
        DateRange::new(self.anchor.unwrap_or(self.cursor), self.cursor)
    }
}

/// Monday-first calendar rows. Padding stays empty instead of borrowing adjacent dates.
pub fn month_weeks(month: NaiveDate) -> Vec<[Option<NaiveDate>; 7]> {
    let first = month.with_day(1).unwrap();
    let mut weeks = Vec::new();
    let mut row = [None; 7];
    let mut date = first;
    loop {
        let weekday = date.weekday().num_days_from_monday() as usize;
        row[weekday] = Some(date);
        let next = date.succ_opt();
        if weekday == 6 || next.is_none_or(|next| next.month() != first.month()) {
            weeks.push(row);
            row = [None; 7];
        }
        match next {
            Some(next) if next.month() == first.month() => date = next,
            _ => break,
        }
    }
    weeks
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn leap_days_month_clamping_and_reversed_ranges() {
        let mut calendar = Calendar::new(date(2024, 1, 31));
        calendar.move_months(1);
        assert_eq!(calendar.cursor, date(2024, 2, 29));
        calendar.move_days(1);
        assert_eq!(calendar.cursor, date(2024, 3, 1));
        calendar.anchor = Some(date(2024, 3, 5));
        assert_eq!(
            calendar.range(),
            DateRange::new(date(2024, 3, 1), date(2024, 3, 5))
        );
        calendar.move_months(-3);
        assert_eq!(calendar.cursor, date(2023, 12, 1));
    }

    #[test]
    fn monday_first_rows_and_iso_year_boundary() {
        let weeks = month_weeks(date(2024, 2, 1));
        assert_eq!(weeks.len(), 5);
        assert_eq!(weeks[0][0], None);
        assert_eq!(weeks[0][3], Some(date(2024, 2, 1)));
        assert_eq!(weeks[4][3], Some(date(2024, 2, 29)));
        assert_eq!(month_weeks(date(2026, 11, 1)).len(), 6);
        assert_eq!(date(2024, 12, 30).iso_week().week(), 1);
        assert_eq!(local_date(0), None);
        let timestamp = date(2024, 2, 29)
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let local = local_date(timestamp).unwrap();
        assert!(DateRange::new(local, local).contains(timestamp));
        assert!(!DateRange::new(local, local).contains(0));
        assert!(!DateRange::new(local, local).contains(timestamp + 172800));
    }
}
