//! Who is online, who waits for which game, and the running games.
//!
//! Challenge leagues pair at once: two players naming the same game (and the
//! same start SFEN) with compatible colours. Floodgate leagues (`leagues.toml`)
//! pair on a schedule: one task per league sends [`LobbyMsg::Round`] at each
//! round time, and the lobby pairs the players queued before the cutoff who
//! are not in a game (`pairing.rs`).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chu_proto::{ColorReq, GameName, Side, TimeUnit};
use chu_rules::{Backend, Rules, START_SFEN};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn};

use crate::clock::TimeControl;
use crate::game::{self, GameInput, GameSetup, GameTx, PlayerSlot};
use crate::league::LeagueConfig;
use crate::pairing::{self, Candidate, History, Rng};
use crate::store::{RoundRecord, Standings, StoreMsg, StoreTx};
use crate::util::Utc;
use crate::{Conn, Ctl, GameLink};

pub enum LobbyMsg {
    /// A successful LOGIN; replies once any older connection was kicked.
    Register { name: String, conn: Conn, ctl: mpsc::UnboundedSender<Ctl>, reply: oneshot::Sender<()> },
    Unregister { name: String, conn_id: u64 },
    Join { name: String, conn_id: u64, game_name: String, color: ColorReq, sfen: Option<String> },
    Who { conn: Conn },
    List { conn: Conn },
    Show { conn: Conn, id: String },
    Monitor { conn: Conn, id: String, on: bool },
    Chat { name: String, text: String },
    GameEnded { id: String },
    /// A floodgate league's round time has come.
    Round { league: String, scheduled_at: SystemTime },
}

pub type LobbyTx = mpsc::UnboundedSender<LobbyMsg>;

struct Player {
    conn: Conn,
    ctl: mpsc::UnboundedSender<Ctl>,
    game: Option<String>,
}

struct Waiter {
    name: String,
    game_name: String,
    color: ColorReq,
    sfen: String,
    since: SystemTime,
}

struct RunningGame {
    tx: GameTx,
    game_name: String,
    names: [String; 2],
}

struct Lobby {
    backend: Backend,
    store: StoreTx,
    tx: LobbyTx,
    players: HashMap<String, Player>,
    waiting: Vec<Waiter>,
    games: HashMap<String, RunningGame>,
    /// Ids issued during the current second, so a pair that plays two games
    /// within one second gets distinct ids.
    recent_ids: (String, HashSet<String>),
    leagues: HashMap<String, LeagueConfig>,
    standings: watch::Receiver<Standings>,
    history: HashMap<String, History>,
    rng: Rng,
}

pub fn spawn(backend: Backend, store: StoreTx, standings: watch::Receiver<Standings>, leagues: Vec<LeagueConfig>) -> LobbyTx {
    let (tx, mut rx) = mpsc::unbounded_channel();
    for cfg in &leagues {
        tokio::spawn(schedule(cfg.clone(), tx.clone()));
    }
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
    let mut lobby = Lobby {
        backend,
        store,
        tx: tx.clone(),
        players: HashMap::new(),
        waiting: Vec::new(),
        games: HashMap::new(),
        recent_ids: Default::default(),
        leagues: leagues.into_iter().map(|l| (l.name.clone(), l)).collect(),
        standings,
        history: HashMap::new(),
        rng: Rng::new(seed),
    };
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            lobby.handle(msg);
        }
    });
    tx
}

/// One league's clock: sends a `Round` at every round time. A round missed
/// by more than its interval (a suspended host) is skipped with a warning,
/// not fired late; the `rounds` table shows the gap.
async fn schedule(cfg: LeagueConfig, lobby: LobbyTx) {
    let mut next = cfg.next_round(SystemTime::now());
    info!("{}: first round at {}", cfg.name, Utc::from(next).iso());
    loop {
        let wait = next.duration_since(SystemTime::now()).unwrap_or_default();
        tokio::time::sleep(wait).await;
        let now = SystemTime::now();
        if now.duration_since(next).unwrap_or_default() < cfg.every() {
            if lobby.send(LobbyMsg::Round { league: cfg.name.clone(), scheduled_at: next }).is_err() {
                return;
            }
        } else {
            warn!("{}: round at {} missed (woke at {})", cfg.name, Utc::from(next).iso(), Utc::from(now).iso());
        }
        // The wall clock can lag the timer a little; never repeat a round.
        next = cfg.next_round(now.max(next));
    }
}

fn compatible(a: ColorReq, b: ColorReq) -> bool {
    !matches!((a, b), (ColorReq::Sente, ColorReq::Sente) | (ColorReq::Gote, ColorReq::Gote))
}

