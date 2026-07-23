//! Mechanical profile assembly (D-05, D-07, D-08): build the synthetic units that
//! get embedded by querying the built SQLite store, never by re-reading the
//! per-session normalize NDJSON. Cross-session aggregation (deduped usage, the set
//! of projects an entity appears in, co-occurring entities) is a GROUP BY over
//! `mention`, which only the loaded store can answer.
//!
//! Assembly is a separable, Ollama-free stage (D-11): it turns records into
//! `ProfileUnit`s an `--dump-profiles` run can inspect without any embedder.
//!
//! Three unit kinds (D-08): entity profiles, artifact wrappers, and prose chunks.
//! Every text is drawn ONLY from already-scrubbed columns (secrets are removed
//! upstream at normalize) and never carries a full-code artifact body: an artifact
//! wrapper keeps only mechanically whitelisted signature lines, never the body.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

/// Cap on the deduped usage sentences folded into an entity profile (D-08).
const MAX_USAGE_SENTENCES: usize = 8;

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

/// Assemble every embeddable profile unit from the built store (D-08's three
/// kinds).
pub fn assemble(conn: &Connection) -> Result<Vec<ProfileUnit>> {
    let mut units = Vec::new();
    assemble_entities(conn, &mut units)?;
    assemble_artifacts(conn, &mut units)?;
    assemble_events(conn, &mut units)?;
    Ok(units)
}

/// Entity profiles (D-07, D-08): one unit per `(entity_type, entity)` group over
/// `mention`. The `source_id` is the composite `{entity_type}:{entity}` (NOT
/// `entity` alone): normalize emits both `file` and `command` entity types, so the
/// same surface string (a `build` file and a `build` command) is two distinct
/// units; keying on `entity` alone would collide them on the `embed_ledger`
/// primary key and silently drop one.
fn assemble_entities(conn: &Connection, units: &mut Vec<ProfileUnit>) -> Result<()> {
    // Exact `(entity_type, entity)` groups so `source_id` is always a value that
    // literally exists in `mention` (the no-orphan invariant, criterion 2).
    let mut groups_stmt = conn
        .prepare("SELECT entity_type, entity FROM mention GROUP BY entity_type, entity")
        .context("preparing entity group query")?;
    let groups: Vec<(String, String)> = groups_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("querying entity groups")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading entity groups")?;

    for (entity_type, entity) in groups {
        // Most-frequent project stamps the single vector-table `project` (D-09).
        let project: String = conn
            .query_row(
                "SELECT project FROM mention
                 WHERE entity_type = ?1 AND entity = ?2
                 GROUP BY project ORDER BY count(*) DESC, project ASC LIMIT 1",
                rusqlite::params![entity_type, entity],
                |r| r.get(0),
            )
            .context("resolving most-frequent project for entity")?;

        let aliases = query_column(
            conn,
            "SELECT DISTINCT entity FROM mention
             WHERE entity_type = ?1 AND lower(entity) = lower(?2) ORDER BY entity",
            rusqlite::params![entity_type, entity],
        )?;
        let projects = query_column(
            conn,
            "SELECT DISTINCT project FROM mention
             WHERE entity_type = ?1 AND entity = ?2 ORDER BY project",
            rusqlite::params![entity_type, entity],
        )?;
        let cooccurring = query_column(
            conn,
            "SELECT DISTINCT m2.entity FROM mention m1
             JOIN mention m2 ON m2.event_uuid = m1.event_uuid
             WHERE m1.entity_type = ?1 AND m1.entity = ?2 AND m2.entity <> m1.entity
             ORDER BY m2.entity",
            rusqlite::params![entity_type, entity],
        )?;
        let intro: Option<String> = conn
            .query_row(
                "SELECT e.text FROM mention m JOIN event e ON e.uuid = m.event_uuid
                 WHERE m.entity_type = ?1 AND m.entity = ?2
                   AND e.grain = 'indexed' AND e.text IS NOT NULL
                 ORDER BY e.timestamp ASC, e.id ASC LIMIT 1",
                rusqlite::params![entity_type, entity],
                |r| r.get(0),
            )
            .ok();
        let usage = query_column(
            conn,
            "SELECT DISTINCT e.text FROM mention m JOIN event e ON e.uuid = m.event_uuid
             WHERE m.entity_type = ?1 AND m.entity = ?2
               AND e.grain = 'indexed' AND e.text IS NOT NULL
             ORDER BY e.text LIMIT ?3",
            rusqlite::params![entity_type, entity, MAX_USAGE_SENTENCES as i64],
        )?;

        let mut text = String::new();
        text.push_str(&format!("{entity_type}: {entity}\n"));
        if !aliases.is_empty() {
            text.push_str(&format!("aliases: {}\n", aliases.join(", ")));
        }
        if let Some(intro) = intro {
            text.push_str(&format!("intro: {intro}\n"));
        }
        if !usage.is_empty() {
            text.push_str("usage:\n");
            for u in &usage {
                text.push_str(&format!("- {u}\n"));
            }
        }
        if !cooccurring.is_empty() {
            text.push_str(&format!("co-occurring: {}\n", cooccurring.join(", ")));
        }
        if !projects.is_empty() {
            text.push_str(&format!("projects: {}\n", projects.join(", ")));
        }

        units.push(ProfileUnit {
            unit_kind: "entity".to_string(),
            source_id: format!("{entity_type}:{entity}"),
            project,
            text,
        });
    }
    Ok(())
}

