//! Page markup (maud). Every page is usable without JavaScript; the board,
//! the live updates, the chart's hover layer and the HTMX refreshes are
//! additions on top.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use maud::{html, Markup, PreEscaped, DOCTYPE};

use super::chart::{self, Chart};
use super::{GameRow, HeadToHead, LeagueInfo, MoveRow, Standing};
use crate::game::LiveState;
use crate::lobby::LobbySnapshot;
use crate::rating::PUBLISH_AFTER;
use crate::league::{Kind, LeagueConfig, RuleSet};

pub struct RoundLine {
    pub at: String,
    pub pool: Vec<String>,
    /// sente, gote, game id
    pub pairs: Vec<[String; 3]>,
    pub sat_out: Option<String>,
}

fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · chu shogi" }
                link rel="stylesheet" href="/static/style.css";
                script src="/static/htmx.min.js" defer {}
            }
            body {
                header.top {
                    a.brand href="/" { "中将棋 chu shogi" }
                    nav { a href="/" { "Leagues" } a href="/records/" { "Records" } a href="/api/games?limit=50" { "API" } }
                }
                main { (body) }
                footer { "Times are UTC. " a href="https://github.com/jh85/chushogi" { "chushogi" } }
            }
        }
    }
}

pub fn error_page(code: StatusCode, msg: &str) -> Markup {
    layout(code.canonical_reason().unwrap_or("Error"), html! {
        h1 { (code.as_u16()) " " (code.canonical_reason().unwrap_or("")) }
        p { (msg) }
        p { a href="/" { "Back to the leagues" } }
    })
}

/// `2026-11-07T18:00:00.002Z` → `2026-11-07 18:00`.
pub fn when(iso: &str) -> String {
    iso.get(..16).unwrap_or(iso).replace('T', " ")
}

/// `when` for table cells: the year is dropped on phones.
fn stamp(iso: &str) -> Markup {
    let t = when(iso);
    match t.split_at_checked(5) {
        Some((year, rest)) if year.ends_with('-') => html! { span.yr { (year) } (rest) },
        _ => html! { (t) },
    }
}

/// `600 s + 10 s Fischer`, from `leagues.toml`; a league no longer listed
/// there has only its games left.
fn time_control(cfg: Option<&LeagueConfig>) -> String {
    cfg.map_or_else(|| "not in the league list".into(), LeagueConfig::describe_time)
}

/// How a league pairs: `every 30 min`, `on demand`, `test`.
fn pairing_text(cfg: Option<&LeagueConfig>) -> String {
    let Some(c) = cfg else { return "–".into() };
    let mut t = match (c.kind, c.schedule()) {
        (_, Some(s)) => format!("every {} min", s.every_min),
        (Kind::Test, _) => "test, on demand".into(),
        _ => "on demand".into(),
    };
    if !c.rated {
        t.push_str(", unrated");
    }
    if c.rules == RuleSet::Jcsa {
        t.push_str(", JCSA rules");
    }
    t
}

/// `#ROYALS_LOST` → `royals lost`.
fn reason_text(r: &str) -> String {
    r.trim_start_matches('#').to_lowercase().replace('_', " ")
}

fn score(result: Option<&str>) -> &'static str {
    match result {
        Some("+") => "1–0",
        Some("-") => "0–1",
        Some("=") => "½–½",
        Some(_) => "void",
        None => "unfinished",
    }
}

fn player_link(name: &str) -> Markup {
    html! { a href=(format!("/player/{name}")) { (name) } }
}

fn game_link(id: &str, text: &str) -> Markup {
    html! { a href=(format!("/game/{id}")) { (text) } }
}

/// JSON inside `<script type="application/json">`.
fn json_script(id: &str, v: &serde_json::Value) -> Markup {
    let s = v.to_string().replace("</", "<\\/");
    html! { script type="application/json" id=(id) { (PreEscaped(s)) } }
}

fn results_table(games: &[GameRow], show_league: bool) -> Markup {
    html! {
        div.scroll {
            table.results {
                thead { tr {
                    th { "Ended" } @if show_league { th.wide { "League" } }
                    th { "Sente" } th { "Result" } th { "Gote" } th.num.wide { "Plies" } th.wide {}
                } }
                tbody {
                    @for g in games {
                        tr {
                            td.time { @if let Some(e) = &g.ended_at { (stamp(e)) } }
                            @if show_league { td.wide { a href=(format!("/league/{}", g.league)) { (g.league) } } }
                            td { (player_link(&g.sente)) }
                            td { (game_link(&g.id, score(g.result.as_deref()))) " " span.muted.wide { (g.reason.as_deref().map(reason_text).unwrap_or_default()) } }
                            td { (player_link(&g.gote)) }
                            td.num.wide { (g.ply_count) }
                            td.wide { (game_link(&g.id, "view")) }
                        }
                    }
                }
            }
        }
    }
}

