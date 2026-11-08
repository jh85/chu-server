//! `chu-tools conformance --client <cmd>` (SERVER-PROTOCOL.md §7): a scripted
//! server plays three games against a client and prints each check as PASS
//! or FAIL.
//!
//! - Game A: the opponent resigns after the client's fourth move (#RESIGN).
//! - Game B: on the client's turn after its third move the connection is
//!   dropped; the client must log in again, ask for the same game, take the
//!   resumed summary (`MOVES:`, `Remaining±` at the start of the turn and
//!   `Turn_Used`, at least a second of it), and go on with a legal move within
//!   the turn's time. After two more client moves the game ends in #CHUDAN.
//! - Game C: the client must queue again after #CHUDAN; the opponent resigns
//!   after the client's second move.
//!
//! Every client move must be legal (chu-rules), in turn, with its colour and
//! within its time by the scripted server's clock. The scripted server takes
//! any well-formed game name as its one league and plays it at 60 s + 1 s
//! Fischer, lishogi rules (the client learns that from the Game_Summary).
//! The client runs as `sh -c <cmd>` with `{server}` replaced by the scripted
//! server's address (also in `CHU_SERVER`) and must play at least 3 games.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chu_proto::clock::TimeControl;
use chu_proto::{ClientMsg, ColorReq, GameSummary, PlayedMove, ResumedClock, Side, TimeUnit, MAX_MOVES};
use chu_rules::{Backend, GameEnd, Referee, START_SFEN};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const OPPONENT: &str = "scripted";
const AGREE_WITHIN: Duration = Duration::from_secs(30);
const LINE_WITHIN: Duration = Duration::from_secs(30);

struct Report {
    failed: u32,
}

impl Report {
    fn check(&mut self, name: &str, pass: bool, detail: impl AsRef<str>) -> bool {
        let detail = detail.as_ref();
        let sep = if detail.is_empty() { "" } else { ": " };
        println!("{} {name}{sep}{detail}", if pass { "PASS" } else { "FAIL" });
        if !pass {
            self.failed += 1;
        }
        pass
    }
}

struct Conn {
    rd: BufReader<TcpStream>,
    wr: TcpStream,
}

impl Conn {
    fn accept(listener: &TcpListener, within: Duration) -> Res<Conn> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + within;
        loop {
            match listener.accept() {
                Ok((sock, _)) => {
                    sock.set_nonblocking(false)?;
                    sock.set_nodelay(true)?;
                    return Ok(Conn { rd: BufReader::new(sock.try_clone()?), wr: sock });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Err("the client did not connect".into()),
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn send(&mut self, line: &str) -> Res<()> {
        writeln!(self.wr, "{line}")?;
        Ok(())
    }

    /// The next non-empty line within `within`; None if the client closed.
    fn recv(&mut self, within: Duration) -> Res<Option<String>> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("no line from the client within {within:?}").into());
            }
            self.rd.get_ref().set_read_timeout(Some(left))?;
            let mut line = String::new();
            match self.rd.read_line(&mut line) {
                Ok(0) => return Ok(None),
                Ok(_) if line.trim().is_empty() => {}
                Ok(_) => return Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned())),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// xorshift64*, for the opponent's moves.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n as u64) as usize
    }
}

/// One scripted game: the position, the moves and both clocks.
struct Game {
    id: String,
    client: Side,
    tc: TimeControl,
    referee: Box<dyn Referee>,
    moves: Vec<PlayedMove>,
    rem: [u64; 2],
    to_move: Side,
    turn_start: Instant,
    client_moves: u32,
}

fn idx(s: Side) -> usize {
    match s {
        Side::Sente => 0,
        Side::Gote => 1,
    }
}

impl Game {
    fn new(id: String, client: Side, tc: TimeControl) -> Res<Game> {
        let mut referee = Backend::Ffi.create()?;
        referee.set_sfen(START_SFEN)?;
        let rem = [tc.total_ms; 2];
        Ok(Game { id, client, tc, referee, moves: Vec::new(), rem, to_move: Side::Sente, turn_start: Instant::now(), client_moves: 0 })
    }

