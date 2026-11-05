//! The Game_Summary block (§4) and move lines (§5).

use std::fmt;

use crate::Side;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeUnit {
    Msec,
    Sec,
}

impl TimeUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeUnit::Msec => "1msec",
            TimeUnit::Sec => "1sec",
        }
    }
    /// Milliseconds per unit.
    pub fn ms(self) -> u64 {
        match self {
            TimeUnit::Msec => 1,
            TimeUnit::Sec => 1000,
        }
    }
}

/// `BEGIN Time` … `END Time`; every field in `time_unit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeConfig {
    pub time_unit: TimeUnit,
    pub total_time: u64,
    pub byoyomi: u64,
    pub increment: u64,
    pub least_time_per_move: u64,
    /// `Remaining+:` and `Remaining-:`, sente first: the clocks when a
    /// resumed game's summary was sent (§4). None in a new game's summary.
    pub remaining: Option<[u64; 2]>,
}

/// `+8i8h,T2310`: a move with the time it used, in the game's time unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayedMove {
    pub side: Side,
    pub usi: String,
    pub time: u64,
}

impl PlayedMove {
    pub fn parse(s: &str) -> Option<PlayedMove> {
        let mut chars = s.chars();
        let side = Side::from_sign(chars.next()?)?;
        let (usi, t) = chars.as_str().split_once(",T")?;
        if usi.is_empty() || usi.contains([' ', ',']) || !t.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(PlayedMove { side, usi: usi.to_owned(), time: t.parse().ok()? })
    }
}

impl fmt::Display for PlayedMove {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{},T{}", self.side.sign(), self.usi, self.time)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameSummary {
    pub protocol_version: String,
    pub protocol_mode: String,
    pub format: String,
    /// `lishogi` or `jcsa`.
    pub rules: String,
    pub game_id: String,
    pub name_plus: String,
    pub name_minus: String,
    pub your_turn: Side,
    pub rematch_on_draw: bool,
    pub to_move: Side,
    pub max_moves: u32,
    pub time: TimeConfig,
    pub sfen: String,
    /// Moves already played, for a resumed game (`MOVES:` lines).
    pub moves: Vec<PlayedMove>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryError(pub String);

impl fmt::Display for SummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad Game_Summary: {}", self.0)
    }
}

impl std::error::Error for SummaryError {}

fn sign(s: Side) -> char {
    s.sign()
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "YES"
    } else {
        "NO"
    }
}

impl GameSummary {
    /// The block as sent, `BEGIN Game_Summary` through `END Game_Summary`.
    pub fn to_lines(&self) -> Vec<String> {
        let t = &self.time;
        let mut v = vec![
            "BEGIN Game_Summary".to_owned(),
            format!("Protocol_Version:{}", self.protocol_version),
            format!("Protocol_Mode:{}", self.protocol_mode),
            format!("Format:{}", self.format),
            format!("Rules:{}", self.rules),
            format!("Game_ID:{}", self.game_id),
            format!("Name+:{}", self.name_plus),
            format!("Name-:{}", self.name_minus),
            format!("Your_Turn:{}", sign(self.your_turn)),
            format!("Rematch_On_Draw:{}", yes_no(self.rematch_on_draw)),
            format!("To_Move:{}", sign(self.to_move)),
            format!("Max_Moves:{}", self.max_moves),
            "BEGIN Time".to_owned(),
            format!("Time_Unit:{}", t.time_unit.as_str()),
            format!("Total_Time:{}", t.total_time),
            format!("Byoyomi:{}", t.byoyomi),
            format!("Increment:{}", t.increment),
            format!("Least_Time_Per_Move:{}", t.least_time_per_move),
        ];
        if let Some([plus, minus]) = t.remaining {
            v.push(format!("Remaining+:{plus}"));
            v.push(format!("Remaining-:{minus}"));
        }
        v.extend(["END Time".to_owned(), "BEGIN Position".to_owned(), format!("SFEN:{}", self.sfen)]);
        v.extend(self.moves.iter().map(|m| format!("MOVES:{m}")));
        v.push("END Position".to_owned());
        v.push("END Game_Summary".to_owned());
        v
    }

