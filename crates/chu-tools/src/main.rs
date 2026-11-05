//! chu-tools: command-line tools for the chu-server. Phase 1 has `play`, a
//! test client that plays `chushogi-gen go random` over the server protocol.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use chu_proto::{ClientMsg, ColorReq, GameSummary, ServerMsg, Side};
use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Play games on the server with `chushogi-gen go random`.
    Play {
        #[arg(long, default_value = "127.0.0.1:4081")]
        server: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "random")]
        password: String,
        #[arg(long, default_value = "chu-test-60-1F")]
        game: String,
        /// `+`, `-` or `*`.
        #[arg(long, default_value = "*")]
        color: String,
        #[arg(long, default_value_t = 1)]
        games: u32,
        /// Seed for chushogi-gen's random generator.
        #[arg(long)]
        seed: Option<u64>,
        #[arg(long, default_value = chu_rules::BUILT_CHUSHOGI_GEN)]
        gen: PathBuf,
    },
}

type Res<T> = Result<T, Box<dyn std::error::Error>>;

struct Gen {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Gen {
    fn spawn(path: &PathBuf) -> Res<Gen> {
        let mut child = Command::new(path).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Ok(Gen { _child: child, stdin, stdout })
    }

    fn cmd(&mut self, line: &str) -> Res<String> {
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        let mut out = String::new();
        if self.stdout.read_line(&mut out)? == 0 {
            return Err(format!("chushogi-gen exited on {line:?}").into());
        }
        Ok(out.trim_end().to_owned())
    }
}

/// The server's line bucket (SERVER-PROTOCOL.md §1) mirrored on the sending
/// side with a margin: a random player is far faster than 100 lines/s, so it
/// waits only when the bucket would run dry.
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    const CAP: f64 = chu_proto::LINE_BURST as f64 * 0.9;
    const RATE: f64 = chu_proto::LINES_PER_SEC as f64 * 0.9;

    fn take(&mut self) {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * Self::RATE).min(Self::CAP);
        self.at = now;
        if self.tokens < 1.0 {
            std::thread::sleep(Duration::from_secs_f64((1.0 - self.tokens) / Self::RATE));
            self.tokens = 1.0;
            self.at = Instant::now();
        }
        self.tokens -= 1.0;
    }
}

struct Client {
    rd: BufReader<TcpStream>,
    wr: TcpStream,
    bucket: Bucket,
}

impl Client {
    fn send(&mut self, line: &str) -> Res<()> {
        self.bucket.take();
        writeln!(self.wr, "{line}")?;
        Ok(())
    }
    fn recv(&mut self) -> Res<String> {
        let mut line = String::new();
        if self.rd.read_line(&mut line)? == 0 {
            return Err("server closed the connection".into());
        }
        Ok(line.trim_end_matches(['\n', '\r']).to_owned())
    }
}

#[allow(clippy::too_many_arguments)]
fn play(server: &str, name: &str, password: &str, game: &str, color: &str, games: u32, seed: Option<u64>, gen: &PathBuf) -> Res<()> {
    let color = match color {
        "+" => ColorReq::Sente,
        "-" => ColorReq::Gote,
        "*" => ColorReq::Any,
        c => return Err(format!("bad colour {c}").into()),
    };
    let mut engine = Gen::spawn(gen)?;
    if let Some(s) = seed {
        engine.cmd(&format!("seed {s}"))?;
    }
    let sock = TcpStream::connect(server)?;
    sock.set_nodelay(true)?;
    let bucket = Bucket { tokens: Bucket::CAP, at: Instant::now() };
    let mut c = Client { rd: BufReader::new(sock.try_clone()?), wr: sock, bucket };
    c.send(&ClientMsg::Login { name: name.into(), password: password.into(), x1: true }.to_string())?;
    match ServerMsg::parse(&c.recv()?) {
        ServerMsg::LoginOk(_) => {}
        other => return Err(format!("login failed: {other}").into()),
    }

    let mut played = 0;
    while played < games {
        c.send(&ClientMsg::Game { game_name: game.into(), color, sfen: None }.to_string())?;
        // Summary, AGREE, START.
        let mut lines = Vec::new();
        loop {
            let l = c.recv()?;
            match ServerMsg::parse(&l) {
                ServerMsg::BeginSummary => lines = vec![l],
                ServerMsg::Lobby(l) if l.starts_with("##[ERROR]") => return Err(l.into()),
                _ if !lines.is_empty() => {
                    let end = l == "END Game_Summary";
                    lines.push(l);
                    if end {
                        break;
                    }
                }
                _ => {}
            }
        }
        let summary = GameSummary::parse(&lines)?;
        c.send("AGREE")?;
        match ServerMsg::parse(&c.recv()?) {
            ServerMsg::Start(_) => {}
            ServerMsg::Reject { .. } => continue,
            other => return Err(format!("expected START, got {other}").into()),
        }
        let me = summary.your_turn;
        let mut moves: Vec<String> = summary.moves.iter().map(|m| m.usi.clone()).collect();
        let mut to_move = summary.to_move;
        let mut sent = false;
        let (reason, verdict) = loop {
            if to_move == me && !sent {
                let mut pos = format!("position sfen {}", summary.sfen);
                if !moves.is_empty() {
                    pos.push_str(" moves ");
                    pos.push_str(&moves.join(" "));
                }
                engine.cmd(&pos)?;
                let best = engine.cmd("go random")?;
                let mv = best.strip_prefix("bestmove ").ok_or("no bestmove")?;
                if mv == "resign" {
                    c.send("%TORYO")?;
                } else {
                    c.send(&format!("{}{mv}", me.sign()))?;
                }
                sent = true;
            }
            match ServerMsg::parse(&c.recv()?) {
                ServerMsg::Move(m) => {
                    moves.push(m.usi);
                    to_move = m.side.opponent();
                    sent = false;
                }
                ServerMsg::Reason(r) => match ServerMsg::parse(&c.recv()?) {
                    ServerMsg::Verdict(v) => break (r, v),
                    other => return Err(format!("expected a verdict, got {other}").into()),
                },
                _ => {}
            }
        };
        played += 1;
        println!(
            "{name}: game {played}/{games} {} as {}: {} {} after {} plies",
            summary.game_id,
            if me == Side::Sente { "sente" } else { "gote" },
            reason.as_str(),
            verdict.as_str(),
            moves.len()
        );
    }
    c.send("LOGOUT")?;
    let _ = c.recv();
    Ok(())
}

fn main() -> Res<()> {
    match Cli::parse().cmd {
        Cmd::Play { server, name, password, game, color, games, seed, gen } => {
            play(&server, &name, &password, &game, &color, games, seed, &gen)
        }
    }
}