pub fn live(snap: &LobbySnapshot) -> Markup {
    html! {
        section #live hx-get="/live" hx-trigger="every 10s" hx-swap="outerHTML" {
            h2 { "Games in progress" }
            @if snap.games.is_empty() { p.muted { "None right now." } }
            ul.plain {
                @for (id, name, sente, gote) in &snap.games {
                    li { (game_link(id, &format!("{sente} vs {gote}"))) " " span.muted { (name) } }
                }
            }
            h2 { "Online" }
            @if snap.players.is_empty() { p.muted { "Nobody." } }
            ul.plain {
                @for (name, state) in &snap.players {
                    li { (player_link(name)) " " span.muted { (state.split(' ').next().unwrap_or("")) } }
                }
            }
        }
    }
}

pub fn index(leagues: &[LeagueInfo], snap: &LobbySnapshot, recent: &[GameRow]) -> Markup {
    layout("Leagues", html! {
        h1 { "Leagues" }
        div.scroll {
            table {
                thead { tr { th { "League" } th.wide { "Time control" } th { "Schedule" } th { "Next round" } th.num.wide { "Games" } } }
                tbody {
                    @for l in leagues {
                        tr {
                            td { a href=(format!("/league/{}", l.name)) { (l.name) } }
                            td.wide { (time_control(l.config.as_ref())) }
                            td { (pairing_text(l.config.as_ref())) }
                            td.time { @if let Some(t) = l.next_round() { (stamp(&t)) } @else { "–" } }
                            td.num.wide { (l.games) }
                        }
                    }
                }
            }
        }
        (live(snap))
        h2 { "Latest results" }
        (results_table(recent, true))
    })
}

pub fn standings(league: &str, table: &[Standing]) -> Markup {
    html! {
        div #standings.scroll hx-get=(format!("/league/{league}/standings")) hx-trigger="every 30s" hx-swap="outerHTML" {
            table.standings {
                thead { tr {
                    th.num { "#" } th { "Player" } th.num { "Rating" } th.num { "Games" }
                    th.num { "W" } th.num { "D" } th.num { "L" } th.wide { "Last played" }
                } }
                tbody {
                    @for (i, s) in table.iter().enumerate() {
                        tr.provisional[!s.published()] {
                            td.num { (i + 1) }
                            td { (player_link(&s.player)) }
                            td.num {
                                @match s.rating {
                                    Some(r) => { (format!("{r:.0}")) @if !s.published() { abbr title=(format!("provisional: under {PUBLISH_AFTER} rated games")) { "?" } } }
                                    None => { "–" }
                                }
                            }
                            td.num { (s.rated_games) }
                            td.num { (s.wins) } td.num { (s.draws) } td.num { (s.losses) }
                            td.time.wide { @if let Some(t) = &s.last_played { (stamp(t)) } }
                        }
                    }
                }
            }
            p.muted.small { "Bradley–Terry fit over all rated games, 200 points = 76% expected score, mean 1500; "
                "published from " (PUBLISH_AFTER) " games (“?” = provisional)." }
        }
    }
}

