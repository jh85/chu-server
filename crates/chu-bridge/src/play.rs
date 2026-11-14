//! Playing on the server: one session, one engine, `--games` games, with the
//! translation table of DESIGN.md "USI bridge for the AlphaZero agents".
//!
//! The clock model is the server's (SERVER-PROTOCOL.md §5), fed only by the
//! `T` values of the echoes; local time only measures how long the current
//! turn has run. The engine is told the clocks minus `--margin-ms`, and if it
//! has not answered `stop_lead` before the server's limit, it is sent `stop`
//! and whatever `bestmove` comes back is forwarded.

use std::time::Duration;

use chu_proto::clock::TimeControl;
use chu_proto::{ColorReq, GameSummary, LineBucket, ServerMsg, Side, Verdict, AGREE_TIMEOUT_S};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::{sleep, sleep_until, timeout, timeout_at, Instant};
use tracing::{debug, info, warn};

use crate::engine::{parse_bestmove, Engine};
use crate::Res;

pub struct Config {
    pub server: String,
    pub name: String,
    pub password: String,
    pub game: String,
    pub color: ColorReq,
    pub games: u32,
    pub margin_ms: u64,
}

/// How early before the server's limit a running search is stopped.
const STOP_LEAD: Duration = Duration::from_millis(200);
/// Longest wait for a `bestmove` after `stop`.
const STOP_ANSWER: Duration = Duration::from_secs(10);
/// How long past a deadline the server has to say something (the side to
/// move's time limit in a game, the AGREE limit before one) before the
/// connection counts as dead. A connection that only receives never notices
/// a peer gone from the network.
const LIVENESS_SLACK: Duration = Duration::from_secs(10);
/// Longest wait between reconnection attempts.
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Why a game was left.
enum Stop {
    /// The server connection broke; reconnect and resume.
    Disconnected,
    Fatal(String),
}

impl From<Box<dyn std::error::Error + Send + Sync>> for Stop {
    fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Stop {
        Stop::Fatal(e.to_string())
    }
}

type Step<T> = Result<T, Stop>;

struct Server {
    rd: Lines<BufReader<OwnedReadHalf>>,
    wr: OwnedWriteHalf,
    /// Keeps a fast engine under the server's line limit.
    bucket: LineBucket,
}

impl Server {
    async fn send(&mut self, line: &str) -> Step<()> {
        let wait = self.bucket.wait(std::time::Instant::now());
        if !wait.is_zero() {
            sleep(wait).await;
        }
        if line.starts_with("LOGIN ") {
            debug!(target: "csa", "> LOGIN …");
        } else {
            debug!(target: "csa", "> {line}");
        }
        self.wr.write_all(format!("{line}\n").as_bytes()).await.map_err(|_| Stop::Disconnected)
    }

    async fn recv(&mut self) -> Step<String> {
        match self.rd.next_line().await {
            Ok(Some(l)) => {
                debug!(target: "csa", "< {l}");
                Ok(l)
            }
            _ => Err(Stop::Disconnected),
        }
    }
}

