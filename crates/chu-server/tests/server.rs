//! Sessions, lobby, game and store together, over real TCP, with a SQLite
//! file and the record tree in a temporary directory.

use std::path::PathBuf;
use std::time::Duration;

use chu_rules::Backend;
use chu_server::league::LeagueConfig;
use chu_server::lobby::LobbyMsg;
use chu_server::{store, Config, Server};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};

const SFEN: &str = "g10k/12/12/12/12/11R/12/12/12/12/12/K11 b - 1";

struct TestServer {
    addr: std::net::SocketAddr,
    server: Server,
    dir: PathBuf,
}

async fn server(test: &str) -> TestServer {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("server-{test}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = Config {
        referee: Backend::Ffi,
        db_url: format!("sqlite://{}", dir.join("chu.db").display()),
        records_dir: dir.join("records"),
        // Daily, so the scheduler stays out of the tests; they send rounds.
        leagues: vec![LeagueConfig {
            name: "chu-floodgate-60-1F".into(),
            every_min: 1440,
            offset_min: 0,
            cutoff_s: 5,
            rules: "lishogi".into(),
            rated: true,
        }],
    };
    let server = Server::start(cfg).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let s = server.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            sock.set_nodelay(true).unwrap();
            s.serve(sock);
        }
    });
    TestServer { addr, server, dir }
}

struct Client {
    rd: BufReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
}

impl Client {
    async fn connect(ts: &TestServer) -> Client {
        let (rd, wr) = TcpStream::connect(ts.addr).await.unwrap().into_split();
        Client { rd: BufReader::new(rd), wr }
    }

    async fn login(ts: &TestServer, name: &str) -> Client {
        let mut c = Client::connect(ts).await;
        c.send(&format!("LOGIN {name} pw x1")).await;
        assert_eq!(c.recv().await, format!("LOGIN:{name} OK"));
        c
    }

    async fn send(&mut self, line: &str) {
        self.wr.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    }

    /// Next line, or "<EOF>" if the server closed the connection.
    async fn recv(&mut self) -> String {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(10), self.rd.read_line(&mut line))
            .await
            .expect("timed out waiting for the server")
            .unwrap_or(0);
        if n == 0 {
            return "<EOF>".into();
        }
        line.trim_end().to_owned()
    }

    /// Reads up to and including a line starting with `prefix`.
    async fn until(&mut self, prefix: &str) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let l = self.recv().await;
            let done = l.starts_with(prefix) || l == "<EOF>";
            lines.push(l);
            if done {
                return lines;
            }
        }
    }
}

#[tokio::test]
async fn login_rules_and_second_login() {
    let ts = server("login").await;
    let mut a = Client::login(&ts, "alice").await;
    for bad in ["LOGIN alice wrong x1", "LOGIN bob pw", "LOGIN b@d pw x1", "LOGIN aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa pw x1"] {
        let mut c = Client::connect(&ts).await;
        c.send(bad).await;
        assert_eq!(c.recv().await, "LOGIN:incorrect", "{bad}");
    }
    // The same name again closes the first connection.
    let mut a2 = Client::login(&ts, "alice").await;
    assert_eq!(a.recv().await, "<EOF>");
    a2.send("%%WHO").await;
    assert_eq!(a2.until("##[WHO] +OK").await, ["##[WHO] alice connected", "##[WHO] +OK"]);
    a2.send("LOGOUT").await;
    assert_eq!(a2.recv().await, "LOGOUT:completed");
    assert_eq!(a2.recv().await, "<EOF>");
}

#[tokio::test]
async fn lobby_errors() {
    let ts = server("lobby").await;
    let mut a = Client::login(&ts, "alice").await;
    for (line, err) in [
        ("%%GAME chu-floodgate-600-10F *", "##[ERROR] no floodgate league chu-floodgate-600-10F"),
        ("%%GAME chu-floodgate-60-1F +", "##[ERROR] floodgate leagues take * only"),
        ("%%GAME chu-blitz-60-1F + 11k/12/12/12/12/12/12/12/12/12/12/K11 b - 1", "##[ERROR] a start SFEN is only allowed in chu-test leagues"),
        ("%%GAME chu-blitz-060-1F +", "##[ERROR] bad game name chu-blitz-060-1F"),
        ("%%SHOW nosuchgame", "##[ERROR] no such game nosuchgame"),
    ] {
        a.send(line).await;
        assert_eq!(a.recv().await, err);
    }
}