fn chart_table(c: &Chart) -> Markup {
    html! {
        details.table-view {
            summary { "Rating by " (c.x_title) " as a table" }
            div.scroll {
                table {
                    thead { tr { th { (c.x_title) } @for s in &c.series { th.num { (s.name) } } } }
                    tbody {
                        @for (i, label) in c.labels.iter().enumerate() {
                            tr {
                                td.time { (label) }
                                @for s in &c.series {
                                    td.num { @match s.values[i] {
                                        Some((r, g)) => { (format!("{r:.0}")) @if g < PUBLISH_AFTER { "?" } }
                                        None => { "–" }
                                    } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn league(
    name: &str,
    cfg: Option<&LeagueConfig>,
    table: &[Standing],
    chart: &Chart,
    recent: &[GameRow],
    rounds: &[RoundLine],
    n_rounds: i64,
    live: &[(String, String, String, String)],
) -> Markup {
    layout(name, html! {
        h1 { (name) }
        p.meta {
            (time_control(cfg))
            @if let Some(s) = cfg.and_then(LeagueConfig::schedule) {
                " · a round every " (s.every_min) " min, cutoff " (s.cutoff_s) " s · next "
                (when(&crate::util::Utc::from(s.next_round(std::time::SystemTime::now())).iso()))
            } @else if cfg.is_some() {
                " · " (pairing_text(cfg))
            }
            " · " a href=(format!("/api/games?league={name}")) { "JSON" }
        }
        @if !live.is_empty() {
            p { "Live: "
                @for (i, (id, _, sente, gote)) in live.iter().enumerate() {
                    @if i > 0 { ", " } (game_link(id, &format!("{sente} vs {gote}")))
                }
            }
        }
        h2 { "Standings" }
        (standings(name, table))
        h2 { "Rating curves" }
        figure.chart.rc {
            (chart::svg(chart))
            div.tooltip hidden {}
            figcaption.muted.small {
                "Each player's rating as the next " (chart.x_title) " began"
                @if chart.omitted > 0 { "; " (chart.omitted) " lower-rated players not drawn" }
                ". " a href=(format!("/league/{name}/rating.svg")) { "SVG" }
            }
        }
        (chart_table(chart))
        script src="/static/chart.js" defer {}
        h2 { "Latest results" }
        (results_table(recent, false))
        @if n_rounds > 0 {
            h2 { "Rounds" }
            p.muted.small { (n_rounds) " rounds so far; the latest:" }
            div.scroll {
                table {
                    thead { tr { th { "Round" } th.num.wide { "Queued" } th { "Pairings (sente vs gote)" } th { "Sat out" } } }
                    tbody {
                        @for r in rounds {
                            tr {
                                td.time { (stamp(&r.at)) }
                                td.num.wide { (r.pool.len()) }
                                td { @for (i, [s, g, id]) in r.pairs.iter().enumerate() {
                                    @if i > 0 { br; } (game_link(id, &format!("{s} vs {g}")))
                                } }
                                td { (r.sat_out.as_deref().unwrap_or("–")) }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// The board, clocks, controls and move list; filled in by board.js.
fn board_block(sente: &str, gote: &str, moves: &[(String, String)]) -> Markup {
    html! {
        div.game {
            div.side.gote { span.mark { "☖" } " " (player_link(gote)) span.clock #clock-w {} }
            div #board.board aria-label="board" {}
            div.side.sente { span.mark { "☗" } " " (player_link(sente)) span.clock #clock-b {} }
            div.controls {
                button type="button" data-step="first" title="first position" { "⏮" }
                button type="button" data-step="back" title="back" { "◀" }
                span #ply.muted {}
                button type="button" data-step="fwd" title="forward" { "▶" }
                button type="button" data-step="last" title="latest position" { "⏭" }
                button type="button" #pieces title="letters or kanji" { "漢/A" }
            }
        }
        h2 { "Moves" }
        ol #moves.moves {
            @for (side, usi) in moves {
                li { span.mark { (if side == "b" { "☗" } else { "☖" }) } (usi) }
            }
        }
    }
}

pub fn live_game(st: &LiveState, cfg: Option<&LeagueConfig>) -> Markup {
    let data = serde_json::json!({ "live": true, "events": format!("/game/{}/events", st.id), "state": st });
    let moves: Vec<(String, String)> = st.moves.iter().map(|m| (m.side.to_owned(), m.usi.clone())).collect();
    layout(&format!("{} vs {}", st.sente, st.gote), html! {
        h1 { (st.sente) " vs " (st.gote) }
        p.meta {
            a href=(format!("/league/{}", st.league)) { (st.league) } " · " (time_control(cfg)) " · "
            span #status.live { @if st.started { "live" } @else { "starting" } }
        }
        (board_block(&st.sente, &st.gote, &moves))
        (json_script("game-data", &data))
        script src="/static/board.js" defer {}
    })
}

pub fn finished_game(g: &GameRow, cfg: Option<&LeagueConfig>, initial_sfen: &str, moves: &[MoveRow]) -> Markup {
    let used = |side: &str| moves.iter().filter(|m| m.side == side).map(|m| m.elapsed_ms).sum::<i64>();
    let data = serde_json::json!({
        "live": false,
        "initial_sfen": initial_sfen,
        "moves": moves,
        "used_ms": [used("b"), used("w")],
    });
    let list: Vec<(String, String)> = moves.iter().map(|m| (m.side.to_owned(), m.usi.clone())).collect();
    let dir = g.record_dir();
    layout(&format!("{} vs {}", g.sente, g.gote), html! {
        h1 { (g.sente) " vs " (g.gote) }
        p.meta {
            a href=(format!("/league/{}", g.league)) { (g.league) } " · " (time_control(cfg)) " · "
            span #status {
                @if let Some(e) = &g.ended_at {
                    (score(g.result.as_deref())) " " (g.reason.as_deref().map(reason_text).unwrap_or_default())
                    " after " (g.ply_count) " plies · " (when(e))
                    @if !g.rated { " · unrated" }
                } @else {
                    // Not live and never finished: the server stopped during it.
                    "unfinished after " (moves.len()) " plies (the server stopped during the game)"
                }
            }
        }
        (board_block(&g.sente, &g.gote, &list))
        @if g.ended_at.is_some() {
            h2 { "Record" }
            p.downloads {
                @for f in ["game.csa", "record.json", "moves.usi", "positions.sfen", "positions.tsv"] {
                    a href=(format!("/records/{dir}/{f}")) download=(format!("{}-{f}", g.id)) { (f) } " "
                }
            }
        }
        (json_script("game-data", &data))
        script src="/static/board.js" defer {}
    })
}

pub fn player(
    name: &str,
    online: Option<&str>,
    ratings: &[(String, f64, i64)],
    h2h: &BTreeMap<(String, String), HeadToHead>,
    games: &[GameRow],
) -> Markup {
    let shown = &games[..games.len().min(300)];
    layout(name, html! {
        h1 { (name) }
        p.meta { @match online { Some(s) => { "online, " (s) } None => { "offline" } } }
        @if !ratings.is_empty() {
            h2 { "Ratings" }
            div.scroll { table {
                thead { tr { th { "League" } th.num { "Rating" } th.num { "Rated games" } } }
                tbody { @for (league, r, n) in ratings {
                    tr {
                        td { a href=(format!("/league/{league}")) { (league) } }
                        td.num { (format!("{r:.0}")) @if (*n as u32) < PUBLISH_AFTER { "?" } }
                        td.num { (n) }
                    }
                } }
            } }
        }
        h2 { "Head to head" }
        div.scroll { table {
            thead { tr { th.wide { "League" } th { "Opponent" } th.num { "W" } th.num { "D" } th.num { "L" } th.num { "Score" } } }
            tbody { @for ((league, _), h) in h2h {
                @let n = h.wins + h.draws + h.losses;
                tr {
                    td.wide { (league) } td { (player_link(&h.opponent)) }
                    td.num { (h.wins) } td.num { (h.draws) } td.num { (h.losses) }
                    td.num { @if n > 0 { (format!("{:.0}%", 100.0 * (h.wins as f64 + h.draws as f64 / 2.0) / n as f64)) } @else { "–" } }
                }
            } }
        } }
        h2 { "Games" }
        p.muted.small { (games.len()) " games" @if games.len() > shown.len() { ", the latest " (shown.len()) " shown" } "." }
        div.scroll { table.results {
            thead { tr { th { "Started" } th.wide { "League" } th { "Colour" } th { "Opponent" } th { "Result" } th.num.wide { "Plies" } th.wide { "Record" } } }
            tbody { @for g in shown {
                @let sente = g.sente == name;
                @let mine = match (g.result.as_deref(), sente) {
                    (Some("+"), true) | (Some("-"), false) => "win",
                    (Some("+"), false) | (Some("-"), true) => "loss",
                    (Some("="), _) => "draw",
                    (Some(_), _) => "void",
                    (None, _) => "playing",
                };
                tr {
                    td.time { (stamp(&g.started_at)) }
                    td.wide { (g.league) }
                    td { (if sente { "☗" } else { "☖" }) span.wide { (if sente { " sente" } else { " gote" }) } }
                    td { (player_link(if sente { &g.gote } else { &g.sente })) }
                    td { (game_link(&g.id, mine)) " " span.muted.wide { (g.reason.as_deref().map(reason_text).unwrap_or_default()) } }
                    td.num.wide { (g.ply_count) }
                    td.wide { @if g.ended_at.is_some() {
                        a href=(format!("/records/{}/game.csa", g.record_dir())) download=(format!("{}.csa", g.id)) { "csa" } " "
                        a href=(format!("/records/{}/record.json", g.record_dir())) download=(format!("{}.json", g.id)) { "json" }
                    } }
                }
            } }
        } }
    })
}

pub fn listing(base: &str, entries: &[(String, bool)]) -> Markup {
    let title = if base.is_empty() { "records".to_owned() } else { format!("records/{base}") };
    let prefix = if base.is_empty() { "/records/".to_owned() } else { format!("/records/{base}/") };
    layout(&title, html! {
        h1 { (title) }
        ul.plain.listing {
            @if !base.is_empty() { li { a href=(format!("{prefix}..")) { "../" } } }
            @for (name, dir) in entries {
                li { a href=(format!("{prefix}{name}{}", if *dir { "/" } else { "" })) { (name) @if *dir { "/" } } }
            }
        }
    })
}