/// Artifact wrappers (D-08): one unit per `artifact` row. `project` is derived by
/// joining `source_event_uuid -> event.session_id -> session.project` (the
/// `artifact` table carries no `project`/`session_id`), the same session
/// resolution the loader's artifact FTS post-pass uses. The `text` is the request
/// context, path, language, and mechanically whitelisted signature lines only; the
/// code body is deliberately excluded.
fn assemble_artifacts(conn: &Connection, units: &mut Vec<ProfileUnit>) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT a.artifact_id, a.path, a.language, a.content, a.request_bundle, s.project
             FROM artifact a
             JOIN event e ON e.uuid = a.source_event_uuid
             JOIN session s ON s.session_id = e.session_id
             GROUP BY a.artifact_id",
        )
        .context("preparing artifact query")?;
    let rows: Vec<(String, Option<String>, Option<String>, String, Option<String>, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .context("querying artifacts")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading artifacts")?;

    for (artifact_id, path, language, content, request_bundle, project) in rows {
        let request: Option<String> = match &request_bundle {
            Some(uuid) => conn
                .query_row(
                    "SELECT text FROM event WHERE uuid = ?1 AND text IS NOT NULL
                     ORDER BY id LIMIT 1",
                    rusqlite::params![uuid],
                    |r| r.get(0),
                )
                .ok(),
            None => None,
        };

        let mut text = String::new();
        if let Some(request) = request {
            text.push_str(&format!("request: {request}\n"));
        }
        if let Some(path) = &path {
            text.push_str(&format!("path: {path}\n"));
        }
        if let Some(language) = &language {
            text.push_str(&format!("language: {language}\n"));
        }
        let signature = extract_signature(&content);
        if !signature.is_empty() {
            text.push_str("signature:\n");
            for line in &signature {
                text.push_str(&format!("{line}\n"));
            }
        }

        units.push(ProfileUnit {
            unit_kind: "artifact".to_string(),
            source_id: artifact_id,
            project,
            text,
        });
    }
    Ok(())
}

/// Prose chunks (D-08): one unit per indexed-grain event carrying non-null text.
/// `project` is resolved by joining the event's `session_id` to `session`.
///
/// `source_id` is `{uuid}:{ordinal}`, NOT the row's autoincrement `id`. The id is
/// reassigned every time incremental ingest deletes and re-inserts a session
/// (`ingest_session`), which would orphan every event's embed-ledger stamp and force
/// a full re-embed on each hook-driven re-ingest (Phase 7 regression). The uuid is
/// stable across re-ingest but not unique - a multi-block turn emits several indexed
/// events sharing one uuid - so a deterministic per-uuid ordinal (`ROW_NUMBER` over
/// the same indexed-text population, ordered by id) makes the key unique AND stable.
/// The query side computes the identical expression: `vector.rs`'s TimeFilter for
/// the window candidate set and `ranked.rs`'s uuid-prefix resolution.
fn assemble_events(conn: &Connection, units: &mut Vec<ProfileUnit>) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT e.uuid || ':' || CAST(
                 ROW_NUMBER() OVER (PARTITION BY e.uuid ORDER BY e.id) AS TEXT),
                 s.project, e.text
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

/// Run a single-text-column query with params and collect the strings.
fn query_column(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql).context("preparing column query")?;
    let rows = stmt
        .query_map(params, |r| r.get::<_, String>(0))
        .context("running column query")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("reading a column value")?);
    }
    Ok(out)
}

/// Pull only the declaration/flag "signature" lines out of an artifact body
/// (D-08): mechanically whitelisted lines (`fn`/`def`/`class`/`function`/`struct`
/// declarations and `--flag` lines), never the whole body. The excluded body is
/// what keeps a full-code payload out of the vector path.
fn extract_signature(content: &str) -> Vec<String> {
    // A declaration opener (optionally `pub`/`async`), matched at line start.
    let decl = regex::Regex::new(r"^\s*(pub\s+)?(async\s+)?(fn|def|class|function|struct)\b")
        .expect("valid decl regex");
    // A CLI flag token anywhere on the line.
    let flag = regex::Regex::new(r"(^|\s)--[A-Za-z0-9][\w-]*").expect("valid flag regex");
    content
        .lines()
        .filter(|line| decl.is_match(line) || flag.is_match(line))
        .map(|line| line.trim_end().to_string())
        .collect()
}
