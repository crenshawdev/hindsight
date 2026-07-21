//! The NDJSON loader for `hindsight load`: read a tagged-NDJSON stream (the exact
//! serde output of normalize's `Record` enum, D-03) on stdin and load it into the
//! SQLite index in one transaction.
//!
//! Fresh-build posture (D-10): every load clears the relational tables and the
//! empty `vec_embedding` first, so a load always rebuilds truly from empty.
//! Incremental idempotent upsert over grown/re-swept sessions is Phase 6, out of
//! scope here. Abort-on-error: the input is trusted serde-produced NDJSON, so a
//! parse failure returns `Err` with the line number and rolls the transaction
//! back rather than loading partially in silence (matching normalize's upstream
//! `read_generations` precedent).

use std::io::{BufRead, BufReader, Read};

use anyhow::{Context, Result};
use rusqlite::{params, Transaction};

use crate::config::Config;
use crate::normalize::model::{Grain, Record};
use crate::store::open_db;

/// Tables cleared at the start of every load so the DB rebuilds from empty
/// (D-10). `vec_embedding` is included even though this phase never inserts a
/// vector, so a Phase-4 reload cannot leave orphaned vectors behind stale
/// relational rows. `meta` is deliberately NOT here - its provenance stamp
/// survives a reload and is re-seeded idempotently by the schema.
const FRESH_BUILD_TABLES: [&str; 5] = ["session", "event", "artifact", "mention", "vec_embedding"];

/// Entry point: open the DB at `cfg.db_path()` and load the NDJSON stream on
/// stdin into it.
pub fn run(cfg: &Config) -> Result<()> {
    run_from(cfg, std::io::stdin().lock())
}

/// Buffer-injectable core: load the NDJSON stream from `reader`. Split out so a
/// test can feed an in-memory buffer; the acceptance verification drives the
/// built binary instead.
fn run_from<R: Read>(cfg: &Config, reader: R) -> Result<()> {
    let mut conn = open_db(&cfg.db_path())?;
    let tx = conn.transaction().context("beginning load transaction")?;

    for table in FRESH_BUILD_TABLES {
        tx.execute(&format!("DELETE FROM {table}"), [])
            .with_context(|| format!("clearing table {table} for fresh-build load"))?;
    }

    let buf = BufReader::new(reader);
    for (i, line) in buf.lines().enumerate() {
        let line = line.with_context(|| format!("reading NDJSON line {}", i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(&line)
            .with_context(|| format!("parsing NDJSON line {}", i + 1))?;
        insert_record(&tx, &record)
            .with_context(|| format!("inserting record from NDJSON line {}", i + 1))?;
    }

    tx.commit().context("committing load transaction")?;
    Ok(())
}

/// Insert one record into its table. Booleans store as integers, `grain` as its
/// kebab-case string, and `archive_refs` as a JSON array string. Event and
/// Mention are inserted without an explicit id so AUTOINCREMENT assigns it
/// (multi-block turns and duplicate references both survive, amended D-05).
fn insert_record(tx: &Transaction, record: &Record) -> Result<()> {
    match record {
        Record::Session(s) => {
            let archive_refs =
                serde_json::to_string(&s.archive_refs).context("serializing archive_refs")?;
            tx.execute(
                "INSERT INTO session
                    (session_id, project, git_branch, cc_version, started_at,
                     ended_at, end_reason, title, archive_refs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    s.session_id,
                    s.project,
                    s.git_branch,
                    s.cc_version,
                    s.started_at,
                    s.ended_at,
                    s.end_reason,
                    s.title,
                    archive_refs,
                ],
            )
            .context("inserting session row")?;
        }
        Record::Event(e) => {
            tx.execute(
                "INSERT INTO event
                    (uuid, parent_uuid, session_id, role, kind, timestamp, text,
                     tool_name, is_error, attribution, is_sidechain, agent_id,
                     agent_type, grain)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    e.uuid,
                    e.parent_uuid,
                    e.session_id,
                    e.role,
                    e.kind,
                    e.timestamp,
                    e.text,
                    e.tool_name,
                    e.is_error,
                    e.attribution,
                    e.is_sidechain,
                    e.agent_id,
                    e.agent_type,
                    grain_str(e.grain),
                ],
            )
            .context("inserting event row")?;
        }
        Record::Artifact(a) => {
            tx.execute(
                "INSERT INTO artifact
                    (artifact_id, kind, path, language, content, request_bundle,
                     source_event_uuid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    a.artifact_id,
                    a.kind,
                    a.path,
                    a.language,
                    a.content,
                    a.request_bundle,
                    a.source_event_uuid,
                ],
            )
            .context("inserting artifact row")?;
        }
        Record::Mention(m) => {
            tx.execute(
                "INSERT INTO mention
                    (entity, entity_type, event_uuid, session_id, project, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    m.entity,
                    m.entity_type,
                    m.event_uuid,
                    m.session_id,
                    m.project,
                    m.timestamp,
                ],
            )
            .context("inserting mention row")?;
        }
    }
    Ok(())
}

/// The kebab-case string stored for a grain, matching the serde `rename_all`
/// contract on `Grain` (D-03).
fn grain_str(grain: Grain) -> &'static str {
    match grain {
        Grain::Indexed => "indexed",
        Grain::Skeleton => "skeleton",
        Grain::ArchiveOnly => "archive-only",
    }
}
