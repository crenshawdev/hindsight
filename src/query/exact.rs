//! Exact, recall-complete listing (QRY-01, D-01): the `mention` inventory table
//! answers "which sessions reference this entity" directly, no join. `mention`
//! carries `entity/entity_type/session_id/project/timestamp` per row (schema.rs),
//! so the whole `session_id` set for an entity is a filtered `DISTINCT` over one
//! table. Time-ordered by each session's earliest `mention.timestamp` so the
//! listing is stable and readable, and recall-complete by construction (every
//! matching row's session is returned, none ranked away).

use anyhow::{Context, Result};
use rusqlite::{Connection, ToSql};

/// List every `session_id` whose `mention` rows reference `entity`, filtered by
/// any supplied `entity_type`/`project` and an RFC3339 `since`/`until` timestamp
/// range (string range, D-01, D-06). Ordered by each session's earliest
/// `mention.timestamp` so the listing is time-ordered and deterministic. No join:
/// `mention` is the denormalized inventory (D-01).
pub fn exact_listing(
    conn: &Connection,
    entity: &str,
    entity_type: Option<&str>,
    project: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<String>> {
    let mut sql = String::from(
        "SELECT session_id, MIN(timestamp) AS first_ts FROM mention WHERE entity = ?",
    );
    // References into the function parameters, which outlive the statement; the
    // `is_some()` guards mean each pushed `Option` binds its inner value, never NULL.
    let mut params: Vec<&dyn ToSql> = vec![&entity];
    if entity_type.is_some() {
        sql.push_str(" AND entity_type = ?");
        params.push(&entity_type);
    }
    if project.is_some() {
        sql.push_str(" AND project = ?");
        params.push(&project);
    }
    if since.is_some() {
        sql.push_str(" AND timestamp >= ?");
        params.push(&since);
    }
    if until.is_some() {
        sql.push_str(" AND timestamp <= ?");
        params.push(&until);
    }
    // Group to DISTINCT sessions, ordered by the earliest mention per session
    // (NULL timestamps sort first under SQLite ASC, an acceptable stable tail).
    sql.push_str(" GROUP BY session_id ORDER BY first_ts ASC, session_id ASC");

    let mut stmt = conn.prepare(&sql).context("preparing exact-listing query")?;
    let rows = stmt
        .query_map(params.as_slice(), |r| r.get::<_, String>(0))
        .context("running exact-listing query")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("reading an exact-listing session_id")?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open_db;

    /// Seed a `mention` row directly (the listing reads `mention` alone, D-01).
    fn seed_mention(
        conn: &Connection,
        entity: &str,
        entity_type: &str,
        session_id: &str,
        project: &str,
        timestamp: &str,
    ) {
        conn.execute(
            "INSERT INTO mention(entity, entity_type, event_uuid, session_id, project, timestamp)
             VALUES (?1, ?2, 'u', ?3, ?4, ?5)",
            rusqlite::params![entity, entity_type, session_id, project, timestamp],
        )
        .unwrap();
    }

    /// QRY-01 / acceptance: a file mentioned across two sessions returns exactly
    /// those two `session_id`s, and the count matches a direct
    /// `COUNT(DISTINCT session_id)` over `mention` for that entity (no omissions,
    /// countable). A decoy file must not leak in.
    #[test]
    fn returns_every_session_for_a_file_and_count_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_mention(&conn, "src/main.rs", "file", "sess-a", "proj", "2026-07-21T10:00:00Z");
        seed_mention(&conn, "src/main.rs", "file", "sess-b", "proj", "2026-07-21T11:00:00Z");
        // A second mention in sess-a must still collapse to one session row.
        seed_mention(&conn, "src/main.rs", "file", "sess-a", "proj", "2026-07-21T12:00:00Z");
        // Decoy: a different file must not appear for this query.
        seed_mention(&conn, "src/other.rs", "file", "sess-c", "proj", "2026-07-21T10:00:00Z");

        let sessions = exact_listing(&conn, "src/main.rs", None, None, None, None).unwrap();
        assert_eq!(sessions, vec!["sess-a".to_string(), "sess-b".to_string()]);

        let expected: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT session_id) FROM mention WHERE entity = ?1",
                rusqlite::params!["src/main.rs"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sessions.len() as i64, expected, "count equals direct COUNT(DISTINCT session_id)");
    }

    /// The `project` and time-range filters narrow the listing to a strict subset.
    #[test]
    fn project_and_time_filters_narrow_the_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_mention(&conn, "build", "file", "sess-a", "proj-1", "2026-07-21T10:00:00Z");
        seed_mention(&conn, "build", "file", "sess-b", "proj-2", "2026-07-22T10:00:00Z");

        let only_p1 = exact_listing(&conn, "build", None, Some("proj-1"), None, None).unwrap();
        assert_eq!(only_p1, vec!["sess-a".to_string()]);

        let after = exact_listing(&conn, "build", None, None, Some("2026-07-22T00:00:00Z"), None).unwrap();
        assert_eq!(after, vec!["sess-b".to_string()]);

        // entity_type discriminates a same-named command from the file.
        seed_mention(&conn, "build", "command", "sess-d", "proj-1", "2026-07-21T10:00:00Z");
        let files_only = exact_listing(&conn, "build", Some("file"), Some("proj-1"), None, None).unwrap();
        assert_eq!(files_only, vec!["sess-a".to_string()]);
    }
}
