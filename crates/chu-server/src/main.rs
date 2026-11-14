use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Mutex;

use chu_rules::Backend;
use chu_server::{Config, Server};
use clap::{Parser, ValueEnum};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Copy, Clone, ValueEnum)]
enum RefereeKind {
    Ffi,
    Subprocess,
}

/// Chu shogi tournament server (SERVER-PROTOCOL.md).
#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:4081")]
    listen: String,
    /// The web front end.
    #[arg(long, default_value = "0.0.0.0:8080")]
    http: String,
    #[arg(long, default_value = "sqlite://chu.db")]
    db: String,
    #[arg(long, default_value = "records")]
    records: PathBuf,
    /// The league list: every game the server plays.
    #[arg(long, default_value = "leagues.toml")]
    leagues: PathBuf,
    #[arg(long, value_enum, default_value = "ffi")]
    referee: RefereeKind,
    /// chushogi-gen binary for `--referee subprocess`.
    #[arg(long, default_value = chu_rules::BUILT_CHUSHOGI_GEN)]
    chushogi_gen: PathBuf,
    /// Also append the log to this file (plain text; the console log stays).
    #[arg(long)]
    log_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let file = match &args.log_file {
        Some(path) => Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("--log-file {}: {e}", path.display()))?,
        ),
        None => None,
    };
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        // The file first: a line on the console is already in the file.
        .with(file.map(|f| fmt::layer().with_writer(Mutex::new(f)).with_ansi(false)))
        .with(fmt::layer().with_writer(std::io::stderr).with_ansi(std::io::stderr().is_terminal()))
        .init();
    if let Some(path) = &args.log_file {
        info!("logging to {} as well", path.display());
    }
    let referee = match args.referee {
        RefereeKind::Ffi => Backend::Ffi,
        RefereeKind::Subprocess => Backend::Subprocess(args.chushogi_gen),
    };
    let leagues = chu_server::league::load(&args.leagues)?;
    for l in &leagues {
        let pairing = match l.schedule() {
            Some(s) => format!("every {} min (+{}), cutoff {} s", s.every_min, s.offset_min, s.cutoff_s),
            None => "on demand".into(),
        };
        info!("league {} ({:?}): {}, {}, {}, rated {}", l.name, l.kind, l.describe_time(), l.rules.as_str(), pairing, l.rated);
    }
    let server = Server::start(Config { referee, db_url: args.db, records_dir: args.records, leagues }).await?;
    let http = TcpListener::bind(&args.http).await?;
    info!("web front end on http://{}", http.local_addr()?);
    let router = server.router();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(http, router).await {
            tracing::error!("web front end: {e}");
        }
    });
    let listener = TcpListener::bind(&args.listen).await?;
    info!("listening on {}", listener.local_addr()?);
    loop {
        let (sock, peer) = listener.accept().await?;
        let _ = sock.set_nodelay(true);
        info!("connection from {peer}");
        server.serve(sock);
    }
}
