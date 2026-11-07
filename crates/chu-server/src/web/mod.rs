//! The web front end (DESIGN.md "Persistence, records and web front end"):
//! server-rendered pages over the database and the lobby, an SSE stream per
//! live game, the record tree and a JSON API.
//!
//! - `/` leagues, next rounds, players online, games in progress
//! - `/league/{name}` standings (refreshed by HTMX), latest results, rounds,
//!   rating curves (`/league/{name}/rating.svg` on its own)
//! - `/game/{id}` board and moves; live games stream `/game/{id}/events`
//! - `/player/{name}` ratings, head-to-head, history with record downloads
//! - `/records/…` the record tree; `/api/games?league=&since=&limit=` JSON
//!
//! Reads go through a read-only pool of their own; the store task stays the
//! only writer.

mod chart;
mod pages;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::{Component, Path as FsPath, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chu_proto::GameName;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tokio::sync::oneshot;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::game::LiveEvent;
use crate::league::LeagueConfig;
use crate::lobby::{LobbyMsg, LobbySnapshot, LobbyTx};
use crate::rating::PUBLISH_AFTER;
use crate::util::Utc;

pub use chart::{build as build_chart, svg as chart_svg, Chart, HistoryRow};

pub struct Ctx {
    db: SqlitePool,
    lobby: LobbyTx,
    records_dir: PathBuf,
    leagues: Vec<LeagueConfig>,
}

pub async fn open_ctx(
    db_url: &str,
    lobby: LobbyTx,
    records_dir: PathBuf,
    leagues: Vec<LeagueConfig>,
) -> Result<Arc<Ctx>, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(db_url)?.read_only(true);
    let db = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?;
    Ok(Arc::new(Ctx { db, lobby, records_dir, leagues }))
}

type Shared = State<Arc<Ctx>>;

pub fn router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/live", get(live_fragment))
        .route("/league/{name}", get(league))
        .route("/league/{name}/standings", get(standings_fragment))
        .route("/league/{name}/rating.svg", get(rating_svg))
        .route("/game/{id}", get(game))
        .route("/game/{id}/events", get(game_events))
        .route("/player/{name}", get(player))
        .route("/records", get(records_root))
        .route("/records/", get(records_root))
        .route("/records/{*path}", get(records))
        .route("/api/games", get(api_games))
        .route("/static/{file}", get(static_file))
        .fallback(|| async { not_found("No such page.") })
        .with_state(ctx)
}

/// An error page or a database failure.
pub struct WebError(StatusCode, String);

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        (self.0, pages::error_page(self.0, &self.1)).into_response()
    }
}

impl From<sqlx::Error> for WebError {
    fn from(e: sqlx::Error) -> WebError {
        tracing::warn!("web: {e}");
        WebError(StatusCode::INTERNAL_SERVER_ERROR, "Database error.".into())
    }
}

fn not_found(msg: &str) -> WebError {
    WebError(StatusCode::NOT_FOUND, msg.into())
}

type WebResult<T> = Result<T, WebError>;

// ---------------------------------------------------------------- queries

/// A finished or running game as the pages list it.
#[derive(Debug, Clone)]
pub struct GameRow {
    pub id: String,
    pub league: String,
    pub sente: String,
    pub gote: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub result: Option<String>,
    pub reason: Option<String>,
    pub ply_count: i64,
    pub rated: bool,
}

type GameTuple = (String, String, String, String, String, Option<String>, Option<String>, Option<String>, i64, i64);

impl From<GameTuple> for GameRow {
    fn from(t: GameTuple) -> GameRow {
        let (id, league, sente, gote, started_at, ended_at, result, reason, ply_count, rated) = t;
        GameRow { id, league, sente, gote, started_at, ended_at, result, reason, ply_count, rated: rated != 0 }
    }
}

impl GameRow {
    /// The record directory under `/records/`.
    pub fn record_dir(&self) -> String {
        record_rel(&self.league, &self.started_at, &self.id)
    }
}

/// `records/<league>/<yyyy>/<mm>/<dd>/<id>` as `store::record_dir` lays it out.
fn record_rel(league: &str, started_at: &str, id: &str) -> String {
    let d = |r: std::ops::Range<usize>| started_at.get(r).unwrap_or("0");
    format!("{league}/{}/{}/{}/{id}", d(0..4), d(5..7), d(8..10))
}

