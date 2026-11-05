//! The single writer for SQLite and the record files.
//!
//! Game actors and sessions send messages; the store task applies them in
//! order. Records go to `records/<game-name>/<yyyy>/<mm>/<dd>/<game-id>/` in
//! the layout of vendor/chushogi/tools/random_match.py (moves.usi,
//! positions.sfen, positions.tsv, record.json) plus game.csa.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chu_proto::{Reason, Side};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

use crate::util::Utc;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS players (
  name TEXT PRIMARY KEY,
  password_hash TEXT NOT NULL,
  created_at TEXT NOT NULL,
  banned INTEGER NOT NULL DEFAULT 0,
  alias_of TEXT
);
CREATE TABLE IF NOT EXISTS games (
  id TEXT PRIMARY KEY,
  league TEXT NOT NULL,
  rules TEXT NOT NULL,
  sente TEXT NOT NULL,
  gote TEXT NOT NULL,
  initial_sfen TEXT NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  result TEXT,
  reason TEXT,
  ply_count INTEGER NOT NULL DEFAULT 0,
  final_sfen TEXT,
  record_path TEXT
);
CREATE TABLE IF NOT EXISTS moves (
  game_id TEXT NOT NULL REFERENCES games(id),
  ply INTEGER NOT NULL,
  usi TEXT NOT NULL,
  elapsed_ms INTEGER NOT NULL,
  sfen_after TEXT NOT NULL,
  PRIMARY KEY (game_id, ply)
);
";

#[derive(Debug, Clone)]
pub struct StartedGame {
    pub id: String,
    pub league: String,
    pub rules: String,
    pub sente: String,
    pub gote: String,
    pub initial_sfen: String,
    pub started_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct RecordedMove {
    pub side: Side,
    pub usi: String,
    pub elapsed_ms: u64,
    /// `T` value as echoed, in the game's time unit.
    pub time: u64,
    pub sfen_after: String,
}

#[derive(Debug, Clone)]
pub struct FinishedGame {
    pub game: StartedGame,
    pub rated: bool,
    pub ended_at: SystemTime,
    pub moves: Vec<RecordedMove>,
    pub reason: Reason,
    /// None for a draw or a void game (`#CHUDAN`).
    pub winner: Option<Side>,
    /// Game_Summary lines as sent to sente, for game.csa.
    pub summary: Vec<String>,
}

pub enum StoreMsg {
    Login { name: String, password: String, reply: oneshot::Sender<bool> },
    Started(StartedGame),
    Move { game_id: String, ply: u32, mv: RecordedMove },
    Finished(Box<FinishedGame>),
    /// Replies once every earlier message has been applied.
    Sync(oneshot::Sender<()>),
}

pub type StoreTx = mpsc::UnboundedSender<StoreMsg>;

/// Asks the store whether `name`/`password` may log in (creating the account
/// on first use).
pub async fn login(store: &StoreTx, name: &str, password: &str) -> bool {
    let (reply, rx) = oneshot::channel();
    let msg = StoreMsg::Login { name: name.into(), password: password.into(), reply };
    store.send(msg).is_ok() && rx.await.unwrap_or(false)
}

pub async fn sync(store: &StoreTx) {
    let (reply, rx) = oneshot::channel();
    if store.send(StoreMsg::Sync(reply)).is_ok() {
        let _ = rx.await;
    }
}

pub async fn open(db_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(db_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal);
    // One connection: the store task is the only writer, and `sqlite::memory:`
    // is per connection.
    let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    Ok(pool)
}

pub async fn spawn(db_url: &str, records_dir: PathBuf) -> Result<StoreTx, sqlx::Error> {
    let pool = open(db_url).await?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = apply(&pool, &records_dir, msg).await {
                error!("store: {e}");
            }
        }
    });
    Ok(tx)
}

