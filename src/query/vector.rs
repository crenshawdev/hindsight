//! The two-stage vector read (QRY-02, D-02). Built fresh in Phase 6: the schema
//! carries the columns and inserts quantize on write, but no read existed. Two
//! paths:
//!
//! - Unfiltered (the governing case): stage one is a binary-coarse KNN over
//!   `embedding_coarse` (hamming) collecting a candidate rowid pool a few times
//!   wider than `k`; stage two rescores those rowids by full-precision
//!   `vec_distance_cosine(embedding, :q)` ascending, limited to `k`. Binary-coarse
//!   recall is a locked-design tradeoff (D-02), not a defect - `COARSE_MULTIPLIER`
//!   / `MIN_COARSE_K` are the tuning knobs: too tight drops a true neighbor whose
//!   Hamming proximity is imperfect.
//! - Anchored (D-06): `project` filters inside the vec0 MATCH as a metadata column.
//!   A time window filters OUTSIDE the MATCH via filter-then-exact-rerank - a
//!   precomputed candidate `source_id` set (`TimeFilter`) selects the survivors and
//!   the exact cosine rescore runs only over them, no KNN over the whole set.
//!
//! Takes no embedder dependency: the caller supplies `query_vec` (mirrors the
//! injectable `embed_fn` seam in embed/mod.rs), so this is testable without Ollama.

use std::collections::HashSet;

use anyhow::{Context, Result};
use rusqlite::{Connection, ToSql};

/// Candidate pool width multiplier over `k` for the coarse stage (D-02 tuning
/// knob): the coarse arm is approximate, so it must return several times `k` rows
/// for the full-precision rescore to recover the true neighbors.
const COARSE_MULTIPLIER: usize = 8;

/// Floor on the coarse candidate pool (D-02 tuning knob): even a small `k` pulls a
/// wide enough pool that a near-but-not-bit-identical neighbor is not dropped.
const MIN_COARSE_K: usize = 64;

/// One vector hit: the source record it maps back to (`unit_kind`/`source_id`,
/// resolved to a session and archive downstream), its `project`, and the
/// full-precision cosine `distance` from the rescore (lower is a stronger match).
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub unit_kind: String,
    pub source_id: String,
    pub project: String,
    pub distance: f64,
}

/// A precomputed time-window candidate set (D-06): the `(unit_kind, source_id)`
/// pairs whose source record's relational timestamp falls in the window. Computed
/// from the relational tables (no timestamp column on the vector/FTS tables, no
/// re-embed), then handed to `vector_search` to constrain the exact rescore.
pub struct TimeFilter {
    ids: HashSet<(String, String)>,
}

impl TimeFilter {
    /// Compute the candidate set for an RFC3339 `since`/`until` window from the
    /// relational timestamp columns (D-06), keyed to the vector table's id space:
    /// event -> `CAST(event.id AS TEXT)`; artifact -> `artifact_id` whose source
    /// event is in range; entity -> `{entity_type}:{entity}` with any
    /// `mention.timestamp` in range.
    pub fn compute(conn: &Connection, since: Option<&str>, until: Option<&str>) -> Result<Self> {
        let mut ids = HashSet::new();

        // event units: synthetic event.id, keyed as text (profile.rs prose chunk).
        collect_time_ids(
            conn,
            "SELECT CAST(id AS TEXT) FROM event WHERE timestamp IS NOT NULL",
            "timestamp",
            since,
            until,
            "event",
            &mut ids,
        )?;
        // artifact units: artifact_id whose source event falls in the window.
        collect_time_ids(
            conn,
            "SELECT a.artifact_id FROM artifact a
             JOIN event e ON e.uuid = a.source_event_uuid
             WHERE e.timestamp IS NOT NULL",
            "e.timestamp",
            since,
            until,
            "artifact",
            &mut ids,
        )?;
        // entity units: {entity_type}:{entity} groups with any mention in range.
        collect_time_ids(
            conn,
            "SELECT DISTINCT entity_type || ':' || entity FROM mention
             WHERE timestamp IS NOT NULL",
            "timestamp",
            since,
            until,
            "entity",
            &mut ids,
        )?;

        Ok(TimeFilter { ids })
    }

    fn contains(&self, unit_kind: &str, source_id: &str) -> bool {
        self.ids
            .contains(&(unit_kind.to_string(), source_id.to_string()))
    }
}

/// Append the `source_id`s produced by `base_sql` (already carrying a
/// `timestamp IS NOT NULL` guard) constrained to the `since`/`until` window on
/// `ts_col`, each tagged with `unit_kind`, into `ids`.
fn collect_time_ids(
    conn: &Connection,
    base_sql: &str,
    ts_col: &str,
    since: Option<&str>,
    until: Option<&str>,
    unit_kind: &str,
    ids: &mut HashSet<(String, String)>,
) -> Result<()> {
    let mut sql = base_sql.to_string();
    let mut params: Vec<&dyn ToSql> = Vec::new();
    if since.is_some() {
        sql.push_str(&format!(" AND {ts_col} >= ?"));
        params.push(&since);
    }
    if until.is_some() {
        sql.push_str(&format!(" AND {ts_col} <= ?"));
        params.push(&until);
    }
    let mut stmt = conn.prepare(&sql).context("preparing time-filter id query")?;
    let rows = stmt
        .query_map(params.as_slice(), |r| r.get::<_, String>(0))
        .context("running time-filter id query")?;
    for row in rows {
        ids.insert((unit_kind.to_string(), row.context("reading a time-filter id")?));
    }
    Ok(())
}

