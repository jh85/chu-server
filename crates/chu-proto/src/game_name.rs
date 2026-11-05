use std::fmt;
use std::str::FromStr;

/// `chu-<league>-<total-s>-<byoyomi-s>[F][-jcsa]` (§3). Only the canonical
/// spelling parses (no leading zeros), so a name round-trips exactly.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GameName {
    pub league: String,
    pub total_s: u64,
    /// Byoyomi seconds, or the Fischer increment when `fischer`.
    pub byoyomi_s: u64,
    pub fischer: bool,
    pub jcsa: bool,
}

impl GameName {
    /// `floodgate` leagues pair on the server's schedule; the others on demand.
    pub fn is_floodgate(&self) -> bool {
        self.league == "floodgate"
    }
    /// `chu-test-*` leagues accept a start SFEN and are unrated.
    pub fn is_test(&self) -> bool {
        self.league == "test"
    }
    pub fn rules(&self) -> &'static str {
        if self.jcsa {
            "jcsa"
        } else {
            "lishogi"
        }
    }
}

impl FromStr for GameName {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        // Same alphabet as login names (§2); up to 64 bytes.
        if s.len() > 64 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err(());
        }
        let mut parts: Vec<&str> = s.strip_prefix("chu-").ok_or(())?.split('-').collect();
        let jcsa = parts.last() == Some(&"jcsa");
        if jcsa {
            parts.pop();
        }
        if parts.len() < 3 {
            return Err(());
        }
        let byo = parts.pop().unwrap();
        let (byo, fischer) = match byo.strip_suffix('F') {
            Some(b) => (b, true),
            None => (byo, false),
        };
        let num = |t: &str| -> Result<u64, ()> {
            if t.is_empty() || (t.len() > 1 && t.starts_with('0')) || !t.bytes().all(|b| b.is_ascii_digit()) {
                return Err(());
            }
            t.parse().map_err(|_| ())
        };
        let byoyomi_s = num(byo)?;
        let total_s = num(parts.pop().unwrap())?;
        let league = parts.join("-");
        if parts.iter().any(|p| p.is_empty()) {
            return Err(());
        }
        let name = GameName { league, total_s, byoyomi_s, fischer, jcsa };
        if name.to_string() != s {
            return Err(());
        }
        Ok(name)
    }
}

impl fmt::Display for GameName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chu-{}-{}-{}", self.league, self.total_s, self.byoyomi_s)?;
        if self.fischer {
            f.write_str("F")?;
        }
        if self.jcsa {
            f.write_str("-jcsa")?;
        }
        Ok(())
    }
}
