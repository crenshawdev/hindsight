//! The store schema: four relational tables, an empty `vec0` vector table, and a
//! provenance/version stamp. Applied idempotently on every `open_db` so a
//! fresh-build load (D-10) and a reopen both land on the same shape (D-11).
//!
//! The FTS5 (BM25) index lives here too (PLAN-3): a `content`-only tokenized
//! column with UNINDEXED mapping columns back to the source session and record.
//! The loader populates it in the same pass (D-04). The `vec_embedding` table is
//! created empty this phase (D-07); vectors arrive in Phase 4.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Bumped when the relational or vector schema shape changes.
pub const SCHEMA_VERSION: &str = "1";
/// The normalize parser contract the loaded rows were produced under.
pub const PARSER_VERSION: &str = "1";
/// The secret-scrub ruleset version applied upstream in normalize.
pub const SCRUB_RULESET_VERSION: &str = "1";

/// Non-zero `PRAGMA user_version` stamp so an opened file is detectably a
/// hindsight index at a known schema generation (D-11).
pub const USER_VERSION: i64 = 1;

/// Create every table and stamp provenance. Idempotent: safe to re-run on an
/// existing DB, since `open_db` re-applies it on each run.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS session (
            session_id   TEXT PRIMARY KEY,
            project      TEXT NOT NULL,
            git_branch   TEXT,
            cc_version   TEXT,
            started_at   TEXT,
            ended_at     TEXT,
            end_reason   TEXT,
            title        TEXT,
            archive_refs TEXT
        );

        -- Event is NOT keyed by uuid (amended D-05): normalize emits one Event
        -- per content block, and every block of a source line shares that line's
        -- uuid (src/normalize/parse.rs:157), so a uuid PK would reject any
        -- multi-block assistant turn. A synthetic autoincrement id is the key;
        -- uuid stays a NOT NULL, non-unique indexed column referencing the source
        -- line (downstream joins - artifact.source_event_uuid, mention.event_uuid
        -- - hit event.uuid, and one uuid can match several Event rows).
        CREATE TABLE IF NOT EXISTS event (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid         TEXT NOT NULL,
            parent_uuid  TEXT,
            session_id   TEXT NOT NULL,
            role         TEXT,
            kind         TEXT,
            timestamp    TEXT,
            text         TEXT,
            tool_name    TEXT,
            is_error     INTEGER,
            attribution  TEXT,
            is_sidechain INTEGER NOT NULL,
            agent_id     TEXT,
            agent_type   TEXT,
            grain        TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS event_uuid ON event(uuid);

        CREATE TABLE IF NOT EXISTS artifact (
            artifact_id       TEXT PRIMARY KEY,
            kind              TEXT,
            path              TEXT,
            language          TEXT,
            content           TEXT NOT NULL,
            request_bundle    TEXT,
            source_event_uuid TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS mention (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            entity      TEXT NOT NULL,
            entity_type TEXT NOT NULL,
            event_uuid  TEXT,
            session_id  TEXT NOT NULL,
            project     TEXT NOT NULL,
            timestamp   TEXT
        );

        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT
        );
        ",
    )
    .context("creating relational tables")?;

    // FTS5 (BM25) index (D-04). `content` is the only tokenized column so BM25
    // ranks on it; the UNINDEXED columns carry the mapping back to the source
    // session and record without polluting the term index. Available because
    // rusqlite's bundled SQLite is compiled with -DSQLITE_ENABLE_FTS5 (PLAN-1);
    // a "no such module: fts5" here would be a bundled-build gap, not a schema
    // problem.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
             content,
             session_id  UNINDEXED,
             source_type UNINDEXED,
             source_id   UNINDEXED
         );",
    )
    .context("creating fts5 index")?;

    // Empty 4096-dim float vector table (D-07). Populated in Phase 4; the
    // two-stage rerank companion is Phase 4's concern.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_embedding USING vec0(embedding float[4096]);",
    )
    .context("creating vec_embedding vec0 table")?;

    // Provenance stamp (D-11). PRAGMA user_version takes a literal, not a bound
    // parameter.
    conn.execute_batch(&format!("PRAGMA user_version = {USER_VERSION};"))
        .context("stamping user_version")?;

    // The meta seed is idempotent: `meta` deliberately survives the loader's
    // fresh-build DELETE set, and `open_db` re-applies the schema on every run,
    // so a plain INSERT would raise `UNIQUE constraint failed: meta.key` on the
    // second `hindsight load` against the same file.
    let seed = [
        ("schema_version", SCHEMA_VERSION),
        ("parser_version", PARSER_VERSION),
        ("scrub_ruleset_version", SCRUB_RULESET_VERSION),
    ];
    for (key, value) in seed {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .with_context(|| format!("seeding meta row {key}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::store::open_db;

    #[test]
    fn open_db_creates_tables_and_stamps_version() {
        let tmp = tempfile::tempdir().unwrap();
        // Nested path so the parent-dir creation in open_db is exercised too.
        let db = tmp.path().join("index").join("hindsight.db");
        let conn = open_db(&db).unwrap();

        for table in [
            "session",
            "event",
            "artifact",
            "mention",
            "vec_embedding",
            "meta",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} should exist");
        }

        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_ne!(user_version, 0, "user_version must be non-zero");

        let schema_version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!schema_version.is_empty(), "schema_version meta row present");

        // Re-applying the schema on an existing file must not raise a UNIQUE
        // violation on meta (the idempotent-seed guarantee).
        super::apply(&conn).expect("schema re-apply is idempotent");
    }
}
