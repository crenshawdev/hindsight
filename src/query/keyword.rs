//! The FTS5 (BM25) keyword arm (QRY-02, D-03). A MATCH over the `fts.content`
//! column, ranked by `bm25(fts)` ascending (more negative = stronger), reading the
//! UNINDEXED `session_id/source_type/source_id` mapping columns back to the source
//! record. Structural pre-filters: `project` via the `session` row, and an RFC3339
//! time window via the mapped source record's timestamp (D-06). Embedder-free by
//! construction - this is one half of the CLI ground-truth surface (D-10).

use anyhow::{Context, Result};
use rusqlite::{Connection, ToSql};

/// One keyword hit: the session it belongs to, the source record kind/id it maps
/// back to, and its BM25 rank (lower is a stronger match).
#[derive(Debug, Clone)]
pub struct KeywordHit {
    pub session_id: String,
    pub source_type: String,
    pub source_id: String,
    pub rank: f64,
}

/// The mapped source record's timestamp as a scalar SQL expression, so the time
/// pre-filter (D-06) reads a per-row timestamp without adding a column to `fts`.
/// An event maps by `uuid`; an artifact maps through its `source_event_uuid`. One
/// uuid can back several event rows (multi-block turns) sharing a timestamp, so
/// `MIN` picks a single deterministic value.
const RECORD_TS: &str = "(CASE fts.source_type
        WHEN 'event' THEN (SELECT MIN(e.timestamp) FROM event e WHERE e.uuid = fts.source_id)
        WHEN 'artifact' THEN (SELECT MIN(e.timestamp) FROM event e
            JOIN artifact a ON a.source_event_uuid = e.uuid WHERE a.artifact_id = fts.source_id)
        ELSE NULL END)";

/// Keyword-search the FTS5 index for `query`, ranked by BM25 ascending. Optional
/// `project` and RFC3339 `since`/`until` structural pre-filters narrow the result
/// (D-06). Takes no embedder dependency (D-10).
pub fn keyword_search(
    conn: &Connection,
    query: &str,
    project: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<KeywordHit>> {
    // Sanitize the user query into a safe FTS5 MATCH expression. An empty query
    // (all whitespace) matches nothing rather than erroring on a bare `MATCH ''`.
    let match_expr = fts_match_expr(query);
    if match_expr.is_empty() {
        return Ok(Vec::new());
    }

    let mut sql = String::from(
        "SELECT fts.session_id, fts.source_type, fts.source_id, bm25(fts) AS rank
         FROM fts WHERE fts MATCH ?",
    );
    // References outlive the statement; `is_some()` guards bind inner values.
    let mut params: Vec<&dyn ToSql> = vec![&match_expr];
    if project.is_some() {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM session s
                 WHERE s.session_id = fts.session_id AND s.project = ?)",
        );
        params.push(&project);
    }
    if since.is_some() {
        sql.push_str(&format!(" AND {RECORD_TS} >= ?"));
        params.push(&since);
    }
    if until.is_some() {
        sql.push_str(&format!(" AND {RECORD_TS} <= ?"));
        params.push(&until);
    }
    sql.push_str(" ORDER BY rank ASC");

    let mut stmt = conn.prepare(&sql).context("preparing keyword-search query")?;
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok(KeywordHit {
                session_id: r.get(0)?,
                source_type: r.get(1)?,
                source_id: r.get(2)?,
                rank: r.get(3)?,
            })
        })
        .context("running keyword-search query")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("reading a keyword hit")?);
    }
    Ok(out)
}

