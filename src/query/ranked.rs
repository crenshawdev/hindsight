//! RRF fusion of the keyword and vector arms at session granularity (QRY-02,
//! D-03, D-05). Both arms and the exact-listing path already speak `session_id`,
//! and an `entity` vector unit is a cross-session aggregate with no single record
//! key, so fusing at `session_id` accommodates all three unit kinds uniformly and
//! matches the recall use case (which sessions are relevant). Lost within-session
//! ordering is recovered by the resolve path (Task 7), which re-pinpoints the
//! record.
//!
//! Two id-space facts bind the reconciliation (verified against the loader and
//! profile code):
//! - The `event` unit lives in two id spaces: FTS keys it by `event.uuid`, the
//!   vector arm by the synthetic `CAST(event.id AS TEXT)`. So a fused session
//!   carries a CANONICAL resolvable target (a uuid / artifact_id), never the
//!   arm-native `source_id`: a vector event `source_id` is translated to its
//!   `event.uuid` before annotating.
//! - The `entity` unit has no record key, but `mention` carries `event_uuid`, so
//!   an entity-ranked session resolves to a representative `mention.event_uuid`.
//!
//! Strict-subset invariant (D-06): the entity -> session remap MUST carry the
//! active `project`/time predicate. An entity's `mention` rows span sessions and
//! projects, so remapping to ALL of them would re-widen past a `--project`/time
//! anchor the vector arm already applied - the remap joins only `mention` rows
//! that themselves satisfy the filter.
//!
//! Fuzzy fallback (D-05): when the injected query-embed closure returns `Err`, the
//! vector arm is skipped, the keyword arm alone is fused, and `degraded` is set
//! with the reason - the query never errors on an unreachable embedder.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, ToSql};

use crate::query::keyword::{keyword_search, KeywordHit};
use crate::query::vector::{vector_search, TimeFilter, VectorHit};

/// Reciprocal-rank-fusion constant (D-03): `score = sum_arms 1/(RRF_K + rank)`.
/// 60 is the standard RRF damping.
const RRF_K: usize = 60;

/// How many vector hits the ranked search pulls before fusion.
const RANKED_K: usize = 50;

/// The canonical resolvable target annotated onto a fused session (id-space
/// facts): always an `event.uuid` or an `artifact_id` Task 7 can pinpoint, never
/// an arm-native synthetic id or a bare `{type}:{name}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveTarget {
    /// `"event"` or `"artifact"`.
    pub source_type: String,
    /// An `event.uuid` (for `event`) or an `artifact_id` (for `artifact`).
    pub source_id: String,
}

/// One fused session: its id, its RRF score (higher is stronger), and the
/// canonical resolvable target the resolve path pinpoints.
#[derive(Debug, Clone)]
pub struct RankedSession {
    pub session_id: String,
    pub score: f64,
    pub target: ResolveTarget,
}

/// The fused result. `degraded` is set when the vector arm was skipped because the
/// query-embed closure failed (D-05); `degraded_reason` carries why.
#[derive(Debug, Clone)]
pub struct RankedResult {
    pub sessions: Vec<RankedSession>,
    pub degraded: bool,
    pub degraded_reason: Option<String>,
}

/// Ranked fuzzy search fusing the keyword arm and (when the embed closure
/// succeeds) the vector arm by RRF at session granularity. `embed_query` is the
/// injectable query-embed seam (D-04, D-05): the MCP path supplies
/// `ollama::embed_query`; tests and the fallback drive it without Ollama.
pub fn ranked_search<F>(
    conn: &Connection,
    query: &str,
    project: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    embed_query: F,
) -> Result<RankedResult>
where
    F: FnOnce(&str) -> Result<Vec<f32>>,
{
    // Keyword arm: always runs, needs no embedder.
    let keyword_hits = keyword_search(conn, query, project, since, until)?;
    let keyword_entries = keyword_entries(keyword_hits);

    // Vector arm: only when the query embed succeeds; otherwise degrade to
    // keyword-only and report it (D-05).
    let (vector_entries, degraded, degraded_reason) = match embed_query(query) {
        Ok(query_vec) => {
            let time_filter = if since.is_some() || until.is_some() {
                Some(TimeFilter::compute(conn, since, until)?)
            } else {
                None
            };
            let hits = vector_search(conn, &query_vec, project, time_filter.as_ref(), RANKED_K)?;
            let entries = vector_entries(conn, hits, project, since, until)?;
            (entries, false, None)
        }
        Err(e) => (Vec::new(), true, Some(format!("{e:#}"))),
    };

    let sessions = fuse(&[keyword_entries, vector_entries]);
    Ok(RankedResult {
        sessions,
        degraded,
        degraded_reason,
    })
}

