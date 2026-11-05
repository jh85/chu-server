//! One task per game: sole owner of the referee, the clocks and the history.
//!
//! Per move (DESIGN.md "Game rules", SERVER-PROTOCOL.md §5): turn and colour
//! check, clock check, referee legality, echo, repetition, referee status,
//! ply cap. Every result is computed here once and sent to both players,
//! the monitors, the lobby and the store from the same place.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use chu_proto::{ClientMsg, GameName, GameSummary, PlayedMove, Reason, Side, Verdict, AGREE_TIMEOUT_S, MAX_MOVES};
use chu_rules::{GameEnd, Referee};
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};
use tracing::{info, warn};

use crate::clock::TimeControl;
use crate::lobby::{LobbyMsg, LobbyTx};
use crate::store::{FinishedGame, RecordedMove, StartedGame, StoreMsg, StoreTx};
use crate::{chu_side, proto_side, Conn, Ctl};

#[derive(Debug)]
pub enum GameInput {
    /// AGREE, REJECT, a move or %TORYO from the player on `side`, stamped
    /// when the session read it.
    Msg { side: Side, msg: ClientMsg, at: Instant },
    Disconnected { side: Side, conn_id: u64 },
    /// Line too long or flooding: the session closes the connection.
    Violation { side: Side, conn_id: u64 },
    /// The player logged in again and asked for this game.
    Reattach { side: Side, conn: Conn, ctl: mpsc::UnboundedSender<Ctl> },
    Monitor { conn: Conn, on: bool },
    Show { conn: Conn },
}

pub type GameTx = mpsc::UnboundedSender<GameInput>;

pub struct PlayerSlot {
    pub name: String,
    pub conn: Option<Conn>,
    pub ctl: Option<mpsc::UnboundedSender<Ctl>>,
}

pub struct GameSetup {
    pub id: String,
    pub name: GameName,
    pub initial_sfen: String,
    pub tc: TimeControl,
    pub rated: bool,
    /// Sente first.
    pub players: [PlayerSlot; 2],
    /// Set to `initial_sfen` and the game's rules already.
    pub referee: Box<dyn Referee>,
    pub store: StoreTx,
    pub lobby: LobbyTx,
}

fn idx(s: Side) -> usize {
    match s {
        Side::Sente => 0,
        Side::Gote => 1,
    }
}

struct Game {
    id: String,
    name: GameName,
    initial_sfen: String,
    tc: TimeControl,
    rated: bool,
    players: [PlayerSlot; 2],
    referee: Box<dyn Referee>,
    store: StoreTx,
    lobby: LobbyTx,
    monitors: Vec<Conn>,
    started_at: SystemTime,
    remaining_ms: [u64; 2],
    to_move: Side,
    turn_start: Instant,
    moves: Vec<RecordedMove>,
    /// After each move: did it leave the opponent in check?
    gave_check: Vec<bool>,
    /// Repetition key (board, side, lion field) -> position indices (0 = initial).
    seen: HashMap<String, Vec<usize>>,
    /// Players that reattached and have not answered the summary yet.
    resuming: [bool; 2],
}

/// Ending of a game: the reason line and the winner (None = draw or void).
struct Outcome(Reason, Option<Side>);

pub fn spawn(setup: GameSetup) -> GameTx {
    let (tx, rx) = mpsc::unbounded_channel();
    spawn_with(setup, rx);
    tx
}

/// Runs a game on a channel created by the caller.
pub fn spawn_with(setup: GameSetup, rx: mpsc::UnboundedReceiver<GameInput>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(setup, rx))
}

fn rep_key(sfen: &str) -> String {
    sfen.split(' ').take(3).collect::<Vec<_>>().join(" ")
}