/// Turn arbitrary user text into a safe FTS5 MATCH expression: split on
/// whitespace, wrap each token in double quotes (doubling any internal quote so a
/// `"` cannot break out), and join with spaces (implicit AND). Quoting neutralizes
/// FTS5 operators (`AND`/`OR`/`NEAR`/`*`/`:`/`^`) and punctuation, so a bare term
/// or phrase from a user is always a literal keyword search, never an injection
/// into the query grammar.
fn fts_match_expr(query: &str) -> String {
    query
        .split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open_db;

    fn seed_session(conn: &Connection, session_id: &str, project: &str) {
        conn.execute(
            "INSERT INTO session(session_id, project) VALUES (?1, ?2)",
            rusqlite::params![session_id, project],
        )
        .unwrap();
    }

    /// Seed an indexed event and its FTS row (mirrors the loader's event FTS path).
    fn seed_event_fts(
        conn: &Connection,
        uuid: &str,
        session_id: &str,
        timestamp: &str,
        text: &str,
    ) {
        conn.execute(
            "INSERT INTO event(uuid, session_id, is_sidechain, grain, timestamp, text)
             VALUES (?1, ?2, 0, 'indexed', ?3, ?4)",
            rusqlite::params![uuid, session_id, timestamp, text],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts(content, session_id, source_type, source_id)
             VALUES (?1, ?2, 'event', ?3)",
            rusqlite::params![text, session_id, uuid],
        )
        .unwrap();
    }

    /// A matching term returns the containing session (nonzero), and a `project`
    /// filter narrows the result to a strict subset.
    #[test]
    fn matches_indexed_term_and_project_narrows() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_session(&conn, "sess-a", "proj-1");
        seed_session(&conn, "sess-b", "proj-2");
        seed_event_fts(&conn, "ua", "sess-a", "2026-07-21T10:00:00Z", "deploy the widget service");
        seed_event_fts(&conn, "ub", "sess-b", "2026-07-22T10:00:00Z", "deploy the other thing");

        let all = keyword_search(&conn, "deploy", None, None, None).unwrap();
        assert!(all.len() >= 2, "both sessions match the indexed term");
        assert!(all.iter().all(|h| h.source_type == "event"));

        let only_p1 = keyword_search(&conn, "deploy", Some("proj-1"), None, None).unwrap();
        assert_eq!(only_p1.len(), 1, "project filter narrows to one session");
        assert_eq!(only_p1[0].session_id, "sess-a");
    }

    /// An artifact FTS row is matchable and maps back to its session via
    /// `source_event_uuid`; the time filter reads the source event's timestamp.
    #[test]
    fn matches_artifact_and_time_filter_narrows() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_session(&conn, "sess-a", "proj-1");
        // The source event carries the timestamp the artifact time-filter reads.
        conn.execute(
            "INSERT INTO event(uuid, session_id, is_sidechain, grain, timestamp)
             VALUES ('ev', 'sess-a', 0, 'indexed', '2026-07-21T10:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO artifact(artifact_id, kind, content, source_event_uuid)
             VALUES ('art-1', 'file', 'fn deploy_widget() {}', 'ev')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts(content, session_id, source_type, source_id)
             VALUES ('fn deploy_widget() {}', 'sess-a', 'artifact', 'art-1')",
            [],
        )
        .unwrap();

        let hit = keyword_search(&conn, "deploy_widget", None, None, None).unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].source_type, "artifact");
        assert_eq!(hit[0].source_id, "art-1");

        // A window that excludes the source event's timestamp drops the hit.
        let excluded =
            keyword_search(&conn, "deploy_widget", None, Some("2026-07-22T00:00:00Z"), None).unwrap();
        assert!(excluded.is_empty(), "time filter excludes the out-of-window artifact");
        // A window that includes it keeps the hit.
        let included =
            keyword_search(&conn, "deploy_widget", None, Some("2026-07-20T00:00:00Z"), None).unwrap();
        assert_eq!(included.len(), 1);
    }

    /// A query carrying FTS5 operator syntax is treated as literal text, not an
    /// injection into the MATCH grammar (no error, no operator behavior).
    #[test]
    fn operator_syntax_is_quoted_literal() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();
        seed_session(&conn, "sess-a", "proj-1");
        seed_event_fts(&conn, "ua", "sess-a", "2026-07-21T10:00:00Z", "plain content here");

        // A bare `NOT` / `*` / unbalanced quote would be an FTS5 syntax error if
        // passed raw; quoting makes it a literal search that simply finds nothing.
        let r = keyword_search(&conn, "NOT * \"unbalanced", None, None, None).unwrap();
        assert!(r.is_empty(), "operator-like query is literal and matches nothing, not an error");
    }
}
