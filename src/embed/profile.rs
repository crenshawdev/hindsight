//! Mechanical profile assembly (D-05, D-07, D-08): build the synthetic units that
//! get embedded by querying the built SQLite store, never by re-reading the
//! per-session normalize NDJSON. Cross-session aggregation (deduped usage, the set
//! of projects an entity appears in, co-occurring entities) is a GROUP BY over
//! `mention`, which only the loaded store can answer.
//!
//! Assembly is a separable, Ollama-free stage (D-11): it turns records into
//! `ProfileUnit`s an `--dump-profiles` run can inspect without any embedder.
//!
//! This tracer step assembles ONLY the prose-chunk unit (one per indexed-grain
//! event). Entity profiles and artifact wrappers land in the next task.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

/// One unit of text to embed, tagged with the mapping back to its source record
/// and the `project` that materializes on the vector table for the structural
/// pre-filter (D-09).
#[derive(Debug, Clone, Serialize)]
pub struct ProfileUnit {
    /// `'entity'` / `'artifact'` / `'event'` (D-08).
    pub unit_kind: String,
    /// The mapping id back to the source record: entity `{type}:{name}`, artifact
    /// id, or event id as text (D-09).
    pub source_id: String,
    /// The materialized structural pre-filter column (D-09); never empty.
    pub project: String,
    /// The assembled text sent to the embedder; carries no raw secret (scrubbed
    /// upstream at normalize) and no full-code artifact body (D-08).
    pub text: String,
}

/// Assemble every embeddable profile unit from the built store.
pub fn assemble(conn: &Connection) -> Result<Vec<ProfileUnit>> {
    let mut units = Vec::new();
    assemble_events(conn, &mut units)?;
    Ok(units)
}

/// Prose chunks (D-08): one unit per indexed-grain event carrying non-null text.
/// `project` is resolved by joining the event's `session_id` to `session`.
fn assemble_events(conn: &Connection, units: &mut Vec<ProfileUnit>) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT CAST(e.id AS TEXT), s.project, e.text
             FROM event e
             JOIN session s ON s.session_id = e.session_id
             WHERE e.grain = 'indexed' AND e.text IS NOT NULL",
        )
        .context("preparing event profile query")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ProfileUnit {
                unit_kind: "event".to_string(),
                source_id: r.get(0)?,
                project: r.get(1)?,
                text: r.get(2)?,
            })
        })
        .context("querying event profile units")?;
    for unit in rows {
        units.push(unit.context("reading an event profile unit")?);
    }
    Ok(())
}