async fn run(setup: GameSetup, mut rx: mpsc::UnboundedReceiver<GameInput>) {
    let to_move = if setup.initial_sfen.split(' ').nth(1) == Some("w") { Side::Gote } else { Side::Sente };
    let mut g = Game {
        id: setup.id,
        name: setup.name,
        initial_sfen: setup.initial_sfen,
        tc: setup.tc,
        rated: setup.rated,
        players: setup.players,
        referee: setup.referee,
        store: setup.store,
        lobby: setup.lobby,
        monitors: Vec::new(),
        started_at: SystemTime::now(),
        remaining_ms: [setup.tc.total_ms; 2],
        to_move,
        turn_start: Instant::now(),
        moves: Vec::new(),
        gave_check: Vec::new(),
        seen: HashMap::new(),
        resuming: [false; 2],
    };
    g.seen.insert(rep_key(&g.initial_sfen), vec![0]);
    if g.agree_phase(&mut rx).await {
        let outcome = g.play(&mut rx).await;
        g.finish(outcome);
    }
}

impl Game {
    fn summary(&self, your_turn: Side) -> GameSummary {
        GameSummary {
            protocol_version: "1.2".into(),
            protocol_mode: "Server".into(),
            format: "Chushogi 1.0".into(),
            rules: self.name.rules().into(),
            game_id: self.id.clone(),
            name_plus: self.players[0].name.clone(),
            name_minus: self.players[1].name.clone(),
            your_turn,
            rematch_on_draw: false,
            to_move: self.to_move,
            max_moves: MAX_MOVES,
            time: self.tc.summary(),
            sfen: self.initial_sfen.clone(),
            moves: self
                .moves
                .iter()
                .map(|m| PlayedMove { side: m.side, usi: m.usi.clone(), time: m.time })
                .collect(),
        }
    }

    fn send(&self, side: Side, line: &str) {
        if let Some(c) = &self.players[idx(side)].conn {
            c.send(line);
        }
    }

    fn send_both(&self, line: &str) {
        self.send(Side::Sente, line);
        self.send(Side::Gote, line);
    }

    fn send_monitors(&mut self, line: &str) {
        let tagged = format!("##[MONITOR][{}] {line}", self.id);
        self.monitors.retain(|m| m.send(tagged.clone()));
    }

    fn send_summary(&self, side: Side) {
        for line in self.summary(side).to_lines() {
            self.send(side, &line);
        }
    }

    fn release_players(&mut self) {
        // The lobby hears first, so a %%GAME sent after the result lines is
        // never handled while the lobby still thinks the player is busy.
        let _ = self.lobby.send(LobbyMsg::GameEnded { id: self.id.clone() });
        for p in &self.players {
            if let Some(ctl) = &p.ctl {
                let _ = ctl.send(Ctl::Left);
            }
        }
    }

    /// Inputs that are handled the same way in every phase; returns the
    /// input back if the phase must handle it.
    fn common(&mut self, input: GameInput) -> Option<GameInput> {
        match input {
            GameInput::Monitor { conn, on } => {
                self.monitors.retain(|m| m.id != conn.id);
                if on {
                    self.monitors.push(conn.clone());
                }
                conn.send(format!("##[{}][{}] +OK", if on { "MONITORON" } else { "MONITOROFF" }, self.id));
                None
            }
            GameInput::Show { conn } => {
                for m in &self.moves {
                    conn.send(format!("##[SHOW][{}] {}{},T{}", self.id, m.side.sign(), m.usi, m.time));
                }
                conn.send(format!("##[SHOW][{}] +OK", self.id));
                None
            }
            other => Some(other),
        }
    }

    /// Sends the summaries and waits for both AGREEs. False if cancelled.
    async fn agree_phase(&mut self, rx: &mut mpsc::UnboundedReceiver<GameInput>) -> bool {
        self.send_summary(Side::Sente);
        self.send_summary(Side::Gote);
        let deadline = Instant::now() + Duration::from_secs(AGREE_TIMEOUT_S);
        let mut agreed = [false; 2];
        let rejected_by = loop {
            tokio::select! {
                biased;
                input = rx.recv() => {
                    let Some(input) = input else { break Side::Sente };
                    match self.common(input) {
                        Some(GameInput::Msg { side, msg: ClientMsg::Agree(id), .. }) => {
                            if id.as_ref().is_none_or(|id| *id == self.id) {
                                agreed[idx(side)] = true;
                                if agreed == [true; 2] {
                                    self.start();
                                    return true;
                                }
                            }
                        }
                        Some(GameInput::Msg { side, msg: ClientMsg::Reject(_), .. })
                        | Some(GameInput::Disconnected { side, .. })
                        | Some(GameInput::Violation { side, .. }) => break side,
                        _ => {}
                    }
                }
                _ = sleep_until(deadline) => {
                    break if agreed[0] { Side::Gote } else { Side::Sente };
                }
            }
        };
        let line = format!("REJECT:{} by {}", self.id, self.players[idx(rejected_by)].name);
        info!("{line}");
        self.release_players();
        self.send_both(&line);
        false
    }