/// Two-stage vector search (D-02, D-06). Returns up to `k` hits ranked by
/// full-precision cosine distance ascending.
pub fn vector_search(
    conn: &Connection,
    query_vec: &[f32],
    project: Option<&str>,
    time_ids: Option<&TimeFilter>,
    k: usize,
) -> Result<Vec<VectorHit>> {
    let query_blob = vector_blob(query_vec);

    // Candidate rowids: filter-then-rerank when anchored on a time window (no KNN
    // over the whole set), else the binary-coarse KNN pool (D-02, D-06).
    let candidate_rowids = match time_ids {
        Some(tf) => time_filtered_rowids(conn, project, tf)?,
        None => {
            let coarse_k = k.saturating_mul(COARSE_MULTIPLIER).max(MIN_COARSE_K);
            coarse_rowids(conn, &query_blob, project, coarse_k)?
        }
    };

    rescore(conn, &query_blob, &candidate_rowids, k)
}

/// Stage one (unfiltered path): binary-coarse KNN over `embedding_coarse`
/// (hamming), optionally filtered by the `project` metadata column inside the
/// MATCH, returning the candidate rowid pool ordered by coarse distance.
fn coarse_rowids(
    conn: &Connection,
    query_blob: &[u8],
    project: Option<&str>,
    coarse_k: usize,
) -> Result<Vec<i64>> {
    let mut sql = String::from(
        "SELECT rowid FROM vec_embedding WHERE embedding_coarse MATCH vec_quantize_binary(?)",
    );
    let mut params: Vec<&dyn ToSql> = vec![&query_blob];
    if project.is_some() {
        sql.push_str(" AND project = ?");
        params.push(&project);
    }
    // vec0 KNN requires an ascending `ORDER BY distance` and a LIMIT (or `k = ?`).
    sql.push_str(" ORDER BY distance LIMIT ?");
    let ck = coarse_k as i64;
    params.push(&ck);

    let mut stmt = conn.prepare(&sql).context("preparing coarse KNN query")?;
    let rows = stmt
        .query_map(params.as_slice(), |r| r.get::<_, i64>(0))
        .context("running coarse KNN query")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("reading a coarse candidate rowid")?);
    }
    Ok(out)
}

/// Anchored path (D-06): the survivors are the vector rows (optionally
/// project-filtered) whose `(unit_kind, source_id)` is in the precomputed time
/// window. A full scan of the metadata/aux columns filtered in memory, then the
/// exact cosine rescore runs over these rowids alone - no KNN over the whole set.
fn time_filtered_rowids(
    conn: &Connection,
    project: Option<&str>,
    tf: &TimeFilter,
) -> Result<Vec<i64>> {
    let mut sql = String::from("SELECT rowid, unit_kind, source_id FROM vec_embedding");
    let mut params: Vec<&dyn ToSql> = Vec::new();
    if project.is_some() {
        sql.push_str(" WHERE project = ?");
        params.push(&project);
    }
    let mut stmt = conn.prepare(&sql).context("preparing time-filter scan")?;
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .context("running time-filter scan")?;
    let mut out = Vec::new();
    for row in rows {
        let (rowid, unit_kind, source_id) = row.context("reading a time-filter candidate")?;
        if tf.contains(&unit_kind, &source_id) {
            out.push(rowid);
        }
    }
    Ok(out)
}

/// Stage two: full-precision cosine rescore over the candidate rowids (D-02),
/// ordered ascending, limited to `k`.
fn rescore(
    conn: &Connection,
    query_blob: &[u8],
    rowids: &[i64],
    k: usize,
) -> Result<Vec<VectorHit>> {
    if rowids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = rowids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT unit_kind, source_id, project, vec_distance_cosine(embedding, ?) AS dist
         FROM vec_embedding WHERE rowid IN ({placeholders}) ORDER BY dist ASC LIMIT ?"
    );
    // Param order matches the SQL text: the query blob first (vec_distance_cosine),
    // then each rowid (the IN list), then the k limit.
    let mut params: Vec<&dyn ToSql> = Vec::with_capacity(rowids.len() + 2);
    params.push(&query_blob);
    for rowid in rowids {
        params.push(rowid);
    }
    let k_i = k as i64;
    params.push(&k_i);

    let mut stmt = conn.prepare(&sql).context("preparing cosine rescore query")?;
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok(VectorHit {
                unit_kind: r.get(0)?,
                source_id: r.get(1)?,
                project: r.get(2)?,
                distance: r.get(3)?,
            })
        })
        .context("running cosine rescore query")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("reading a rescored vector hit")?);
    }
    Ok(out)
}

