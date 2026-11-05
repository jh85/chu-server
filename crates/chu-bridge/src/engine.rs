//! A USI-Chu engine as a child process (USI-CHU.md).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};
use tracing::trace;

use crate::Res;

pub struct Engine {
    _child: Child,
    stdin: ChildStdin,
    lines: mpsc::UnboundedReceiver<String>,
    /// From `id name`.
    pub name: String,
    /// Names of the `option` lines.
    pub options: Vec<String>,
}

/// `bestmove <move> [id <n>]`: the move and the echoed id.
pub fn parse_bestmove(line: &str) -> Option<(String, Option<String>)> {
    let mut t = line.split_whitespace();
    if t.next()? != "bestmove" {
        return None;
    }
    let mv = t.next()?.to_owned();
    let mut id = None;
    while let Some(k) = t.next() {
        if k == "id" {
            id = t.next().map(str::to_owned);
        }
    }
    Some((mv, id))
}

impl Engine {
    pub fn spawn(path: &Path, args: &[String]) -> Res<Engine> {
        let mut child = Command::new(path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", path.display()))?;
        let stdin = child.stdin.take().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
        let (tx, lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(Some(line)) = out.next_line().await {
                trace!(target: "usi", "< {line}");
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Engine { _child: child, stdin, lines, name: String::new(), options: Vec::new() })
    }

    pub async fn send(&mut self, line: &str) -> Res<()> {
        trace!(target: "usi", "> {line}");
        self.stdin.write_all(format!("{line}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// The next line; an error if the engine exited.
    pub async fn recv(&mut self) -> Res<String> {
        self.lines.recv().await.ok_or_else(|| "the engine exited".into())
    }

    /// The next line within `d`; None on timeout.
    pub async fn recv_within(&mut self, d: Duration) -> Res<Option<String>> {
        match timeout(d, self.recv()).await {
            Ok(r) => r.map(Some),
            Err(_) => Ok(None),
        }
    }

    /// Reads up to a line whose first token is `token`, within `d`.
    pub async fn expect(&mut self, token: &str, d: Duration) -> Res<String> {
        let deadline = Instant::now() + d;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv_within(left).await? {
                Some(l) if l.split_whitespace().next() == Some(token) => return Ok(l),
                Some(_) => {}
                None => return Err(format!("no {token} within {d:?}").into()),
            }
        }
    }

    /// `usi` up to `usiok`, recording the name and option names.
    pub async fn usi(&mut self) -> Res<()> {
        self.send("usi").await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(l) = self.recv_within(left).await? else { return Err("no usiok within 10 s".into()) };
            if l.trim() == "usiok" {
                return Ok(());
            }
            if let Some(name) = l.strip_prefix("id name ") {
                self.name = name.trim().to_owned();
            } else if let Some(rest) = l.strip_prefix("option name ") {
                if let Some(name) = rest.split_whitespace().next() {
                    self.options.push(name.to_owned());
                }
            }
        }
    }

    pub async fn set_option(&mut self, name: &str, value: &str) -> Res<()> {
        self.send(&format!("setoption name {name} value {value}")).await
    }

    /// `isready` up to `readyok`; returns how long it took.
    pub async fn ready(&mut self, d: Duration) -> Res<Duration> {
        let t0 = Instant::now();
        self.send("isready").await?;
        self.expect("readyok", d).await?;
        Ok(t0.elapsed())
    }
}
