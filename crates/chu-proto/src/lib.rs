//! Chu-Server Protocol 1.0 (SERVER-PROTOCOL.md): line tokenizer, client and
//! server messages, game names, and the Game_Summary block. Shared by the
//! server and the bridge; no I/O here.

mod game_name;
mod summary;

pub use game_name::GameName;
pub use summary::{GameSummary, PlayedMove, SummaryError, TimeConfig, TimeUnit};

use std::fmt;

/// Longest line a client may send (SERVER-PROTOCOL.md §1).
pub const MAX_LINE: usize = 1024;
/// Most lines per second from one connection (§1).
pub const MAX_LINES_PER_SEC: u32 = 100;
/// Idle limit outside a game, seconds (§1).
pub const IDLE_TIMEOUT_S: u64 = 600;
/// Time to answer a Game_Summary, seconds (§4).
pub const AGREE_TIMEOUT_S: u64 = 30;
/// Ply cap (§5).
pub const MAX_MOVES: u32 = 1000;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Sente,
    Gote,
}

impl Side {
    pub fn sign(self) -> char {
        match self {
            Side::Sente => '+',
            Side::Gote => '-',
        }
    }
    pub fn from_sign(c: char) -> Option<Side> {
        match c {
            '+' => Some(Side::Sente),
            '-' => Some(Side::Gote),
            _ => None,
        }
    }
    pub fn opponent(self) -> Side {
        match self {
            Side::Sente => Side::Gote,
            Side::Gote => Side::Sente,
        }
    }
}

/// Colour asked for in `%%GAME`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ColorReq {
    Sente,
    Gote,
    Any,
}

impl ColorReq {
    pub fn sign(self) -> char {
        match self {
            ColorReq::Sente => '+',
            ColorReq::Gote => '-',
            ColorReq::Any => '*',
        }
    }
}

/// Splits a line into tokens separated by one or more spaces.
pub fn tokens(line: &str) -> impl Iterator<Item = &str> {
    line.split(' ').filter(|t| !t.is_empty())
}

/// `[A-Za-z0-9_-]{1,32}` (§2).
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A line from a client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMsg {
    Login { name: String, password: String, x1: bool },
    Logout,
    Game { game_name: String, color: ColorReq, sfen: Option<String> },
    Who,
    List,
    Show(String),
    MonitorOn(String),
    MonitorOff(String),
    Chat(String),
    Agree(Option<String>),
    Reject(Option<String>),
    Move { side: Side, usi: String },
    Toryo,
    KeepAlive,
    /// Anything else, including malformed known commands.
    Unknown(String),
}

impl ClientMsg {
    pub fn parse(line: &str) -> ClientMsg {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            return ClientMsg::KeepAlive;
        }
        let unknown = || ClientMsg::Unknown(line.to_owned());
        let t: Vec<&str> = tokens(line).collect();
        let one_arg = |f: fn(String) -> ClientMsg| match t.len() {
            2 => f(t[1].to_owned()),
            _ => unknown(),
        };
        match t[0] {
            "LOGIN" if t.len() == 3 || (t.len() == 4 && t[3] == "x1") => ClientMsg::Login {
                name: t[1].to_owned(),
                password: t[2].to_owned(),
                x1: t.len() == 4,
            },
            "LOGOUT" if t.len() == 1 => ClientMsg::Logout,
            "%%GAME" if t.len() == 3 || t.len() >= 4 => {
                let color = match t[2] {
                    "+" => ColorReq::Sente,
                    "-" => ColorReq::Gote,
                    "*" => ColorReq::Any,
                    _ => return unknown(),
                };
                let sfen = (t.len() > 3).then(|| t[3..].join(" "));
                ClientMsg::Game { game_name: t[1].to_owned(), color, sfen }
            }
            "%%WHO" if t.len() == 1 => ClientMsg::Who,
            "%%LIST" if t.len() == 1 => ClientMsg::List,
            "%%SHOW" => one_arg(ClientMsg::Show),
            "%%MONITORON" => one_arg(ClientMsg::MonitorOn),
            "%%MONITOROFF" => one_arg(ClientMsg::MonitorOff),
            "%%CHAT" => match line.split_once("%%CHAT ") {
                Some((_, text)) => ClientMsg::Chat(text.to_owned()),
                None => unknown(),
            },
            "AGREE" if t.len() <= 2 => ClientMsg::Agree(t.get(1).map(|s| s.to_string())),
            "REJECT" if t.len() <= 2 => ClientMsg::Reject(t.get(1).map(|s| s.to_string())),
            "%TORYO" if t.len() == 1 => ClientMsg::Toryo,
            m if t.len() == 1 && m.len() > 1 => {
                let mut chars = m.chars();
                match Side::from_sign(chars.next().unwrap()) {
                    Some(side) => ClientMsg::Move { side, usi: chars.as_str().to_owned() },
                    None => unknown(),
                }
            }
            _ => unknown(),
        }
    }
}