    /// Parses a block from `BEGIN Game_Summary` through `END Game_Summary`
    /// (both included). Unknown `Key:value` lines are ignored.
    pub fn parse<S: AsRef<str>>(lines: &[S]) -> Result<GameSummary, SummaryError> {
        let err = |m: &str| SummaryError(m.to_owned());
        let lines: Vec<&str> = lines.iter().map(|l| l.as_ref().trim_end_matches('\r')).collect();
        if lines.first() != Some(&"BEGIN Game_Summary") || lines.last() != Some(&"END Game_Summary") {
            return Err(err("not delimited by BEGIN/END Game_Summary"));
        }
        let mut kv: Vec<(&str, &str)> = Vec::new();
        let mut moves = Vec::new();
        let mut section = "";
        for line in &lines[1..lines.len() - 1] {
            match *line {
                "BEGIN Time" | "BEGIN Position" if section.is_empty() => section = *line,
                "END Time" if section == "BEGIN Time" => section = "",
                "END Position" if section == "BEGIN Position" => section = "",
                _ => {
                    let (k, v) = line.split_once(':').ok_or_else(|| err(&format!("line {line:?}")))?;
                    if k == "MOVES" && section == "BEGIN Position" {
                        let m = PlayedMove::parse(v).ok_or_else(|| err(&format!("move {v:?}")))?;
                        moves.push(m);
                    } else {
                        if kv.iter().any(|(k2, _)| *k2 == k) {
                            return Err(err(&format!("duplicate {k}")));
                        }
                        kv.push((k, v));
                    }
                }
            }
        }
        if !section.is_empty() {
            return Err(err("unterminated section"));
        }
        let get = |k: &str| {
            kv.iter()
                .find(|(k2, _)| *k2 == k)
                .map(|(_, v)| *v)
                .ok_or_else(|| err(&format!("missing {k}")))
        };
        let num = |k: &str| -> Result<u64, SummaryError> {
            get(k)?.parse().map_err(|_| err(&format!("{k} not a number")))
        };
        let side = |k: &str| -> Result<Side, SummaryError> {
            let v = get(k)?;
            let mut c = v.chars();
            match (c.next().and_then(Side::from_sign), c.next()) {
                (Some(s), None) => Ok(s),
                _ => Err(err(&format!("{k}:{v}"))),
            }
        };
        let time_unit = match get("Time_Unit")? {
            "1msec" => TimeUnit::Msec,
            "1sec" => TimeUnit::Sec,
            u => return Err(err(&format!("Time_Unit:{u}"))),
        };
        let rematch_on_draw = match get("Rematch_On_Draw")? {
            "YES" => true,
            "NO" => false,
            v => return Err(err(&format!("Rematch_On_Draw:{v}"))),
        };
        let has = |k: &str| kv.iter().any(|(k2, _)| *k2 == k);
        let remaining = match (has("Remaining+"), has("Remaining-")) {
            (true, true) => Some([num("Remaining+")?, num("Remaining-")?]),
            (false, false) => None,
            _ => return Err(err("Remaining+ and Remaining- come together")),
        };
        Ok(GameSummary {
            protocol_version: get("Protocol_Version")?.to_owned(),
            protocol_mode: get("Protocol_Mode")?.to_owned(),
            format: get("Format")?.to_owned(),
            rules: get("Rules")?.to_owned(),
            game_id: get("Game_ID")?.to_owned(),
            name_plus: get("Name+")?.to_owned(),
            name_minus: get("Name-")?.to_owned(),
            your_turn: side("Your_Turn")?,
            rematch_on_draw,
            to_move: side("To_Move")?,
            max_moves: u32::try_from(num("Max_Moves")?).map_err(|_| err("Max_Moves"))?,
            time: TimeConfig {
                time_unit,
                total_time: num("Total_Time")?,
                byoyomi: num("Byoyomi")?,
                increment: num("Increment")?,
                least_time_per_move: num("Least_Time_Per_Move")?,
                remaining,
            },
            sfen: get("SFEN")?.to_owned(),
            moves,
        })
    }
}