#[tokio::test]
async fn full_game_from_sfen_with_records() {
    let ts = server("game").await;
    let mut a = Client::login(&ts, "alice").await;
    let mut b = Client::login(&ts, "bob").await;
    a.send(&format!("%%GAME chu-test-60-1F + {SFEN}")).await;
    // Same game name, different SFEN: no match.
    b.send("%%GAME chu-test-60-1F -").await;
    b.send("%%WHO").await;
    let who = b.until("##[WHO] +OK").await;
    assert_eq!(who[0], "##[WHO] alice waiting chu-test-60-1F +");
    b.send(&format!("%%GAME chu-test-60-1F - {SFEN}")).await;

    let sa = a.until("END Game_Summary").await;
    let sb = b.until("END Game_Summary").await;
    let id = sa.iter().find_map(|l| l.strip_prefix("Game_ID:")).unwrap().to_owned();
    assert!(id.starts_with("chu-test-60-1F+alice+bob+"));
    assert!(sa.contains(&"Your_Turn:+".to_owned()) && sb.contains(&"Your_Turn:-".to_owned()));
    assert!(sa.contains(&format!("SFEN:{SFEN}")));
    a.send("AGREE").await;
    b.send("AGREE").await;
    assert_eq!(a.recv().await, format!("START:{id}"));
    assert_eq!(b.recv().await, format!("START:{id}"));

    let mut m = Client::login(&ts, "watcher").await;
    m.send("%%LIST").await;
    assert_eq!(m.until("##[LIST] +OK").await, [format!("##[LIST] {id}"), "##[LIST] +OK".into()]);
    m.send(&format!("%%MONITORON {id}")).await;
    assert_eq!(m.recv().await, format!("##[MONITORON][{id}] +OK"));

    a.send("+1f1a").await;
    let echo = a.recv().await;
    assert!(echo.starts_with("+1f1a,T"), "{echo}");
    assert_eq!(b.recv().await, echo);
    assert_eq!((a.recv().await, a.recv().await), ("#ROYALS_LOST".into(), "#WIN".into()));
    assert_eq!((b.recv().await, b.recv().await), ("#ROYALS_LOST".into(), "#LOSE".into()));
    assert_eq!(m.recv().await, format!("##[MONITOR][{id}] {echo}"));
    assert_eq!(m.recv().await, format!("##[MONITOR][{id}] #ROYALS_LOST"));

    // Back in the lobby: a new challenge pairs again.
    a.send("%%GAME chu-test-60-1F *").await;
    b.send("%%GAME chu-test-60-1F *").await;
    let id2 = a.until("Game_ID:").await.pop().unwrap();
    assert_ne!(id2, format!("Game_ID:{id}"));

    store::sync(&ts.server.store).await;
    let pool = store::open(&format!("sqlite://{}", ts.dir.join("chu.db").display())).await.unwrap();
    let row: (String, String, i64, String, String) =
        sqlx::query_as("SELECT result, reason, ply_count, initial_sfen, record_path FROM games WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((row.0.as_str(), row.1.as_str(), row.2, row.3.as_str()), ("+", "#ROYALS_LOST", 1, SFEN));
    let rec = PathBuf::from(&row.4);
    assert!(rec.starts_with(ts.dir.join("records/chu-test-60-1F")));
    assert_eq!(std::fs::read_to_string(rec.join("moves.usi")).unwrap(), "1f1a\n");
    assert_eq!(std::fs::read_to_string(rec.join("positions.sfen")).unwrap().lines().count(), 2);
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(rec.join("record.json")).unwrap()).unwrap();
    assert_eq!(json["status"], "royalsLost");
    assert_eq!(json["winner"], "sente");
    assert_eq!(json["end_reason"], "royalslost");
    assert_eq!(json["rated"], false);
    let csa = std::fs::read_to_string(rec.join("game.csa")).unwrap();
    assert!(csa.contains(&echo) && csa.contains("#ROYALS_LOST") && !csa.contains("Your_Turn"));
}