impl fmt::Display for ClientMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientMsg::Login { name, password, x1 } => {
                write!(f, "LOGIN {name} {password}{}", if *x1 { " x1" } else { "" })
            }
            ClientMsg::Logout => f.write_str("LOGOUT"),
            ClientMsg::Game { game_name, color, sfen } => {
                write!(f, "%%GAME {game_name} {}", color.sign())?;
                match sfen {
                    Some(s) => write!(f, " {s}"),
                    None => Ok(()),
                }
            }
            ClientMsg::Who => f.write_str("%%WHO"),
            ClientMsg::List => f.write_str("%%LIST"),
            ClientMsg::Show(id) => write!(f, "%%SHOW {id}"),
            ClientMsg::MonitorOn(id) => write!(f, "%%MONITORON {id}"),
            ClientMsg::MonitorOff(id) => write!(f, "%%MONITOROFF {id}"),
            ClientMsg::Chat(text) => write!(f, "%%CHAT {text}"),
            ClientMsg::Agree(id) => opt_arg(f, "AGREE", id),
            ClientMsg::Reject(id) => opt_arg(f, "REJECT", id),
            ClientMsg::Move { side, usi } => write!(f, "{}{usi}", side.sign()),
            ClientMsg::Toryo => f.write_str("%TORYO"),
            ClientMsg::KeepAlive => Ok(()),
            ClientMsg::Unknown(line) => f.write_str(line),
        }
    }
}

fn opt_arg(f: &mut fmt::Formatter<'_>, word: &str, arg: &Option<String>) -> fmt::Result {
    match arg {
        Some(a) => write!(f, "{word} {a}"),
        None => f.write_str(word),
    }
}

/// Reason line of a finished game (§5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Resign,
    TimeUp,
    IllegalMove,
    RoyalsLost,
    BareKing,
    Stalemate,
    Insufficient,
    Sennichite,
    OuteSennichite,
    MaxMoves,
    Chudan,
}

impl Reason {
    pub const ALL: [Reason; 11] = [
        Reason::Resign,
        Reason::TimeUp,
        Reason::IllegalMove,
        Reason::RoyalsLost,
        Reason::BareKing,
        Reason::Stalemate,
        Reason::Insufficient,
        Reason::Sennichite,
        Reason::OuteSennichite,
        Reason::MaxMoves,
        Reason::Chudan,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Resign => "#RESIGN",
            Reason::TimeUp => "#TIME_UP",
            Reason::IllegalMove => "#ILLEGAL_MOVE",
            Reason::RoyalsLost => "#ROYALS_LOST",
            Reason::BareKing => "#BARE_KING",
            Reason::Stalemate => "#STALEMATE",
            Reason::Insufficient => "#INSUFFICIENT",
            Reason::Sennichite => "#SENNICHITE",
            Reason::OuteSennichite => "#OUTE_SENNICHITE",
            Reason::MaxMoves => "#MAX_MOVES",
            Reason::Chudan => "#CHUDAN",
        }
    }

    pub fn parse(s: &str) -> Option<Reason> {
        Reason::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// Verdict line, per player (§5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Win,
    Lose,
    Draw,
    Censored,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Win => "#WIN",
            Verdict::Lose => "#LOSE",
            Verdict::Draw => "#DRAW",
            Verdict::Censored => "#CENSORED",
        }
    }
    pub fn parse(s: &str) -> Option<Verdict> {
        [Verdict::Win, Verdict::Lose, Verdict::Draw, Verdict::Censored]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// One line from the server. A Game_Summary arrives as `BeginSummary`
/// followed by lines that `GameSummary::parse` reads up to `END Game_Summary`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerMsg {
    LoginOk(String),
    LoginIncorrect,
    LogoutCompleted,
    BeginSummary,
    Start(String),
    /// `REJECT:<game-id> by <name>`: the game was cancelled before START.
    Reject { game_id: String, by: String },
    Move(PlayedMove),
    Reason(Reason),
    Verdict(Verdict),
    /// `##[...]` lobby replies, monitor and chat lines.
    Lobby(String),
    Other(String),
}

impl ServerMsg {
    pub fn parse(line: &str) -> ServerMsg {
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "LOGIN:incorrect" {
            return ServerMsg::LoginIncorrect;
        }
        if line == "LOGOUT:completed" {
            return ServerMsg::LogoutCompleted;
        }
        if line == "BEGIN Game_Summary" {
            return ServerMsg::BeginSummary;
        }
        if let Some(name) = line.strip_prefix("LOGIN:").and_then(|r| r.strip_suffix(" OK")) {
            return ServerMsg::LoginOk(name.to_owned());
        }
        if let Some(id) = line.strip_prefix("START:") {
            return ServerMsg::Start(id.to_owned());
        }
        if let Some((id, by)) = line.strip_prefix("REJECT:").and_then(|r| r.split_once(" by ")) {
            return ServerMsg::Reject { game_id: id.to_owned(), by: by.to_owned() };
        }
        if line.starts_with("##") {
            return ServerMsg::Lobby(line.to_owned());
        }
        if let Some(r) = Reason::parse(line) {
            return ServerMsg::Reason(r);
        }
        if let Some(v) = Verdict::parse(line) {
            return ServerMsg::Verdict(v);
        }
        if let Some(m) = PlayedMove::parse(line) {
            return ServerMsg::Move(m);
        }
        ServerMsg::Other(line.to_owned())
    }
}

impl fmt::Display for ServerMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerMsg::LoginOk(name) => write!(f, "LOGIN:{name} OK"),
            ServerMsg::LoginIncorrect => f.write_str("LOGIN:incorrect"),
            ServerMsg::LogoutCompleted => f.write_str("LOGOUT:completed"),
            ServerMsg::BeginSummary => f.write_str("BEGIN Game_Summary"),
            ServerMsg::Start(id) => write!(f, "START:{id}"),
            ServerMsg::Reject { game_id, by } => write!(f, "REJECT:{game_id} by {by}"),
            ServerMsg::Move(m) => write!(f, "{m}"),
            ServerMsg::Reason(r) => f.write_str(r.as_str()),
            ServerMsg::Verdict(v) => f.write_str(v.as_str()),
            ServerMsg::Lobby(l) | ServerMsg::Other(l) => f.write_str(l),
        }
    }
}
