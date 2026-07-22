//! The store schema: four relational tables, the two-stage `vec0` vector table,
//! its resumable embed ledger, and a provenance/version stamp. Applied
//! idempotently on every `open_db` so a fresh-build load (D-10) and a reopen both
//! land on the same shape (D-11).
//!
//! The FTS5 (BM25) index lives here too (PLAN-3): a `content`-only tokenized
//! column with UNINDEXED mapping columns back to the source session and record.
//! The loader populates it in the same pass (D-04). The `vec_embedding` table
//! carries the two-stage retrieval shape (D-09): a bit-quantized `embedding_coarse`
//! companion for the coarse first pass and a full-precision `embedding` for the
//! cosine rescore, a filterable `project` metadata column for the structural
//! pre-filter, and `+unit_kind`/`+source_id` auxiliary columns mapping each vector
//! back to its source `entity`/`artifact`/`event` record and on to the archive.
//! `embed_ledger` is the durable resumable queue (D-06): it stamps every embedded
//! unit under its embedder version so a deferred, interrupted, or CPU-fallback run
//! resumes without re-embedding, and it is wiped in lockstep with `vec_embedding`
//! on every `hindsight load` so ledger-empty safely means not-embedded.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Bumped when the relational or vector schema shape changes. Bumped to `2` in
/// Phase 4 when `vec_embedding` gained the two-stage shape and `embed_ledger`.
pub const SCHEMA_VERSION: &str = "2";
/// The normalize parser contract the loaded rows were produced under.
pub const PARSER_VERSION: &str = "1";
/// The secret-scrub ruleset version applied upstream in normalize.
pub const SCRUB_RULESET_VERSION: &str = "1";

/// Non-zero `PRAGMA user_version` stamp so an opened file is detectably a
/// hindsight index at a known schema generation (D-11). Bumped to `2` alongside
/// `SCHEMA_VERSION` when the vector shape changed; the migration guard in `apply`
/// drops both derived tables (`vec_embedding` and `embed_ledger`) in lockstep on a
/// below-version file so an old single-column file is rebuilt on the new shape and
/// no stale ledger stamps survive an emptied vector table.
pub const USER_VERSION: i64 = 2;

/// Create every table and stamp provenance. Idempotent: safe to re-run on an
/// existing DB, since `open_db` re-applies it on each run.
pub fn apply(conn: &Connection) -> Result<()> {
    // Read the existing schema generation before creating anything. A file
    // written under an older USER_VERSION carries the old single-column
    // `vec_embedding` shape (Phase 3: `vec0(embedding float[4096])`), and
    // `CREATE ... IF NOT EXISTS` would leave it untouched so a Phase-4 insert
    // fails with `no such column: embedding_coarse`. A fresh file reads 0.
    let existing_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .context("reading user_version for migration guard")?;

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

    // Migration guard (D-09): a below-version file carries the old single-column
    // `vec_embedding`; drop it so the two-stage shape is (re)created. Both derived
    // tables - `vec_embedding` and its `embed_ledger` - are dropped in lockstep on
    // a below-version file so they are recreated empty together, preserving the
    // ledger/vector invariant (D-06): a future USER_VERSION migration must not
    // empty `vec_embedding` while leaving stale ledger stamps, or a later
    // `hindsight embed` would skip everything as already-embedded and vector
    // search would silently break with no re-embed. Dropping is safe because both
    // tables are derived, never authoritative (the archive is ground truth), they
    // are already wiped on every `hindsight load`, and D-10 re-embeds the whole
    // corpus after a load. A same-version reopen (existing_version == USER_VERSION)
    // does NOT drop, so vectors written by a prior `embed` and their ledger stamps
    // persist for later query. A fresh file (existing_version == 0) hits a no-op
    // `DROP ... IF EXISTS`.
    if existing_version < USER_VERSION {
        conn.execute_batch(
            "DROP TABLE IF EXISTS vec_embedding;
             DROP TABLE IF EXISTS embed_ledger;",
        )
        .context("dropping pre-migration derived tables in lockstep")?;
    }

    // Two-stage vector table (D-09): `embedding_coarse` is the binary-quantized
    // first-pass column (hamming by default), `embedding` is the full-precision
    // cosine rescore column, `project` is a filterable metadata column for the
    // structural pre-filter, and `unit_kind`/`source_id` are auxiliary mapping
    // columns (`'entity'`/`'artifact'`/`'event'` plus the entity name /
    // artifact_id / event.id as text) so a KNN rowid hit resolves to a record and
    // back to the archive. sqlite-vec `=0.1.9` carries `bit[N]`, a per-column
    // `distance_metric`, filterable metadata, and `+aux` columns with no version
    // bump.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_embedding USING vec0(
             embedding_coarse bit[4096],
             embedding        float[4096] distance_metric=cosine,
             project          text,
             +unit_kind       text,
             +source_id       text
         );",
    )
    .context("creating vec_embedding vec0 table")?;

    // Resumable embed ledger (D-06): one row per embedded unit under its embedder
    // version. The non-dump embed drain stamps this in the same transaction as the
    // vector insert, so a resumed run's skip-check is exact. Wiped in lockstep
    // with `vec_embedding` on every `hindsight load` (see load.rs FRESH_BUILD_TABLES).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embed_ledger (
             unit_kind        TEXT NOT NULL,
             source_id        TEXT NOT NULL,
             embedder_version TEXT NOT NULL,
             embedded_at      TEXT NOT NULL,
             PRIMARY KEY (unit_kind, source_id)
         );",
    )
    .context("creating embed_ledger table")?;

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
            "embed_ledger",
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

    /// Serialize an f32 slice to the little-endian byte blob sqlite-vec expects
    /// for a `float[N]` vector column (matches tests/sqlite_vec_linkage.rs).
    fn vector_blob(v: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(v.len() * 4);
        for x in v {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        bytes
    }

    /// The two-stage `vec_embedding` shape accepts a coarse+full insert and a
    /// KNN MATCH resolves back to the row's `source_id` (D-09). This is the
    /// schema-level proof that the coarse companion, the cosine rescore column,
    /// and the `+source_id` aux mapping all link and round-trip.
    #[test]
    fn vec_embedding_two_stage_insert_and_knn_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        let probe: Vec<f32> = vec![0.25_f32; 4096];
        let blob = vector_blob(&probe);
        conn.execute(
            "INSERT INTO vec_embedding(embedding_coarse, embedding, project, unit_kind, source_id)
             VALUES (vec_quantize_binary(?1), ?1, 'p', 'event', '1')",
            rusqlite::params![blob],
        )
        .expect("two-stage insert succeeds");

        let source_id: String = conn
            .query_row(
                "SELECT source_id FROM vec_embedding WHERE embedding MATCH ?1
                 ORDER BY distance LIMIT 1",
                rusqlite::params![blob],
                |r| r.get(0),
            )
            .expect("knn query returns the inserted row");
        assert_eq!(source_id, "1", "KNN of a vector returns its own source_id");
    }
}