/// Connects and logs in. Retries at most 5 s apart for `fast`, then up to
/// RETRY_MAX apart; gives up after `fast` only if `give_up`. A rejected
/// password is never retried.
async fn connect(cfg: &Config, fast: Duration, give_up: bool) -> Res<Server> {
    let start = Instant::now();
    let mut backoff = Duration::from_millis(250);
    for attempt in 1.. {
        match TcpStream::connect(&cfg.server).await {
            Ok(sock) => {
                sock.set_nodelay(true)?;
                let (rd, wr) = sock.into_split();
                let bucket = LineBucket::client(std::time::Instant::now());
                let mut s = Server { rd: BufReader::new(rd).lines(), wr, bucket };
                let login = format!("LOGIN {} {} x1", cfg.name, cfg.password);
                if s.send(&login).await.is_ok() {
                    match s.recv().await.map(|l| ServerMsg::parse(&l)) {
                        Ok(ServerMsg::LoginOk(_)) => {
                            info!(server = %cfg.server, attempt, "logged in");
                            return Ok(s);
                        }
                        Ok(ServerMsg::LoginIncorrect) => return Err("LOGIN:incorrect".into()),
                        _ => warn!(server = %cfg.server, attempt, "no login reply"),
                    }
                }
            }
            Err(e) => warn!(server = %cfg.server, attempt, error = %e, "cannot connect"),
        }
        let slow = start.elapsed() + backoff > fast;
        if slow && give_up {
            return Err(format!("cannot reach {} after {attempt} attempts", cfg.server).into());
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(if slow { RETRY_MAX } else { Duration::from_secs(5) });
    }
    unreachable!()
}

/// AGREE, then START (true) or REJECT (false).
async fn agree(srv: &mut Server) -> Step<bool> {
    srv.send("AGREE").await?;
    let deadline = Instant::now() + Duration::from_secs(AGREE_TIMEOUT_S) + LIVENESS_SLACK;
    loop {
        let line = timeout_at(deadline, srv.recv()).await.map_err(|_| {
            warn!("no START or REJECT from the server");
            Stop::Disconnected
        })??;
        match ServerMsg::parse(&line) {
            ServerMsg::Start(_) => return Ok(true),
            ServerMsg::Reject { game_id, by } => {
                info!(game = %game_id, by = %by, "game cancelled");
                return Ok(false);
            }
            _ => {}
        }
    }
}

/// A game in progress when the connection broke.
struct Track {
    id: String,
    plies: usize,
    turn_start: Instant,
    dropped_at: Instant,
    /// How long to keep trying to reconnect.
    budget: Duration,
}

/// A `go` without its `bestmove` yet.
struct Pending {
    id: String,
    started: Instant,
    /// When to send `stop`.
    deadline: Instant,
    stopped: bool,
}

fn idx(s: Side) -> usize {
    match s {
        Side::Sente => 0,
        Side::Gote => 1,
    }
}

/// Keep-alive interval, queued or in a game; well under the 600 s idle limit.
const KEEP_ALIVE: Duration = Duration::from_secs(60);

pub async fn run(cfg: Config, mut engine: Engine) -> Res<()> {
    let mut bridge = Bridge { cfg: &cfg, engine: &mut engine, go_seq: 0, played: 0, track: None };
    let mut server: Option<Server> = None;
    // Only the first login gives up (a wrong address shows at once); after
    // that the bridge waits for the server however long it is away. A game
    // cut off is resumed if the server is back within its budget.
    let mut first = true;
    while bridge.played < cfg.games {
        let srv = match server.as_mut() {
            Some(s) => s,
            None => {
                let fast = bridge.track.as_ref().map_or(Duration::from_secs(60), |t| t.budget);
                let s = connect(&cfg, fast, first).await?;
                first = false;
                server.insert(s)
            }
        };
        match bridge.one_game(srv).await {
            Ok(()) => {}
            Err(Stop::Disconnected) => {
                warn!("server connection lost");
                server = None;
            }
            Err(Stop::Fatal(e)) => return Err(e.into()),
        }
    }
    if let Some(s) = server.as_mut() {
        if s.send("LOGOUT").await.is_ok() {
            let _ = timeout(Duration::from_secs(5), s.recv()).await;
        }
    }
    let _ = engine.send("quit").await;
    Ok(())
}

struct Bridge<'a> {
    cfg: &'a Config,
    engine: &'a mut Engine,
    go_seq: u64,
    played: u32,
    track: Option<Track>,
}