async fn apply(pool: &SqlitePool, records_dir: &Path, msg: StoreMsg) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match msg {
        StoreMsg::Login { name, password, reply } => {
            let ok = check_login(pool, &name, &password).await;
            let _ = reply.send(ok.as_ref().copied().unwrap_or(false));
            ok?;
        }
        StoreMsg::Started(g) => {
            sqlx::query(
                "INSERT INTO games (id, league, rules, sente, gote, initial_sfen, started_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&g.id)
            .bind(&g.league)
            .bind(&g.rules)
            .bind(&g.sente)
            .bind(&g.gote)
            .bind(&g.initial_sfen)
            .bind(Utc::from(g.started_at).iso())
            .execute(pool)
            .await?;
        }
        StoreMsg::Move { game_id, ply, mv } => {
            sqlx::query("INSERT INTO moves (game_id, ply, usi, elapsed_ms, sfen_after) VALUES (?, ?, ?, ?, ?)")
                .bind(&game_id)
                .bind(ply as i64)
                .bind(&mv.usi)
                .bind(mv.elapsed_ms as i64)
                .bind(&mv.sfen_after)
                .execute(pool)
                .await?;
        }
        StoreMsg::Finished(f) => {
            let dir = record_dir(records_dir, &f.game);
            let f2 = f.clone();
            let dir2 = dir.clone();
            tokio::task::spawn_blocking(move || write_record(&dir2, &f2)).await??;
            let result = match (f.reason, f.winner) {
                (Reason::Chudan, _) => "void",
                (_, Some(Side::Sente)) => "+",
                (_, Some(Side::Gote)) => "-",
                (_, None) => "=",
            };
            sqlx::query(
                "UPDATE games SET ended_at = ?, result = ?, reason = ?, ply_count = ?, final_sfen = ?, record_path = ? WHERE id = ?",
            )
            .bind(Utc::from(f.ended_at).iso())
            .bind(result)
            .bind(f.reason.as_str())
            .bind(f.moves.len() as i64)
            .bind(f.moves.last().map_or(&f.game.initial_sfen, |m| &m.sfen_after))
            .bind(dir.display().to_string())
            .bind(&f.game.id)
            .execute(pool)
            .await?;
            info!("game {} ended {} {result} after {} plies", f.game.id, f.reason.as_str(), f.moves.len());
        }
        StoreMsg::Sync(reply) => {
            let _ = reply.send(());
        }
    }
    Ok(())
}

async fn check_login(pool: &SqlitePool, name: &str, password: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let row: Option<(String, i64)> = sqlx::query_as("SELECT password_hash, banned FROM players WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    let password = password.to_owned();
    match row {
        Some((hash, banned)) => {
            if banned != 0 {
                return Ok(false);
            }
            Ok(tokio::task::spawn_blocking(move || {
                PasswordHash::new(&hash)
                    .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
                    .unwrap_or(false)
            })
            .await?)
        }
        None => {
            let hash = tokio::task::spawn_blocking(move || {
                let salt = SaltString::generate(&mut OsRng);
                Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string())
            })
            .await?
            .map_err(|e| e.to_string())?;
            sqlx::query("INSERT INTO players (name, password_hash, created_at) VALUES (?, ?, ?)")
                .bind(name)
                .bind(hash)
                .bind(Utc::from(SystemTime::now()).iso())
                .execute(pool)
                .await?;
            info!("created account {name}");
            Ok(true)
        }
    }
}

pub fn record_dir(root: &Path, g: &StartedGame) -> PathBuf {
    let t = Utc::from(g.started_at);
    root.join(&g.league)
        .join(format!("{:04}", t.year))
        .join(format!("{:02}", t.month))
        .join(format!("{:02}", t.day))
        .join(&g.id)
}

/// lishogi-style `status` and the `end_reason` random_match.py writes; the
/// server-only endings use lishogi's names where one exists.
fn status_names(reason: Reason) -> (&'static str, &'static str) {
    match reason {
        Reason::RoyalsLost => ("royalsLost", "royalslost"),
        Reason::BareKing => ("bareKing", "bareking"),
        Reason::Stalemate => ("stalemate", "stalemate"),
        Reason::Insufficient => ("draw", "draw"),
        Reason::Sennichite => ("draw", "repetition"),
        Reason::MaxMoves => ("draw", "moveLimit"),
        Reason::OuteSennichite => ("perpetualCheck", "perpetualCheck"),
        Reason::Resign => ("resign", "resign"),
        Reason::TimeUp => ("outoftime", "outoftime"),
        Reason::IllegalMove => ("illegalMove", "illegalMove"),
        Reason::Chudan => ("aborted", "aborted"),
    }
}

