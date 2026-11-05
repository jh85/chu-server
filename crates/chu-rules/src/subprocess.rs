//! `subprocess` backend: drives `chushogi-gen` over its stdin protocol
//! (`position`, `rules`, `move`, `legal`, `status`, `check`, `sfen`).
//!
//! `chushogi-gen` exits on a bad SFEN or an illegal move, so moves are checked
//! against the legal list before they are sent, and a dead child is respawned
//! and brought back to the last good state (rules, SFEN, moves) on next use.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::{sfen_shape_ok, usi_syntax_ok, Color, Error, GameEnd, Referee, Rules, Status, START_SFEN};

struct Proc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Proc {
    fn spawn(path: &Path) -> Result<Proc, Error> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Engine(format!("spawn {}: {e}", path.display())))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Ok(Proc { child, stdin, stdout })
    }

    /// Sends one line and returns the reply after `expect` (`"ok"` replies give "").
    fn cmd(&mut self, line: &str, expect: &str) -> Result<String, Error> {
        let io = |e: std::io::Error| Error::Engine(format!("chushogi-gen: {e}"));
        writeln!(self.stdin, "{line}").map_err(io)?;
        self.stdin.flush().map_err(io)?;
        let mut out = String::new();
        if self.stdout.read_line(&mut out).map_err(io)? == 0 {
            return Err(Error::Engine(format!("chushogi-gen exited on {line:?}")));
        }
        let out = out.trim_end_matches(['\n', '\r']);
        let (head, rest) = out.split_once(' ').unwrap_or((out, ""));
        if head != expect {
            return Err(Error::Engine(format!("{line:?} -> {out:?}")));
        }
        Ok(rest.to_owned())
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

pub struct SubprocessReferee {
    path: PathBuf,
    proc: Option<Proc>,
    rules: Rules,
    initial: String,
    moves: Vec<String>,
    legal_cache: Option<Vec<String>>,
}

impl SubprocessReferee {
    pub fn spawn(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let mut r = SubprocessReferee {
            path: path.into(),
            proc: None,
            rules: Rules::Lishogi,
            initial: START_SFEN.to_owned(),
            moves: Vec::new(),
            legal_cache: None,
        };
        r.proc()?;
        Ok(r)
    }

    /// The live child, respawned and restored to the last good state if needed.
    fn proc(&mut self) -> Result<&mut Proc, Error> {
        if self.proc.is_none() {
            let mut p = Proc::spawn(&self.path)?;
            p.cmd(&format!("rules {}", self.rules.name()), "ok")?;
            let mut line = format!("position sfen {}", self.initial);
            if !self.moves.is_empty() {
                line.push_str(" moves ");
                line.push_str(&self.moves.join(" "));
            }
            p.cmd(&line, "ok")?;
            self.proc = Some(p);
        }
        Ok(self.proc.as_mut().unwrap())
    }

    /// Runs a command; a failure kills the child so the next call starts fresh.
    fn cmd(&mut self, line: &str, expect: &str) -> Result<String, Error> {
        let r = self.proc()?.cmd(line, expect);
        if r.is_err() {
            self.proc = None;
        }
        r
    }
}

impl Referee for SubprocessReferee {
    fn set_sfen(&mut self, sfen: &str) -> Result<(), Error> {
        if !sfen_shape_ok(sfen) {
            return Err(Error::Syntax);
        }
        match self.cmd(&format!("position sfen {sfen}"), "ok") {
            Ok(_) => {
                self.initial = sfen.to_owned();
                self.moves.clear();
                self.legal_cache = None;
                Ok(())
            }
            // The child died on the SFEN; the restore on next use replays the old state.
            Err(_) => Err(Error::Syntax),
        }
    }

    fn set_rules(&mut self, rules: Rules) -> Result<(), Error> {
        self.cmd(&format!("rules {}", rules.name()), "ok")?;
        self.rules = rules;
        self.legal_cache = None;
        Ok(())
    }

    fn apply(&mut self, usi: &str) -> Result<(), Error> {
        if !usi_syntax_ok(usi) {
            return Err(Error::Syntax);
        }
        if !self.legal()?.iter().any(|m| m == usi) {
            return Err(Error::Illegal);
        }
        self.cmd(&format!("move {usi}"), "ok")?;
        self.moves.push(usi.to_owned());
        self.legal_cache = None;
        Ok(())
    }

    fn legal(&mut self) -> Result<Vec<String>, Error> {
        if self.legal_cache.is_none() {
            let out = self.cmd("legal", "legal")?;
            self.legal_cache = Some(out.split_ascii_whitespace().map(str::to_owned).collect());
        }
        Ok(self.legal_cache.clone().unwrap())
    }

    fn status(&mut self) -> Result<Status, Error> {
        let out = self.cmd("status", "status")?;
        let mut it = out.split_ascii_whitespace();
        let end = match it.next() {
            Some("playing") => GameEnd::Playing,
            Some("royalslost") => GameEnd::RoyalsLost,
            Some("bareking") => GameEnd::BareKing,
            Some("stalemate") => GameEnd::Stalemate,
            Some("draw") => GameEnd::Draw,
            other => return Err(Error::Engine(format!("status reply {other:?}"))),
        };
        let winner = match it.next() {
            Some("sente") => Some(Color::Sente),
            Some("gote") => Some(Color::Gote),
            _ => None,
        };
        Ok(Status { end, winner })
    }

    fn in_check(&mut self, side: Color) -> Result<bool, Error> {
        let out = self.cmd(&format!("check {}", side.name()), "check")?;
        Ok(out == "yes")
    }

    fn sfen(&mut self) -> Result<String, Error> {
        self.cmd("sfen", "sfen")
    }

    fn ply(&mut self) -> Result<u32, Error> {
        Ok(self.moves.len() as u32)
    }
}
