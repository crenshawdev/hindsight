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
/// (D-10). `vec_embedding` and its `embed_ledger` are wiped in lockstep so a
/// reload cannot leave orphaned vectors or a stale embed stamp behind fresh
/// relational rows, and so ledger-empty safely means not-embedded: the next
/// `hindsight embed` re-embeds the whole corpus (D-06, D-10). `embed_run` is wiped
/// with them so `hindsight embed --status` after a fresh load reports no stale
/// prior run against the just-loaded corpus (D-07). `fts` is cleared in step with
/// the relational tables so a reload rebuilds the FTS index with no stale rows from
/// a prior load. `meta` is deliberately NOT here - its provenance stamp survives a
/// reload and is re-seeded idempotently by the schema.
const FRESH_BUILD_TABLES: [&str; 8] = [
    "session",
    "event",
    "artifact",
    "mention",
    "vec_embedding",
    "embed_ledger",
    "embed_run",
    "fts",
];

/// Entry point: open the DB at `cfg.db_path()` and load the NDJSON stream on
/// stdin into it.
pub fn run(cfg: &Config) -> Result<()> {
    run_from(cfg, std::io::stdin().lock())
}

/// Buffer-injectable core: load the NDJSON stream from `reader`. Split out so a
/// test can feed an in-memory buffer; the acceptance verification drives the
/// built binary instead.
pub(crate) fn run_from<R: Read>(cfg: &Config, reader: R) -> Result<()> {
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

    // Artifact FTS5 post-pass (D-04). An Artifact carries no `session_id`, only
    // `source_event_uuid`, and events are not guaranteed to precede their
    // artifact in the stream, so the session_id is resolved by a set-based join
    // AFTER the record loop - order-independent. `source_event_uuid` can match
    // several event rows sharing one uuid (amended D-05), all with the same
    // session_id, so SELECT DISTINCT keeps each artifact to exactly one FTS row.
    tx.execute(
        "INSERT INTO fts(content, session_id, source_type, source_id)
         SELECT DISTINCT a.content, e.session_id, 'artifact', a.artifact_id
         FROM artifact a JOIN event e ON e.uuid = a.source_event_uuid",
        [],
    )
    .context("populating artifact fts rows")?;

    tx.commit().context("committing load transaction")?;
    Ok(())
}