#[tokio::test]
async fn reconnect_resumes_with_moves() {
    let ts = server("resume").await;
    let mut a = Client::login(&ts, "alice").await;
    let mut b = Client::login(&ts, "bob").await;
    a.send("%%GAME chu-test-600-10F +").await;
    b.send("%%GAME chu-test-600-10F -").await;
    a.until("END Game_Summary").await;
    b.until("END Game_Summary").await;
    a.send("AGREE").await;
    b.send("AGREE").await;
    a.recv().await;
    b.recv().await;
    a.send("+8i8h").await;
    a.recv().await;
    b.recv().await;
    drop(b);

    let mut b = Client::login(&ts, "bob").await;
    b.send("%%GAME chu-test-600-10F -").await;
    let summary = b.until("END Game_Summary").await;
    let moves: Vec<&String> = summary.iter().filter(|l| l.starts_with("MOVES:")).collect();
    assert_eq!(moves.len(), 1);
    assert!(moves[0].starts_with("MOVES:+8i8h,T"));
    assert!(summary.contains(&"To_Move:-".to_owned()));
    b.send("AGREE").await;
    assert!(b.recv().await.starts_with("START:"));
    b.send("-6c5e").await;
    assert!(b.recv().await.starts_with("-6c5e,T"));
    assert!(a.recv().await.starts_with("-6c5e,T"));
}

#[tokio::test]
async fn long_line_closes_connection() {
    let ts = server("longline").await;
    let mut a = Client::login(&ts, "alice").await;
    a.send(&"x".repeat(2000)).await;
    assert_eq!(a.recv().await, "<EOF>");
}

/// Two logged-in players in a started chu-test-600-10F game.
async fn started(ts: &TestServer) -> (Client, Client) {
    let mut a = Client::login(ts, "alice").await;
    let mut b = Client::login(ts, "bob").await;
    a.send("%%GAME chu-test-600-10F +").await;
    b.send("%%GAME chu-test-600-10F -").await;
    a.until("END Game_Summary").await;
    b.until("END Game_Summary").await;
    a.send("AGREE").await;
    b.send("AGREE").await;
    assert!(a.recv().await.starts_with("START:"));
    assert!(b.recv().await.starts_with("START:"));
    (a, b)
}

#[tokio::test]
async fn rate_limit_is_a_token_bucket() {
    let ts = server("bucket").await;
    let (mut a, mut b) = started(&ts).await;
    // A burst within the bucket is fine...
    a.wr.write_all("\n".repeat(450).as_bytes()).await.unwrap();
    a.send("+8i8h").await;
    assert!(a.recv().await.starts_with("+8i8h,T"));
    assert!(b.recv().await.starts_with("+8i8h,T"));
    b.send("-6c5e").await;
    a.recv().await;
    b.recv().await;
    // ...and it refills at 100 lines/s: alice has about 49 lines left, then
    // 150 more after 1.5 s.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    a.wr.write_all("\n".repeat(180).as_bytes()).await.unwrap();
    a.send("+8h8g").await;
    assert!(a.recv().await.starts_with("+8h8g,T"));
    b.recv().await;
    // Emptying it in a game loses on #ILLEGAL_ACTION, then the connection closes.
    a.wr.write_all("\n".repeat(600).as_bytes()).await.unwrap();
    assert_eq!(a.until("<EOF>").await, ["#ILLEGAL_ACTION", "#LOSE", "<EOF>"]);
    assert_eq!((b.recv().await, b.recv().await), ("#ILLEGAL_ACTION".into(), "#WIN".into()));
}

#[tokio::test]
async fn long_line_in_game_is_illegal_action() {
    let ts = server("longline-game").await;
    let (mut a, mut b) = started(&ts).await;
    b.send(&"x".repeat(2000)).await;
    assert_eq!(b.until("<EOF>").await, ["#ILLEGAL_ACTION", "#LOSE", "<EOF>"]);
    assert_eq!((a.recv().await, a.recv().await), ("#ILLEGAL_ACTION".into(), "#WIN".into()));
}

/// Reads a client's Game_Summary (part of it may be in `seen` already) and
/// answers AGREE; returns (game id, sente?).
async fn agree(c: &mut Client, mut lines: Vec<String>) -> (String, bool) {
    if let Some(i) = lines.iter().position(|l| l == "BEGIN Game_Summary") {
        lines.drain(..i);
    } else {
        lines.clear();
    }
    if !lines.iter().any(|l| l == "END Game_Summary") {
        lines.extend(c.until("END Game_Summary").await);
    }
    let field = |k: &str| lines.iter().find_map(|l| l.strip_prefix(k)).unwrap().to_owned();
    c.send("AGREE").await;
    (field("Game_ID:"), field("Your_Turn:") == "+")
}

