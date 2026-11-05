//! Gate 0a: perft depth 1-2 from the start position and the 287 scalashogi
//! fixtures in vendor/chushogi/tests/perft_cases.tsv, through both backends.

mod common;

use chu_rules::{Referee, START_SFEN};

fn perft(r: &mut dyn Referee, sfen: &str, depth: u32) -> u64 {
    r.set_sfen(sfen).unwrap();
    let moves = r.legal().unwrap();
    if depth == 1 {
        return moves.len() as u64;
    }
    let mut n = 0;
    for m in &moves {
        r.set_sfen(sfen).unwrap();
        r.apply(m).unwrap();
        let child = r.sfen().unwrap();
        n += perft(r, &child, depth - 1);
    }
    n
}

#[test]
fn perft_start_and_fixtures() {
    let cases = std::fs::read_to_string(common::vendor_tests().join("perft_cases.tsv")).unwrap();
    let cases: Vec<(&str, u64)> = cases
        .lines()
        .map(|l| {
            let (sfen, n) = l.split_once('\t').unwrap();
            (sfen, n.parse().unwrap())
        })
        .collect();
    assert_eq!(cases.len(), 287);

    for (name, mut r) in common::backends() {
        assert_eq!(perft(r.as_mut(), START_SFEN, 1), 36, "{name} start depth 1");
        assert_eq!(perft(r.as_mut(), START_SFEN, 2), 1296, "{name} start depth 2");
        let mut failures = Vec::new();
        for (i, (sfen, want)) in cases.iter().enumerate() {
            let got = perft(r.as_mut(), sfen, 1);
            if got != *want {
                failures.push(format!("line {}: {sfen}: got {got}, want {want}", i + 1));
            }
        }
        assert!(failures.is_empty(), "{name}: {}", failures.join("\n"));
        eprintln!("{name}: 2 start-position + {} fixture perfts OK", cases.len());
    }
}
