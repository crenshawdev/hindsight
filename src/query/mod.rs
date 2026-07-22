//! The two-path query core (Phase 6): the recall-complete exact listing (QRY-01),
//! the FTS5 keyword arm plus the two-stage vector read fused by RRF (QRY-02), and
//! hit resolution to verbatim archived bytes (QRY-03). The CLI `hindsight search`
//! surface is the no-model ground-truth view (keyword + exact, D-10); the fuzzy
//! RRF vector path is owned by the MCP server (Task 5, Task 8).

pub mod exact;

use anyhow::Result;

use crate::config::Config;
use crate::store::open_db;

/// CLI `hindsight search` entry point (D-10, the no-model ground-truth surface).
/// `--exact <entity>` runs the recall-complete listing (QRY-01). The positional
/// keyword path is wired in Task 2; both CLI modes are embedder-free.
#[allow(clippy::too_many_arguments)]
pub fn run_search(
    cfg: &Config,
    _query: Option<String>,
    exact: Option<String>,
    entity_type: Option<String>,
    project: Option<String>,
    since: Option<String>,
    until: Option<String>,
) -> Result<()> {
    let conn = open_db(&cfg.db_path())?;

    if let Some(entity) = exact {
        let sessions = exact::exact_listing(
            &conn,
            &entity,
            entity_type.as_deref(),
            project.as_deref(),
            since.as_deref(),
            until.as_deref(),
        )?;
        for session_id in sessions {
            println!("{session_id}");
        }
        return Ok(());
    }

    // The positional keyword ground-truth path arrives in Task 2.
    anyhow::bail!("provide --exact <entity>; positional keyword search lands in Task 2");
}
