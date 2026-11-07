//! UTC calendar formatting without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utc {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub millis: u32,
}

impl Utc {
    pub fn from(t: SystemTime) -> Utc {
        let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
        let secs = d.as_secs() as i64;
        let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
        // civil_from_days (H. Hinnant)
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);
        Utc {
            year,
            month,
            day,
            hour: (rem / 3600) as u32,
            min: (rem / 60 % 60) as u32,
            sec: (rem % 60) as u32,
            millis: d.subsec_millis(),
        }
    }

    /// `20261104213000`, as in floodgate game ids.
    pub fn compact(&self) -> String {
        format!("{:04}{:02}{:02}{:02}{:02}{:02}", self.year, self.month, self.day, self.hour, self.min, self.sec)
    }

    /// Parses what [`Utc::iso`] writes.
    pub fn parse_iso(s: &str) -> Option<SystemTime> {
        let b = s.as_bytes();
        if b.len() != 24 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'.' || b[23] != b'Z' {
            return None;
        }
        let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
        let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
        let (hh, mm, ss, ms) = (n(11..13)?, n(14..16)?, n(17..19)?, n(20..23)?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59 {
            return None;
        }
        // days_from_civil (H. Hinnant)
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        let ms = ((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000 + ms).try_into().ok()?;
        Some(UNIX_EPOCH + std::time::Duration::from_millis(ms))
    }

    /// `2026-11-04T21:30:00.123Z`
    pub fn iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            self.year, self.month, self.day, self.hour, self.min, self.sec, self.millis
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn known_dates() {
        let t = |s: u64| Utc::from(UNIX_EPOCH + Duration::from_millis(s));
        assert_eq!(t(0).iso(), "1970-01-01T00:00:00.000Z");
        assert_eq!(t(951_782_400_000).iso(), "2000-02-29T00:00:00.000Z");
        assert_eq!(t(1_793_827_800_250).iso(), "2026-11-04T21:30:00.250Z");
        assert_eq!(t(1_793_827_800_250).compact(), "20261104213000");
        for ms in [0, 951_782_400_000, 1_793_827_800_250, 4_102_444_799_999] {
            assert_eq!(Utc::parse_iso(&t(ms).iso()), Some(UNIX_EPOCH + Duration::from_millis(ms)));
        }
        assert_eq!(Utc::parse_iso("2026-13-04T21:30:00.250Z"), None);
        assert_eq!(Utc::parse_iso("2026-11-04 21:30:00"), None);
    }
}