impl Lobby {
    fn handle(&mut self, msg: LobbyMsg) {
        match msg {
            LobbyMsg::Register { name, conn, ctl, reply } => {
                if let Some(old) = self.players.remove(&name) {
                    info!("{name}: second login, closing connection {}", old.conn.id);
                    let _ = old.ctl.send(Ctl::Kick);
                }
                self.waiting.retain(|w| w.name != name);
                self.players.insert(name, Player { conn, ctl, game: None });
                let _ = reply.send(());
            }
            LobbyMsg::Unregister { name, conn_id } => {
                if self.players.get(&name).is_some_and(|p| p.conn.id == conn_id) {
                    self.players.remove(&name);
                    self.waiting.retain(|w| w.name != name);
                }
            }
            LobbyMsg::Join { name, conn_id, game_name, color, sfen } => {
                self.join(name, conn_id, game_name, color, sfen)
            }
            LobbyMsg::Who { conn } => {
                let mut names: Vec<_> = self.players.iter().collect();
                names.sort_by(|a, b| a.0.cmp(b.0));
                for (name, p) in names {
                    let state = match (&p.game, self.waiting.iter().find(|w| &w.name == name)) {
                        (Some(id), _) => format!("playing {id}"),
                        (None, Some(w)) => format!("waiting {} {}", w.game_name, w.color.sign()),
                        (None, None) => "connected".into(),
                    };
                    conn.send(format!("##[WHO] {name} {state}"));
                }
                conn.send("##[WHO] +OK");
            }
            LobbyMsg::List { conn } => {
                let mut ids: Vec<_> = self.games.keys().collect();
                ids.sort();
                for id in ids {
                    conn.send(format!("##[LIST] {id}"));
                }
                conn.send("##[LIST] +OK");
            }
            LobbyMsg::Show { conn, id } => match self.games.get(&id) {
                Some(g) => {
                    let _ = g.tx.send(GameInput::Show { conn });
                }
                None => {
                    conn.send(format!("##[ERROR] no such game {id}"));
                }
            },
            LobbyMsg::Monitor { conn, id, on } => match self.games.get(&id) {
                Some(g) => {
                    let _ = g.tx.send(GameInput::Monitor { conn, on });
                }
                None => {
                    conn.send(format!("##[ERROR] no such game {id}"));
                }
            },
            LobbyMsg::Chat { name, text } => {
                let line = format!("##[CHAT][{name}] {text}");
                for p in self.players.values() {
                    p.conn.send(line.clone());
                }
            }
            LobbyMsg::GameEnded { id } => {
                if let Some(g) = self.games.remove(&id) {
                    for n in g.names {
                        if let Some(p) = self.players.get_mut(&n) {
                            if p.game.as_deref() == Some(id.as_str()) {
                                p.game = None;
                            }
                        }
                    }
                }
            }
            LobbyMsg::Round { league, scheduled_at } => self.round(league, scheduled_at),
        }
    }

    fn round(&mut self, league: String, scheduled_at: SystemTime) {
        let Some(cfg) = self.leagues.get(&league).cloned() else { return };
        let Ok(parsed) = league.parse::<GameName>() else { return };
        let fired_at = SystemTime::now();
        let cutoff = scheduled_at - Duration::from_secs(cfg.cutoff_s);
        let ratings = self.standings.borrow().get(&league).cloned().unwrap_or_default();
        let pool: Vec<Candidate> = self
            .waiting
            .iter()
            .filter(|w| w.game_name == league && w.since <= cutoff)
            .filter(|w| self.players.get(&w.name).is_some_and(|p| p.game.is_none()))
            .map(|w| Candidate {
                name: w.name.clone(),
                rating: ratings.get(&w.name).filter(|r| r.published()).map(|r| r.rating),
            })
            .collect();
        let hist = self.history.entry(league.clone()).or_default();
        let round = pairing::pair(&pool, hist, &mut self.rng);
        let mut pairs = Vec::new();
        for (sente, gote) in round.pairs {
            self.waiting.retain(|w| w.name != sente && w.name != gote);
            let names = [sente.clone(), gote.clone()];
            match self.start_game(parsed.clone(), league.clone(), names, START_SFEN.to_owned(), cfg.rated) {
                Ok(id) => pairs.push((sente, gote, id)),
                Err(e) => warn!("{league}: cannot start {sente} vs {gote}: {e}"),
            }
        }
        info!(
            "{league}: round {} with {} queued, {} games, sat out {:?}",
            Utc::from(scheduled_at).iso(),
            pool.len(),
            pairs.len(),
            round.sat_out
        );
        let pool = pool.into_iter().map(|c| c.name).collect();
        let rec = RoundRecord { league, scheduled_at, fired_at, pool, pairs, sat_out: round.sat_out };
        let _ = self.store.send(StoreMsg::Round(rec));
    }

