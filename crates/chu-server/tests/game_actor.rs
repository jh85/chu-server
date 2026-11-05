//! The game actor driven directly over its channels, with paused tokio time
//! so clock tests are exact and instant.

use std::collections::HashMap;
use std::time::Duration;

use chu_proto::{ClientMsg, GameName, GameSummary, Side, TimeUnit};
use chu_rules::{Backend, GameEnd, START_SFEN};
use chu_server::clock::TimeControl;
use chu_server::game::{self, GameInput, GameSetup, GameTx, PlayerSlot};
use chu_server::lobby::LobbyMsg;
use chu_server::store::StoreMsg;
use chu_server::{Conn, Ctl, OutMsg};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::time::{sleep, Instant};

const ID: &str = "chu-test-60-1F+a+b+20261105000000";

struct Harness {
    tx: GameTx,
    out: [UnboundedReceiver<OutMsg>; 2],
    conn_ids: [u64; 2],
    _ctl: [UnboundedReceiver<Ctl>; 2],
    store: UnboundedReceiver<StoreMsg>,
    lobby: UnboundedReceiver<LobbyMsg>,
}

fn idx(s: Side) -> usize {
    if s == Side::Sente { 0 } else { 1 }
}

fn setup(game_name: &str, sfen: &str) -> Harness {
    let name: GameName = game_name.parse().unwrap();
    let mut referee = Backend::Ffi.create().unwrap();
    referee.set_sfen(sfen).unwrap();
    let (c0, o0) = Conn::new(1);
    let (c1, o1) = Conn::new(2);
    let (k0, r0) = unbounded_channel();
    let (k1, r1) = unbounded_channel();
    let (store, store_rx) = unbounded_channel();
    let (lobby, lobby_rx) = unbounded_channel();
    let tx = game::spawn(GameSetup {
        id: ID.into(),
        tc: TimeControl::from_game_name(&name, TimeUnit::Msec),
        name,
        initial_sfen: referee.sfen().unwrap(),
        rated: false,
        players: [
            PlayerSlot { name: "a".into(), conn: Some(c0), ctl: Some(k0) },
            PlayerSlot { name: "b".into(), conn: Some(c1), ctl: Some(k1) },
        ],
        referee,
        store,
        lobby,
    });
    Harness { tx, out: [o0, o1], conn_ids: [1, 2], _ctl: [r0, r1], store: store_rx, lobby: lobby_rx }
}

impl Harness {
    async fn line(&mut self, side: Side) -> String {
        match tokio::time::timeout(Duration::from_secs(3600), self.out[idx(side)].recv()).await {
            Ok(Some(OutMsg::Line(l))) => l,
            other => panic!("{side:?}: expected a line, got {other:?}"),
        }
    }

    async fn summary(&mut self, side: Side) -> GameSummary {
        let mut lines = vec![self.line(side).await];
        assert_eq!(lines[0], "BEGIN Game_Summary");
        while lines.last().unwrap() != "END Game_Summary" {
            lines.push(self.line(side).await);
        }
        GameSummary::parse(&lines).unwrap()
    }

    fn send(&self, side: Side, line: &str) {
        let msg = ClientMsg::parse(line);
        self.tx.send(GameInput::Msg { side, msg, at: Instant::now() }).unwrap();
    }

    async fn start(&mut self) {
        for s in [Side::Sente, Side::Gote] {
            let sum = self.summary(s).await;
            assert_eq!(sum.your_turn, s);
        }
        self.send(Side::Sente, "AGREE");
        self.send(Side::Gote, &format!("AGREE {ID}"));
        for s in [Side::Sente, Side::Gote] {
            assert_eq!(self.line(s).await, format!("START:{ID}"));
        }
    }

    /// Plays a move and checks the echo on both sides; returns the T value.
    async fn play(&mut self, side: Side, usi: &str) -> u64 {
        self.send(side, &format!("{}{usi}", side.sign()));
        let mut t = 0;
        for s in [Side::Sente, Side::Gote] {
            let echo = self.line(s).await;
            let (mv, tv) = echo.split_once(",T").unwrap_or_else(|| panic!("echo {echo:?}"));
            assert_eq!(mv, format!("{}{usi}", side.sign()));
            t = tv.parse().unwrap();
        }
        t
    }

    /// Reads the reason and verdict lines of both players.
    async fn result(&mut self) -> (String, String, String) {
        let r0 = self.line(Side::Sente).await;
        let v0 = self.line(Side::Sente).await;
        let r1 = self.line(Side::Gote).await;
        let v1 = self.line(Side::Gote).await;
        assert_eq!(r0, r1);
        (r0, v0, v1)
    }