/// Serialize an f32 slice to the little-endian byte blob sqlite-vec expects for a
/// `float[N]` vector column (matches embed/mod.rs::vector_blob and schema.rs).
fn vector_blob(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open_db;

    const DIM: usize = 4096;

    /// Insert one vec_embedding row (coarse+full from the same float vector),
    /// mirroring the schema.rs/embed round-trip insert pattern.
    fn insert_vec(
        conn: &Connection,
        vector: &[f32],
        project: &str,
        unit_kind: &str,
        source_id: &str,
    ) {
        let blob = vector_blob(vector);
        conn.execute(
            "INSERT INTO vec_embedding(embedding_coarse, embedding, project, unit_kind, source_id)
             VALUES (vec_quantize_binary(?1), ?1, ?2, ?3, ?4)",
            rusqlite::params![blob, project, unit_kind, source_id],
        )
        .unwrap();
    }

    /// The exact-vector query ranks its own row first, and a `project` filter
    /// narrows the result to a strict subset carrying that project (D-02, D-06).
    #[test]
    fn exact_vector_ranks_first_and_project_narrows() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        let a = vec![0.25_f32; DIM];
        let b = vec![0.90_f32; DIM];
        let mut c = vec![0.10_f32; DIM];
        for x in c.iter_mut().take(DIM / 2) {
            *x = -0.40;
        }
        insert_vec(&conn, &a, "proj-1", "event", "1");
        insert_vec(&conn, &b, "proj-2", "event", "2");
        insert_vec(&conn, &c, "proj-1", "entity", "file:x");

        let hits = vector_search(&conn, &a, None, None, 3).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].source_id, "1", "the exact-vector row ranks first");

        let p1 = vector_search(&conn, &a, Some("proj-1"), None, 3).unwrap();
        assert!(
            p1.iter().all(|h| h.project == "proj-1"),
            "project filter yields only proj-1 rows"
        );
        assert!(
            !p1.iter().any(|h| h.source_id == "2"),
            "the proj-2 row is excluded (strict subset)"
        );
    }

    /// Two-stage recall (D-02): a row whose full-precision vector is the true
    /// cosine-nearest neighbor of the query but whose binary quantization is NOT
    /// bit-identical to the query survives the coarse stage into the rescore and
    /// ranks first. If the coarse pool were too tight it would be dropped and this
    /// assertion would fail, flagging `COARSE_MULTIPLIER`/`MIN_COARSE_K`.
    #[test]
    fn near_but_not_bit_identical_neighbor_survives_coarse() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        // Query is all +1: its binary quantization is all-ones.
        let query = vec![1.0_f32; DIM];

        // Near neighbor N: same direction on all but the last 6 dims, which flip
        // sign. cosine(query, N) ~ 0.997 (the true nearest), but binary(N) differs
        // from binary(query) in 6 bits, so it is NOT bit-identical at the coarse
        // stage. No all-+1 row exists, so N is genuinely the nearest.
        let mut near = vec![1.0_f32; DIM];
        for x in near.iter_mut().skip(DIM - 6) {
            *x = -1.0;
        }
        // Decoy D: orthogonal-ish (second half negated), cosine ~ 0, hamming huge.
        let mut decoy = vec![1.0_f32; DIM];
        for x in decoy.iter_mut().skip(DIM / 2) {
            *x = -1.0;
        }

        insert_vec(&conn, &near, "proj", "event", "near");
        insert_vec(&conn, &decoy, "proj", "event", "decoy");

        let hits = vector_search(&conn, &query, None, None, 2).unwrap();
        assert!(!hits.is_empty(), "the true neighbor was not dropped by the coarse stage");
        assert_eq!(
            hits[0].source_id, "near",
            "the full-precision nearest neighbor ranks first after rescore"
        );
        assert!(
            hits[0].distance < hits.iter().find(|h| h.source_id == "decoy").map_or(f64::MAX, |h| h.distance),
            "the near neighbor's cosine distance is smaller than the decoy's"
        );
    }

    /// The anchored time path (D-06): a `TimeFilter` restricts the rescore to the
    /// rows whose `(unit_kind, source_id)` is in the window, no KNN over the set.
    #[test]
    fn time_filter_restricts_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        let v = vec![0.25_f32; DIM];
        insert_vec(&conn, &v, "proj", "event", "1");
        insert_vec(&conn, &v, "proj", "event", "2");

        // Build a TimeFilter by hand admitting only event source_id "1".
        let mut ids = HashSet::new();
        ids.insert(("event".to_string(), "1".to_string()));
        let tf = TimeFilter { ids };

        let hits = vector_search(&conn, &v, None, Some(&tf), 5).unwrap();
        assert_eq!(hits.len(), 1, "only the in-window row survives");
        assert_eq!(hits[0].source_id, "1");
    }
}