impl Bridge<'_> {
    /// Queues, then plays one game (or resumes the tracked one) to its end.
    async fn one_game(&mut self, srv: &mut Server) -> Step<()> {
        srv.send(&format!("%%GAME {} {}", self.cfg.game, self.cfg.color.sign())).await?;
        let mut lines = Vec::new();
        loop {
            // A floodgate round can be half an hour away; keep the queued
            // connection under the server's idle limit (§1).
            let l = match tokio::time::timeout(KEEP_ALIVE, srv.recv()).await {
                Ok(l) => l?,
                Err(_) if lines.is_empty() => {
                    srv.send("").await?;
                    continue;
                }
                Err(_) => continue,
            };
            match ServerMsg::parse(&l) {
                ServerMsg::BeginSummary => lines = vec![l],
                ServerMsg::Lobby(l) if l.starts_with("##[ERROR]") => return Err(Stop::Fatal(l)),
                _ if !lines.is_empty() => {
                    let end = l == "END Game_Summary";
                    lines.push(l);
                    if end {
                        break;
                    }
                }
                _ => {}
            }
        }
        let summary_at = Instant::now();
        let summary = GameSummary::parse(&lines).map_err(|e| Stop::Fatal(format!("bad Game_Summary: {e}")))?;

        let resumed = match self.track.take() {
            Some(t) if t.id == summary.game_id => Some(t),
            Some(t) => {
                // The old game ended while we were away; this is a new one.
                warn!(game = %t.id, "the game ended while disconnected");
                self.engine.send("gameover lose").await?;
                self.played += 1;
                if self.played >= self.cfg.games {
                    srv.send("REJECT").await?;
                    return Ok(());
                }
                None
            }
            None => None,
        };
        if resumed.is_none() {
            if self.engine.options.iter().any(|o| o == "Rules") {
                self.engine.set_option("Rules", &summary.rules).await?;
            }
            self.engine.ready(Duration::from_secs(20)).await?;
            self.engine.send("usinewgame").await?;
        }
        match agree(srv).await {
            Ok(true) => self.play(srv, summary, summary_at, resumed).await,
            Ok(false) => {
                if resumed.is_none() {
                    self.engine.send("gameover draw").await?;
                }
                Ok(())
            }
            Err(e) => {
                // Still the same game for the next attempt.
                self.track = resumed;
                Err(e)
            }
        }
    }

    async fn play(
        &mut self,
        srv: &mut Server,
        summary: GameSummary,
        summary_at: Instant,
        resumed: Option<Track>,
    ) -> Step<()> {
        let id = summary.game_id.clone();
        let me = summary.your_turn;
        let tc = TimeControl::from_summary(&summary.time);
        let unit = tc.unit.ms();
        let mut rem = [tc.total_ms; 2];
        let mut moves = Vec::new();
        for m in &summary.moves {
            rem[idx(m.side)] = tc.after_move(rem[idx(m.side)], m.time * unit);
            moves.push(m.usi.clone());
        }
        let mut to_move = summary.to_move;
        let mut turn_start = match (&resumed, summary.time.resumed) {
            // A resumed game's clocks as the server sent them (§4): main time
            // at the start of the turn, and the turn began Turn_Used ago, so
            // the usual byoyomi or increment rule applies from here.
            (Some(_), Some(r)) => {
                rem = r.remaining.map(|units| units * unit);
                let used = Duration::from_millis(r.turn_used * unit);
                summary_at.checked_sub(used).unwrap_or(summary_at)
            }
            // Without them: the turn began at the last echo we saw, or, if the
            // opponent moved while we were away, no earlier than the drop.
            (Some(t), None) if t.plies == moves.len() => t.turn_start,
            (Some(t), None) => t.dropped_at,
            (None, _) => Instant::now(),
        };
        info!(game = %id, side = %me.sign(), resumed = resumed.is_some(), plies = moves.len(), "game started");

        let mut pending: Option<Pending> = None;
        let mut sent: Option<String> = None;
        let mut keep_alive_at = Instant::now() + KEEP_ALIVE;
        loop {
            if to_move == me && pending.is_none() && sent.is_none() {
                pending = Some(self.go(&summary.sfen, &moves, me, &tc, rem, turn_start).await?);
            }
            let watchdog = pending.as_ref().filter(|p| !p.stopped).map(|p| p.deadline);
            // By the side to move's time limit the server sends a move or a
            // result; silence past it means the connection is dead.
            let live_until = turn_start + Duration::from_millis(tc.allowance_ms(rem[idx(to_move)])) + LIVENESS_SLACK;
            tokio::select! {
                line = timeout_at(live_until, srv.recv()) => {
                    let line = match line {
                        Ok(Ok(l)) => {
                            keep_alive_at = Instant::now() + KEEP_ALIVE;
                            l
                        }
                        r => {
                            if r.is_err() {
                                warn!(game = %id, "no word from the server past the time limit");
                            }
                            let stop = Stop::Disconnected;
                            self.abandon(&mut pending).await?;
                            let now = Instant::now();
                            let mine = rem[idx(me)] + tc.byoyomi_ms;
                            let used = if to_move == me { turn_start.elapsed().as_millis() as u64 } else { 0 };
                            let budget = Duration::from_millis(mine.saturating_sub(used)).max(Duration::from_secs(10));
                            self.track = Some(Track { id: id.clone(), plies: moves.len(), turn_start, dropped_at: now, budget });
                            return Err(stop);
                        }
                    };
                    match ServerMsg::parse(&line) {
                        ServerMsg::Move(m) => {
                            if m.side == me {
                                if sent.as_deref() != Some(m.usi.as_str()) {
                                    warn!(game = %id, echo = %line, sent = ?sent, "echo differs from the move sent");
                                }
                                sent = None;
                            }
                            rem[idx(m.side)] = tc.after_move(rem[idx(m.side)], m.time * unit);
                            moves.push(m.usi);
                            to_move = m.side.opponent();
                            turn_start = Instant::now();
                        }
                        ServerMsg::Reason(reason) => {
                            let verdict = loop {
                                match timeout(Duration::from_secs(10), srv.recv()).await {
                                    Ok(Ok(l)) => {
                                        if let ServerMsg::Verdict(v) = ServerMsg::parse(&l) {
                                            break v;
                                        }
                                    }
                                    Ok(Err(stop)) => return Err(stop),
                                    Err(_) => return Err(Stop::Fatal("no verdict after the reason line".into())),
                                }
                            };
                            self.abandon(&mut pending).await?;
                            let result = match verdict {
                                Verdict::Win => "win",
                                Verdict::Lose => "lose",
                                Verdict::Draw | Verdict::Censored => "draw",
                            };
                            self.engine.send(&format!("gameover {result}")).await?;
                            self.played += 1;
                            info!(
                                game = %id, reason = reason.as_str(), verdict = verdict.as_str(),
                                plies = moves.len(), played = self.played, "game over"
                            );
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                line = self.engine.recv() => {
                    let line = line?;
                    match parse_bestmove(&line) {
                        Some((mv, echo)) => {
                            let ours = pending.as_ref().is_some_and(|p| echo.as_ref().is_none_or(|e| *e == p.id));
                            if !ours {
                                warn!(game = %id, line = %line, "bestmove without a go outstanding; discarded");
                                continue;
                            }
                            let p = pending.take().unwrap();
                            let think_ms = p.started.elapsed().as_millis() as u64;
                            let ply = moves.len() + 1;
                            if mv == "resign" {
                                info!(game = %id, ply, think_ms, "resign");
                                srv.send("%TORYO").await?;
                            } else {
                                info!(game = %id, ply, usi = %mv, think_ms, stopped = p.stopped, "move");
                                srv.send(&format!("{}{mv}", me.sign())).await?;
                            }
                            sent = Some(mv);
                        }
                        None if line.starts_with("info string") => info!(game = %id, engine = %line, "engine says"),
                        None => debug!(target: "usi", "{line}"),
                    }
                }
                _ = sleep_until(keep_alive_at) => {
                    // A failed write shows up as a failed read right after.
                    let _ = srv.send("").await;
                    keep_alive_at = Instant::now() + KEEP_ALIVE;
                }
                _ = sleep_until(watchdog.unwrap_or_else(Instant::now)), if watchdog.is_some() => {
                    let p = pending.as_mut().unwrap();
                    warn!(game = %id, ply = moves.len() + 1, go = %p.id, "no bestmove near the time limit; stop");
                    self.engine.send("stop").await?;
                    p.stopped = true;
                }
            }
        }
    }

    /// Sends `position` and `go` for our turn.
    async fn go(
        &mut self,
        sfen: &str,
        moves: &[String],
        me: Side,
        tc: &TimeControl,
        rem: [u64; 2],
        turn_start: Instant,
    ) -> Step<Pending> {
        let margin = self.cfg.margin_ms as i64;
        let elapsed = turn_start.elapsed().as_millis() as i64;
        let mut own = rem[idx(me)] as i64 - elapsed - margin;
        let mut byoyomi = tc.byoyomi_ms as i64;
        if own < 0 {
            byoyomi = (byoyomi + own).max(0);
            own = 0;
        }
        let opp = (rem[idx(me.opponent())] as i64 - margin).max(0);
        let (btime, wtime) = if me == Side::Sente { (own, opp) } else { (opp, own) };
        self.go_seq += 1;
        let id = self.go_seq.to_string();
        let mut pos = format!("position sfen {sfen}");
        if !moves.is_empty() {
            pos.push_str(" moves ");
            pos.push_str(&moves.join(" "));
        }
        let mut go = format!("go id {id} btime {btime} wtime {wtime}");
        if tc.increment_ms > 0 {
            go.push_str(&format!(" binc {0} winc {0}", tc.increment_ms));
        }
        if tc.byoyomi_ms > 0 {
            go.push_str(&format!(" byoyomi {byoyomi}"));
        }
        self.engine.send(&pos).await?;
        self.engine.send(&go).await?;
        debug!(ply = moves.len() + 1, go = %go, "go");
        let limit = Duration::from_millis(tc.allowance_ms(rem[idx(me)])).saturating_sub(STOP_LEAD);
        Ok(Pending { id, started: Instant::now(), deadline: turn_start + limit, stopped: false })
    }

    /// Stops a running search and drops its `bestmove`.
    async fn abandon(&mut self, pending: &mut Option<Pending>) -> Step<()> {
        let Some(p) = pending.take() else { return Ok(()) };
        self.engine.send("stop").await?;
        let deadline = Instant::now() + STOP_ANSWER;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.engine.recv_within(left).await? {
                Some(l) => {
                    if let Some((_, echo)) = parse_bestmove(&l) {
                        if echo.as_ref().is_none_or(|e| *e == p.id) {
                            return Ok(());
                        }
                    }
                }
                None => return Err(Stop::Fatal("the engine did not answer stop".into())),
            }
        }
    }
}