    fn join(&mut self, name: String, conn_id: u64, game_name: String, color: ColorReq, sfen: Option<String>) {
        let Some(player) = self.players.get(&name).filter(|p| p.conn.id == conn_id) else { return };
        let conn = player.conn.clone();
        let error = |msg: &str| {
            conn.send(format!("##[ERROR] {msg}"));
        };

        // A running game of this name: resume it (SERVER-PROTOCOL.md §6).
        if let Some((id, g)) = self.games.iter().find(|(_, g)| g.game_name == game_name && g.names.contains(&name)) {
            let side = if g.names[0] == name { Side::Sente } else { Side::Gote };
            let p = self.players.get_mut(&name).unwrap();
            p.game = Some(id.clone());
            let _ = p.ctl.send(Ctl::Joined(GameLink { id: id.clone(), side, tx: g.tx.clone() }));
            let _ = g.tx.send(GameInput::Reattach { side, conn: p.conn.clone(), ctl: p.ctl.clone() });
            return;
        }
        if player.game.is_some() {
            return error("already playing");
        }
        let Ok(parsed) = game_name.parse::<GameName>() else { return error(&format!("bad game name {game_name}")) };
        if sfen.is_some() && !parsed.is_test() {
            return error("a start SFEN is only allowed in chu-test leagues");
        }
        let sfen = sfen.unwrap_or_else(|| START_SFEN.to_owned());
        let since = SystemTime::now();
        if parsed.is_floodgate() {
            if !self.leagues.contains_key(&game_name) {
                return error(&format!("no floodgate league {game_name}"));
            }
            if color != ColorReq::Any {
                return error("floodgate leagues take * only");
            }
            self.waiting.retain(|w| w.name != name);
            self.waiting.push(Waiter { name, game_name, color, sfen, since });
            return;
        }

        self.waiting.retain(|w| w.name != name);
        let found = self.waiting.iter().position(|w| {
            w.game_name == game_name && w.sfen == sfen && compatible(w.color, color) && w.name != name
        });
        let Some(i) = found else {
            self.waiting.push(Waiter { name, game_name, color, sfen, since });
            return;
        };
        let other = self.waiting.remove(i);
        // The waiter's explicit colour wins, then the newcomer's; `*` vs `*`
        // gives sente to whoever waited.
        let other_is_sente = matches!(
            (other.color, color),
            (ColorReq::Sente, _) | (ColorReq::Any, ColorReq::Gote) | (ColorReq::Any, ColorReq::Any)
        );
        let names = if other_is_sente { [other.name, name] } else { [name, other.name] };
        let rated = !parsed.is_test() && sfen == START_SFEN;
        if let Err(e) = self.start_game(parsed, game_name, names, sfen, rated) {
            warn!("cannot start game: {e}");
        }
    }

    fn new_id(&mut self, game_name: &str, names: &[String; 2]) -> String {
        let stamp = Utc::from(SystemTime::now()).compact();
        if self.recent_ids.0 != stamp {
            self.recent_ids = (stamp.clone(), HashSet::new());
        }
        let base = format!("{game_name}+{}+{}+{stamp}", names[0], names[1]);
        let taken = |id: &String| self.recent_ids.1.contains(id) || self.games.contains_key(id);
        let id = std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}-{n}")))
            .find(|id| !taken(id))
            .unwrap();
        self.recent_ids.1.insert(id.clone());
        id
    }

    fn start_game(
        &mut self,
        parsed: GameName,
        game_name: String,
        names: [String; 2],
        sfen: String,
        rated: bool,
    ) -> Result<String, String> {
        let referee = (|| {
            let mut r = self.backend.create()?;
            r.set_rules(if parsed.jcsa { Rules::Jcsa } else { Rules::Lishogi })?;
            r.set_sfen(&sfen)?;
            Ok::<_, chu_rules::Error>(r)
        })();
        let referee = match referee {
            Ok(r) => r,
            Err(e) => {
                for n in &names {
                    if let Some(p) = self.players.get(n) {
                        p.conn.send(format!("##[ERROR] cannot start game: {e}"));
                    }
                }
                return Err(e.to_string());
            }
        };
        // Use the referee's spelling of the SFEN from here on.
        let mut referee = referee;
        let initial_sfen = referee.sfen().map_err(|e| e.to_string())?;
        let id = self.new_id(&game_name, &names);
        let slot = |n: &String| {
            let p = self.players.get(n);
            PlayerSlot { name: n.clone(), conn: p.map(|p| p.conn.clone()), ctl: p.map(|p| p.ctl.clone()) }
        };
        let players = [slot(&names[0]), slot(&names[1])];
        let tc = TimeControl::from_game_name(&parsed, TimeUnit::Msec);
        // The sessions route game lines before the summary goes out.
        let (tx, rx) = mpsc::unbounded_channel();
        for (i, n) in names.iter().enumerate() {
            let p = self.players.get_mut(n).unwrap();
            p.game = Some(id.clone());
            let side = if i == 0 { Side::Sente } else { Side::Gote };
            let _ = p.ctl.send(Ctl::Joined(GameLink { id: id.clone(), side, tx: tx.clone() }));
        }
        game::spawn_with(
            GameSetup {
                id: id.clone(),
                name: parsed,
                initial_sfen,
                tc,
                rated,
                players,
                referee,
                store: self.store.clone(),
                lobby: self.tx.clone(),
            },
            rx,
        );
        info!("paired {} vs {} in {id}", names[0], names[1]);
        self.games.insert(id.clone(), RunningGame { tx, game_name, names });
        Ok(id)
    }
}
