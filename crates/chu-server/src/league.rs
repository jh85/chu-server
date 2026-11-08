//! The league list (`leagues.toml`): every game the server plays belongs to
//! one of these leagues. The name is an opaque label (SERVER-PROTOCOL.md §3).
//!
//! ```toml
//! [[league]]
//! name = "floodgate-600-10F"   # [A-Za-z0-9_-]{1,32}, the %%GAME name
//! kind = "floodgate"           # floodgate | challenge | test
//! total_s = 600                # main time
//! byoyomi_s = 0                # byoyomi and increment: at most one > 0
//! increment_s = 10
//! rules = "lishogi"            # lishogi | jcsa
//! rated = true                 # test leagues: never
//! every_min = 30               # floodgate only: a round every 30 minutes,
//! offset_min = 0               # at :00 and :30 UTC (default 0);
//! cutoff_s = 15                # joins in the last 15 s wait a round
//! ```

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chu_proto::clock::TimeControl;
use chu_proto::TimeUnit;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Scheduled rounds.
    Floodgate,
    /// Pairs the first two queued players whose colours fit.
    Challenge,
    /// Like challenge, with an optional start SFEN; never rated.
    Test,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuleSet {
    Lishogi,
    Jcsa,
}

impl RuleSet {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleSet::Lishogi => "lishogi",
            RuleSet::Jcsa => "jcsa",
        }
    }
    pub fn referee(self) -> chu_rules::Rules {
        match self {
            RuleSet::Lishogi => chu_rules::Rules::Lishogi,
            RuleSet::Jcsa => chu_rules::Rules::Jcsa,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeagueConfig {
    pub name: String,
    pub kind: Kind,
    pub total_s: u64,
    pub byoyomi_s: u64,
    pub increment_s: u64,
    pub rules: RuleSet,
    pub rated: bool,
    /// Floodgate only.
    pub every_min: Option<u64>,
    pub offset_min: Option<u64>,
    pub cutoff_s: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    league: Vec<LeagueConfig>,
}

/// A floodgate league's schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub every_min: u64,
    pub offset_min: u64,
    pub cutoff_s: u64,
}

impl LeagueConfig {
    /// A challenge or test league (tests and tools).
    pub fn on_demand(name: &str, kind: Kind, total_s: u64, byoyomi_s: u64, increment_s: u64) -> LeagueConfig {
        LeagueConfig {
            name: name.into(),
            kind,
            total_s,
            byoyomi_s,
            increment_s,
            rules: RuleSet::Lishogi,
            rated: false,
            every_min: None,
            offset_min: None,
            cutoff_s: None,
        }
    }

    /// The schedule of a floodgate league (checked at load), else None.
    pub fn schedule(&self) -> Option<Schedule> {
        match (self.kind, self.every_min, self.cutoff_s) {
            (Kind::Floodgate, Some(every_min), Some(cutoff_s)) => {
                Some(Schedule { every_min, offset_min: self.offset_min.unwrap_or(0), cutoff_s })
            }
            _ => None,
        }
    }

    pub fn time_control(&self, unit: TimeUnit) -> TimeControl {
        TimeControl::from_secs(self.total_s, self.byoyomi_s, self.increment_s, unit)
    }

    /// `600 s + 10 s Fischer`, `900 s + 30 s byoyomi`, `600 s`.
    pub fn describe_time(&self) -> String {
        match (self.byoyomi_s, self.increment_s) {
            (0, 0) => format!("{} s", self.total_s),
            (0, inc) => format!("{} s + {inc} s Fischer", self.total_s),
            (byo, _) => format!("{} s + {byo} s byoyomi", self.total_s),
        }
    }

    fn check(&self) -> Result<(), String> {
        let fail = |m: &str| Err(format!("{}: {m}", self.name));
        if !chu_proto::valid_name(&self.name) {
            return Err(format!("{:?}: a league name must match [A-Za-z0-9_-]{{1,32}}", self.name));
        }
        if self.byoyomi_s > 0 && self.increment_s > 0 {
            return fail("byoyomi_s and increment_s may not both be > 0");
        }
        if self.total_s == 0 && self.byoyomi_s == 0 {
            return fail("no time to move: total_s and byoyomi_s are both 0");
        }
        let floodgate_fields = [("every_min", self.every_min), ("offset_min", self.offset_min), ("cutoff_s", self.cutoff_s)];
        if self.kind != Kind::Floodgate {
            if let Some((field, _)) = floodgate_fields.iter().find(|(_, v)| v.is_some()) {
                return fail(&format!("{field} is for floodgate leagues only"));
            }
        }
        if self.kind == Kind::Test && self.rated {
            return fail("test leagues are never rated");
        }
        if self.kind == Kind::Floodgate {
            let Some(s) = self.schedule() else { return fail("a floodgate league needs every_min and cutoff_s") };
            if s.every_min == 0 || s.offset_min >= s.every_min {
                return fail("every_min must be positive and offset_min smaller");
            }
            if s.cutoff_s >= s.every_min * 60 {
                return fail("cutoff_s must be shorter than the round interval");
            }
        }
        Ok(())
    }
}

impl Schedule {
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
name = "floodgate-600-10F"
kind = "floodgate"
total_s = 600
byoyomi_s = 0
increment_s = 10
rules = "lishogi"
rated = true
every_min = 30
offset_min = 0
cutoff_s = 15

[[league]]
name = "blitz"
kind = "challenge"
total_s = 300
byoyomi_s = 10
increment_s = 0
rules = "jcsa"
rated = true

[[league]]
name = "test"
kind = "test"
total_s = 60
byoyomi_s = 0
increment_s = 1
rules = "lishogi"
rated = false
"#;

    fn sched() -> Schedule {
        parse(GOOD).unwrap()[0].schedule().unwrap()
    }

    #[test]
    fn parses_a_valid_file() {
        let l = parse(GOOD).unwrap();
        assert_eq!(l.iter().map(|l| (l.name.as_str(), l.kind)).collect::<Vec<_>>(),
            [("floodgate-600-10F", Kind::Floodgate), ("blitz", Kind::Challenge), ("test", Kind::Test)]);
        assert_eq!(l[0].schedule(), Some(Schedule { every_min: 30, offset_min: 0, cutoff_s: 15 }));
        assert_eq!((l[1].schedule(), l[1].rules, l[1].rated), (None, RuleSet::Jcsa, true));
        let tc = l[0].time_control(TimeUnit::Msec);
        assert_eq!((tc.total_ms, tc.byoyomi_ms, tc.increment_ms), (600_000, 0, 10_000));
        assert_eq!(
            l.iter().map(LeagueConfig::describe_time).collect::<Vec<_>>(),
            ["600 s + 10 s Fischer", "300 s + 10 s byoyomi", "60 s + 1 s Fischer"]
        );
        // offset_min defaults to 0.
        let no_offset = parse(&GOOD.replace("offset_min = 0\n", "")).unwrap();
        assert_eq!(no_offset[0].schedule(), l[0].schedule());
        assert_eq!(parse("").unwrap(), vec![]);
    }

    #[test]
    fn rejects_each_error() {
        let floodgate = GOOD.split("\n\n").next().unwrap();
        let challenge = GOOD.split("\n\n").nth(1).unwrap();
        let test = GOOD.split("\n\n").nth(2).unwrap();
        let cases: &[(&str, &str, &str, &str)] = &[
            (floodgate, "name = \"floodgate-600-10F\"", "name = \"flood gate\"", "a league name must match"),
            (floodgate, "name = \"floodgate-600-10F\"", "name = \"\"", "a league name must match"),
            (floodgate, "name = \"floodgate-600-10F\"", &format!("name = \"{}\"", "x".repeat(33)), "a league name must match"),
            (floodgate, "kind = \"floodgate\"", "kind = \"rapid\"", "unknown variant"),
            (floodgate, "rules = \"lishogi\"", "rules = \"chess\"", "unknown variant"),
            (floodgate, "byoyomi_s = 0", "byoyomi_s = 5", "may not both be > 0"),
            (floodgate, "total_s = 600", "total_s = -1", "invalid value"),
            (floodgate, "increment_s = 10\n", "", "missing field `increment_s`"),
            (floodgate, "rated = true\n", "", "missing field `rated`"),
            (floodgate, "every_min = 30\n", "", "needs every_min and cutoff_s"),
            (floodgate, "cutoff_s = 15", "", "needs every_min and cutoff_s"),
            (floodgate, "every_min = 30", "every_min = 0", "every_min must be positive"),
            (floodgate, "offset_min = 0", "offset_min = 30", "offset_min smaller"),
            (floodgate, "cutoff_s = 15", "cutoff_s = 1800", "cutoff_s must be shorter"),
            (floodgate, "rated = true", "rated = true\ncadence = 3", "unknown field `cadence`"),
            (challenge, "total_s = 300\nbyoyomi_s = 10", "total_s = 0\nbyoyomi_s = 0", "no time to move"),
            (challenge, "rated = true", "rated = true\nevery_min = 30", "every_min is for floodgate leagues only"),
            (challenge, "rated = true", "rated = true\noffset_min = 0", "offset_min is for floodgate leagues only"),
            (test, "rated = false", "rated = false\ncutoff_s = 5", "cutoff_s is for floodgate leagues only"),
            (test, "rated = false", "rated = true", "test leagues are never rated"),
        ];
        for (block, from, to, want) in cases {
            assert!(block.contains(from), "{from}");
            let text = format!("[[league]]{}", block.replace(from, to).trim_start_matches("\n[[league]]").trim_start_matches("[[league]]"));
            let err = parse(&text).expect_err(to);
            assert!(err.contains(want), "{to}: {err}");
        }
        let err = parse(&format!("{GOOD}\n{test}")).unwrap_err();
        assert!(err.contains("test: listed twice"), "{err}");
    }

    #[test]
    fn schedules() {
        let l = sched();
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
        // 1_800_000_000 is a multiple of 1800.
        assert_eq!(l.next_round(at(1_800_000_000)), at(1_800_001_800));
        assert_eq!(l.next_round(at(1_800_000_001)), at(1_800_001_800));
        assert_eq!(l.next_round(at(1_800_001_799)), at(1_800_001_800));
        let shifted = Schedule { offset_min: 5, ..l };
        assert_eq!(shifted.next_round(at(1_800_000_000)), at(1_800_000_300));
        assert_eq!(shifted.next_round(at(1_800_000_300)), at(1_800_002_100));
    }

    #[test]
    fn missed_rounds_at_startup() {
        let l = sched();
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
    fn the_shipped_file_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../leagues.toml");
        let l = load(&path).unwrap();
        let summary: Vec<_> = l
            .iter()
            .map(|l| (l.name.as_str(), l.kind, l.total_s, l.byoyomi_s, l.increment_s, l.rules, l.rated, l.schedule()))
            .collect();
        let fg = |every_min, cutoff_s| Some(Schedule { every_min, offset_min: 0, cutoff_s });
        assert_eq!(
            summary,
            [
                ("floodgate-600-10F", Kind::Floodgate, 600, 0, 10, RuleSet::Lishogi, true, fg(30, 15)),
                ("floodgate-60-1F", Kind::Floodgate, 60, 0, 1, RuleSet::Lishogi, true, fg(5, 5)),
                ("test", Kind::Test, 60, 0, 1, RuleSet::Lishogi, false, None),
            ]
        );
    }
}
