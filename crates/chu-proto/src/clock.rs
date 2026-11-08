//! Clock arithmetic in milliseconds (SERVER-PROTOCOL.md §5).
//!
//! One formula covers both modes: a move may take up to `remaining + byoyomi`;
//! afterwards `remaining -= max(0, charged - byoyomi)` and `remaining += increment`.
//! Byoyomi leagues have `increment = 0`, Fischer leagues `byoyomi = 0`.

use crate::{TimeConfig, TimeUnit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeControl {
    pub unit: TimeUnit,
    pub total_ms: u64,
    pub byoyomi_ms: u64,
    pub increment_ms: u64,
    pub least_ms: u64,
}

impl TimeControl {
    /// A league's time control, given in seconds.
    pub fn from_secs(total_s: u64, byoyomi_s: u64, increment_s: u64, unit: TimeUnit) -> TimeControl {
        TimeControl { unit, total_ms: total_s * 1000, byoyomi_ms: byoyomi_s * 1000, increment_ms: increment_s * 1000, least_ms: 0 }
    }

    /// The clock a client reads from a Game_Summary's `BEGIN Time` block
    /// (`Remaining±` aside).
    pub fn from_summary(t: &TimeConfig) -> TimeControl {
        let u = t.time_unit.ms();
        TimeControl {
            unit: t.time_unit,
            total_ms: t.total_time * u,
            byoyomi_ms: t.byoyomi * u,
            increment_ms: t.increment * u,
            least_ms: t.least_time_per_move * u,
        }
    }

    /// The `BEGIN Time` block, in `unit`.
    pub fn summary(&self) -> TimeConfig {
        let u = self.unit.ms();
        TimeConfig {
            time_unit: self.unit,
            total_time: self.total_ms / u,
            byoyomi: self.byoyomi_ms / u,
            increment: self.increment_ms / u,
            least_time_per_move: self.least_ms / u,
            resumed: None,
        }
    }

    /// Longest a move may take, given the mover's remaining main time.
    pub fn allowance_ms(&self, remaining_ms: u64) -> u64 {
        remaining_ms + self.byoyomi_ms
    }

    /// Time charged for a move that took `elapsed_ms`: truncated to whole
    /// units (CSA practice for `1sec`), at least `Least_Time_Per_Move`.
    pub fn charged_ms(&self, elapsed_ms: u64) -> u64 {
        let u = self.unit.ms();
        (elapsed_ms / u * u).max(self.least_ms)
    }

    /// Remaining main time after a move charged `charged_ms`.
    pub fn after_move(&self, remaining_ms: u64, charged_ms: u64) -> u64 {
        remaining_ms.saturating_sub(charged_ms.saturating_sub(self.byoyomi_ms)) + self.increment_ms
    }

    /// `T` value of a move echo.
    pub fn units(&self, ms: u64) -> u64 {
        ms / self.unit.ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fischer() {
        let t = TimeControl::from_secs(60, 0, 1, TimeUnit::Msec);
        assert_eq!((t.total_ms, t.byoyomi_ms, t.increment_ms), (60_000, 0, 1000));
        assert_eq!(t.allowance_ms(60_000), 60_000);
        assert_eq!(t.after_move(60_000, 2_500), 58_500);
        assert_eq!(t.after_move(500, 500), 1000);
        assert_eq!(t.units(t.charged_ms(2_310)), 2_310);
        let s = t.summary();
        assert_eq!((s.total_time, s.byoyomi, s.increment), (60_000, 0, 1000));
        assert_eq!(TimeControl::from_summary(&s), t);
    }

    #[test]
    fn byoyomi_moves_within_byoyomi_are_free() {
        let t = TimeControl::from_secs(900, 30, 0, TimeUnit::Msec);
        assert_eq!((t.byoyomi_ms, t.increment_ms), (30_000, 0));
        assert_eq!(t.after_move(900_000, 29_999), 900_000);
        assert_eq!(t.after_move(900_000, 31_000), 899_000);
        assert_eq!(t.allowance_ms(0), 30_000);
        assert_eq!(t.after_move(0, 30_000), 0);
    }

    #[test]
    fn whole_seconds() {
        let t = TimeControl::from_secs(600, 10, 0, TimeUnit::Sec);
        assert_eq!(t.charged_ms(999), 0);
        assert_eq!(t.charged_ms(2_999), 2_000);
        assert_eq!(t.units(2_000), 2);
        assert_eq!(t.summary().total_time, 600);
        assert_eq!(TimeControl::from_summary(&t.summary()), t);
    }
}
