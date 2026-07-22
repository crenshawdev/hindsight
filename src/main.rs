//! Hindsight: local, cross-session memory for Claude Code.
//!
//! One static binary with three Phase 1 subcommands: `daemon`, `precompact`,
//! `poke` (D-14). The index/query subcommands arrive in later phases.

mod archive;
mod config;
mod daemon;
mod embed;
mod ingest;
mod mcp;
mod normalize;
mod poke;
mod precompact;
mod query;
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
    /// Incremental capture->index->embed pass (Phase 7): sweep, session-scoped
    /// re-index of new-or-changed sessions, then a detached embed drain. The
    /// hook-driven live-ingest entrypoint; idempotent and single-flight.
    Ingest,
    /// Assemble synthetic profiles and embed them into the vector store.
    Embed {
        /// Print the assembled profile units as NDJSON and write no vectors.
        #[arg(long)]
        dump_profiles: bool,
        /// Self-detach (setsid) and return immediately; a detached child runs the
        /// drain. This is the hook-fired entrypoint (D-01), distinct from `poke`.
        #[arg(long)]
        detach: bool,
        /// Report drain status from the DB and exit without embedding (D-07).
        #[arg(long)]
        status: bool,
    },
    /// Ground-truth search over the index (no embedder, no GPU): a positional
    /// keyword query (FTS5) or `--exact <entity>` recall-complete listing (D-10).
    Search {
        /// Positional keyword query, run over the FTS5 index.
        query: Option<String>,
        /// Recall-complete exact listing for this entity (file/command name).
        #[arg(long)]
        exact: Option<String>,
        /// Restrict an `--exact` listing to this entity_type (`file`/`command`).
        #[arg(long)]
        entity_type: Option<String>,
        /// Structural pre-filter: only this project.
        #[arg(long)]
        project: Option<String>,
        /// Time pre-filter: RFC3339 lower bound (inclusive).
        #[arg(long)]
        since: Option<String>,
        /// Time pre-filter: RFC3339 upper bound (inclusive).
        #[arg(long)]
        until: Option<String>,
    },
    /// Serve the MCP recall server over stdio (rmcp JSON-RPC). The tokio runtime
    /// is confined to this subcommand; the rest of the binary stays synchronous.
    Mcp,
}

fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon => report(daemon::run()),
        Command::Poke => report(poke::run()),
        Command::Normalize { session_dir } => report(normalize::run(&session_dir)),
        Command::Load => report(load_stream()),
        Command::Ingest => report(ingest_run()),
        Command::Embed {
            dump_profiles,
            detach,
            status,
        } => report(embed_run(dump_profiles, detach, status)),
        Command::Search {
            query,
            exact,
            entity_type,
            project,
            since,
            until,
        } => report(run_search(query, exact, entity_type, project, since, until)),
        Command::Mcp => report(run_mcp()),
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

/// Incremental live-ingest pass (Phase 7): sweep, re-index new-or-changed sessions,
/// then trigger a detached embed drain. The hook-driven entrypoint.
fn ingest_run() -> anyhow::Result<()> {
    ingest::run(&config::Config::load()?)
}

/// Assemble synthetic profiles from the loaded index and embed them into the
/// vector store (or dump them when `dump_profiles` is set, or report drain status
/// when `status` is set). With `detach`, spawn a detached child to run the drain and
/// return immediately (D-01).
fn embed_run(dump_profiles: bool, detach: bool, status: bool) -> anyhow::Result<()> {
    embed::run(&config::Config::load()?, dump_profiles, detach, status)
}

/// Ground-truth search (D-10): open the configured DB and run the no-model
/// keyword or exact-listing path (`hindsight search`).
fn run_search(
    query: Option<String>,
    exact: Option<String>,
    entity_type: Option<String>,
    project: Option<String>,
    since: Option<String>,
    until: Option<String>,
) -> anyhow::Result<()> {
    query::run_search(
        &config::Config::load()?,
        query,
        exact,
        entity_type,
        project,
        since,
        until,
    )
}

/// Serve the MCP recall server over stdio (D-09). The tokio runtime is built
/// inside `mcp::run`, never on `main`, so the rest of the binary stays synchronous.
fn run_mcp() -> anyhow::Result<()> {
    mcp::run(&config::Config::load()?)
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
