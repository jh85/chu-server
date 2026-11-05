//! The wire examples in SERVER-PROTOCOL.md are the fixtures: the spec file is
//! read at compile time, so editing an example without the code (or the
//! reverse) fails here.

use chu_proto::*;

const SPEC: &str = include_str!("../../../SERVER-PROTOCOL.md");

/// Code blocks of the spec, as lists of lines.
fn code_blocks() -> Vec<Vec<&'static str>> {
    let mut blocks = Vec::new();
    let mut cur: Option<Vec<&str>> = None;
    for line in SPEC.lines() {
        if line.starts_with("```") {
            match cur.take() {
                Some(b) => blocks.push(b),
                None => cur = Some(Vec::new()),
            }
        } else if let Some(b) = cur.as_mut() {
            b.push(line);
        }
    }
    blocks
}

/// Strips the `< ` / `> ` direction marker and any trailing comment
/// (two or more spaces, as in the §5 block).
fn wire(line: &str) -> (char, &str) {
    let dir = line.chars().next().unwrap();
    let body = &line[2..];
    let body = body.split("  ").next().unwrap().trim_end();
    (dir, body)
}

fn block_containing(needle: &str) -> Vec<&'static str> {
    code_blocks().into_iter().find(|b| b.iter().any(|l| l.contains(needle))).unwrap()
}

#[test]
fn game_summary_example() {
    let block = block_containing("BEGIN Game_Summary");
    let server: Vec<&str> = block.iter().map(|l| wire(l)).filter(|(d, _)| *d == '<').map(|(_, b)| b).collect();
    let end = server.iter().position(|l| *l == "END Game_Summary").unwrap();
    let summary_lines = &server[..=end];

    let s = GameSummary::parse(summary_lines).unwrap();
    assert_eq!(s.format, "Chushogi 1.0");
    assert_eq!(s.rules, "lishogi");
    assert_eq!(s.game_id, "chu-floodgate-600-10F+az_v12+random_bot+20261104213000");
    assert_eq!((s.name_plus.as_str(), s.name_minus.as_str()), ("az_v12", "random_bot"));
    assert_eq!((s.your_turn, s.to_move), (Side::Sente, Side::Sente));
    assert!(!s.rematch_on_draw);
    assert_eq!(s.max_moves, MAX_MOVES);
    assert_eq!(
        s.time,
        TimeConfig { time_unit: TimeUnit::Msec, total_time: 600000, byoyomi: 0, increment: 10000, least_time_per_move: 0 }
    );
    assert!(s.sfen.starts_with("lfcsgekgscfl/") && s.sfen.ends_with(" b - 1"));
    assert!(s.moves.is_empty());
    // Encoding gives back the spec's lines exactly.
    assert_eq!(s.to_lines(), summary_lines);

    // The game name inside the Game_ID follows the §3 grammar.
    let name: GameName = s.game_id.split('+').next().unwrap().parse().unwrap();
    assert_eq!((name.league.as_str(), name.total_s, name.byoyomi_s, name.fischer, name.jcsa), ("floodgate", 600, 10, true, false));

    // Then the client agrees and the server starts the game.
    let rest: Vec<(char, &str)> = block.iter().map(|l| wire(l)).skip_while(|(_, b)| *b != "END Game_Summary").skip(1).collect();
    assert_eq!(ClientMsg::parse(rest[0].1), ClientMsg::Agree(None));
    assert_eq!(ServerMsg::parse(rest[1].1), ServerMsg::Start("<game-id>".into()));
}

#[test]
fn move_and_resign_example() {
    let block = block_containing("%TORYO");
    let lines: Vec<(char, &str)> = block.iter().map(|l| wire(l)).collect();
    assert_eq!(lines[0], ('>', "+8i8h"));
    assert_eq!(ClientMsg::parse(lines[0].1), ClientMsg::Move { side: Side::Sente, usi: "8i8h".into() });
    let echo = ServerMsg::parse(lines[1].1);
    assert_eq!(echo, ServerMsg::Move(PlayedMove { side: Side::Sente, usi: "8i8h".into(), time: 2310 }));
    assert_eq!(echo.to_string(), lines[1].1);
    assert_eq!(ClientMsg::parse(lines[2].1), ClientMsg::Toryo);
}

