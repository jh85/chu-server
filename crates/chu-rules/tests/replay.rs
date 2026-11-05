//! Gate 0b: replay the 49 lishogi games through both backends.
//!
//! The games are tracked in vendor/chushogi/tests/chushogi_cases.tsv (one
//! `position sfen <initial> moves <m1..mk>\tbestmove <mk+1>` line per ply;
//! 49 games, 8688 plies). That file holds the moves but not the per-ply
//! SFENs, so the check is: every recorded move is in both backends' legal
//! list, the lists are identical, and after every move both backends report
//! the same SFEN, status and check state.
//!
//! When the original record directories are available (`positions.sfen` +
//! `moves.usi`, as read by tests/replay_test.py), point CHU_REPLAY_GAMES at
//! one or more of them (colon-separated) and every SFEN is also compared with
//! the recorded one.

mod common;

use std::path::Path;

use chu_rules::{Color, Referee};

struct Game {
    name: String,
    initial: String,
    moves: Vec<String>,
    /// Recorded SFEN after each ply (index 0 = initial), when known.
    sfens: Option<Vec<String>>,
}

fn games_from_casefile(path: &Path) -> Vec<Game> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut games: Vec<Game> = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let (input, output) = line.split_once('\t').unwrap();
        let mv = output.strip_prefix("bestmove ").unwrap().to_owned();
        let rest = input.strip_prefix("position sfen ").unwrap();
        let (initial, prior) = match rest.split_once(" moves ") {
            Some((s, m)) => (s, m.split(' ').collect::<Vec<_>>()),
            None => (rest, Vec::new()),
        };
        if prior.is_empty() {
            games.push(Game {
                name: format!("case-game-{:02} (line {})", games.len() + 1, lineno + 1),
                initial: initial.to_owned(),
                moves: Vec::new(),
                sfens: None,
            });
        }
        let g = games.last_mut().unwrap();
        assert_eq!(g.initial, initial, "line {}: does not chain", lineno + 1);
        assert_eq!(g.moves, prior, "line {}: does not chain", lineno + 1);
        g.moves.push(mv);
    }
    games
}

fn games_from_dirs(dirs: &str) -> Vec<Game> {
    let mut games = Vec::new();
    for dir in dirs.split(':') {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("positions.sfen").exists())
            .collect();
        entries.sort();
        for p in entries {
            let sfens: Vec<String> = std::fs::read_to_string(p.join("positions.sfen"))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect();
            let moves: Vec<String> = std::fs::read_to_string(p.join("moves.usi"))
                .unwrap()
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            games.push(Game {
                name: p.display().to_string(),
                initial: sfens[0].clone(),
                moves,
                sfens: Some(sfens),
            });
        }
    }
    games
}

fn replay(game: &Game, refs: &mut [(&'static str, Box<dyn Referee>)]) -> Result<(), String> {
    let ctx = |ply: usize, msg: String| format!("{} ply {ply}: {msg}", game.name);
    for (_, r) in refs.iter_mut() {
        r.set_sfen(&game.initial).map_err(|e| ctx(0, e.to_string()))?;
    }
    let check_state = |refs: &mut [(&'static str, Box<dyn Referee>)], ply: usize| -> Result<(), String> {
        let mut first: Option<(String, chu_rules::Status, bool, bool, u32)> = None;
        for (name, r) in refs.iter_mut() {
            let state = (
                r.sfen().unwrap(),
                r.status().unwrap(),
                r.in_check(Color::Sente).unwrap(),
                r.in_check(Color::Gote).unwrap(),
                r.ply().unwrap(),
            );
            if state.4 != ply as u32 {
                return Err(ctx(ply, format!("{name} ply() = {}", state.4)));
            }
            match &first {
                None => first = Some(state),
                Some(f) if *f != state => {
                    return Err(ctx(ply, format!("backends disagree: {f:?} vs {name} {state:?}")))
                }
                _ => {}
            }
        }
        let sfen = first.unwrap().0;
        let want = match &game.sfens {
            Some(s) => &s[ply],
            None if ply == 0 => &game.initial,
            None => return Ok(()),
        };
        if &sfen != want {
            return Err(ctx(ply, format!("sfen\n  got {sfen}\n  exp {want}")));
        }
        Ok(())
    };
    check_state(refs, 0)?;
    for (i, mv) in game.moves.iter().enumerate() {
        let mut lists = refs.iter_mut().map(|(n, r)| (*n, r.legal().unwrap()));
        let (_, first) = lists.next().unwrap();
        for (name, l) in lists {
            if l != first {
                return Err(ctx(i, format!("{name} legal list differs from ffi")));
            }
        }
        if !first.contains(mv) {
            return Err(ctx(i + 1, format!("{mv} not in legal list ({} moves)", first.len())));
        }
        for (name, r) in refs.iter_mut() {
            r.apply(mv).map_err(|e| ctx(i + 1, format!("{name} apply {mv}: {e}")))?;
        }
        check_state(refs, i + 1)?;
    }
    Ok(())
}

#[test]
fn replay_lishogi_games() {
    let games = match std::env::var("CHU_REPLAY_GAMES") {
        Ok(dirs) => games_from_dirs(&dirs),
        Err(_) => {
            let g = games_from_casefile(&common::vendor_tests().join("chushogi_cases.tsv"));
            assert_eq!(g.len(), 49);
            assert_eq!(g.iter().map(|g| g.moves.len()).sum::<usize>(), 8688);
            g
        }
    };
    let mut refs = common::backends();
    let failures: Vec<String> = games.iter().filter_map(|g| replay(g, &mut refs).err()).collect();
    let plies: usize = games.iter().map(|g| g.moves.len()).sum();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!("{} games, {plies} plies replayed identically through ffi and subprocess", games.len());
}
