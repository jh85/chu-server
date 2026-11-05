//! One task per connection, plus a reader and a writer task.
//!
//! The reader enforces the line limits of SERVER-PROTOCOL.md §1 and stamps
//! every line with its receive time, which is what the game clock charges.

use std::time::Duration;

use chu_proto::{valid_name, ClientMsg, LineBucket, IDLE_TIMEOUT_S, MAX_LINE};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, Instant};
use tracing::{debug, info};

use crate::game::GameInput;
use crate::lobby::{LobbyMsg, LobbyTx};
use crate::store::{self, StoreTx};
use crate::{Conn, Ctl, GameLink, OutMsg};

enum Input {
    Line(String, Instant),
    /// Line too long or the line bucket ran dry.
    Violation(&'static str),
}

pub async fn run<S>(stream: S, conn_id: u64, lobby: LobbyTx, store: StoreTx)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (rd, wr) = tokio::io::split(stream);
    let (conn, out_rx) = Conn::new(conn_id);
    let writer = tokio::spawn(write_loop(wr, out_rx));
    let (in_tx, mut in_rx) = mpsc::channel(256);
    let reader = tokio::spawn(read_loop(rd, in_tx));
    let (ctl_tx, mut ctl_rx) = mpsc::unbounded_channel();

    let mut s = Session { conn: conn.clone(), ctl: ctl_tx, lobby, store, name: None, game: None };
    let idle = Duration::from_secs(IDLE_TIMEOUT_S);
    loop {
        let in_game = s.game.is_some();
        tokio::select! {
            // Control first: a Joined is always queued before the summary the
            // client answers, so the AGREE that follows finds the game.
            biased;
            Some(ctl) = ctl_rx.recv() => match ctl {
                Ctl::Joined(link) => s.game = Some(link),
                Ctl::Left => s.game = None,
                Ctl::Kick => break,
            },
            input = in_rx.recv() => match input {
                Some(Input::Line(line, at)) => {
                    if !s.on_line(&line, at).await {
                        break;
                    }
                }
                Some(Input::Violation(why)) => {
                    info!("connection {conn_id} ({:?}): {why}, closing", s.name);
                    if let Some(g) = s.game.take() {
                        // #ILLEGAL_ACTION: let the result lines reach the
                        // violator before the connection closes.
                        let _ = g.tx.send(GameInput::Violation { side: g.side, conn_id });
                        let wait = async {
                            while let Some(ctl) = ctl_rx.recv().await {
                                if matches!(ctl, Ctl::Left | Ctl::Kick) {
                                    return;
                                }
                            }
                        };
                        let _ = tokio::time::timeout(Duration::from_secs(5), wait).await;
                    }
                    break;
                }
                None => break,
            },
            _ = sleep(idle), if !in_game => {
                info!("connection {conn_id} ({:?}) idle, closing", s.name);
                break;
            }
        }
    }

    if let Some(g) = &s.game {
        let _ = g.tx.send(GameInput::Disconnected { side: g.side, conn_id });
    }
    if let Some(name) = s.name.take() {
        let _ = s.lobby.send(LobbyMsg::Unregister { name, conn_id });
    }
    conn.close();
    reader.abort();
    let _ = writer.await;
}

struct Session {
    conn: Conn,
    ctl: mpsc::UnboundedSender<Ctl>,
    lobby: LobbyTx,
    store: StoreTx,
    name: Option<String>,
    game: Option<GameLink>,
}