    fn summary(&self, client_name: &str, resumed: bool) -> GameSummary {
        let names = |s: Side| if s == self.client { client_name.to_owned() } else { OPPONENT.to_owned() };
        let mut time = self.tc.summary();
        if resumed {
            // Main time at the start of the turn, and the turn so far (§4).
            let used = self.turn_start.elapsed().as_millis() as u64;
            time.resumed = Some(ResumedClock { remaining: self.rem.map(|ms| self.tc.units(ms)), turn_used: self.tc.units(used) });
        }
        GameSummary {
            protocol_version: "1.2".into(),
            protocol_mode: "Server".into(),
            format: "Chushogi 1.0".into(),
            rules: "lishogi".into(),
            game_id: self.id.clone(),
            name_plus: names(Side::Sente),
            name_minus: names(Side::Gote),
            your_turn: self.client,
            rematch_on_draw: false,
            to_move: self.to_move,
            max_moves: MAX_MOVES,
            time,
            sfen: START_SFEN.into(),
            moves: if resumed { self.moves.clone() } else { Vec::new() },
        }
    }

    /// Applies a move by the side to move and returns its echo.
    fn play(&mut self, usi: &str) -> Res<String> {
        let elapsed = self.turn_start.elapsed().as_millis() as u64;
        self.referee.apply(usi)?;
        let charged = self.tc.charged_ms(elapsed);
        let s = self.to_move;
        self.rem[idx(s)] = self.tc.after_move(self.rem[idx(s)], charged);
        let m = PlayedMove { side: s, usi: usi.to_owned(), time: self.tc.units(charged) };
        let echo = m.to_string();
        self.moves.push(m);
        self.to_move = s.opponent();
        self.turn_start = Instant::now();
        Ok(echo)
    }

    /// A random legal move that does not end the game.
    fn opponent_move(&mut self, rng: &mut Rng) -> Res<String> {
        let before = self.referee.sfen()?;
        let mut legal = self.referee.legal()?;
        while !legal.is_empty() {
            let mv = legal.swap_remove(rng.below(legal.len()));
            self.referee.apply(&mv)?;
            let quiet = self.referee.status()?.end == GameEnd::Playing;
            self.referee.set_sfen(&before)?;
            if quiet {
                return Ok(mv);
            }
        }
        Err("the scripted opponent has no quiet move".into())
    }
}

struct Script<'a> {
    rep: &'a mut Report,
    rng: Rng,
    name: String,
    game_name: String,
    color: ColorReq,
    games: u32,
    moves_checked: u32,
}

