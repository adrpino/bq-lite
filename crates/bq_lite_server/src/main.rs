use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use bq_lite_server::grpc::serve_grpc_with_listener;
use bq_lite_server::repl::{run_exec, run_repl, OutputFormat};
use bq_lite_server::{build_router, ServerState};
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tracing::info;

#[derive(Debug, Parser)]
#[command(
    name = "bq-lite-server",
    version,
    disable_version_flag = true,
    about = "Local BigQuery REST v2, gRPC Storage API emulator, one-shot SQL runner & REPL"
)]
struct Cli {
    /// Print version information
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: (),

    #[command(subcommand)]
    command: Option<Command>,

    /// Bind host address for HTTP REST and gRPC listeners (env: `BQ_LITE_HOST`)
    #[arg(long, global = true)]
    host: Option<IpAddr>,

    /// Port for the HTTP REST v2 API (env: `BQ_LITE_PORT`, default: 9050)
    #[arg(long, global = true)]
    port: Option<u16>,

    /// Port for the gRPC Storage Read/Write API (env: `BQ_LITE_GRPC_PORT`, default: 9060)
    #[arg(long, global = true)]
    grpc_port: Option<u16>,

    /// Default GCP project ID (env: `BQ_LITE_PROJECT`, default: local)
    #[arg(long, global = true)]
    project: Option<String>,

    /// Directory for redb persistence across restarts (env: `BQ_LITE_DATA_DIR`)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Run completely in memory without disk persistence
    #[arg(long, global = true)]
    memory: bool,

    /// Execute a SQL seed file on server startup before serving requests
    #[arg(long, global = true)]
    seed: Option<PathBuf>,

    /// Suppress startup logs (useful for CI and agent harnesses)
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the HTTP REST v2 + gRPC Storage API server (default)
    Serve,
    /// Launch an interactive `GoogleSQL` CLI REPL
    Repl,
    /// Execute a one-shot `GoogleSQL` query or script and exit
    Exec {
        /// Inline SQL query or script to execute
        sql: Option<String>,
        /// Path to a `.sql` file to execute
        #[arg(short = 'f', long)]
        file: Option<PathBuf>,
        /// Output format (table, json, csv)
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
}

struct ResolvedConfig {
    host: IpAddr,
    port: u16,
    grpc_port: u16,
    project: String,
    data_dir: Option<PathBuf>,
}

fn resolve_config(cli: &Cli) -> ResolvedConfig {
    let host = cli
        .host
        .or_else(|| {
            std::env::var("BQ_LITE_HOST")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or_else(|| IpAddr::from([0, 0, 0, 0]));
    let port = cli
        .port
        .or_else(|| {
            std::env::var("BQ_LITE_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(9050);
    let grpc_port = cli
        .grpc_port
        .or_else(|| {
            std::env::var("BQ_LITE_GRPC_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(9060);
    let project = cli
        .project
        .clone()
        .or_else(|| std::env::var("BQ_LITE_PROJECT").ok())
        .unwrap_or_else(|| "local".to_owned());
    let data_dir = if cli.memory {
        None
    } else {
        cli.data_dir
            .clone()
            .or_else(|| std::env::var("BQ_LITE_DATA_DIR").ok().map(PathBuf::from))
    };
    ResolvedConfig {
        host,
        port,
        grpc_port,
        project,
        data_dir,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cfg = resolve_config(&cli);
    let dir_opt = cfg.data_dir.as_deref();

    match cli.command {
        Some(Command::Repl) => return run_repl(&cfg.project, dir_opt),
        Some(Command::Exec { sql, file, format }) => {
            let query = match (sql, file) {
                (Some(s), _) => s,
                (None, Some(path)) => std::fs::read_to_string(path)?,
                (None, None) => {
                    return Err("Provide either an inline SQL string or --file <path>".into())
                }
            };
            let mut out = std::io::stdout();
            return run_exec(&cfg.project, dir_opt, &query, format, &mut out);
        }
        Some(Command::Serve) | None => {}
    }

    if !cli.quiet {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    let state = match dir_opt {
        Some(dir) => ServerState::with_data_dir(&cfg.project, dir.to_path_buf())?,
        None => ServerState::new(&cfg.project),
    };
    if let Some(ref seed_path) = cli.seed {
        let seed_sql = std::fs::read_to_string(seed_path)?;
        state.engine.write().exec_script(&seed_sql)?;
        state.persist_if_enabled();
    }

    let http_listener = TcpListener::bind(SocketAddr::new(cfg.host, cfg.port)).await?;
    let grpc_listener = TcpListener::bind(SocketAddr::new(cfg.host, cfg.grpc_port)).await?;
    info!(
        host = %cfg.host,
        port = cfg.port,
        grpc_port = cfg.grpc_port,
        project = %cfg.project,
        data_dir = ?dir_opt,
        memory = cli.memory,
        "bq-lite-server listening (HTTP REST + gRPC Storage API)"
    );

    let app = build_router(state.clone());
    let http_fut = async move {
        axum::serve(http_listener, app)
            .await
            .map_err(|e| format!("HTTP server error: {e}"))
    };
    let grpc_fut = async move {
        serve_grpc_with_listener(grpc_listener, state)
            .await
            .map_err(|e| format!("gRPC server error: {e}"))
    };
    tokio::try_join!(http_fut, grpc_fut)?;
    Ok(())
}
