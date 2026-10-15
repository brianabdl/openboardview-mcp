//! OpenBoardView MCP server.
//!
//! Exposes PCB boardview files to MCP clients using OpenBoardView's own
//! parsers. Run without arguments (or with `serve`) to speak MCP over stdio.

mod analysis;
mod annotations;
mod board;
mod dto;
mod parse;
mod render;
mod schematic;
mod server;
mod store;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

use crate::parse::keys::{DecryptionKeys, default_config_path};
use crate::parse::worker::{WORKER_SUBCOMMAND, parse_in_worker, run_worker};
use crate::store::{BoardStore, StoreConfig, resolve_path};

#[derive(Parser)]
#[command(name = "openboardview-mcp", version, about, long_version = long_version())]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(clap::Args, Clone)]
struct ServeArgs {
    /// OpenBoardView config file to read FZ/CAE/XZZPCB decryption keys from
    /// [default: the OpenBoardView user config, obv.conf].
    #[arg(long, env = "OBV_CONFIG", global = true)]
    obv_config: Option<PathBuf>,

    /// OpenBoardView executable used by the open_in_viewer tool.
    #[arg(long, env = "OBV_VIEWER", default_value = "openboardview", global = true)]
    viewer: String,

    /// Seconds before a parse is aborted.
    #[arg(long, default_value_t = 120, global = true)]
    parse_timeout: u64,
}

#[derive(Subcommand)]
enum Command {
    /// Serve MCP over stdio (the default).
    Serve,
    /// Parse a board file and print a summary, or the raw parser output as JSON.
    Dump {
        file: String,
        /// Print the full parser output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Internal: parse one file for the server (request on stdin, JSON on stdout).
    #[command(name = WORKER_SUBCOMMAND, hide = true)]
    ParseWorker,
}

fn long_version() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), " (OpenBoardView parsers ", env!("OBV_VERSION"), ")")
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // stdout is reserved for the MCP protocol (and worker output): log to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("OBV_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    match cli.command {
        Some(Command::ParseWorker) => run_worker(),
        Some(Command::Dump { file, json }) => runtime()?.block_on(dump(&cli.serve, &file, json)),
        Some(Command::Serve) | None => runtime()?.block_on(serve(cli.serve)),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().context("starting async runtime")
}

fn store_config(args: &ServeArgs) -> StoreConfig {
    StoreConfig {
        obv_config: args.obv_config.clone(),
        parse_timeout: Duration::from_secs(args.parse_timeout.max(1)),
        ..StoreConfig::default()
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    let config = store_config(&args);
    let keys_from = config.obv_config.clone().or_else(default_config_path);
    tracing::info!(
        "openboardview-mcp {} (OpenBoardView {}) starting; keys from {}: {}",
        env!("CARGO_PKG_VERSION"),
        env!("OBV_VERSION"),
        keys_from.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "(none)".into()),
        DecryptionKeys::load(config.obv_config.as_deref()).summary()
    );

    let server = server::ObvServer::new(BoardStore::new(config), args.viewer);
    let service = server.serve(rmcp::transport::stdio()).await.context("starting MCP service")?;
    service.waiting().await.context("MCP service failed")?;
    Ok(())
}

async fn dump(args: &ServeArgs, file: &str, json: bool) -> Result<()> {
    let path = resolve_path(file)?;
    let keys = DecryptionKeys::load(args.obv_config.as_deref());
    let outcome = parse_in_worker(&path, &keys, Duration::from_secs(args.parse_timeout.max(1))).await?;
    for warning in &outcome.warnings {
        eprintln!("parser: {warning}");
    }
    if json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &outcome.board)?;
        println!();
        return Ok(());
    }
    let b = board::Board::from_raw(path, outcome.board);
    println!("file:     {}", b.path.display());
    println!("format:   {}", b.format);
    println!("parts:    {}", b.parts.len());
    println!("pins:     {} ({} test pads)", b.pins.len(), b.test_pad_count());
    println!("nets:     {}", b.nets.len());
    println!("nails:    {}", b.nails.len());
    println!(
        "size:     {:.1} x {:.1} mm{}",
        b.bbox.width() * board::MM_PER_MIL,
        b.bbox.height() * board::MM_PER_MIL,
        if b.outline_generated { " (outline generated from pins)" } else { "" }
    );
    Ok(())
}
