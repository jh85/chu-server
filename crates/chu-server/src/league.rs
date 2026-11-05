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
