//! Korean calendar periods the assistant and the worker both name: one
//! week arithmetic, so "이번 주" in an answer and in a push is the same days.
use chrono::{Datelike, Days, NaiveDate};

/// The Monday-to-Sunday week holding `day`.
pub fn week_of(day: NaiveDate) -> (NaiveDate, NaiveDate) {
    let monday = day - Days::new(u64::from(day.weekday().num_days_from_monday()));
    (monday, monday + Days::new(6))
}

/// The rest of `day`'s week: `day` through Sunday.
pub fn rest_of_week(day: NaiveDate) -> (NaiveDate, NaiveDate) {
    (day, week_of(day).1)
}

/// The week after `day`'s.
pub fn next_week(day: NaiveDate) -> (NaiveDate, NaiveDate) {
    week_of(week_of(day).1 + Days::new(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_run_monday_to_sunday() {
        let day = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        // 2026-11-10 is a Tuesday; 11-15 a Sunday.
        assert_eq!(week_of(day(11, 10)), (day(11, 9), day(11, 15)));
        assert_eq!(rest_of_week(day(11, 15)), (day(11, 15), day(11, 15)));
        assert_eq!(next_week(day(11, 15)), (day(11, 16), day(11, 22)));
        assert_eq!(
            next_week(day(12, 30)),
            (
                day(1, 4).with_year(2027).unwrap(),
                day(1, 10).with_year(2027).unwrap()
            )
        );
    }
}