/// Fuse the arms' ordered `(session_id, target)` lists by RRF at session
/// granularity. Each arm is collapsed to a per-session best (first-appearance)
/// rank, and scores accumulate across arms. A session's target is recorded from
/// the first arm that contributes it (both arms yield resolvable targets).
fn fuse(arms: &[Vec<(String, ResolveTarget)>]) -> Vec<RankedSession> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut targets: HashMap<String, ResolveTarget> = HashMap::new();

    for arm in arms {
        let mut ranked_in_arm: HashMap<String, usize> = HashMap::new();
        let mut next_rank = 1usize;
        for (session_id, target) in arm {
            if ranked_in_arm.contains_key(session_id) {
                continue; // per-session best (lowest) rank = first appearance.
            }
            ranked_in_arm.insert(session_id.clone(), next_rank);
            *scores.entry(session_id.clone()).or_insert(0.0) +=
                1.0 / ((RRF_K + next_rank) as f64);
            targets
                .entry(session_id.clone())
                .or_insert_with(|| target.clone());
            next_rank += 1;
        }
    }

    let mut out: Vec<RankedSession> = scores
        .into_iter()
        .map(|(session_id, score)| {
            let target = targets.remove(&session_id).expect("every scored session has a target");
            RankedSession {
                session_id,
                score,
                target,
            }
        })
        .collect();
    // Descending score; deterministic session_id tie-break.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out
}

/// Keyword hits already speak `session_id` and a resolvable `source_id`
/// (event.uuid or artifact_id, D-03), so each maps straight to a canonical target.
fn keyword_entries(hits: Vec<KeywordHit>) -> Vec<(String, ResolveTarget)> {
    hits.into_iter()
        .map(|h| {
            let target = ResolveTarget {
                source_type: h.source_type,
                source_id: h.source_id,
            };
            (h.session_id, target)
        })
        .collect()
}