impl Script<'_> {
    fn login(&mut self, c: &mut Conn, check: &str) -> Res<()> {
        let line = c.recv(LINE_WITHIN)?.ok_or("the client closed before LOGIN")?;
        let ok = match ClientMsg::parse(&line) {
            ClientMsg::Login { name, x1: true, .. } if chu_proto::valid_name(&name) => {
                c.send(&format!("LOGIN:{name} OK"))?;
                self.name = name;
                true
            }
            _ => false,
        };
        self.rep.check(check, ok, &line);
        if ok {
            Ok(())
        } else {
            Err("bad LOGIN".into())
        }
    }

    /// Waits for `%%GAME`; the first one fixes the game name and colour.
    fn queue(&mut self, c: &mut Conn, check: &str) -> Res<()> {
        let line = c.recv(LINE_WITHIN)?.ok_or("the client closed instead of queueing")?;
        let ok = match ClientMsg::parse(&line) {
            ClientMsg::Game { game_name, color, sfen: None } if chu_proto::valid_name(&game_name) => {
                if self.game_name.is_empty() {
                    self.game_name = game_name;
                    self.color = color;
                    true
                } else {
                    game_name == self.game_name
                }
            }
            _ => false,
        };
        self.rep.check(check, ok, &line);
        if ok {
            Ok(())
        } else {
            Err("bad %%GAME".into())
        }
    }

    /// Sends a summary and waits for AGREE, then START.
    fn start(&mut self, c: &mut Conn, g: &Game, resumed: bool, check: &str) -> Res<()> {
        for line in g.summary(&self.name, resumed).to_lines() {
            c.send(&line)?;
        }
        let t0 = Instant::now();
        let line = c.recv(AGREE_WITHIN)?.ok_or("the client closed instead of AGREE")?;
        let ok = matches!(ClientMsg::parse(&line), ClientMsg::Agree(ref id) if id.as_ref().is_none_or(|id| *id == g.id));
        self.rep.check(check, ok, format!("{line} after {:?}", t0.elapsed()));
        if !ok {
            return Err("no AGREE".into());
        }
        c.send(&format!("START:{}", g.id))?;
        Ok(())
    }

    fn new_game(&mut self, n: u32) -> Res<Game> {
        let client = match self.color {
            ColorReq::Sente => Side::Sente,
            ColorReq::Gote => Side::Gote,
            ColorReq::Any if n % 2 == 1 => Side::Gote,
            ColorReq::Any => Side::Sente,
        };
        let names = if client == Side::Sente { [self.name.as_str(), OPPONENT] } else { [OPPONENT, self.name.as_str()] };
        let suffix = if n == 0 { String::new() } else { format!("-{}", n + 1) };
        let id = format!("{}+{}+{}+20260101000000{suffix}", self.game_name, names[0], names[1]);
        Game::new(id, client, TimeControl::from_secs(60, 0, 1, TimeUnit::Msec))
    }

    /// Plays until the client has made `until` moves in total and it is the
    /// client's turn again (or, with `then_drop`, until right after that).
    /// Every client move is checked. False if the client broke a rule.
    fn play(&mut self, c: &mut Conn, g: &mut Game, until: u32) -> Res<bool> {
        loop {
            if g.to_move != g.client {
                if g.client_moves >= until {
                    return Ok(true);
                }
                let mv = g.opponent_move(&mut self.rng)?;
                let echo = g.play(&mv)?;
                c.send(&echo)?;
                continue;
            }
            if g.client_moves >= until {
                return Ok(true);
            }
            let allowance = Duration::from_millis(g.tc.allowance_ms(g.rem[idx(g.client)]) + 5_000);
            let line = c.recv(allowance)?.ok_or("the client closed during its turn")?;
            match ClientMsg::parse(&line) {
                ClientMsg::Move { side, usi } => {
                    let in_turn = side == g.client;
                    if !(in_turn && g.referee.legal()?.contains(&usi)) {
                        let at = format!("{} ply {}: {line}", g.id, g.moves.len() + 1);
                        self.rep.check("client move is legal, in turn, with its colour", false, at);
                        return Ok(false);
                    }
                    let used = g.turn_start.elapsed().as_millis() as u64;
                    let allowed = g.tc.allowance_ms(g.rem[idx(g.client)]);
                    if used > allowed {
                        let at = format!("{} ply {}: {used} ms of {allowed}", g.id, g.moves.len() + 1);
                        self.rep.check("client move within its time", false, at);
                        return Ok(false);
                    }
                    self.moves_checked += 1;
                    let echo = g.play(&usi)?;
                    c.send(&echo)?;
                    g.client_moves += 1;
                }
                ClientMsg::Toryo => {
                    self.rep.check("client resigns with %TORYO", true, &g.id);
                    c.send("#RESIGN")?;
                    c.send("#LOSE")?;
                    return Ok(false);
                }
                _ => {
                    self.rep.check("only moves or %TORYO during play", false, &line);
                    return Ok(false);
                }
            }
        }
    }

    fn end(&mut self, c: &mut Conn, reason: &str, verdict: &str) -> Res<()> {
        c.send(reason)?;
        c.send(verdict)
    }

    fn run(&mut self, listener: &TcpListener) -> Res<()> {
        let mut c = Conn::accept(listener, Duration::from_secs(60))?;
        self.login(&mut c, "LOGIN <name> <password> x1")?;
        self.queue(&mut c, "%%GAME <game-name> <colour>")?;

        // Game A: the opponent resigns.
        let mut a = self.new_game(0)?;
        self.start(&mut c, &a, false, "game A: AGREE within 30 s")?;
        let played = self.play(&mut c, &mut a, 4)?;
        if played {
            self.end(&mut c, "#RESIGN", "#WIN")?;
        }
        self.games = 1;
        self.queue(&mut c, "re-queues after #RESIGN")?;

        // Game B: dropped on the client's turn, resumed, then #CHUDAN.
        let mut b = self.new_game(1)?;
        self.start(&mut c, &b, false, "game B: AGREE within 30 s")?;
        if !self.play(&mut c, &mut b, 3)? {
            return Err("game B did not reach the drop".into());
        }
        if b.to_move != b.client {
            let mv = b.opponent_move(&mut self.rng)?;
            let echo = b.play(&mv)?;
            c.send(&echo)?;
        }
        drop(c);
        let mut c = Conn::accept(listener, Duration::from_secs(60))?;
        self.rep.check("reconnects after a drop", true, "");
        self.login(&mut c, "logs in again")?;
        self.queue(&mut c, "asks for the same game again")?;
        // Make sure part of the turn is gone before the summary says so.
        std::thread::sleep(Duration::from_secs(1).saturating_sub(b.turn_start.elapsed()));
        let used = b.turn_start.elapsed().as_millis();
        self.start(&mut c, &b, true, &format!("resumed game B: AGREE to MOVES, Remaining± and Turn_Used ({used} ms)"))?;
        let before = b.client_moves;
        let resumed_ok = self.play(&mut c, &mut b, before + 2)?;
        self.rep.check("resumed game B: continues with legal moves in turn", resumed_ok, format!("{} plies", b.moves.len()));
        self.end(&mut c, "#CHUDAN", "#CENSORED")?;
        self.games = 2;
        self.queue(&mut c, "re-queues after #CHUDAN")?;

        // Game C: plays on after #CHUDAN; the opponent resigns.
        let mut g = self.new_game(2)?;
        self.start(&mut c, &g, false, "game C: AGREE within 30 s")?;
        if self.play(&mut c, &mut g, 2)? {
            self.end(&mut c, "#RESIGN", "#WIN")?;
        }
        self.games = 3;
        // LOGOUT, another %%GAME or a close are all fine now.
        if let Ok(Some(line)) = c.recv(Duration::from_secs(10)) {
            if line == "LOGOUT" {
                let _ = c.send("LOGOUT:completed");
            }
        }
        Ok(())
    }
}

pub fn run(client: &str, client_log: Option<&std::path::Path>, seed: u64) -> Res<bool> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?.to_string();
    let cmd = client.replace("{server}", &addr);
    let stderr = match client_log {
        Some(p) => Stdio::from(std::fs::File::create(p)?),
        None => Stdio::null(),
    };
    let mut child: Child = Command::new("sh")
        .args(["-c", &cmd])
        .env("CHU_SERVER", &addr)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()?;
    let mut rep = Report { failed: 0 };
    let mut script = Script {
        rep: &mut rep,
        rng: Rng(seed | 1),
        name: String::new(),
        game_name: String::new(),
        color: ColorReq::Any,
        games: 0,
        moves_checked: 0,
    };
    let outcome = script.run(&listener);
    let (games, moves) = (script.games, script.moves_checked);
    rep.check("client moves legal, in turn, with their colour, in time", moves > 0, format!("{moves} moves checked"));
    if let Err(e) = outcome {
        rep.check("the script ran to its end", false, format!("{e} (after {games} games)"));
    }
    let _ = child.kill();
    let _ = child.wait();
    println!("{} failed", rep.failed);
    Ok(rep.failed == 0)
}
