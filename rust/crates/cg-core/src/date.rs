//! Calendar days written exactly YYYY-MM-DD (`facts_extractor.valid_date`): no trimming, no repair, no other format.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day {
    /// Days since 0001-01-01 (day 1), so ordering and subtraction are plain integer operations.
    ordinal: i64,
    y: i32,
    m: u8,
    d: u8,
}

fn is_leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i32, m: u8) -> u8 {
    match m {
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Day {
    /// Some(day) only for ten ASCII characters `dddd-dd-dd` that are a real date in years 1 to 9999.
    pub fn parse(text: &str) -> Option<Day> {
        let b = text.as_bytes();
        if b.len() != 10 {
            return None;
        }
        for (i, c) in b.iter().enumerate() {
            let ok = if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() };
            if !ok {
                return None;
            }
        }
        let n = |i: usize| (b[i] - b'0') as i32;
        let y = n(0) * 1000 + n(1) * 100 + n(2) * 10 + n(3);
        let m = (n(5) * 10 + n(6)) as u8;
        let d = (n(8) * 10 + n(9)) as u8;
        if y < 1 || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
            return None;
        }
        let yy = (y - 1) as i64;
        let mut ordinal = yy * 365 + yy / 4 - yy / 100 + yy / 400;
        const CUM: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        ordinal += CUM[(m - 1) as usize] + d as i64;
        if m > 2 && is_leap(y) {
            ordinal += 1;
        }
        Some(Day { ordinal, y, m, d })
    }

    /// `(a - b).days`
    pub fn days_since(&self, other: &Day) -> i64 {
        self.ordinal - other.ordinal
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_dates_parse_and_print_back() {
        assert_eq!(Day::parse("2026-10-09").unwrap().to_string(), "2026-10-09");
        assert!(Day::parse("2024-02-29").is_some());
        assert!(Day::parse("2025-02-29").is_none());
        assert!(Day::parse("1900-02-29").is_none());
        assert!(Day::parse("2000-02-29").is_some());
    }

    #[test]
    fn only_the_exact_format_is_accepted() {
        for bad in ["20261231", "2026-W52-4", "2026-1-1", " 2026-10-09", "2026-10-09 ", "2026/10/09", "0000-01-01", "2026-13-01", "2026-00-10", "２０２６-10-09", ""] {
            assert!(Day::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn differences_count_days_across_months_and_leap_years() {
        let a = Day::parse("2024-03-01").unwrap();
        let b = Day::parse("2024-02-28").unwrap();
        assert_eq!(a.days_since(&b), 2);
        assert_eq!(Day::parse("2026-01-01").unwrap().days_since(&Day::parse("2025-01-01").unwrap()), 365);
        assert!(b < a);
    }
}
