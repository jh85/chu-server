//! `chu-bridge --conformance <engine>`: the session of USI-CHU.md §6 and the
//! checks of §7, each printed as PASS or FAIL. Legality is judged by the
//! referee (`chu-rules`), whose list is `chushogi-gen legal`'s.

use std::time::Duration;

use chu_rules::{Backend, Referee, START_SFEN};
use tokio::time::{sleep, Instant};

use crate::engine::{parse_bestmove, Engine};
use crate::Res;

struct Report {
    failed: u32,
}

impl Report {
    fn check(&mut self, name: &str, pass: bool, detail: impl AsRef<str>) {
        let detail = detail.as_ref();
        let sep = if detail.is_empty() { "" } else { ": " };
        println!("{} {name}{sep}{detail}", if pass { "PASS" } else { "FAIL" });
        if !pass {
            self.failed += 1;
        }
    }
}

/// What came back for one `go`: bestmove lines (move, id) seen within the
/// wait, the time to the first one, and `info string` lines.
struct Answer {
    bestmoves: Vec<(String, Option<String>)>,
    first: Option<Duration>,
}

/// Reads for `within`, or until the first bestmove plus `linger`.
async fn collect(engine: &mut Engine, within: Duration, linger: Duration) -> Res<Answer> {
    let t0 = Instant::now();
    let mut deadline = t0 + within;
    let mut a = Answer { bestmoves: Vec::new(), first: None };
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Some(line) = engine.recv_within(left).await? else { return Ok(a) };
        if let Some(bm) = parse_bestmove(&line) {
            if a.first.is_none() {
                a.first = Some(t0.elapsed());
                deadline = Instant::now() + linger;
            }
            a.bestmoves.push(bm);
        }
    }
}

fn referee(moves: &[String]) -> Res<Box<dyn Referee>> {
    let mut r = Backend::Ffi.create()?;
    r.set_sfen(START_SFEN)?;
    for m in moves {
        r.apply(m)?;
    }
    Ok(r)
}

/// One bestmove, with the expected id, legal after `moves`.
fn judge(rep: &mut Report, name: &str, a: &Answer, id: &str, moves: &[String]) -> Res<Option<String>> {
    let legal = referee(moves)?.legal()?;
    let ok = match a.bestmoves.as_slice() {
        [(mv, Some(echo))] => echo == id && (legal.contains(mv) || (mv == "resign" && legal.is_empty())),
        _ => false,
    };
    rep.check(name, ok, format!("{:?} after {:?}", a.bestmoves, a.first));
    Ok(a.bestmoves.first().map(|b| b.0.clone()))
}

fn position(moves: &[String]) -> String {
    if moves.is_empty() {
        "position startpos".into()
    } else {
        format!("position startpos moves {}", moves.join(" "))
    }
}

pub async fn run(mut engine: Engine, options: &[(String, String)]) -> Res<bool> {
    let mut rep = Report { failed: 0 };
    let usi = engine.usi().await;
    rep.check("usi: id name, options, usiok", usi.is_ok() && !engine.name.is_empty(), format!("{:?} {:?}", engine.name, usi.err().map(|e| e.to_string())));
    for (k, v) in options {
        engine.set_option(k, v).await?;
    }
    let ready = engine.ready(Duration::from_secs(5)).await;
    rep.check("isready answered within 5 s", ready.is_ok(), format!("{ready:?}"));
    engine.send("usinewgame").await?;

    // §6, first search: a full clock.
    engine.send("position startpos").await?;
    engine.send("go id 1 btime 600000 wtime 600000 binc 10000 winc 10000").await?;
    let a = collect(&mut engine, Duration::from_secs(120), Duration::from_millis(500)).await?;
    judge(&mut rep, "go id 1: one legal bestmove with id 1", &a, "1", &[])?;

    // §6, second search, cut short by stop.
    let moves: Vec<String> = vec!["8i8h".into(), "6c5e".into()];
    engine.send(&position(&moves)).await?;
    engine.send("go id 2 btime 597800 wtime 593100 binc 10000 winc 10000").await?;
    sleep(Duration::from_millis(100)).await;
    engine.send("stop").await?;
    let a = collect(&mut engine, Duration::from_secs(2), Duration::from_millis(500)).await?;
    judge(&mut rep, "stop: bestmove id 2 within 2 s", &a, "2", &moves)?;

    // stop twice in a row: still exactly one bestmove.
    engine.send(&position(&moves)).await?;
    engine.send("go id 3 infinite").await?;
    sleep(Duration::from_millis(300)).await;
    engine.send("stop").await?;
    engine.send("stop").await?;
    let a = collect(&mut engine, Duration::from_secs(2), Duration::from_secs(1)).await?;
    judge(&mut rep, "go infinite, stop, stop: one bestmove id 3", &a, "3", &moves)?;

    // gameover, then a position without usinewgame must be reported.
    engine.send("gameover win").await?;
    let ready = engine.ready(Duration::from_secs(5)).await;
    rep.check("isready after gameover within 5 s", ready.is_ok(), format!("{ready:?}"));
    engine.send("position startpos").await?;
    let mut reported = None;
    let until = Instant::now() + Duration::from_secs(1);
    while let Some(l) = engine.recv_within(until.saturating_duration_since(Instant::now())).await? {
        if l.starts_with("info string error") {
            reported = Some(l);
            break;
        }
    }
    rep.check("position after gameover without usinewgame: info string error", reported.is_some(), reported.unwrap_or_default());

    // A short game against itself: every bestmove legal, every id echoed.
    engine.send("usinewgame").await?;
    let mut moves: Vec<String> = Vec::new();
    let mut ok = true;
    let mut detail = String::new();
    for ply in 1..=20u32 {
        let id = (100 + ply).to_string();
        engine.send(&position(&moves)).await?;
        engine.send(&format!("go id {id} movetime 300")).await?;
        let a = collect(&mut engine, Duration::from_secs(10), Duration::from_millis(50)).await?;
        let legal = referee(&moves)?.legal()?;
        match a.bestmoves.as_slice() {
            [(mv, Some(echo))] if *echo == id && legal.contains(mv) => moves.push(mv.clone()),
            [(mv, _)] if mv == "resign" && legal.is_empty() => break,
            other => {
                ok = false;
                detail = format!("ply {ply}: {other:?}");
                break;
            }
        }
    }
    if ok {
        detail = format!("{} plies", moves.len());
    }
    rep.check("20 plies at movetime 300: legal bestmoves, ids echoed", ok, detail);

    let ready = engine.ready(Duration::from_secs(5)).await;
    rep.check("readyok within 5 s while idle", ready.is_ok(), format!("{ready:?}"));
    engine.send("quit").await?;
    let exited = engine.recv_within(Duration::from_secs(5)).await.is_err();
    rep.check("quit: the engine exits", exited, "");

    println!("{} failed", rep.failed);
    Ok(rep.failed == 0)
}