    async fn play(&mut self, rx: &mut mpsc::UnboundedReceiver<GameInput>) -> Outcome {
        loop {
            let mover = self.to_move;
            let allowance = self.tc.allowance_ms(self.remaining_ms[idx(mover)]);
            let deadline = self.turn_start + Duration::from_millis(allowance);
            let input = tokio::select! {
                biased;
                input = rx.recv() => match input {
                    Some(i) => i,
                    None => return Outcome(Reason::Chudan, None),
                },
                _ = sleep_until(deadline) => return Outcome(Reason::TimeUp, Some(mover.opponent())),
            };
            let Some(input) = self.common(input) else { continue };
            match input {
                GameInput::Msg { side, msg, at } => match msg {
                    ClientMsg::Move { side: colour, usi } => {
                        if let Some(end) = self.on_move(side, colour, &usi, at) {
                            return end;
                        }
                    }
                    ClientMsg::Toryo => return Outcome(Reason::Resign, Some(side.opponent())),
                    ClientMsg::Agree(_) if self.resuming[idx(side)] => {
                        self.resuming[idx(side)] = false;
                        self.send(side, &format!("START:{}", self.id));
                    }
                    _ => {}
                },
                GameInput::Disconnected { side, conn_id } => {
                    let p = &mut self.players[idx(side)];
                    if p.conn.as_ref().is_some_and(|c| c.id == conn_id) {
                        info!("game {}: {} disconnected; clock keeps running", self.id, p.name);
                        p.conn = None;
                        p.ctl = None;
                    }
                }
                GameInput::Violation { side, conn_id } => {
                    if self.players[idx(side)].conn.as_ref().is_some_and(|c| c.id == conn_id) {
                        return Outcome(Reason::Chudan, None);
                    }
                }
                GameInput::Reattach { side, conn, ctl } => {
                    info!("game {}: {} reattached", self.id, self.players[idx(side)].name);
                    let p = &mut self.players[idx(side)];
                    p.conn = Some(conn);
                    p.ctl = Some(ctl);
                    self.resuming[idx(side)] = true;
                    self.send_summary(side);
                }
                GameInput::Monitor { .. } | GameInput::Show { .. } => unreachable!(),
            }
        }
    }