#[test]
fn login_example() {
    let block = block_containing("LOGIN");
    let (_, login) = wire(block[0]);
    assert_eq!(login, "LOGIN <name> <password> x1");
    let concrete = login.replace("<name>", "az_v12").replace("<password>", "pw");
    let msg = ClientMsg::parse(&concrete);
    assert_eq!(msg, ClientMsg::Login { name: "az_v12".into(), password: "pw".into(), x1: true });
    assert_eq!(msg.to_string(), concrete);
    // `< LOGIN:<name> OK          |  < LOGIN:incorrect`
    let replies: Vec<&str> = block[1].split('|').map(|s| s.trim().trim_start_matches("< ")).collect();
    assert_eq!(ServerMsg::parse(&replies[0].replace("<name>", "az_v12")), ServerMsg::LoginOk("az_v12".into()));
    assert_eq!(ServerMsg::parse(replies[1]), ServerMsg::LoginIncorrect);
    assert_eq!(ClientMsg::parse(wire(block[2]).1), ClientMsg::Logout);
    assert_eq!(ServerMsg::parse(wire(block[3]).1), ServerMsg::LogoutCompleted);
}

#[test]
fn reason_table() {
    let mut seen = Vec::new();
    for line in SPEC.lines().filter(|l| l.starts_with("| `#")) {
        let tok = line.split('`').nth(1).unwrap();
        let r = Reason::parse(tok).unwrap_or_else(|| panic!("unknown reason {tok}"));
        assert_eq!(ServerMsg::parse(tok), ServerMsg::Reason(r));
        seen.push(r);
    }
    assert_eq!(seen, Reason::ALL, "spec table and Reason::ALL differ");
    for v in ["#WIN", "#LOSE", "#DRAW", "#CENSORED"] {
        assert!(SPEC.contains(&format!("`{v}`")));
        assert_eq!(ServerMsg::parse(v).to_string(), v);
        assert!(matches!(ServerMsg::parse(v), ServerMsg::Verdict(_)));
    }
}

#[test]
fn game_name_examples() {
    let a: GameName = "chu-floodgate-600-10F".parse().unwrap();
    assert!(SPEC.contains("`chu-floodgate-600-10F` = 600 s + 10 s Fischer, lishogi rules"));
    assert_eq!((a.total_s, a.byoyomi_s, a.fischer, a.rules()), (600, 10, true, "lishogi"));
    let b: GameName = "chu-floodgate-900-30-jcsa".parse().unwrap();
    assert!(SPEC.contains("`chu-floodgate-900-30-jcsa` = 900 s + 30 s byoyomi, JCSA rules"));
    assert_eq!((b.total_s, b.byoyomi_s, b.fischer, b.rules()), (900, 30, false, "jcsa"));
    let t: GameName = "chu-test-60-1F".parse().unwrap();
    assert!(t.is_test() && !t.is_floodgate());
    for bad in ["chu-test-60", "chu-test-060-1F", "shogi-test-60-1", "chu--60-1", "chu-test-60-1f", "chu-test-60-1F-lishogi", "chu-te st-60-1"] {
        assert!(bad.parse::<GameName>().is_err(), "{bad}");
    }
}

