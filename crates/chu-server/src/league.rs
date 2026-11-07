//! Floodgate league configuration (`leagues.toml`) and round times.
//!
//! ```toml
//! [[league]]
//! name = "chu-floodgate-600-10F"   # the game name players queue for
//! every_min = 30                   # a round every 30 minutes ...
//! offset_min = 0                   # ... at :00 and :30 UTC
//! cutoff_s = 15                    # joins in the last 15 s wait a round
//! rules = "lishogi"                # must match the name; JCSA is deferred
//! rated = true
//! ```

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chu_proto::GameName;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeagueConfig {
    pub name: String,
    pub every_min: u64,
    #[serde(default)]
    pub offset_min: u64,
    pub cutoff_s: u64,
    pub rules: String,
    pub rated: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    league: Vec<LeagueConfig>,
}

impl LeagueConfig {
    pub fn every(&self) -> Duration {
        Duration::from_secs(self.every_min * 60)
    }

    /// The first round time strictly after `t`.
    pub fn next_round(&self, t: SystemTime) -> SystemTime {
        let every = self.every_min * 60;
        let offset = self.offset_min * 60;
        let now = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let k = (now + every - offset % every) / every;
        UNIX_EPOCH + Duration::from_secs(k * every + offset % every)
    }

    /// The missed-round rule (SERVER-PROTOCOL.md §3) at startup, given the
    /// league's latest round in the `rounds` table: of the rounds whose time
    /// passed while the server was down, the latest is fired late, at
    /// `now + grace` (time for the players to reconnect and queue), if the
    /// next round is not yet due then; the others are skipped.
    pub fn startup(&self, last: Option<SystemTime>, now: SystemTime, grace: Duration) -> Startup {
        let next = self.next_round(now);
        let mut missed = 0;
        let mut latest = None;
        if let Some(last) = last {
            let first = self.next_round(last);
            if first < next {
                let n = next.duration_since(first).unwrap_or_default().as_secs() / (self.every_min * 60);
                missed = n;
                latest = Some(next - self.every());
            }
        }
        let late = latest.filter(|_| now + grace < next);
        let skipped = missed - u64::from(late.is_some());
        Startup { late, skipped, next }
    }

    fn check(&self) -> Result<(), String> {
        let name: GameName = self.name.parse().map_err(|_| format!("{}: not a game name", self.name))?;
        let fail = |m: &str| Err(format!("{}: {m}", self.name));
        if !name.is_floodgate() {
            return fail("only floodgate leagues are scheduled");
        }
        if self.rules != name.rules() {
            return fail(&format!("rules = {:?} but the name says {}", self.rules, name.rules()));
        }
        if name.jcsa {
            return fail("JCSA leagues are deferred");
        }
        if self.every_min == 0 || self.offset_min >= self.every_min {
            return fail("every_min must be positive and offset_min smaller");
        }
        if self.cutoff_s >= self.every_min * 60 {
            return fail("cutoff_s must be shorter than the round interval");
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Startup {
    /// The missed round to fire late, if any.
    pub late: Option<SystemTime>,
    /// How many missed rounds are skipped.
    pub skipped: u64,
    /// The first round on schedule.
    pub next: SystemTime,
}

pub fn parse(text: &str) -> Result<Vec<LeagueConfig>, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    for l in &file.league {
        l.check()?;
        if !seen.insert(&l.name) {
            return Err(format!("{}: listed twice", l.name));
        }
    }
    Ok(file.league)
}

pub fn load(path: &Path) -> Result<Vec<LeagueConfig>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[[league]]
name = "chu-floodgate-600-10F"
every_min = 30
cutoff_s = 15
rules = "lishogi"
rated = true
"#;

    #[test]
    fn parses_and_schedules() {
        let l = &parse(GOOD).unwrap()[0];
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
        // 1_800_000_000 is a multiple of 1800.
        assert_eq!(l.next_round(at(1_800_000_000)), at(1_800_001_800));
        assert_eq!(l.next_round(at(1_800_000_001)), at(1_800_001_800));
        assert_eq!(l.next_round(at(1_800_001_799)), at(1_800_001_800));
        let shifted = LeagueConfig { offset_min: 5, ..l.clone() };
        assert_eq!(shifted.next_round(at(1_800_000_000)), at(1_800_000_300));
        assert_eq!(shifted.next_round(at(1_800_000_300)), at(1_800_002_100));
    }

    #[test]
    fn missed_rounds_at_startup() {
        let l = &parse(GOOD).unwrap()[0];
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
        let g = Duration::from_secs(45);
        let base = 1_800_000_000; // a round time
        // First start ever, or nothing missed: just the next round.
        assert_eq!(l.startup(None, at(base + 10), g), Startup { late: None, skipped: 0, next: at(base + 1800) });
        assert_eq!(l.startup(Some(at(base)), at(base + 10), g), Startup { late: None, skipped: 0, next: at(base + 1800) });
        // Down across one round: it fires late.
        let s = l.startup(Some(at(base)), at(base + 1800 + 600), g);
        assert_eq!(s, Startup { late: Some(at(base + 1800)), skipped: 0, next: at(base + 3600) });
        // Down across three: the latest fires late, two are skipped.
        let s = l.startup(Some(at(base)), at(base + 3 * 1800 + 60), g);
        assert_eq!(s, Startup { late: Some(at(base + 3 * 1800)), skipped: 2, next: at(base + 4 * 1800) });
        // Back so close to the next round that it would be due: skipped too.
        let s = l.startup(Some(at(base)), at(base + 2 * 1800 - 30), g);
        assert_eq!(s, Startup { late: None, skipped: 1, next: at(base + 2 * 1800) });
    }

    #[test]
    fn rejects_bad_leagues() {
        for (from, to) in [
            ("chu-floodgate-600-10F", "chu-test-600-10F"),
            ("chu-floodgate-600-10F", "chu-floodgate-600-10F-jcsa"),
            ("rules = \"lishogi\"", "rules = \"jcsa\""),
            ("every_min = 30", "every_min = 0"),
            ("cutoff_s = 15", "cutoff_s = 1800"),
            ("rated = true", "rated = true\ncadence = 3"),
        ] {
            assert!(parse(&GOOD.replace(from, to)).is_err(), "{to}");
        }
        assert!(parse(&format!("{GOOD}{GOOD}")).is_err());
        assert_eq!(parse("").unwrap(), vec![]);
    }

    #[test]
    fn the_shipped_file_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../leagues.toml");
        assert!(!load(&path).unwrap().is_empty());
    }
}