#[derive(Serialize)]
struct PlayerJson<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct Players<'a> {
    sente: PlayerJson<'a>,
    gote: PlayerJson<'a>,
}

/// random_match.py's record.json fields, in its order, then the server's.
#[derive(Serialize)]
struct RecordJson<'a> {
    id: &'a str,
    variant: &'a str,
    rated: bool,
    created_at: String,
    status: &'a str,
    winner: Option<&'a str>,
    end_reason: &'a str,
    players: Players<'a>,
    plies: usize,
    initial_sfen: &'a str,
    final_sfen: &'a str,
    moves_usi: Vec<&'a str>,
    league: &'a str,
    rules: &'a str,
    reason: &'a str,
    ended_at: String,
    time_ms: Vec<u64>,
}

pub fn write_record(dir: &Path, f: &FinishedGame) -> std::io::Result<()> {
    use std::fmt::Write as _;
    std::fs::create_dir_all(dir)?;
    let g = &f.game;
    let moves: Vec<&str> = f.moves.iter().map(|m| m.usi.as_str()).collect();
    let mut sfens = vec![g.initial_sfen.as_str()];
    sfens.extend(f.moves.iter().map(|m| m.sfen_after.as_str()));

    std::fs::write(dir.join("moves.usi"), moves.join(" ") + "\n")?;
    std::fs::write(dir.join("positions.sfen"), sfens.join("\n") + "\n")?;
    let mut tsv = String::from("ply\tusi\tsfen\n");
    for (ply, sfen) in sfens.iter().enumerate() {
        let usi = if ply == 0 { "" } else { moves[ply - 1] };
        let _ = writeln!(tsv, "{ply}\t{usi}\t{sfen}");
    }
    std::fs::write(dir.join("positions.tsv"), tsv)?;

    let (status, end_reason) = status_names(f.reason);
    let winner = match (f.reason, f.winner) {
        (Reason::Chudan, _) | (_, None) => None,
        (_, Some(Side::Sente)) => Some("sente"),
        (_, Some(Side::Gote)) => Some("gote"),
    };
    let rec = RecordJson {
        id: &g.id,
        variant: "chushogi",
        rated: f.rated,
        created_at: Utc::from(g.started_at).iso(),
        status,
        winner,
        end_reason,
        players: Players { sente: PlayerJson { name: &g.sente }, gote: PlayerJson { name: &g.gote } },
        plies: moves.len(),
        initial_sfen: &g.initial_sfen,
        final_sfen: sfens.last().unwrap(),
        moves_usi: moves.clone(),
        league: &g.league,
        rules: &g.rules,
        reason: f.reason.as_str(),
        ended_at: Utc::from(f.ended_at).iso(),
        time_ms: f.moves.iter().map(|m| m.elapsed_ms).collect(),
    };
    let json = serde_json::to_string_pretty(&rec).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("record.json"), json + "\n")?;

    // CSA-like text: the summary, one echo line per move, the result.
    let mut csa = String::new();
    for line in f.summary.iter().filter(|l| !l.starts_with("Your_Turn:")) {
        let _ = writeln!(csa, "{line}");
    }
    for m in &f.moves {
        let _ = writeln!(csa, "{}{},T{}", m.side.sign(), m.usi, m.time);
    }
    let _ = writeln!(csa, "{}", f.reason.as_str());
    let result = match (f.reason, winner) {
        (Reason::Chudan, _) => "'void".to_owned(),
        (_, Some(w)) => format!("'winner {} {}", if w == "sente" { '+' } else { '-' }, w),
        (_, None) => "'draw".to_owned(),
    };
    let _ = writeln!(csa, "{result}");
    std::fs::write(dir.join("game.csa"), csa)
}
