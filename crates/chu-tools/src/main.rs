//! chu-tools: command-line tools for the chu-server. `play` is a test client
//! that plays `chushogi-gen go random` over the server protocol;
//! `conformance --client` checks a client against SERVER-PROTOCOL.md §7;
//! `rate` refits the ratings from the games table and compares them with the
//! server's.

mod conformance;
mod rate;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use chu_proto::{ClientMsg, ColorReq, GameSummary, LineBucket, ServerMsg, Side};
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
        #[arg(long, default_value = "test")]
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
    /// Check a client against a scripted server (SERVER-PROTOCOL.md §7).
    Conformance {
        /// Shell command starting the client; `{server}` becomes the scripted
        /// server's address. The client must play at least 3 games.
        #[arg(long)]
        client: String,
        /// Where the client's stderr goes (default: discarded).
        #[arg(long)]
        client_log: Option<PathBuf>,
        /// Seed for the scripted opponent's moves.
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Refit every league's ratings from the games table, print them, and
    /// fail unless they equal the server's `ratings` table.
    Rate {
        #[arg(long, default_value = "sqlite://chu.db")]
        db: String,
        /// Only this league (default: every league with rated games).
        #[arg(long)]
        league: Option<String>,
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

struct Client {
    rd: BufReader<TcpStream>,
    wr: TcpStream,
    /// A random player is far faster than the server's line limit.
    bucket: LineBucket,
}

impl Client {
    fn send(&mut self, line: &str) -> Res<()> {
        std::thread::sleep(self.bucket.wait(Instant::now()));
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
    let mut c = Client { rd: BufReader::new(sock.try_clone()?), wr: sock, bucket: LineBucket::client(Instant::now()) };
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
        Cmd::Rate { db, league } => {
            if !rate::run(&db, league.as_deref())? {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Conformance { client, client_log, seed } => {
            if !conformance::run(&client, client_log.as_deref(), seed)? {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
