//! The chu shogi referee behind one trait, with two backends:
//! `ffi` (libchushogi.a, linked in) and `subprocess` (a `chushogi-gen` child).

#[cfg(feature = "ffi")]
mod ffi;
mod subprocess;

use std::fmt;
use std::path::PathBuf;

#[cfg(feature = "ffi")]
pub use ffi::FfiReferee;
pub use subprocess::SubprocessReferee;

/// The standard start position.
pub const START_SFEN: &str = "lfcsgekgscfl/a1b1txot1b1a/mvrhdqndhrvm/pppppppppppp/3i4i3/12/12/3I4I3/PPPPPPPPPPPP/MVRHDNQDHRVM/A1B1TOXT1B1A/LFCSGKEGSCFL b - 1";

/// Path of the `chushogi-gen` binary that build.rs compiled from vendor/chushogi.
pub const BUILT_CHUSHOGI_GEN: &str = env!("CHUSHOGI_GEN");

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Sente,
    Gote,
}

impl Color {
    pub fn opponent(self) -> Color {
        match self {
            Color::Sente => Color::Gote,
            Color::Gote => Color::Sente,
        }
    }
    /// `+` for sente, `-` for gote, as in the server protocol.
    pub fn sign(self) -> char {
        match self {
            Color::Sente => '+',
            Color::Gote => '-',
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Color::Sente => "sente",
            Color::Gote => "gote",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Rules {
    Lishogi,
    Jcsa,
}

impl Rules {
    pub fn name(self) -> &'static str {
        match self {
            Rules::Lishogi => "lishogi",
            Rules::Jcsa => "jcsa",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GameEnd {
    Playing,
    RoyalsLost,
    BareKing,
    Stalemate,
    /// Insufficient material: only two royals remain.
    Draw,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub end: GameEnd,
    pub winner: Option<Color>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Well-formed move that is not in the legal list.
    Illegal,
    /// Unparseable SFEN or move.
    Syntax,
    /// The engine failed (exception, crashed process, I/O error).
    Engine(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Illegal => f.write_str("illegal move"),
            Error::Syntax => f.write_str("syntax error"),
            Error::Engine(e) => write!(f, "referee engine error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// A position under a rule set, advanced one move at a time.
///
/// Methods take `&mut self` because the subprocess backend does I/O for every
/// query. Calls are synchronous and take microseconds (ffi) or one pipe round
/// trip (subprocess).
pub trait Referee: Send {
    /// Sets the position from a full SFEN; keeps the rule set, resets `ply`.
    /// On error the position is unchanged.
    fn set_sfen(&mut self, sfen: &str) -> Result<(), Error>;
    fn set_rules(&mut self, rules: Rules) -> Result<(), Error>;
    /// Applies a move spelled exactly as `legal` prints it.
    fn apply(&mut self, usi: &str) -> Result<(), Error>;
    fn legal(&mut self) -> Result<Vec<String>, Error>;
    fn status(&mut self) -> Result<Status, Error>;
    fn in_check(&mut self, side: Color) -> Result<bool, Error>;
    fn sfen(&mut self) -> Result<String, Error>;
    /// Plies applied since the last `set_sfen` (or creation).
    fn ply(&mut self) -> Result<u32, Error>;
}

/// Which referee implementation to instantiate.
#[derive(Clone, Debug)]
pub enum Backend {
    #[cfg(feature = "ffi")]
    Ffi,
    Subprocess(PathBuf),
}

impl Default for Backend {
    fn default() -> Self {
        #[cfg(feature = "ffi")]
        return Backend::Ffi;
        #[cfg(not(feature = "ffi"))]
        return Backend::Subprocess(PathBuf::from(BUILT_CHUSHOGI_GEN));
    }
}

impl Backend {
    /// A referee at the start position under lishogi rules.
    pub fn create(&self) -> Result<Box<dyn Referee>, Error> {
        Ok(match self {
            #[cfg(feature = "ffi")]
            Backend::Ffi => Box::new(FfiReferee::new()?),
            Backend::Subprocess(path) => Box::new(SubprocessReferee::spawn(path)?),
        })
    }
}

/// SFEN shape check shared by both backends: one to four whitespace-separated
/// fields, ASCII only. Contents are checked by the engine.
pub(crate) fn sfen_shape_ok(sfen: &str) -> bool {
    let n = sfen.split_ascii_whitespace().count();
    sfen.is_ascii() && (1..=4).contains(&n) && !sfen.contains(['\n', '\r'])
}

/// Mirrors the engine's `parseUsiMove`: two or three squares `<file 1-12><rank a-l>`,
/// optionally followed by `+`, `=` or `?`. Says nothing about legality.
pub fn usi_syntax_ok(token: &str) -> bool {
    let body = token
        .strip_suffix(['+', '=', '?'])
        .unwrap_or(token)
        .as_bytes();
    let mut i = 0;
    let mut squares = 0;
    if body.is_empty() {
        return false;
    }
    while i < body.len() {
        if squares == 3 || !body[i].is_ascii_digit() {
            return false;
        }
        let start = i;
        while i < body.len() && body[i].is_ascii_digit() {
            i += 1;
        }
        if i >= body.len() || !(b'a'..=b'l').contains(&body[i]) {
            return false;
        }
        let file: u64 = match std::str::from_utf8(&body[start..i]).unwrap().parse() {
            Ok(f) => f,
            Err(_) => return false,
        };
        if !(1..=12).contains(&file) {
            return false;
        }
        squares += 1;
        i += 1;
    }
    squares == 2 || squares == 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usi_syntax() {
        for ok in ["8i8h", "4d4b+", "7e7f7g", "7e7f7e", "12h12i+", "8i8h=", "08i8h"] {
            assert!(usi_syntax_ok(ok), "{ok}");
        }
        for bad in ["", "8i", "8i8", "13a1a", "0a1a", "8m8h", "1a2a3a4a", "+8i8h", "8i8h++", "99999999999999999999a1a"] {
            assert!(!usi_syntax_ok(bad), "{bad}");
        }
    }
}
