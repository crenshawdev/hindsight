//! Hindsight: local, cross-session memory for Claude Code.
//!
//! One static binary with three Phase 1 subcommands: `daemon`, `precompact`,
//! `poke` (D-14). The index/query subcommands arrive in later phases.

mod archive;
mod config;
mod daemon;
mod normalize;
mod poke;
mod precompact;
mod store;
mod sweep;
mod watermark;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "hindsight",
    version,
    about = "Local, cross-session memory for Claude Code: capture transcripts before cleanup."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the socket-activated capture daemon (sweep, then idle-loop until timeout).
    Daemon,
    /// PreCompact hook: snapshot a transcript before compaction. Reads a JSON payload on stdin.
    Precompact,
    /// Poke the daemon socket to trigger a sweep.
    Poke,
    /// Normalize an archived session directory to tagged NDJSON on stdout.
    Normalize {
        /// Path to an archived session directory (parent plus nested subagents/).
        session_dir: PathBuf,
    },
    /// Load a normalize NDJSON stream from stdin into the SQLite index.
    Load,
}

fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon => report(daemon::run()),
        Command::Poke => report(poke::run()),
        Command::Normalize { session_dir } => report(normalize::run(&session_dir)),
        Command::Load => report(load_stream()),
        // D-05: PreCompact fails loud and blocks compaction with exit 2 on any error.
        Command::Precompact => match precompact::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("hindsight precompact: {e:#}");
                ExitCode::from(2)
            }
        },
    }
}

/// Load a normalize NDJSON stream from stdin into the SQLite index at the
/// configured `db_path()`.
fn load_stream() -> anyhow::Result<()> {
    store::load::run(&config::Config::load()?)
}

fn report(result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hindsight: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();
}