    /// Steps 1-7 for one move line. Some(outcome) ends the game.
    fn on_move(&mut self, side: Side, colour: Side, usi: &str, at: Instant) -> Option<Outcome> {
        let mover = self.to_move;
        // 1. Turn and colour.
        if side != mover || colour != side {
            return Some(Outcome(Reason::IllegalMove, Some(side.opponent())));
        }
        // 2. Clock, at the time the session read the line.
        let elapsed_ms = at.saturating_duration_since(self.turn_start).as_millis() as u64;
        let allowance = self.tc.allowance_ms(self.remaining_ms[idx(mover)]);
        if elapsed_ms > allowance {
            return Some(Outcome(Reason::TimeUp, Some(mover.opponent())));
        }
        // 3. Legality.
        match self.referee.apply(usi) {
            Ok(()) => {}
            Err(chu_rules::Error::Illegal | chu_rules::Error::Syntax) => {
                return Some(Outcome(Reason::IllegalMove, Some(mover.opponent())))
            }
            Err(e) => {
                warn!("game {}: {e}", self.id);
                return Some(Outcome(Reason::Chudan, None));
            }
        }
        let charged = self.tc.charged_ms(elapsed_ms);
        self.remaining_ms[idx(mover)] = self.tc.after_move(self.remaining_ms[idx(mover)], charged);
        let (sfen, gave_check, status) = match (|| {
            Ok::<_, chu_rules::Error>((
                self.referee.sfen()?,
                self.referee.in_check(chu_side(mover.opponent()))?,
                self.referee.status()?,
            ))
        })() {
            Ok(v) => v,
            Err(e) => {
                warn!("game {}: {e}", self.id);
                return Some(Outcome(Reason::Chudan, None));
            }
        };
        let mv = RecordedMove { side: mover, usi: usi.to_owned(), elapsed_ms, time: self.tc.units(charged), sfen_after: sfen };
        // Echo every applied move, including the one that ends the game.
        let echo = format!("{}{},T{}", mover.sign(), usi, mv.time);
        self.send_both(&echo);
        self.send_monitors(&echo);
        let ply = self.moves.len() as u32 + 1;
        let _ = self.store.send(StoreMsg::Move { game_id: self.id.clone(), ply, mv: mv.clone() });
        let key = rep_key(&mv.sfen_after);
        self.moves.push(mv);
        self.gave_check.push(gave_check);
        self.to_move = mover.opponent();
        self.turn_start = Instant::now();

        // 4. Repetition.
        let n = self.moves.len();
        let occ = self.seen.entry(key).or_default();
        occ.push(n);
        if occ.len() >= 4 {
            let first = occ[0];
            return Some(self.repetition(first, n));
        }
        // 5. Referee status.
        let winner = status.winner.map(proto_side);
        match status.end {
            GameEnd::Playing => {}
            GameEnd::RoyalsLost => return Some(Outcome(Reason::RoyalsLost, winner)),
            GameEnd::BareKing => return Some(Outcome(Reason::BareKing, winner)),
            GameEnd::Stalemate => return Some(Outcome(Reason::Stalemate, winner)),
            GameEnd::Draw => return Some(Outcome(Reason::Insufficient, None)),
        }
        // 6. Ply cap.
        if n >= MAX_MOVES as usize {
            return Some(Outcome(Reason::MaxMoves, None));
        }
        None
    }

    /// Fourfold repetition between positions `first` and `now`: a draw, unless
    /// every move of one side in that span gave check and not every move of
    /// the other did; then the checking side loses.
    fn repetition(&self, first: usize, now: usize) -> Outcome {
        let mut all_check = [true; 2];
        for k in first..now {
            if !self.gave_check[k] {
                all_check[idx(self.moves[k].side)] = false;
            }
        }
        match all_check {
            [true, false] => Outcome(Reason::OuteSennichite, Some(Side::Gote)),
            [false, true] => Outcome(Reason::OuteSennichite, Some(Side::Sente)),
            _ => Outcome(Reason::Sennichite, None),
        }
    }

    fn finish(&mut self, Outcome(reason, winner): Outcome) {
        let verdict = |s: Side| match (reason, winner) {
            (Reason::Chudan, _) => Verdict::Censored,
            (_, None) => Verdict::Draw,
            (_, Some(w)) if w == s => Verdict::Win,
            _ => Verdict::Lose,
        };
        self.release_players();
        for s in [Side::Sente, Side::Gote] {
            self.send(s, reason.as_str());
            self.send(s, verdict(s).as_str());
        }
        self.send_monitors(reason.as_str());
        info!("game {} {} winner {:?} after {} plies", self.id, reason.as_str(), winner, self.moves.len());
        let finished = FinishedGame {
            game: self.started_game(),
            rated: self.rated,
            ended_at: SystemTime::now(),
            moves: std::mem::take(&mut self.moves),
            reason,
            winner: if reason == Reason::Chudan { None } else { winner },
            summary: self.summary(Side::Sente).to_lines(),
        };
        let _ = self.store.send(StoreMsg::Finished(Box::new(finished)));
    }

    /// Both agreed: START to both, clocks begin, and the game row is written.
    fn start(&mut self) {
        self.started_at = SystemTime::now();
        let line = format!("START:{}", self.id);
        self.send_both(&line);
        self.turn_start = Instant::now();
        let _ = self.store.send(StoreMsg::Started(self.started_game()));
        info!("{line}");
    }

    fn started_game(&self) -> StartedGame {
        StartedGame {
            id: self.id.clone(),
            league: self.name.to_string(),
            rules: self.name.rules().into(),
            sente: self.players[0].name.clone(),
            gote: self.players[1].name.clone(),
            initial_sfen: self.initial_sfen.clone(),
            started_at: self.started_at,
        }
    }
}
