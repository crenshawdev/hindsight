//! Hit resolution to verbatim archived bytes (QRY-03, D-07, D-08). A ranked or
//! listed hit carries a canonical resolvable target (`event`+uuid or
//! `artifact`+artifact_id, guaranteed by the Task 5 annotation). Resolution:
//! translate the target to an event uuid, read the session's `archive_refs` (as
//! stored, never by re-walking the archive tree), decompress each referenced
//! generation, and pinpoint the line whose uuid matches - returning its ORIGINAL
//! raw bytes (D-08 verbatim constraint, see `normalize::pinpoint`).

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::archive;
use crate::config::Config;
use crate::normalize;

/// Resolve a hit to the verbatim archived bytes of its pinpointed record.
///
/// `source_type` is `"event"` (then `source_id` is already the `event.uuid` the
/// Task 5 annotation guarantees) or `"artifact"` (then `source_id` is an
/// `artifact_id`, translated to its `source_event_uuid`; the whole line carrying
/// the tool_use satisfies "appears byte-for-byte in the source"). The line's
/// verbatim bytes are returned, never scrubbed or re-serialized.
pub fn resolve(
    conn: &Connection,
    cfg: &Config,
    session_id: &str,
    source_type: &str,
    source_id: &str,
) -> Result<Vec<u8>> {
    // 1. Translate the target to an event uuid.
    let uuid: String = match source_type {
        "event" => source_id.to_string(),
        "artifact" => conn
            .query_row(
                "SELECT source_event_uuid FROM artifact WHERE artifact_id = ?1",
                rusqlite::params![source_id],
                |r| r.get(0),
            )
            .optional()
            .context("looking up artifact source_event_uuid")?
            .with_context(|| format!("no artifact row for artifact_id {source_id}"))?,
        other => bail!("cannot resolve unknown source_type {other:?}"),
    };

    // 2. Read the session's project and archive_refs (JSON array, as stored).
    let (project, archive_refs_json): (String, String) = conn
        .query_row(
            "SELECT project, archive_refs FROM session WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("reading session project/archive_refs")?
        .with_context(|| format!("no session row for session_id {session_id}"))?;
    let refs: Vec<String> = serde_json::from_str(&archive_refs_json)
        .with_context(|| format!("parsing archive_refs of session {session_id}"))?;

    // 3. Pinpoint the uuid line across the session's generations (D-07, D-08).
    //    Resolve against archive_refs as stored, never by re-walking the tree.
    for gen_ref in &refs {
        let bytes = archive::read_generation(cfg, &project, session_id, gen_ref)?;
        if let Some(found) = normalize::pinpoint(&bytes, &uuid) {
            return Ok(found);
        }
    }

    bail!(
        "no line with uuid {uuid} found in {} archive generation(s) of session {session_id}",
        refs.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Kind;
    use crate::store::open_db;
    use serde_json::Value;
    use std::path::Path;

    fn test_config(base: &Path) -> Config {
        Config::from_toml_str(&format!("base_dir = {:?}\nidle_timeout_secs = 5\n", base)).unwrap()
    }

    /// QRY-03 end to end: a transcript line with a Write tool_use, whose keys are
    /// in transcript order (`type` before `uuid`, NOT alphabetical), is archived,
    /// normalized+loaded, and resolved through the artifact hit. The returned bytes
    /// appear byte-for-byte in the source generation, and a re-serialized `Value`
    /// of the same line does NOT byte-match - proving the raw-bytes path is
    /// load-bearing, not incidental.
    #[test]
    fn resolves_artifact_to_verbatim_source_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());

        // Keys in transcript order (type, uuid, timestamp, sessionId, message) -
        // deliberately NOT alphabetical, so a BTreeMap round-trip would reorder.
        let line = r#"{"type":"assistant","uuid":"line-uuid-1","timestamp":"2026-07-21T10:00:00Z","sessionId":"sess-1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/repo/src/x.rs","content":"fn x() {}"}}]}}"#;
        let transcript = format!("{line}\n");
        let src = tmp.path().join("source.jsonl");
        std::fs::write(&src, &transcript).unwrap();

        // Archive it, then normalize | load into the temp DB (the real pipeline).
        archive::write_generation(&cfg, "proj", "sess-1", "", &src, Kind::Sweep).unwrap();
        let session_dir = cfg.archive_dir().join("proj").join("sess-1");
        let mut ndjson: Vec<u8> = Vec::new();
        normalize::run_to(&session_dir, &mut ndjson).unwrap();
        crate::store::load::run_from(&cfg, &ndjson[..]).unwrap();

        let conn = open_db(&cfg.db_path()).unwrap();
        let artifact_id: String = conn
            .query_row("SELECT artifact_id FROM artifact LIMIT 1", [], |r| r.get(0))
            .expect("the Write tool_use produced an artifact");

        let resolved = resolve(&conn, &cfg, "sess-1", "artifact", &artifact_id).unwrap();

        // The resolved bytes appear byte-for-byte in the source generation.
        let generation = archive::read_generation(&cfg, "proj", "sess-1", "0000.zst").unwrap();
        assert!(
            generation
                .windows(resolved.len())
                .any(|w| w == resolved.as_slice()),
            "resolved bytes must appear byte-for-byte in the source generation"
        );
        // And they are exactly the raw line (no trailing newline).
        assert_eq!(resolved, line.as_bytes(), "resolved bytes are the verbatim raw line");

        // Re-serializing the same line through a serde_json::Value does NOT
        // byte-match: preserve_order is off, so keys re-emit alphabetically.
        let value: Value = serde_json::from_slice(&resolved).unwrap();
        let reserialized = serde_json::to_vec(&value).unwrap();
        assert_ne!(
            reserialized, resolved,
            "a re-serialized Value reorders keys, so the raw-bytes path is load-bearing"
        );
    }

    /// An event target resolves directly by its uuid (no artifact translation).
    #[test]
    fn resolves_event_target_by_uuid() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());

        let line = r#"{"type":"user","uuid":"u-42","timestamp":"2026-07-21T10:00:00Z","sessionId":"sess-1","message":{"role":"user","content":"hello world"}}"#;
        let transcript = format!("{line}\n");
        let src = tmp.path().join("source.jsonl");
        std::fs::write(&src, &transcript).unwrap();

        archive::write_generation(&cfg, "proj", "sess-1", "", &src, Kind::Sweep).unwrap();
        let session_dir = cfg.archive_dir().join("proj").join("sess-1");
        let mut ndjson: Vec<u8> = Vec::new();
        normalize::run_to(&session_dir, &mut ndjson).unwrap();
        crate::store::load::run_from(&cfg, &ndjson[..]).unwrap();

        let conn = open_db(&cfg.db_path()).unwrap();
        let resolved = resolve(&conn, &cfg, "sess-1", "event", "u-42").unwrap();
        assert_eq!(resolved, line.as_bytes(), "the event uuid resolves to its verbatim line");
    }
}
