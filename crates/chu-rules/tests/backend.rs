//! In-check on crafted positions, error codes, and status, through both backends.

mod common;

use chu_rules::{Color, Error, GameEnd, Rules, START_SFEN};

#[test]
fn in_check_crafted() {
    use Color::*;
    // (sfen, side, expected): sente pieces uppercase, rank a at the top, file 12 on the left.
    let cases = [
        ("11k/12/11R/12/12/12/12/12/12/12/12/K11 w - 1", Gote, true), // rook, adjacent file square
        ("11k/12/11R/12/12/12/12/12/12/12/12/K11 w - 1", Sente, false),
        ("11k/12/12/12/12/11R/12/12/12/12/12/K11 w - 1", Gote, true), // rook along open file
        ("11k/12/11p/12/12/11R/12/12/12/12/12/K11 w - 1", Gote, false), // blocked by own pawn
        ("11k/11G/12/12/12/12/12/12/12/12/12/K11 w - 1", Gote, true), // gold straight ahead
        ("11k/10G1/12/12/12/12/12/12/12/12/12/K11 w - 1", Gote, true), // gold diagonal forward
        ("11k/12/10N1/12/12/12/12/12/12/12/12/K11 w - 1", Gote, true), // lion two squares away
        ("11k/12/12/9N2/12/12/12/12/12/12/12/K11 w - 1", Gote, false), // lion three away
        ("11k/12/12/12/12/12/12/12/12/12/12/K10b w - 1", Sente, false), // bishop on same rank
        ("11k/12/12/12/12/12/12/12/12/12/1p10/K11 b - 1", Sente, false), // gote pawn beside
        ("11k/12/12/12/12/12/12/12/12/12/p11/K11 b - 1", Sente, true), // gote pawn moves down
        ("11k/12/12/12/12/12/12/12/12/1e10/12/K11 b - 1", Sente, false), // elephant two away
        ("11+e/12/11R/12/12/12/12/12/12/12/12/K11 w - 1", Gote, true), // prince is royal
    ];
    for (name, mut r) in common::backends() {
        for (sfen, side, want) in cases {
            r.set_sfen(sfen).unwrap();
            assert_eq!(r.in_check(side).unwrap(), want, "{name}: {sfen} {side:?}");
        }
    }
}

#[test]
fn errors_and_recovery() {
    for (name, mut r) in common::backends() {
        r.set_sfen(START_SFEN).unwrap();
        r.apply("8i8h").unwrap();
        let after = r.sfen().unwrap();
        assert_eq!(r.apply("8i8h"), Err(Error::Illegal), "{name}");
        assert_eq!(r.apply("6c5e="), Err(Error::Illegal), "{name}: non-canonical spelling");
        assert_eq!(r.apply("zz"), Err(Error::Syntax), "{name}");
        assert_eq!(r.apply("99999999999a1a"), Err(Error::Syntax), "{name}");
        assert_eq!(r.set_sfen("not a sfen"), Err(Error::Syntax), "{name}");
        assert_eq!(r.set_sfen("x/y b - 1 extra"), Err(Error::Syntax), "{name}");
        // Errors leave the position as it was.
        assert_eq!(r.sfen().unwrap(), after, "{name}");
        assert_eq!(r.ply().unwrap(), 1, "{name}");
        r.set_rules(Rules::Jcsa).unwrap();
        assert_eq!(r.sfen().unwrap(), after, "{name}");
        r.apply("4c4b").unwrap();
        assert_eq!(r.ply().unwrap(), 2, "{name}");
    }
}

#[test]
fn status_end_positions() {
    // One move from each referee-decided ending; see chu-server's end-condition tests.
    let cases = [
        // rook takes the only king
        ("g10k/12/12/12/12/11R/12/12/12/12/12/K11 b - 1", "1f1a", GameEnd::RoyalsLost, Some(Color::Sente)),
        // rook takes gote's last gold: lone king against king and rook
        ("11k/12/11g/12/12/12/12/12/12/12/12/K10R b - 1", "1l1c", GameEnd::BareKing, Some(Color::Sente)),
        // gote's king and pawns are wedged in: no legal move
        ("K11/12/12/12/12/12/12/12/12/12/9ppp/9pkp b - 1", "12a11a", GameEnd::Stalemate, Some(Color::Sente)),
        // king takes the last non-royal piece
        ("11k/12/12/12/6g5/6K5/12/12/12/12/12/12 b - 1", "6f6e", GameEnd::Draw, None),
    ];
    for (name, mut r) in common::backends() {
        for (sfen, mv, end, winner) in cases {
            r.set_sfen(sfen).unwrap();
            assert_eq!(r.status().unwrap().end, GameEnd::Playing, "{name}: {sfen}");
            r.apply(mv).unwrap();
            let st = r.status().unwrap();
            assert_eq!((st.end, st.winner), (end, winner), "{name}: {sfen} {mv}");
        }
    }
}