impl Session {
    /// Handles one line; false closes the connection.
    async fn on_line(&mut self, line: &str, at: Instant) -> bool {
        let msg = ClientMsg::parse(line);
        let Some(name) = self.name.clone() else {
            if let ClientMsg::Login { name, password, x1 } = msg {
                self.login(name, password, x1).await;
            }
            return true;
        };
        let conn = self.conn.clone();
        let lobby = |m: LobbyMsg| {
            let _ = self.lobby.send(m);
        };
        match msg {
            ClientMsg::Logout => {
                self.conn.send("LOGOUT:completed");
                return false;
            }
            ClientMsg::Game { game_name, color, sfen } => {
                lobby(LobbyMsg::Join { name, conn_id: conn.id, game_name, color, sfen })
            }
            ClientMsg::Who => lobby(LobbyMsg::Who { conn }),
            ClientMsg::List => lobby(LobbyMsg::List { conn }),
            ClientMsg::Show(id) => lobby(LobbyMsg::Show { conn, id }),
            ClientMsg::MonitorOn(id) => lobby(LobbyMsg::Monitor { conn, id, on: true }),
            ClientMsg::MonitorOff(id) => lobby(LobbyMsg::Monitor { conn, id, on: false }),
            ClientMsg::Chat(text) => lobby(LobbyMsg::Chat { name, text }),
            ClientMsg::Agree(_) | ClientMsg::Reject(_) | ClientMsg::Move { .. } | ClientMsg::Toryo => {
                if let Some(g) = &self.game {
                    let _ = g.tx.send(GameInput::Msg { side: g.side, msg, at });
                }
            }
            ClientMsg::Login { .. } | ClientMsg::KeepAlive => {}
            ClientMsg::Unknown(l) => debug!("{name}: ignored {l:?}"),
        }
        true
    }

    async fn login(&mut self, name: String, password: String, x1: bool) {
        if !x1 || !valid_name(&name) || !store::login(&self.store, &name, &password).await {
            self.conn.send("LOGIN:incorrect");
            return;
        }
        let (reply, done) = oneshot::channel();
        let reg = LobbyMsg::Register { name: name.clone(), conn: self.conn.clone(), ctl: self.ctl.clone(), reply };
        if self.lobby.send(reg).is_ok() && done.await.is_ok() {
            info!("{name} logged in on connection {}", self.conn.id);
            self.conn.send(format!("LOGIN:{name} OK"));
            self.name = Some(name);
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(rd: R, tx: mpsc::Sender<Input>) {
    let mut rd = BufReader::new(rd);
    let mut buf = Vec::new();
    let mut bucket = LineBucket::server(Instant::now().into_std());
    loop {
        buf.clear();
        // Read one line, refusing to buffer more than MAX_LINE bytes.
        let complete = loop {
            let avail = match rd.fill_buf().await {
                Ok(a) => a,
                Err(_) => return,
            };
            if avail.is_empty() {
                return; // EOF; a partial last line is dropped
            }
            if let Some(i) = avail.iter().position(|&b| b == b'\n') {
                buf.extend_from_slice(&avail[..i]);
                rd.consume(i + 1);
                break buf.len() <= MAX_LINE;
            }
            let n = avail.len();
            buf.extend_from_slice(avail);
            rd.consume(n);
            if buf.len() > MAX_LINE {
                break false;
            }
        };
        let now = Instant::now();
        if !complete {
            let _ = tx.send(Input::Violation("line over 1 KiB")).await;
            return;
        }
        if !bucket.try_take(now.into_std()) {
            let _ = tx.send(Input::Violation("line rate limit")).await;
            return;
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        let line = String::from_utf8_lossy(&buf).into_owned();
        if tx.send(Input::Line(line, now)).await.is_err() {
            return;
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(wr: W, mut rx: mpsc::UnboundedReceiver<OutMsg>) {
    let mut wr = BufWriter::new(wr);
    while let Some(msg) = rx.recv().await {
        let OutMsg::Line(line) = msg else { break };
        if wr.write_all(line.as_bytes()).await.is_err() || wr.write_all(b"\n").await.is_err() {
            return;
        }
        // Flush when nothing else is queued, so a summary goes out in one write.
        if rx.is_empty() && wr.flush().await.is_err() {
            return;
        }
    }
    let _ = wr.flush().await;
    let _ = wr.shutdown().await;
}