const GAME_COLS: &str = "id, league, sente, gote, started_at, ended_at, result, reason, ply_count, rated";

fn rows(v: Vec<GameTuple>) -> Vec<GameRow> {
    v.into_iter().map(GameRow::from).collect()
}

async fn recent_games(db: &SqlitePool, league: Option<&str>, limit: i64) -> Result<Vec<GameRow>, sqlx::Error> {
    let q = format!(
        "SELECT {GAME_COLS} FROM games WHERE ended_at IS NOT NULL AND (?1 IS NULL OR league = ?1)
         ORDER BY ended_at DESC LIMIT ?2"
    );
    Ok(rows(sqlx::query_as(&q).bind(league).bind(limit).fetch_all(db).await?))
}

/// One line of a league's standings.
#[derive(Debug, Clone, Default)]
pub struct Standing {
    pub player: String,
    pub rating: Option<f64>,
    pub rated_games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub last_played: Option<String>,
}

impl Standing {
    pub fn published(&self) -> bool {
        self.rated_games >= PUBLISH_AFTER
    }
}

/// Ratings first (published, then provisional, each by rating), then
/// players without a rating by name.
async fn standings(db: &SqlitePool, league: &str) -> Result<Vec<Standing>, sqlx::Error> {
    let mut by: BTreeMap<String, Standing> = BTreeMap::new();
    let rated: Vec<(String, f64, i64)> =
        sqlx::query_as("SELECT player, rating, games FROM ratings WHERE league = ?").bind(league).fetch_all(db).await?;
    for (player, rating, games) in rated {
        let s = by.entry(player.clone()).or_insert_with(|| Standing { player, ..Default::default() });
        s.rating = Some(rating);
        s.rated_games = games as u32;
    }
    let games: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT sente, gote, result, ended_at FROM games WHERE league = ? AND result IN ('+', '-', '=')",
    )
    .bind(league)
    .fetch_all(db)
    .await?;
    for (sente, gote, result, ended) in games {
        for (me, sign) in [(sente, '+'), (gote, '-')] {
            let s = by.entry(me.clone()).or_insert_with(|| Standing { player: me, ..Default::default() });
            match result.chars().next() {
                Some('=') => s.draws += 1,
                Some(c) if c == sign => s.wins += 1,
                _ => s.losses += 1,
            }
            if s.last_played.as_deref() < Some(ended.as_str()) {
                s.last_played = Some(ended.clone());
            }
        }
    }
    let mut v: Vec<Standing> = by.into_values().collect();
    v.sort_by(|a, b| {
        let key = |s: &Standing| (s.rating.is_some(), s.published(), s.rating.unwrap_or(0.0));
        let (ka, kb) = (key(a), key(b));
        (kb.0, kb.1).cmp(&(ka.0, ka.1)).then(kb.2.total_cmp(&ka.2)).then(a.player.cmp(&b.player))
    });
    Ok(v)
}

async fn league_chart(db: &SqlitePool, league: &str) -> Result<Chart, sqlx::Error> {
    let rounds: Vec<(String,)> = sqlx::query_as("SELECT scheduled_at FROM rounds WHERE league = ? ORDER BY scheduled_at")
        .bind(league)
        .fetch_all(db)
        .await?;
    let rows: Vec<(String, f64, i64, String)> = sqlx::query_as(
        "SELECT player, rating, games, computed_at FROM rating_history WHERE league = ? ORDER BY computed_at, rowid",
    )
    .bind(league)
    .fetch_all(db)
    .await?;
    let rounds: Vec<String> = rounds.into_iter().map(|r| r.0).collect();
    let history: Vec<HistoryRow> = rows
        .into_iter()
        .map(|(player, rating, games, computed_at)| HistoryRow { player, rating, games: games as u32, computed_at })
        .collect();
    Ok(chart::build(&rounds, &history))
}

/// A league as the index lists it.
pub struct LeagueInfo {
    pub name: String,
    pub config: Option<LeagueConfig>,
    pub games: i64,
    pub last_game: Option<String>,
}

impl LeagueInfo {
    pub fn next_round(&self) -> Option<String> {
        self.config.as_ref().map(|c| Utc::from(c.next_round(SystemTime::now())).iso())
    }
}