/// Incrementally ingest a SINGLE session's NDJSON stream with a session-scoped
/// replace (Phase 7). Unlike `run`/`run_from`, this does NOT wipe the whole index
/// or the embed ledger: it deletes only the named session's existing rows, inserts
/// the fresh records, and rebuilds that session's artifact FTS rows, all in one
/// transaction. Idempotent - re-ingesting an unchanged or grown session leaves
/// exactly one current copy, never duplicates - and it leaves every other session's
/// rows and all already-landed vectors untouched, so a later `hindsight embed`
/// drains only genuinely-new units. Returns the session_id it replaced.
///
/// The whole (single-session) stream is parsed into memory first so the session_id
/// is known before any delete and a parse error aborts before any mutation. The
/// artifact delete runs BEFORE the event delete: artifacts carry no session_id and
/// are scoped to the session only through their `source_event_uuid` -> `event.uuid`
/// link, which needs the event rows still present to resolve. A forked/copied
/// session that shares event uuids with another leaves exactly one content-identical
/// artifact copy (the INSERT below is OR IGNORE), which is benign for recall.
pub fn ingest_session<R: Read>(conn: &mut rusqlite::Connection, reader: R) -> Result<String> {
    let mut records: Vec<Record> = Vec::new();
    let buf = BufReader::new(reader);
    for (i, line) in buf.lines().enumerate() {
        let line = line.with_context(|| format!("reading NDJSON line {}", i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(&line)
            .with_context(|| format!("parsing NDJSON line {}", i + 1))?;
        records.push(record);
    }
    let session_id = records
        .iter()
        .find_map(|r| match r {
            Record::Session(s) => Some(s.session_id.clone()),
            _ => None,
        })
        .context("ingest stream carried no Session record")?;

    let tx = conn.transaction().context("beginning ingest transaction")?;

    // Delete this session's prior rows. Artifacts first (their session scope is the
    // event-uuid join, which needs the events still present), then fts/event/
    // mention/session.
    tx.execute(
        "DELETE FROM artifact WHERE source_event_uuid IN
            (SELECT uuid FROM event WHERE session_id = ?1)",
        params![session_id],
    )
    .context("clearing prior artifacts for session")?;
    tx.execute("DELETE FROM fts WHERE session_id = ?1", params![session_id])
        .context("clearing prior fts rows for session")?;
    tx.execute("DELETE FROM event WHERE session_id = ?1", params![session_id])
        .context("clearing prior events for session")?;
    tx.execute("DELETE FROM mention WHERE session_id = ?1", params![session_id])
        .context("clearing prior mentions for session")?;
    tx.execute("DELETE FROM session WHERE session_id = ?1", params![session_id])
        .context("clearing prior session row")?;

    for record in &records {
        insert_record(&tx, record).context("inserting ingest record")?;
    }

    // Artifact FTS post-pass, scoped to this session (same join as the full load's).
    tx.execute(
        "INSERT INTO fts(content, session_id, source_type, source_id)
         SELECT DISTINCT a.content, e.session_id, 'artifact', a.artifact_id
         FROM artifact a JOIN event e ON e.uuid = a.source_event_uuid
         WHERE e.session_id = ?1",
        params![session_id],
    )
    .context("populating artifact fts rows for session")?;

    tx.commit().context("committing ingest transaction")?;
    Ok(session_id)
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

            // FTS5 (D-04): only indexed-grain events with a body feed the term
            // index. Guarding on `grain == Indexed` (not just `text.is_some()`)
            // means a future non-blanked skeleton body still cannot leak in.
            if e.grain == Grain::Indexed {
                if let Some(text) = &e.text {
                    tx.execute(
                        "INSERT INTO fts(content, session_id, source_type, source_id)
                         VALUES (?1, ?2, 'event', ?3)",
                        params![text, e.session_id, e.uuid],
                    )
                    .context("inserting event fts row")?;
                }
            }
        }
        Record::Artifact(a) => {
            // OR IGNORE: a resumed/forked session's transcript carries its
            // parent's events verbatim, so a multi-session stream can replay
            // the same artifact_id ({event-uuid}-{n}) with identical content.
            // First write wins; under the newest-first backfill ordering that
            // is the newest session's copy.
            tx.execute(
                "INSERT OR IGNORE INTO artifact
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

#[cfg(test)]
mod ingest_tests {
    use super::*;
    use crate::normalize::model::{Artifact, Event, Mention, Session};
    use crate::store::open_db;
    use std::io::Cursor;

    fn sess(id: &str, project: &str) -> Record {
        Record::Session(Session {
            session_id: id.to_string(),
            project: project.to_string(),
            git_branch: None,
            cc_version: None,
            started_at: None,
            ended_at: None,
            end_reason: None,
            title: None,
            archive_refs: vec!["0000.zst".to_string()],
        })
    }

    fn ev(uuid: &str, session: &str, text: &str) -> Record {
        Record::Event(Event {
            uuid: uuid.to_string(),
            parent_uuid: None,
            session_id: session.to_string(),
            role: "assistant".to_string(),
            kind: "message".to_string(),
            timestamp: None,
            text: Some(text.to_string()),
            tool_name: None,
            is_error: None,
            attribution: None,
            is_sidechain: false,
            agent_id: None,
            agent_type: None,
            grain: Grain::Indexed,
        })
    }

    fn art(id: &str, source_uuid: &str, content: &str) -> Record {
        Record::Artifact(Artifact {
            artifact_id: id.to_string(),
            kind: "file".to_string(),
            path: Some("/tmp/x".to_string()),
            language: None,
            content: content.to_string(),
            request_bundle: None,
            source_event_uuid: source_uuid.to_string(),
        })
    }

    fn men(entity: &str, uuid: &str, session: &str, project: &str) -> Record {
        Record::Mention(Mention {
            entity: entity.to_string(),
            entity_type: "file".to_string(),
            event_uuid: uuid.to_string(),
            session_id: session.to_string(),
            project: project.to_string(),
            timestamp: None,
        })
    }

    /// Serialize records to the tagged NDJSON that `ingest_session` parses (the same
    /// serde shape normalize emits).
    fn ndjson(records: &[Record]) -> Vec<u8> {
        let mut buf = Vec::new();
        for r in records {
            let line = serde_json::to_string(r).unwrap();
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
        buf
    }

    fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Ingesting a session, then ingesting the identical stream again, leaves exactly
    /// one copy of every row - the session-scoped delete makes the second pass a
    /// replace, not a duplicate append.
    #[test]
    fn re_ingesting_an_unchanged_session_does_not_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = open_db(&tmp.path().join("h.db")).unwrap();
        let stream = ndjson(&[
            sess("s1", "proj"),
            ev("u1", "s1", "hello alpha"),
            ev("u2", "s1", "hello beta"),
            art("u1-0", "u1", "file body alpha"),
            men("/tmp/x", "u1", "s1", "proj"),
        ]);

        let sid = ingest_session(&mut conn, Cursor::new(stream.clone())).unwrap();
        assert_eq!(sid, "s1");
        assert_eq!(count(&conn, "SELECT count(*) FROM session"), 1);
        assert_eq!(count(&conn, "SELECT count(*) FROM event"), 2);
        assert_eq!(count(&conn, "SELECT count(*) FROM artifact"), 1);
        assert_eq!(count(&conn, "SELECT count(*) FROM mention"), 1);
        // fts: two indexed events + one artifact post-pass row.
        assert_eq!(count(&conn, "SELECT count(*) FROM fts"), 3);

        ingest_session(&mut conn, Cursor::new(stream)).unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM session"), 1, "no dup session");
        assert_eq!(count(&conn, "SELECT count(*) FROM event"), 2, "no dup events");
        assert_eq!(count(&conn, "SELECT count(*) FROM artifact"), 1, "no dup artifact");
        assert_eq!(count(&conn, "SELECT count(*) FROM mention"), 1, "no dup mention");
        assert_eq!(count(&conn, "SELECT count(*) FROM fts"), 3, "no dup fts rows");
    }

    /// A grown session (same id, changed content) replaces the prior rows: the old
    /// text is gone, the new text is present, and nothing accumulates.
    #[test]
    fn re_ingesting_a_changed_session_replaces_its_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = open_db(&tmp.path().join("h.db")).unwrap();

        ingest_session(
            &mut conn,
            Cursor::new(ndjson(&[sess("s1", "proj"), ev("u1", "s1", "old content")])),
        )
        .unwrap();

        // Re-ingest the same session with different events (as if it grew).
        ingest_session(
            &mut conn,
            Cursor::new(ndjson(&[
                sess("s1", "proj"),
                ev("u1", "s1", "new content"),
                ev("u2", "s1", "extra turn"),
            ])),
        )
        .unwrap();

        assert_eq!(count(&conn, "SELECT count(*) FROM session"), 1);
        assert_eq!(count(&conn, "SELECT count(*) FROM event"), 2, "replaced, not appended");
        assert_eq!(
            count(&conn, "SELECT count(*) FROM event WHERE text = 'old content'"),
            0,
            "prior content is gone"
        );
        assert_eq!(
            count(&conn, "SELECT count(*) FROM fts WHERE fts MATCH 'old'"),
            0,
            "prior fts rows are gone"
        );
        assert_eq!(
            count(&conn, "SELECT count(*) FROM fts WHERE fts MATCH 'extra'"),
            1,
            "new fts rows are present"
        );
    }

    /// Ingesting one session leaves another session's rows untouched - the replace is
    /// scoped by session_id.
    #[test]
    fn ingesting_one_session_leaves_others_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = open_db(&tmp.path().join("h.db")).unwrap();

        ingest_session(
            &mut conn,
            Cursor::new(ndjson(&[sess("sA", "proj"), ev("a1", "sA", "alpha")])),
        )
        .unwrap();
        ingest_session(
            &mut conn,
            Cursor::new(ndjson(&[sess("sB", "proj"), ev("b1", "sB", "bravo")])),
        )
        .unwrap();

        assert_eq!(count(&conn, "SELECT count(*) FROM session"), 2, "both sessions present");
        assert_eq!(
            count(&conn, "SELECT count(*) FROM event WHERE session_id = 'sA'"),
            1,
            "session A survived session B's ingest"
        );

        // Re-ingesting B again still leaves A alone.
        ingest_session(
            &mut conn,
            Cursor::new(ndjson(&[sess("sB", "proj"), ev("b1", "sB", "bravo two")])),
        )
        .unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM event WHERE session_id = 'sA'"), 1);
    }
}