/// Map each vector hit to its session(s) and a canonical resolvable target,
/// translating the arm-native id space (D-03) and carrying the `project`/time
/// predicate on the entity remap (strict-subset invariant, D-06).
fn vector_entries(
    conn: &Connection,
    hits: Vec<VectorHit>,
    project: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<(String, ResolveTarget)>> {
    let mut out = Vec::new();
    for hit in hits {
        match hit.unit_kind.as_str() {
            // Synthetic event.id -> (event.session_id, event.uuid).
            "event" => {
                let id: i64 = match hit.source_id.parse() {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let row: Option<(String, String)> = conn
                    .query_row(
                        "SELECT session_id, uuid FROM event WHERE id = ?1",
                        rusqlite::params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                    .context("mapping vector event id to session")?;
                if let Some((session_id, uuid)) = row {
                    out.push((
                        session_id,
                        ResolveTarget {
                            source_type: "event".to_string(),
                            source_id: uuid,
                        },
                    ));
                }
            }
            // artifact_id -> its source event's session; target stays the artifact.
            "artifact" => {
                let session_id: Option<String> = conn
                    .query_row(
                        "SELECT e.session_id FROM artifact a
                         JOIN event e ON e.uuid = a.source_event_uuid
                         WHERE a.artifact_id = ?1 LIMIT 1",
                        rusqlite::params![hit.source_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .context("mapping vector artifact to session")?;
                if let Some(session_id) = session_id {
                    out.push((
                        session_id,
                        ResolveTarget {
                            source_type: "artifact".to_string(),
                            source_id: hit.source_id,
                        },
                    ));
                }
            }
            // {entity_type}:{entity} -> the sessions whose mention rows satisfy the
            // active filter (strict-subset invariant), each with a representative
            // resolvable mention.event_uuid.
            "entity" => {
                let (entity_type, entity) = match hit.source_id.split_once(':') {
                    Some(pair) => pair,
                    None => continue,
                };
                let mut sql = String::from(
                    "SELECT session_id, event_uuid FROM mention
                     WHERE entity_type = ? AND entity = ? AND event_uuid IS NOT NULL",
                );
                let (entity_type, entity) = (entity_type.to_string(), entity.to_string());
                let mut params: Vec<&dyn ToSql> = vec![&entity_type, &entity];
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
                // One representative event_uuid per session, deterministic order.
                sql.push_str(" GROUP BY session_id ORDER BY session_id ASC");

                let mut stmt = conn.prepare(&sql).context("preparing entity remap query")?;
                let rows = stmt
                    .query_map(params.as_slice(), |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })
                    .context("running entity remap query")?;
                for row in rows {
                    let (session_id, event_uuid) = row.context("reading an entity remap row")?;
                    out.push((
                        session_id,
                        ResolveTarget {
                            source_type: "event".to_string(),
                            source_id: event_uuid,
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open_db;

    const DIM: usize = 4096;

    fn vector_blob(v: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(v.len() * 4);
        for x in v {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        bytes
    }

    fn seed_session(conn: &Connection, sid: &str, project: &str) {
        conn.execute(
            "INSERT INTO session(session_id, project) VALUES (?1, ?2)",
            rusqlite::params![sid, project],
        )
        .unwrap();
    }

    /// Insert an event (+ its FTS row for the keyword arm) and return its id.
    fn seed_event(conn: &Connection, uuid: &str, sid: &str, ts: &str, text: &str) -> i64 {
        conn.execute(
            "INSERT INTO event(uuid, session_id, is_sidechain, grain, timestamp, text)
             VALUES (?1, ?2, 0, 'indexed', ?3, ?4)",
            rusqlite::params![uuid, sid, ts, text],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO fts(content, session_id, source_type, source_id)
             VALUES (?1, ?2, 'event', ?3)",
            rusqlite::params![text, sid, uuid],
        )
        .unwrap();
        id
    }

    fn insert_vec(conn: &Connection, v: &[f32], project: &str, unit_kind: &str, source_id: &str) {
        let blob = vector_blob(v);
        conn.execute(
            "INSERT INTO vec_embedding(embedding_coarse, embedding, project, unit_kind, source_id)
             VALUES (vec_quantize_binary(?1), ?1, ?2, ?3, ?4)",
            rusqlite::params![blob, project, unit_kind, source_id],
        )
        .unwrap();
    }

    fn seed_mention(
        conn: &Connection,
        entity: &str,
        entity_type: &str,
        event_uuid: &str,
        sid: &str,
        project: &str,
        ts: &str,
    ) {
        conn.execute(
            "INSERT INTO mention(entity, entity_type, event_uuid, session_id, project, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![entity, entity_type, event_uuid, sid, project, ts],
        )
        .unwrap();
    }

    /// (a) Fusion draws from both arms, and a `project` filter is a strict subset.
    #[test]
    fn fuses_both_arms_and_project_narrows() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_session(&conn, "s1", "proj-1");
        seed_session(&conn, "s2", "proj-2");
        // Keyword arm: an indexed term in each session.
        let e1 = seed_event(&conn, "ev1", "s1", "2026-07-21T10:00:00Z", "deploy the widget");
        let _e2 = seed_event(&conn, "ev2", "s2", "2026-07-22T10:00:00Z", "deploy the other");
        // Vector arm: an event vector for s1 (via e1's id) with a known vector.
        let qvec = vec![0.25_f32; DIM];
        insert_vec(&conn, &qvec, "proj-1", "event", &e1.to_string());

        let result = ranked_search(&conn, "deploy", None, None, None, |_q| Ok(qvec.clone())).unwrap();
        assert!(!result.degraded, "embed succeeded, not degraded");
        assert!(result.sessions.iter().any(|s| s.session_id == "s1"), "keyword+vector session present");
        assert!(result.sessions.iter().any(|s| s.session_id == "s2"), "keyword-only session present");
        // s1 draws from BOTH arms, so it outscores the keyword-only s2.
        let s1 = result.sessions.iter().find(|s| s.session_id == "s1").unwrap();
        let s2 = result.sessions.iter().find(|s| s.session_id == "s2").unwrap();
        assert!(s1.score > s2.score, "the two-arm session outranks the one-arm session");

        // A project filter narrows to a strict subset.
        let p1 = ranked_search(&conn, "deploy", Some("proj-1"), None, None, |_q| Ok(qvec.clone())).unwrap();
        assert!(p1.sessions.iter().any(|s| s.session_id == "s1"));
        assert!(!p1.sessions.iter().any(|s| s.session_id == "s2"), "proj-2 session excluded");
    }

    /// (b) Fallback: a failing embed closure degrades to keyword-only, nonzero, Ok.
    #[test]
    fn embed_failure_degrades_to_keyword_only() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_session(&conn, "s1", "proj-1");
        seed_event(&conn, "ev1", "s1", "2026-07-21T10:00:00Z", "deploy the widget");

        let result = ranked_search(&conn, "deploy", None, None, None, |_q| {
            Err(anyhow::anyhow!("ollama unreachable"))
        })
        .unwrap();

        assert!(result.degraded, "a failed embed sets degraded");
        assert!(result.degraded_reason.is_some(), "the degradation reason is reported");
        assert!(!result.sessions.is_empty(), "keyword-only results are still nonzero");
        assert!(result.sessions.iter().any(|s| s.session_id == "s1"));
    }

    /// (c) Strict-subset leak: an entity vector unit whose mention rows span an
    /// in-anchor session A and an out-of-anchor session B must not re-widen to B.
    #[test]
    fn entity_remap_does_not_rewiden_past_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_session(&conn, "sess-a", "proj-1");
        seed_session(&conn, "sess-b", "proj-2");
        // The events the representative mention.event_uuid resolves against.
        seed_event(&conn, "ea", "sess-a", "2026-07-21T10:00:00Z", "in anchor");
        seed_event(&conn, "eb", "sess-b", "2026-07-22T10:00:00Z", "out of anchor");
        // One entity, mentions in both sessions/projects.
        seed_mention(&conn, "foo", "file", "ea", "sess-a", "proj-1", "2026-07-21T10:00:00Z");
        seed_mention(&conn, "foo", "file", "eb", "sess-b", "proj-2", "2026-07-22T10:00:00Z");
        // Its vector carries project proj-1 so the anchored vector arm returns it.
        let qvec = vec![0.25_f32; DIM];
        insert_vec(&conn, &qvec, "proj-1", "entity", "file:foo");

        let result =
            ranked_search(&conn, "anything", Some("proj-1"), None, None, |_q| Ok(qvec.clone())).unwrap();
        assert!(result.sessions.iter().any(|s| s.session_id == "sess-a"), "in-anchor session A returned");
        assert!(
            !result.sessions.iter().any(|s| s.session_id == "sess-b"),
            "out-of-anchor session B must NOT be re-widened in via the shared entity"
        );
    }

    /// (d) Annotation resolvability: a vector-event-ranked session carries an
    /// `(event, uuid)` target whose uuid exists in `event`, and an entity-ranked
    /// session carries an `(event, mention.event_uuid)` target, never a bare
    /// `{type}:{name}`.
    #[test]
    fn fused_sessions_carry_resolvable_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("hs.db")).unwrap();

        seed_session(&conn, "s1", "proj");
        seed_session(&conn, "s2", "proj");
        // s1 ranked by the vector event arm.
        let e1 = seed_event(&conn, "ev1", "s1", "2026-07-21T10:00:00Z", "event body");
        // s2 ranked by the vector entity arm (its representative event uuid = ev2).
        seed_event(&conn, "ev2", "s2", "2026-07-21T11:00:00Z", "entity body");
        seed_mention(&conn, "foo", "file", "ev2", "s2", "proj", "2026-07-21T11:00:00Z");

        let qvec = vec![0.25_f32; DIM];
        insert_vec(&conn, &qvec, "proj", "event", &e1.to_string());
        insert_vec(&conn, &qvec, "proj", "entity", "file:foo");

        let result = ranked_search(&conn, "zzz-no-keyword-match", None, None, None, |_q| Ok(qvec.clone()))
            .unwrap();

        let s1 = result.sessions.iter().find(|s| s.session_id == "s1").expect("s1 present");
        assert_eq!(s1.target.source_type, "event");
        assert_eq!(s1.target.source_id, "ev1", "vector event id translated to its uuid");
        let uuid_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM event WHERE uuid = ?1",
                rusqlite::params![s1.target.source_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(uuid_exists > 0, "the annotated uuid exists in event");

        let s2 = result.sessions.iter().find(|s| s.session_id == "s2").expect("s2 present");
        assert_eq!(s2.target.source_type, "event", "entity resolves to a representative event");
        assert_eq!(s2.target.source_id, "ev2", "the representative mention.event_uuid, not file:foo");
        assert!(!s2.target.source_id.contains(':'), "never a bare {{type}}:{{name}}");
    }
}