    /// The store's Finished message (skipping Started and Move).
    async fn finished(&mut self) -> chu_server::store::FinishedGame {
        loop {
            match self.store.recv().await.unwrap() {
                StoreMsg::Finished(f) => return *f,
                _ => continue,
            }
        }
    }
}

fn r(a: &str, b: &str, c: &str) -> (String, String, String) {
    (a.into(), b.into(), c.into())
}

use Side::{Gote as G, Sente as S};

#[tokio::test(start_paused = true)]
async fn referee_endings() {
    let cases = [
        ("g10k/12/12/12/12/11R/12/12/12/12/12/K11 b - 1", "1f1a", r("#ROYALS_LOST", "#WIN", "#LOSE")),
        ("11k/12/11g/12/12/12/12/12/12/12/12/K10R b - 1", "1l1c", r("#BARE_KING", "#WIN", "#LOSE")),
        ("K11/12/12/12/12/12/12/12/12/12/9ppp/9pkp b - 1", "12a11a", r("#STALEMATE", "#WIN", "#LOSE")),
        ("11k/12/12/12/6g5/6K5/12/12/12/12/12/12 b - 1", "6f6e", r("#INSUFFICIENT", "#DRAW", "#DRAW")),
    ];
    for (sfen, mv, want) in cases {
        let mut h = setup("chu-test-60-1F", sfen);
        h.start().await;
        h.play(S, mv).await;
        assert_eq!(h.result().await, want, "{sfen}");
        let f = h.finished().await;
        assert_eq!(f.moves.len(), 1);
        assert!(matches!(h.lobby.recv().await, Some(LobbyMsg::GameEnded { .. })));
    }
}

#[tokio::test(start_paused = true)]
async fn sennichite() {
    let mut h = setup("chu-test-60-1F", "10gk/12/12/12/12/12/12/12/12/12/12/KG10 b - 1");
    h.start().await;
    for _ in 0..3 {
        for (side, mv) in [(S, "12l12k"), (G, "1a1b"), (S, "12k12l"), (G, "1b1a")] {
            h.play(side, mv).await;
        }
    }
    assert_eq!(h.result().await, r("#SENNICHITE", "#DRAW", "#DRAW"));
    assert_eq!(h.finished().await.moves.len(), 12);
}

#[tokio::test(start_paused = true)]
async fn perpetual_check_loses() {
    // Sente's rook checks on every move; gote's king steps between 1a and 2a.
    let mut h = setup("chu-test-60-1F", "g10k/12/10R1/12/12/12/12/12/12/12/12/K11 b - 1");
    h.start().await;
    for _ in 0..3 {
        for (side, mv) in [(S, "2c1c"), (G, "1a2a"), (S, "1c2c"), (G, "2a1a")] {
            h.play(side, mv).await;
        }
    }
    assert_eq!(h.result().await, r("#OUTE_SENNICHITE", "#LOSE", "#WIN"));
    assert_eq!(h.finished().await.winner, Some(G));
}