#[tokio::test]
async fn floodgate_round_pairs_the_pool_and_rates_the_games() {
    let ts = server("floodgate").await;
    let names = ["fa", "fb", "fc", "fd", "fe"];
    let mut cs = Vec::new();
    for n in names {
        let mut c = Client::login(&ts, n).await;
        c.send("%%GAME chu-floodgate-60-1F *").await;
        cs.push(c);
    }
    // Syncs with the lobby: every %%GAME before has been handled.
    cs[0].send("%%WHO").await;
    let who = cs[0].until("##[WHO] +OK").await;
    assert_eq!(who.iter().filter(|l| l.ends_with("waiting chu-floodgate-60-1F *")).count(), 5, "{who:?}");

    // Queued less than the cutoff (5 s) before the round: nobody plays.
    let league = "chu-floodgate-60-1F".to_owned();
    let now = std::time::SystemTime::now();
    let cutoff = |t: std::time::SystemTime| t - Duration::from_secs(5);
    ts.server.lobby.send(LobbyMsg::Round { league: league.clone(), scheduled_at: now, cutoff: cutoff(now) }).unwrap();
    // Queued long enough: four play, one sits out.
    let later = now + Duration::from_secs(10);
    ts.server.lobby.send(LobbyMsg::Round { league, scheduled_at: later, cutoff: cutoff(later) }).unwrap();

    let mut playing = Vec::new();
    for (i, c) in cs.iter_mut().enumerate() {
        c.send("%%WHO").await;
        let who = c.until("##[WHO] +OK").await;
        let me = who.iter().find(|l| l.starts_with(&format!("##[WHO] {} ", names[i]))).unwrap();
        if me.contains(" playing ") {
            playing.push((i, who));
        }
    }
    assert_eq!(playing.len(), 4);
    // Every sente resigns at once.
    let mut sides = Vec::new();
    for (i, seen) in playing {
        let (_, sente) = agree(&mut cs[i], seen).await;
        sides.push((i, sente));
    }
    for &(i, sente) in &sides {
        let c = &mut cs[i];
        c.until("START:").await;
        if sente {
            c.send("%TORYO").await;
        }
    }
    for &(i, sente) in &sides {
        let lines = cs[i].until("#RESIGN").await;
        assert_eq!(lines.last().unwrap(), "#RESIGN");
        assert_eq!(cs[i].recv().await, if sente { "#LOSE" } else { "#WIN" });
    }
    let playing: Vec<usize> = sides.iter().map(|s| s.0).collect();
    store::sync(&ts.server.store).await;

    let pool = store::open(&format!("sqlite://{}", ts.dir.join("chu.db").display())).await.unwrap();
    let rounds: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT pool_json, paired_json, sat_out FROM rounds ORDER BY scheduled_at").fetch_all(&pool).await.unwrap();
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0].0, "[]");
    let pairs: Vec<[String; 3]> = serde_json::from_str(&rounds[1].1).unwrap();
    assert_eq!(pairs.len(), 2);
    let sat_out = rounds[1].2.clone().unwrap();
    assert!(!playing.iter().any(|&i| names[i] == sat_out));

    let ratings: Vec<(String, f64, i64)> =
        sqlx::query_as("SELECT player, rating, games FROM ratings WHERE league = 'chu-floodgate-60-1F' ORDER BY player")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(ratings.len(), 4);
    assert!(ratings.iter().all(|r| r.2 == 1));
    let mean = ratings.iter().map(|r| r.1).sum::<f64>() / 4.0;
    assert!((mean - 1500.0).abs() < 1e-9);
    for [sente, gote, _] in &pairs {
        let r = |n: &str| ratings.iter().find(|x| x.0 == n).unwrap().1;
        assert!(r(gote) > r(sente));
    }
    let (history,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM rating_history").fetch_one(&pool).await.unwrap();
    assert_eq!(history, 2 + 4, "a refit after each game: 2 players, then 4");
}
