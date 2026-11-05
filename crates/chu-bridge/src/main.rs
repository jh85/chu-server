//! chu-bridge: plays a USI-Chu engine (USI-CHU.md) on a chu-server
//! (SERVER-PROTOCOL.md), the way floodgate's usiToCsa.rb plays a USI shogi
//! engine. `--conformance <engine>` checks an engine instead.

mod conformance;
mod engine;
mod play;

use std::path::PathBuf;

use chu_proto::ColorReq;
use clap::{Parser, ValueEnum};

use crate::engine::Engine;

pub type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

#[derive(Parser)]
#[command(about = "Plays a USI-Chu engine on a chu-server")]
struct Cli {
    /// Run the USI-CHU.md §6-7 checks on this engine instead of playing.
    #[arg(long, value_name = "ENGINE", conflicts_with = "engine")]
    conformance: Option<PathBuf>,
    #[arg(long, required_unless_present = "conformance")]
    engine: Option<PathBuf>,
    /// Argument for the engine; repeat for several.
    #[arg(long = "engine-arg", value_name = "ARG", allow_hyphen_values = true)]
    engine_args: Vec<String>,
    /// `setoption` sent after `usi`; repeat for several.
    #[arg(long = "usi-option", value_name = "NAME=VALUE")]
    usi_options: Vec<String>,
    #[arg(long, default_value = "127.0.0.1:4081")]
    server: String,
    #[arg(long, required_unless_present = "conformance")]
    name: Option<String>,
    #[arg(long, required_unless_present = "conformance")]
    password: Option<String>,
    /// Game name, e.g. chu-floodgate-600-10F.
    #[arg(long, required_unless_present = "conformance")]
    game: Option<String>,
    /// `+`, `-` or `*`.
    #[arg(long, default_value = "*")]
    color: String,
    #[arg(long, default_value_t = 1)]
    games: u32,
    /// Subtracted from the clocks sent to the engine (USI-CHU.md §5).
    #[arg(long, default_value_t = 1000)]
    margin_ms: u64,
    #[arg(long, value_enum, default_value = "text")]
    log_format: LogFormat,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let logs = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr);
    match cli.log_format {
        LogFormat::Text => logs.init(),
        LogFormat::Json => logs.json().flatten_event(true).init(),
    }
    match run(cli).await {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(1);
        }
    }
}

async fn run(cli: Cli) -> Res<bool> {
    let mut options = Vec::new();
    for o in &cli.usi_options {
        let (k, v) = o.split_once('=').ok_or_else(|| format!("--usi-option {o}: expected NAME=VALUE"))?;
        options.push((k.to_owned(), v.to_owned()));
    }
    if let Some(path) = &cli.conformance {
        let engine = Engine::spawn(path, &cli.engine_args)?;
        return conformance::run(engine, &options).await;
    }
    let color = match cli.color.as_str() {
        "+" => ColorReq::Sente,
        "-" => ColorReq::Gote,
        "*" => ColorReq::Any,
        c => return Err(format!("--color {c}: expected +, - or *").into()),
    };
    let mut engine = Engine::spawn(cli.engine.as_ref().unwrap(), &cli.engine_args)?;
    engine.usi().await?;
    for (k, v) in &options {
        engine.set_option(k, v).await?;
    }
    engine.ready(std::time::Duration::from_secs(30)).await?;
    tracing::info!(engine = %engine.name, "engine ready");
    let cfg = play::Config {
        server: cli.server,
        name: cli.name.unwrap(),
        password: cli.password.unwrap(),
        game: cli.game.unwrap(),
        color,
        games: cli.games,
        margin_ms: cli.margin_ms,
    };
    play::run(cfg, engine).await?;
    Ok(true)
}