#[tokio::test(start_paused = true)]
async fn resign_illegal_wrong_turn_wrong_colour() {
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    h.play(S, "8i8h").await;
    h.send(G, "%TORYO");
    assert_eq!(h.result().await, r("#RESIGN", "#WIN", "#LOSE"));

    for (side, line, want) in [
        (S, "+8i8i", r("#ILLEGAL_MOVE", "#LOSE", "#WIN")),  // not a legal move
        (S, "+8i8h=", r("#ILLEGAL_MOVE", "#LOSE", "#WIN")), // legal move, non-canonical spelling
        (S, "+zz", r("#ILLEGAL_MOVE", "#LOSE", "#WIN")),    // not a move at all
        (G, "-6c5e", r("#ILLEGAL_MOVE", "#WIN", "#LOSE")),  // out of turn
        (S, "-8i8h", r("#ILLEGAL_MOVE", "#LOSE", "#WIN")),  // wrong colour prefix
    ] {
        let mut h = setup("chu-test-60-1F", START_SFEN);
        h.start().await;
        h.send(side, line);
        assert_eq!(h.result().await, want, "{line}");
        assert!(h.finished().await.moves.is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn time_up_on_main_time() {
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    let t0 = Instant::now();
    assert_eq!(h.result().await, r("#TIME_UP", "#LOSE", "#WIN"));
    assert_eq!(t0.elapsed(), Duration::from_secs(60));
}

#[tokio::test(start_paused = true)]
async fn byoyomi_accounting() {
    // 10 s + 5 s byoyomi: moves within 5 s are free; the main clock pays the rest.
    let mut h = setup("chu-test-10-5", START_SFEN);
    let sum = h.summary(S).await;
    assert_eq!((sum.time.total_time, sum.time.byoyomi, sum.time.increment), (10_000, 5_000, 0));
    h.summary(G).await;
    h.send(S, "AGREE");
    h.send(G, "AGREE");
    h.line(S).await;
    h.line(G).await;

    sleep(Duration::from_millis(4_900)).await;
    assert_eq!(h.play(S, "8i8h").await, 4_900); // free: main time stays 10 s
    h.play(G, "6c5e").await;
    sleep(Duration::from_millis(14_900)).await; // 10 s + 5 s allowed
    assert_eq!(h.play(S, "8h8g").await, 14_900); // main time now 0.1 s
    h.play(G, "5e5f").await;
    let t0 = Instant::now();
    assert_eq!(h.result().await, r("#TIME_UP", "#LOSE", "#WIN"));
    assert_eq!(t0.elapsed(), Duration::from_millis(5_100));
}

#[tokio::test(start_paused = true)]
async fn fischer_accounting() {
    // 1 s + 1 s increment.
    let mut h = setup("chu-test-1-1F", START_SFEN);
    h.start().await;
    sleep(Duration::from_millis(900)).await;
    assert_eq!(h.play(S, "8i8h").await, 900); // 0.1 + 1 = 1.1 s left
    h.play(G, "6c5e").await;
    sleep(Duration::from_millis(1_050)).await;
    h.play(S, "8h8g").await; // 0.05 + 1 = 1.05 s left
    h.play(G, "5e5f").await;
    let t0 = Instant::now();
    assert_eq!(h.result().await, r("#TIME_UP", "#LOSE", "#WIN"));
    assert_eq!(t0.elapsed(), Duration::from_millis(1_050));
}

#[tokio::test(start_paused = true)]
async fn late_move_is_time_up_not_applied() {
    let mut h = setup("chu-test-1-1F", START_SFEN);
    h.start().await;
    // A move stamped after the deadline loses even if it reaches the actor first.
    let late = Instant::now() + Duration::from_millis(1_001);
    h.tx.send(GameInput::Msg { side: S, msg: ClientMsg::parse("+8i8h"), at: late }).unwrap();
    assert_eq!(h.result().await, r("#TIME_UP", "#LOSE", "#WIN"));
    assert!(h.finished().await.moves.is_empty());
}

#[tokio::test(start_paused = true)]
async fn reject_and_agree_timeout() {
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.summary(S).await;
    h.summary(G).await;
    h.send(S, "AGREE");
    h.send(G, "REJECT");
    assert_eq!(h.line(S).await, format!("REJECT:{ID} by b"));
    assert_eq!(h.line(G).await, format!("REJECT:{ID} by b"));
    assert!(matches!(h.lobby.recv().await, Some(LobbyMsg::GameEnded { .. })));
    assert!(h.store.try_recv().is_err(), "a cancelled game is not stored");

    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.summary(S).await;
    h.summary(G).await;
    h.send(G, "AGREE");
    let t0 = Instant::now();
    assert_eq!(h.line(S).await, format!("REJECT:{ID} by a"));
    assert_eq!(t0.elapsed(), Duration::from_secs(30));

    // A move before START is out of place; no game is in progress, so it
    // cancels like REJECT.
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.summary(S).await;
    h.summary(G).await;
    h.send(S, "+8i8h");
    assert_eq!(h.line(G).await, format!("REJECT:{ID} by a"));
}

#[tokio::test(start_paused = true)]
async fn thousand_plies_is_max_moves() {
    // A driver with its own referee plays pseudo-random moves that neither end
    // the game nor repeat a position four times.
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    let mut me = Backend::Ffi.create().unwrap();
    let mut seen: HashMap<String, u32> = HashMap::new();
    seen.insert(START_SFEN.rsplit_once(' ').unwrap().0.into(), 1);
    let mut seed = 0x2545F4914F6CDD1Du64;
    for ply in 0..1000 {
        let side = if ply % 2 == 0 { S } else { G };
        let before = me.sfen().unwrap();
        let legal = me.legal().unwrap();
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let start = (seed % legal.len() as u64) as usize;
        let mv = (0..legal.len())
            .map(|k| &legal[(start + k) % legal.len()])
            .find(|mv| {
                me.set_sfen(&before).unwrap();
                me.apply(mv).unwrap();
                let key: String = me.sfen().unwrap().rsplit_once(' ').unwrap().0.into();
                me.status().unwrap().end == GameEnd::Playing && seen.get(&key).copied().unwrap_or(0) < 3
            })
            .expect("a quiet move")
            .clone();
        me.set_sfen(&before).unwrap();
        me.apply(&mv).unwrap();
        *seen.entry(me.sfen().unwrap().rsplit_once(' ').unwrap().0.into()).or_default() += 1;
        h.play(side, &mv).await;
    }
    assert_eq!(h.result().await, r("#MAX_MOVES", "#DRAW", "#DRAW"));
    assert_eq!(h.finished().await.moves.len(), 1000);
}

#[tokio::test(start_paused = true)]
async fn disconnect_and_resume() {
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    h.play(S, "8i8h").await;
    h.play(G, "6c5e").await;
    h.tx.send(GameInput::Disconnected { side: G, conn_id: h.conn_ids[1] }).unwrap();
    // A stale disconnect from an older connection is ignored later on.
    h.play_one_sided(S, "8h8g").await;

    let (conn, out) = Conn::new(3);
    let (ctl, _ctl_rx) = unbounded_channel();
    h.out[1] = out;
    h.tx.send(GameInput::Reattach { side: G, conn, ctl }).unwrap();
    let sum = h.summary(G).await;
    assert_eq!(sum.your_turn, G);
    assert_eq!(sum.to_move, G, "To_Move gives the side to move after the MOVES lines");
    let moves: Vec<_> = sum.moves.iter().map(|m| (m.side, m.usi.as_str())).collect();
    assert_eq!(moves, [(S, "8i8h"), (G, "6c5e"), (S, "8h8g")]);
    h.send(G, "AGREE");
    assert_eq!(h.line(G).await, format!("START:{ID}"));
    h.tx.send(GameInput::Disconnected { side: G, conn_id: 2 }).unwrap(); // stale
    h.play(G, "5e5f").await;
    h.send(S, "%TORYO");
    assert_eq!(h.result().await, r("#RESIGN", "#LOSE", "#WIN"));
}

#[tokio::test(start_paused = true)]
async fn disconnected_player_loses_on_time() {
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    h.play(S, "8i8h").await;
    h.tx.send(GameInput::Disconnected { side: G, conn_id: h.conn_ids[1] }).unwrap();
    let t0 = Instant::now();
    // Sente alone sees the result; gote's clock ran out while away.
    assert_eq!(h.line(S).await, "#TIME_UP");
    assert_eq!(h.line(S).await, "#WIN");
    assert_eq!(t0.elapsed(), Duration::from_secs(60));
    let f = h.finished().await;
    assert_eq!(f.winner, Some(S));
}

#[tokio::test(start_paused = true)]
async fn commands_out_of_place_are_illegal_action() {
    // AGREE (or REJECT) during play.
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    h.play(S, "8i8h").await;
    h.send(S, "AGREE");
    assert_eq!(h.result().await, r("#ILLEGAL_ACTION", "#LOSE", "#WIN"));

    // A move from a resumed player before it answered the summary.
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    h.play(S, "8i8h").await;
    h.tx.send(GameInput::Disconnected { side: G, conn_id: h.conn_ids[1] }).unwrap();
    let (conn, out) = Conn::new(3);
    let (ctl, _ctl_rx) = unbounded_channel();
    h.out[1] = out;
    h.tx.send(GameInput::Reattach { side: G, conn, ctl }).unwrap();
    h.summary(G).await;
    h.send(G, "-6c5e");
    assert_eq!(h.result().await, r("#ILLEGAL_ACTION", "#WIN", "#LOSE"));
}

#[tokio::test(start_paused = true)]
async fn violation_is_illegal_action_and_monitors_see_moves() {
    let mut h = setup("chu-test-60-1F", START_SFEN);
    h.start().await;
    let (mon, mut mon_rx) = Conn::new(9);
    h.tx.send(GameInput::Monitor { conn: mon, on: true }).unwrap();
    h.play(S, "8i8h").await;
    h.tx.send(GameInput::Violation { side: G, conn_id: h.conn_ids[1] }).unwrap();
    assert_eq!(h.result().await, r("#ILLEGAL_ACTION", "#WIN", "#LOSE"));
    let mut seen = Vec::new();
    while let Ok(OutMsg::Line(l)) = mon_rx.try_recv() {
        seen.push(l);
    }
    assert_eq!(
        seen,
        [
            format!("##[MONITORON][{ID}] +OK"),
            format!("##[MONITOR][{ID}] +8i8h,T0"),
            format!("##[MONITOR][{ID}] #ILLEGAL_ACTION"),
        ]
    );
    let f = h.finished().await;
    assert_eq!((f.winner, f.moves.len()), (Some(S), 1));
}

impl Harness {
    /// A move while the opponent is disconnected: only the mover sees the echo.
    async fn play_one_sided(&mut self, side: Side, usi: &str) {
        self.send(side, &format!("{}{usi}", side.sign()));
        let echo = self.line(side).await;
        assert!(echo.starts_with(&format!("{}{usi},T", side.sign())), "{echo}");
    }
}
