//! Two bridges driving `chushogi-gen` play through an in-process server; one
//! of them reaches it through a proxy that drops the connection once, in the
//! middle of the first game, and must reconnect and resume.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chu_rules::Backend;
use chu_server::{Config, Server};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

async fn start_server(dir: &std::path::Path) -> std::net::SocketAddr {
    let cfg = Config {
        referee: Backend::Ffi,
        db_url: format!("sqlite://{}", dir.join("chu.db").display()),
        records_dir: dir.join("records"),
        leagues: vec![],
    };
    let server = Server::start(cfg).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            sock.set_nodelay(true).unwrap();
            server.serve(sock);
        }
    });
    addr
}

/// Forwards to `upstream`; the first connection is cut after `cut_after`
/// lines from the server.
async fn start_proxy(upstream: std::net::SocketAddr, cut_after: usize) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cut_done = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        loop {
            let (client, _) = listener.accept().await.unwrap();
            let server = TcpStream::connect(upstream).await.unwrap();
            client.set_nodelay(true).unwrap();
            server.set_nodelay(true).unwrap();
            let cut = !cut_done.swap(true, Ordering::SeqCst);
            tokio::spawn(async move {
                let (mut crd, mut cwr) = client.into_split();
                let (srd, mut swr) = server.into_split();
                let up = tokio::spawn(async move { tokio::io::copy(&mut crd, &mut swr).await });
                let mut lines = BufReader::new(srd).lines();
                let mut n = 0;
                while let Ok(Some(l)) = lines.next_line().await {
                    if cwr.write_all(format!("{l}\n").as_bytes()).await.is_err() {
                        break;
                    }
                    n += 1;
                    if cut && n == cut_after {
                        break;
                    }
                }
                up.abort();
            });
        }
    });
    addr
}

fn bridge(server: std::net::SocketAddr, name: &str, color: &str, seed: u32) -> Command {
    let mut c = bridge_for(server, name, color, "chu-test-60-1F", 2);
    c.args(["--engine", chu_rules::BUILT_CHUSHOGI_GEN, "--usi-option", &format!("Seed={seed}")]);
    c
}

fn bridge_for(server: std::net::SocketAddr, name: &str, color: &str, game: &str, games: u32) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_chu-bridge"));
    c.args(["--server", &server.to_string(), "--name", name, "--password", "pw"])
        .args(["--game", game, "--color", color, "--games", &games.to_string(), "--log-format", "json"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn bridges_play_and_one_resumes_after_a_drop() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bridge-resume");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let addr = start_server(&dir).await;
    // 22 lines of summary and START, then a few echoes.
    let proxy = start_proxy(addr, 30).await;

    let a = bridge(proxy, "bridge_a", "+", 1).spawn().unwrap();
    let b = bridge(addr, "bridge_b", "-", 2).spawn().unwrap();
    let run = async { tokio::join!(a.wait_with_output(), b.wait_with_output()) };
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(60), run).await.expect("bridges finish");
    let (a, b) = (a.unwrap(), b.unwrap());
    let (la, lb) = (String::from_utf8_lossy(&a.stderr), String::from_utf8_lossy(&b.stderr));
    if std::env::var_os("SHOW_LOGS").is_some() {
        eprintln!("{la}\n{lb}");
    }
    assert!(a.status.success(), "{la}");
    assert!(b.status.success(), "{lb}");
    assert!(la.contains("server connection lost"), "{la}");
    assert!(la.contains(r#""resumed":true"#), "{la}");

    // Both bridges saw the same two results, neither a time loss.
    let results = |log: &str| -> Vec<(String, String)> {
        log.lines()
            .filter(|l| l.contains(r#""message":"game over""#))
            .map(|l| {
                let field = |k: &str| l.split(&format!(r#""{k}":""#)).nth(1).unwrap().split('"').next().unwrap().to_owned();
                (field("game"), field("reason"))
            })
            .collect()
    };
    let (ra, rb) = (results(&la), results(&lb));
    assert_eq!(ra.len(), 2, "{la}");
    assert_eq!(ra, rb);
    assert!(ra.iter().all(|(_, r)| r != "#TIME_UP" && r != "#ILLEGAL_MOVE" && r != "#ILLEGAL_ACTION"), "{ra:?}");
}

/// Never answers `go` until `stop`, then resigns.
const STALLING_ENGINE: &str = r#"
while read -r cmd rest; do
  case "$cmd" in
    usi) echo "id name stall"; echo usiok ;;
    isready) echo readyok ;;
    go) id=$(echo "$rest" | sed -n 's/.*id \([0-9]*\).*/\1/p') ;;
    stop) echo "bestmove resign id $id" ;;
    quit) exit 0 ;;
  esac
done
"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_hung_engine_is_stopped_before_the_server_limit() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bridge-stall");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let addr = start_server(&dir).await;
    // 2 s + 1 s byoyomi: the server allows 3 s for the first move.
    let game = "chu-test-2-1";
    let mut stall = bridge_for(addr, "stall", "+", game, 1);
    stall.args(["--engine", "sh", "--engine-arg", "-c", "--engine-arg", STALLING_ENGINE]);
    let mut random = bridge_for(addr, "random", "-", game, 1);
    random.args(["--engine", chu_rules::BUILT_CHUSHOGI_GEN]);
    let (s, r) = (stall.spawn().unwrap(), random.spawn().unwrap());
    let run = async { tokio::join!(s.wait_with_output(), r.wait_with_output()) };
    let (s, r) = tokio::time::timeout(std::time::Duration::from_secs(30), run).await.expect("bridges finish");
    let (s, r) = (s.unwrap(), r.unwrap());
    let (ls, lr) = (String::from_utf8_lossy(&s.stderr), String::from_utf8_lossy(&r.stderr));
    assert!(s.status.success() && r.status.success(), "{ls}\n{lr}");
    assert!(ls.contains("no bestmove near the time limit; stop"), "{ls}");
    assert!(ls.contains(r##""reason":"#RESIGN","verdict":"#LOSE""##), "{ls}");
    assert!(lr.contains(r##""reason":"#RESIGN","verdict":"#WIN""##), "{lr}");
}
