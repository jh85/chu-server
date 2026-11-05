//! Who is online, who waits for which game, and the running games.
//!
//! Phase 1 pairs challenge leagues only: two players naming the same game
//! (and the same start SFEN) with compatible colours start at once. Floodgate
//! leagues are scheduled in phase 3 and are refused for now.

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use chu_proto::{ColorReq, GameName, Side, TimeUnit};
use chu_rules::{Backend, Rules, START_SFEN};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::clock::TimeControl;
use crate::game::{self, GameInput, GameSetup, GameTx, PlayerSlot};
use crate::store::StoreTx;
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
}

pub fn spawn(backend: Backend, store: StoreTx) -> LobbyTx {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut lobby = Lobby {
        backend,
        store,
        tx: tx.clone(),
        players: HashMap::new(),
        waiting: Vec::new(),
        games: HashMap::new(),
        recent_ids: Default::default(),
    };
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            lobby.handle(msg);
        }
    });
    tx
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
        }
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
        if parsed.is_floodgate() {
            return error("floodgate leagues are not scheduled yet");
        }
        if sfen.is_some() && !parsed.is_test() {
            return error("a start SFEN is only allowed in chu-test leagues");
        }
        let sfen = sfen.unwrap_or_else(|| START_SFEN.to_owned());

        self.waiting.retain(|w| w.name != name);
        let found = self.waiting.iter().position(|w| {
            w.game_name == game_name && w.sfen == sfen && compatible(w.color, color) && w.name != name
        });
        let Some(i) = found else {
            self.waiting.push(Waiter { name, game_name, color, sfen });
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
        if let Err(e) = self.start_game(parsed, game_name, names, sfen) {
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

    fn start_game(&mut self, parsed: GameName, game_name: String, names: [String; 2], sfen: String) -> Result<(), String> {
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
        let rated = !parsed.is_test() && sfen == START_SFEN;
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
        self.games.insert(id, RunningGame { tx, game_name, names });
        Ok(())
    }
}