#[test]
fn client_lines() {
    use ClientMsg::*;
    let sfen = "11k/12/12/12/12/12/12/12/12/12/12/K11 b - 1";
    let cases = [
        ("%%GAME chu-test-60-1F *", Game { game_name: "chu-test-60-1F".into(), color: ColorReq::Any, sfen: None }),
        (&format!("%%GAME chu-test-60-1F + {sfen}"), Game { game_name: "chu-test-60-1F".into(), color: ColorReq::Sente, sfen: Some(sfen.into()) }),
        ("%%CHAT hello  wörld", Chat("hello  wörld".into())),
        ("%%MONITORON g1", MonitorOn("g1".into())),
        ("AGREE g1", Agree(Some("g1".into()))),
        ("REJECT", Reject(None)),
        ("-7e7f7g", Move { side: Side::Gote, usi: "7e7f7g".into() }),
        ("", KeepAlive),
        ("%%GAME x ?", Unknown("%%GAME x ?".into())),
        ("LOGIN onlyname", Unknown("LOGIN onlyname".into())),
        ("+", Unknown("+".into())),
    ];
    for (line, want) in cases {
        let got = ClientMsg::parse(line);
        assert_eq!(got, want, "{line:?}");
        if !matches!(got, Unknown(_)) {
            assert_eq!(ClientMsg::parse(&got.to_string()), got);
        }
    }
}

/// xorshift64*, enough for generating test cases without a dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn word(&mut self, alphabet: &[u8], max: u64) -> String {
        let n = 1 + self.below(max);
        (0..n).map(|_| alphabet[self.below(alphabet.len() as u64) as usize] as char).collect()
    }
    fn side(&mut self) -> Side {
        if self.below(2) == 0 { Side::Sente } else { Side::Gote }
    }
}

#[test]
fn summary_encode_parse_identity() {
    const NAME: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCXYZ0123456789_-";
    const SFEN: &[u8] = b"abcdefghijklmnopqrstuvwxyzKQ0123456789/+- ";
    let mut rng = Rng(0x9E3779B97F4A7C15);
    for _ in 0..2000 {
        let moves = (0..rng.below(6))
            .map(|_| PlayedMove { side: rng.side(), usi: rng.word(b"0123456789abcdefghijkl+", 10), time: rng.next() >> rng.below(64) })
            .collect();
        let s = GameSummary {
            protocol_version: "1.2".into(),
            protocol_mode: "Server".into(),
            format: format!("Chushogi {}", rng.word(b"0123456789.", 4)),
            rules: rng.word(b"lishogijcsa", 8),
            game_id: rng.word(NAME, 60).replace('-', "+"),
            name_plus: rng.word(NAME, 32),
            name_minus: rng.word(NAME, 32),
            your_turn: rng.side(),
            rematch_on_draw: rng.below(2) == 0,
            to_move: rng.side(),
            max_moves: rng.next() as u32,
            time: TimeConfig {
                time_unit: if rng.below(2) == 0 { TimeUnit::Msec } else { TimeUnit::Sec },
                total_time: rng.next() >> rng.below(64),
                byoyomi: rng.below(100000),
                increment: rng.below(100000),
                least_time_per_move: rng.below(3),
            },
            sfen: rng.word(SFEN, 120).trim().to_string() + "x",
            moves,
        };
        let lines = s.to_lines();
        assert_eq!(GameSummary::parse(&lines).as_ref(), Ok(&s), "{lines:#?}");
        for m in &s.moves {
            assert_eq!(ServerMsg::parse(&m.to_string()), ServerMsg::Move(m.clone()));
        }
    }
}

#[test]
fn summary_rejects_malformed() {
    let block = block_containing("BEGIN Game_Summary");
    let good: Vec<&str> = block.iter().map(|l| wire(l).1).take_while(|l| *l != "AGREE").collect();
    assert!(GameSummary::parse(&good).is_ok());
    for drop in ["Game_ID:", "END Time", "Time_Unit:", "SFEN:"] {
        let bad: Vec<&str> = good.iter().copied().filter(|l| !l.starts_with(drop)).collect();
        assert!(GameSummary::parse(&bad).is_err(), "without {drop}");
    }
    let mut dup = good.clone();
    dup.insert(3, "Rules:jcsa");
    assert!(GameSummary::parse(&dup).is_err());
}
