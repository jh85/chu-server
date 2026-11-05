use std::io::IsTerminal;
use std::path::PathBuf;

use chu_rules::Backend;
use chu_server::{Config, Server};
use clap::{Parser, ValueEnum};
use tokio::net::TcpListener;
use tracing::info;

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
    #[arg(long, default_value = "sqlite://chu.db")]
    db: String,
    #[arg(long, default_value = "records")]
    records: PathBuf,
    /// Floodgate league schedule.
    #[arg(long, default_value = "leagues.toml")]
    leagues: PathBuf,
    #[arg(long, value_enum, default_value = "ffi")]
    referee: RefereeKind,
    /// chushogi-gen binary for `--referee subprocess`.
    #[arg(long, default_value = chu_rules::BUILT_CHUSHOGI_GEN)]
    chushogi_gen: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    let args = Args::parse();
    let referee = match args.referee {
        RefereeKind::Ffi => Backend::Ffi,
        RefereeKind::Subprocess => Backend::Subprocess(args.chushogi_gen),
    };
    let leagues = chu_server::league::load(&args.leagues)?;
    for l in &leagues {
        info!("league {}: every {} min (+{}), cutoff {} s, rated {}", l.name, l.every_min, l.offset_min, l.cutoff_s, l.rated);
    }
    let server = Server::start(Config { referee, db_url: args.db, records_dir: args.records, leagues }).await?;
    let listener = TcpListener::bind(&args.listen).await?;
    info!("listening on {}", listener.local_addr()?);
    loop {
        let (sock, peer) = listener.accept().await?;
        let _ = sock.set_nodelay(true);
        info!("connection from {peer}");
        server.serve(sock);
    }
}