async fn leagues(ctx: &Ctx) -> Result<Vec<LeagueInfo>, sqlx::Error> {
    let rows: Vec<(String, i64, Option<String>)> =
        sqlx::query_as("SELECT league, COUNT(*), MAX(ended_at) FROM games WHERE ended_at IS NOT NULL GROUP BY league")
            .fetch_all(&ctx.db)
            .await?;
    let mut out: Vec<LeagueInfo> = ctx
        .leagues
        .iter()
        .map(|c| LeagueInfo { name: c.name.clone(), config: Some(c.clone()), games: 0, last_game: None })
        .collect();
    for (name, games, last) in rows {
        match out.iter_mut().find(|l| l.name == name) {
            Some(l) => {
                l.games = games;
                l.last_game = last;
            }
            None => out.push(LeagueInfo { name, config: None, games, last_game: last }),
        }
    }
    Ok(out)
}

async fn snapshot(lobby: &LobbyTx) -> LobbySnapshot {
    let (reply, rx) = oneshot::channel();
    if lobby.send(LobbyMsg::Snapshot { reply }).is_err() {
        return LobbySnapshot::default();
    }
    rx.await.unwrap_or_default()
}

// ---------------------------------------------------------------- pages

async fn index(State(ctx): Shared) -> WebResult<Response> {
    let leagues = leagues(&ctx).await?;
    let snap = snapshot(&ctx.lobby).await;
    let recent = recent_games(&ctx.db, None, 15).await?;
    Ok(pages::index(&leagues, &snap, &recent).into_response())
}

async fn live_fragment(State(ctx): Shared) -> Response {
    pages::live(&snapshot(&ctx.lobby).await).into_response()
}

fn check_league(ctx: &Ctx, name: &str, known: bool) -> WebResult<Option<LeagueConfig>> {
    let cfg = ctx.leagues.iter().find(|l| l.name == name).cloned();
    if cfg.is_none() && !known {
        return Err(not_found(&format!("No league {name}.")));
    }
    Ok(cfg)
}

async fn league_known(db: &SqlitePool, name: &str) -> Result<bool, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM games WHERE league = ?").bind(name).fetch_one(db).await?;
    Ok(n > 0)
}

async fn league(State(ctx): Shared, Path(name): Path<String>) -> WebResult<Response> {
    let cfg = check_league(&ctx, &name, league_known(&ctx.db, &name).await?)?;
    let table = standings(&ctx.db, &name).await?;
    let chart = league_chart(&ctx.db, &name).await?;
    let recent = recent_games(&ctx.db, Some(&name), 30).await?;
    let rounds: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT scheduled_at, pool_json, paired_json, sat_out FROM rounds WHERE league = ? ORDER BY scheduled_at DESC LIMIT 8",
    )
    .bind(&name)
    .fetch_all(&ctx.db)
    .await?;
    let (n_rounds,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM rounds WHERE league = ?").bind(&name).fetch_one(&ctx.db).await?;
    let rounds: Vec<pages::RoundLine> = rounds
        .into_iter()
        .map(|(at, pool, paired, sat_out)| pages::RoundLine {
            at,
            pool: serde_json::from_str(&pool).unwrap_or_default(),
            pairs: serde_json::from_str(&paired).unwrap_or_default(),
            sat_out,
        })
        .collect();
    let snap = snapshot(&ctx.lobby).await;
    let live: Vec<_> = snap.games.into_iter().filter(|g| g.1 == name).collect();
    Ok(pages::league(&name, cfg.as_ref(), &table, &chart, &recent, &rounds, n_rounds, &live).into_response())
}

async fn standings_fragment(State(ctx): Shared, Path(name): Path<String>) -> WebResult<Response> {
    check_league(&ctx, &name, league_known(&ctx.db, &name).await?)?;
    Ok(pages::standings(&name, &standings(&ctx.db, &name).await?).into_response())
}

