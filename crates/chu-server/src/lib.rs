//! The chu shogi tournament server: sessions, lobby, game actors, clocks,
//! storage. `main.rs` only parses flags and calls [`Server::start`].

pub mod game;
pub mod league;
pub mod lobby;
pub mod pairing;
pub mod rating;
pub mod session;
pub mod store;
pub mod util;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

pub use chu_proto::{clock, Side};

/// What a connection's writer task receives.
#[derive(Debug)]
pub enum OutMsg {
    Line(String),
    Close,
}

/// Handle for writing lines to one connection.
#[derive(Clone, Debug)]
pub struct Conn {
    pub id: u64,
    tx: mpsc::UnboundedSender<OutMsg>,
}

impl Conn {
    pub fn new(id: u64) -> (Conn, mpsc::UnboundedReceiver<OutMsg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Conn { id, tx }, rx)
    }
    /// Queues one line; false if the connection is gone.
    pub fn send(&self, line: impl Into<String>) -> bool {
        self.tx.send(OutMsg::Line(line.into())).is_ok()
    }
    pub fn close(&self) {
        let _ = self.tx.send(OutMsg::Close);
    }
}

/// Control messages to a session task.
#[derive(Debug)]
pub enum Ctl {
    /// Route game lines (AGREE, REJECT, moves, %TORYO) to this game.
    Joined(GameLink),
    /// The game is over.
    Left,
    /// Another login took this name.
    Kick,
}

#[derive(Clone, Debug)]
pub struct GameLink {
    pub id: String,
    pub side: Side,
    pub tx: mpsc::UnboundedSender<game::GameInput>,
}

pub fn chu_side(s: Side) -> chu_rules::Color {
    match s {
        Side::Sente => chu_rules::Color::Sente,
        Side::Gote => chu_rules::Color::Gote,
    }
}

pub fn proto_side(c: chu_rules::Color) -> Side {
    match c {
        chu_rules::Color::Sente => Side::Sente,
        chu_rules::Color::Gote => Side::Gote,
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub referee: chu_rules::Backend,
    /// sqlx URL, e.g. `sqlite://chu.db` or `sqlite::memory:`.
    pub db_url: String,
    pub records_dir: PathBuf,
    /// Floodgate leagues to schedule (`leagues.toml`).
    pub leagues: Vec<league::LeagueConfig>,
}

/// A running server: store and lobby tasks, ready to take connections.
#[derive(Clone)]
pub struct Server {
    pub lobby: lobby::LobbyTx,
    pub store: store::StoreTx,
    next_conn: std::sync::Arc<AtomicU64>,
}

impl Server {
    pub async fn start(cfg: Config) -> Result<Server, sqlx::Error> {
        let opened = store::spawn(&cfg.db_url, cfg.records_dir.clone()).await?;
        let store = opened.tx;
        let lobby = lobby::spawn(cfg.referee.clone(), store.clone(), opened.standings, cfg.leagues, &opened.last_rounds);
        Ok(Server { lobby, store, next_conn: Default::default() })
    }

    /// Serves one client connection until it closes.
    pub fn serve<S>(&self, stream: S) -> tokio::task::JoinHandle<()>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let id = self.next_conn.fetch_add(1, Ordering::Relaxed) + 1;
        tokio::spawn(session::run(stream, id, self.lobby.clone(), self.store.clone()))
    }
}