async fn rating_svg(State(ctx): Shared, Path(name): Path<String>) -> WebResult<Response> {
    check_league(&ctx, &name, league_known(&ctx.db, &name).await?)?;
    let chart = league_chart(&ctx.db, &name).await?;
    let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n{}", chart::svg(&chart).into_string());
    Ok(([(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")], body).into_response())
}

/// A finished game's move with the position after it.
#[derive(Debug, Clone, Serialize)]
pub struct MoveRow {
    pub side: &'static str,
    pub usi: String,
    pub elapsed_ms: i64,
    pub sfen: String,
}

async fn game(State(ctx): Shared, Path(id): Path<String>) -> WebResult<Response> {
    if let Some((state, _)) = watch(&ctx.lobby, &id).await {
        return Ok(pages::live_game(&state).into_response());
    }
    let q = format!("SELECT {GAME_COLS} FROM games WHERE id = ?");
    let row: Option<GameTuple> = sqlx::query_as(&q).bind(&id).fetch_optional(&ctx.db).await?;
    let row = row.map(GameRow::from);
    let Some(row) = row else { return Err(not_found(&format!("No game {id}."))) };
    let (initial_sfen,): (String,) =
        sqlx::query_as("SELECT initial_sfen FROM games WHERE id = ?").bind(&id).fetch_one(&ctx.db).await?;
    let moves: Vec<(i64, String, i64, String)> =
        sqlx::query_as("SELECT ply, usi, elapsed_ms, sfen_after FROM moves WHERE game_id = ? ORDER BY ply")
            .bind(&id)
            .fetch_all(&ctx.db)
            .await?;
    let first_gote = initial_sfen.split(' ').nth(1) == Some("w");
    let moves: Vec<MoveRow> = moves
        .into_iter()
        .map(|(ply, usi, elapsed_ms, sfen)| {
            let sente = (ply % 2 == 1) != first_gote;
            MoveRow { side: if sente { "b" } else { "w" }, usi, elapsed_ms, sfen }
        })
        .collect();
    Ok(pages::finished_game(&row, &initial_sfen, &moves).into_response())
}

async fn watch(lobby: &LobbyTx, id: &str) -> Option<(crate::game::LiveState, tokio::sync::broadcast::Receiver<LiveEvent>)> {
    let (reply, rx) = oneshot::channel();
    lobby.send(LobbyMsg::Watch { id: id.to_owned(), reply }).ok()?;
    rx.await.ok().flatten()
}

/// SSE: `state` (the whole game) first, then `start`, `move` and `end`;
/// `resync` if this watcher fell behind, `gone` if the game is not live.
/// A browser's EventSource reconnects by itself and gets `state` again.
async fn game_events(State(ctx): Shared, Path(id): Path<String>) -> Response {
    let Some((state, rx)) = watch(&ctx.lobby, &id).await else {
        let once = tokio_stream::once(Ok::<_, Infallible>(Event::default().event("gone").data(id)));
        return Sse::new(once).into_response();
    };
    let first = tokio_stream::once(Ok::<_, Infallible>(json_event("state", &state)));
    let rest = BroadcastStream::new(rx).map(|r| {
        Ok(match r {
            Ok(ev) => {
                let name = match ev {
                    LiveEvent::Start => "start",
                    LiveEvent::Move { .. } => "move",
                    LiveEvent::End { .. } => "end",
                };
                json_event(name, &ev)
            }
            Err(_) => Event::default().event("resync").data("lagged"),
        })
    });
    Sse::new(first.chain(rest)).keep_alive(KeepAlive::default()).into_response()
}

fn json_event<T: Serialize>(name: &str, v: &T) -> Event {
    Event::default().event(name).data(serde_json::to_string(v).unwrap_or_default())
}

/// One opponent in a player's head-to-head table.
#[derive(Debug, Clone, Default)]
pub struct HeadToHead {
    pub opponent: String,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub void: u32,
}

async fn player(State(ctx): Shared, Path(name): Path<String>) -> WebResult<Response> {
    let q = format!("SELECT {GAME_COLS} FROM games WHERE sente = ?1 OR gote = ?1 ORDER BY started_at DESC");
    let games = rows(sqlx::query_as(&q).bind(&name).fetch_all(&ctx.db).await?);
    let ratings: Vec<(String, f64, i64)> = sqlx::query_as("SELECT league, rating, games FROM ratings WHERE player = ? ORDER BY league")
        .bind(&name)
        .fetch_all(&ctx.db)
        .await?;
    let (exists,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM players WHERE name = ?").bind(&name).fetch_one(&ctx.db).await?;
    if games.is_empty() && exists == 0 {
        return Err(not_found(&format!("No player {name}.")));
    }
    let mut h2h: BTreeMap<(String, String), HeadToHead> = BTreeMap::new();
    for g in &games {
        let (opp, me) = if g.sente == name { (&g.gote, '+') } else { (&g.sente, '-') };
        let Some(result) = g.result.as_deref() else { continue };
        let e = h2h
            .entry((g.league.clone(), opp.clone()))
            .or_insert_with(|| HeadToHead { opponent: opp.clone(), ..Default::default() });
        match result {
            "void" => e.void += 1,
            "=" => e.draws += 1,
            r if r.starts_with(me) => e.wins += 1,
            _ => e.losses += 1,
        }
    }
    let snap = snapshot(&ctx.lobby).await;
    let online = snap.players.iter().find(|p| p.0 == name).map(|p| p.1.clone());
    Ok(pages::player(&name, online.as_deref(), &ratings, &h2h, &games).into_response())
}

// ---------------------------------------------------------------- records and API

async fn records_root(state: Shared) -> WebResult<Response> {
    records(state, Path(String::new())).await
}

/// The record tree: files as they are, directories as a plain listing.
async fn records(State(ctx): Shared, Path(path): Path<String>) -> WebResult<Response> {
    let rel = FsPath::new(&path);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(not_found("No such record."));
    }
    let full = ctx.records_dir.join(rel);
    let meta = tokio::fs::metadata(&full).await.map_err(|_| not_found("No such record."))?;
    if meta.is_dir() {
        let mut entries = Vec::new();
        let mut rd = tokio::fs::read_dir(&full).await.map_err(|_| not_found("No such record."))?;
        while let Ok(Some(e)) = rd.next_entry().await {
            let is_dir = e.file_type().await.is_ok_and(|t| t.is_dir());
            entries.push((e.file_name().to_string_lossy().into_owned(), is_dir));
        }
        entries.sort();
        let base = path.trim_end_matches('/');
        return Ok(pages::listing(base, &entries).into_response());
    }
    let body = tokio::fs::read(&full).await.map_err(|_| not_found("No such record."))?;
    let kind = match full.extension().and_then(|e| e.to_str()) {
        Some("json") => "application/json",
        _ => "text/plain; charset=utf-8",
    };
    Ok(([(header::CONTENT_TYPE, kind)], body).into_response())
}

#[derive(Deserialize)]
pub struct GamesQuery {
    league: Option<String>,
    /// ISO time; only games that ended after it.
    since: Option<String>,
    limit: Option<i64>,
}

/// Finished games, oldest first, so a script can poll with `since` set to
/// the last `ended_at` it saw.
async fn api_games(State(ctx): Shared, Query(q): Query<GamesQuery>) -> WebResult<Response> {
    let limit = q.limit.unwrap_or(1000).clamp(1, 10_000);
    let sql = format!(
        "SELECT {GAME_COLS} FROM games WHERE ended_at IS NOT NULL AND (?1 IS NULL OR league = ?1)
         AND (?2 IS NULL OR ended_at > ?2) ORDER BY ended_at, id LIMIT ?3"
    );
    let games = rows(sqlx::query_as(&sql).bind(&q.league).bind(&q.since).bind(limit).fetch_all(&ctx.db).await?);
    let out: Vec<serde_json::Value> = games
        .iter()
        .map(|g| {
            let winner = match g.result.as_deref() {
                Some("+") => Some(g.sente.as_str()),
                Some("-") => Some(g.gote.as_str()),
                _ => None,
            };
            serde_json::json!({
                "id": g.id,
                "league": g.league,
                "rules": GameName::from_str(&g.league).map(|n| n.rules()).unwrap_or("lishogi"),
                "sente": g.sente,
                "gote": g.gote,
                "started_at": g.started_at,
                "ended_at": g.ended_at,
                "result": g.result,
                "winner": winner,
                "reason": g.reason,
                "plies": g.ply_count,
                "rated": g.rated,
                "record": format!("/records/{}/", g.record_dir()),
            })
        })
        .collect();
    Ok(axum::Json(out).into_response())
}

async fn static_file(Path(file): Path<String>) -> Response {
    let (body, kind): (&'static str, &str) = match file.as_str() {
        "htmx.min.js" => (include_str!("static/htmx.min.js"), "text/javascript; charset=utf-8"),
        "board.js" => (include_str!("static/board.js"), "text/javascript; charset=utf-8"),
        "chart.js" => (include_str!("static/chart.js"), "text/javascript; charset=utf-8"),
        "style.css" => (include_str!("static/style.css"), "text/css; charset=utf-8"),
        _ => return not_found("No such file.").into_response(),
    };
    let mut r = ([(header::CONTENT_TYPE, kind)], body).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    r
}
