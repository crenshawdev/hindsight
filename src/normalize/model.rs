//! The four normalized record types and the tagged-NDJSON emit primitive.
//!
//! Normalize turns an archived transcript into `Session` / `Event` / `Artifact`
//! / `Mention` records (the data-model diagram). Each serializes as one JSON
//! object per line carrying a `type` field so `hindsight normalize <dir> | jq`
//! inspects the stream and Phase 3 loads the same bytes (D-03).

use std::io::Write;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Three-tier grain (D-07). Controls how much of an event reaches the index:
/// `indexed` keeps full text, `skeleton` keeps only structural signal (the body
/// blanked), `archive-only` produces no record at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grain {
    Indexed,
    Skeleton,
    ArchiveOnly,
}

/// One logical session (D-05): the parent generation plus every nested
/// `subagents/` generation sharing the parent `sessionId`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub project: String,
    pub git_branch: Option<String>,
    pub cc_version: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    /// No single obvious source field in the transcript; left null this phase
    /// (flagged assumption) and populated later.
    pub end_reason: Option<String>,
    pub title: Option<String>,
    /// Generation filenames read for this session, in sorted order.
    pub archive_refs: Vec<String>,
}

/// One turn-fragment: a single content block (or a whole bare-string message)
/// mapped mechanically from a transcript line (D-06).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    pub session_id: String,
    pub role: String,
    /// Content kind: `text` / `thinking` / `tool_use` / `tool_result`.
    pub kind: String,
    pub timestamp: Option<String>,
    pub text: Option<String>,
    pub tool_name: Option<String>,
    /// The tool_result error flag that skeleton grain must retain (D-07).
    pub is_error: Option<bool>,
    /// Whichever of attributionSkill / attributionAgent / attributionPlugin is set.
    pub attribution: Option<String>,
    pub is_sidechain: bool,
    pub agent_id: Option<String>,
    /// From the spawning `Agent` tool_use's `input.subagent_type`; best-effort.
    pub agent_type: Option<String>,
    pub grain: Grain,
}

/// A file body or code snippet produced in the run (from tool-call INPUTS and
/// answer text, never tool_result bodies).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_id: String,
    pub kind: String,
    pub path: Option<String>,
    pub language: Option<String>,
    pub content: String,
    /// The uuid of the nearest preceding user-prompt event (None if none seen).
    pub request_bundle: Option<String>,
    pub source_event_uuid: String,
}

/// A high-confidence structural reference: a file path or a command name (D-10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    pub entity: String,
    /// `file` or `command` this phase.
    pub entity_type: String,
    pub event_uuid: String,
    pub session_id: String,
    pub project: String,
    pub timestamp: Option<String>,
}

/// The tagged wrapper: serializes each record with a `type` discriminant so the
/// NDJSON stream is self-describing (D-03).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    Session(Session),
    Event(Event),
    Artifact(Artifact),
    Mention(Mention),
}

/// Scrub fixed-pattern secrets from the free-text indexed fields only (D-08):
/// Event `text` on indexed events and Artifact `content`. Skeleton bodies are
/// already blanked, Mention entities are structural identifiers left intact, and
/// the archive is never touched by this command.
pub fn scrub_indexed(records: &mut [Record]) {
    for record in records {
        match record {
            Record::Event(event) if event.grain == Grain::Indexed => {
                if let Some(text) = event.text.take() {
                    event.text = Some(super::scrub::scrub(&text));
                }
            }
            Record::Artifact(artifact) => {
                artifact.content = super::scrub::scrub(&artifact.content);
            }
            _ => {}
        }
    }
}

/// Write each record as one compact JSON line (tagged NDJSON).
pub fn write_ndjson<W: Write>(records: &[Record], w: &mut W) -> Result<()> {
    for record in records {
        let line = serde_json::to_string(record).context("serializing record to JSON")?;
        w.write_all(line.as_bytes())
            .and_then(|()| w.write_all(b"\n"))
            .context("writing NDJSON line")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every emitted line round-trips through `Deserialize` back into the same
    /// `Record`, so the loader (D-03) parses exactly what normalize emits and
    /// cannot drift from the shared definition.
    #[test]
    fn ndjson_records_round_trip_byte_identical() {
        let records = vec![
            Record::Session(Session {
                session_id: "sess-1".to_string(),
                project: "proj".to_string(),
                git_branch: Some("feat/x".to_string()),
                cc_version: Some("1.2.3".to_string()),
                started_at: Some("2026-07-21T10:00:00Z".to_string()),
                ended_at: Some("2026-07-21T10:05:00Z".to_string()),
                end_reason: None,
                title: Some("A Test Session".to_string()),
                archive_refs: vec!["0001.zst".to_string(), "subagents/a/0001.zst".to_string()],
            }),
            Record::Event(Event {
                uuid: "u-1".to_string(),
                parent_uuid: Some("u-0".to_string()),
                session_id: "sess-1".to_string(),
                role: "assistant".to_string(),
                kind: "text".to_string(),
                timestamp: Some("2026-07-21T10:05:00Z".to_string()),
                text: Some("hello".to_string()),
                tool_name: None,
                is_error: None,
                attribution: None,
                is_sidechain: false,
                agent_id: None,
                agent_type: None,
                grain: Grain::Indexed,
            }),
            Record::Artifact(Artifact {
                artifact_id: "art-1".to_string(),
                kind: "file".to_string(),
                path: Some("src/main.rs".to_string()),
                language: Some("rust".to_string()),
                content: "fn main() {}".to_string(),
                request_bundle: Some("u-0".to_string()),
                source_event_uuid: "u-1".to_string(),
            }),
            Record::Mention(Mention {
                entity: "src/main.rs".to_string(),
                entity_type: "file".to_string(),
                event_uuid: "u-1".to_string(),
                session_id: "sess-1".to_string(),
                project: "proj".to_string(),
                timestamp: Some("2026-07-21T10:05:00Z".to_string()),
            }),
        ];

        let mut buf: Vec<u8> = Vec::new();
        write_ndjson(&records, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();

        for line in text.lines() {
            let parsed: Record = serde_json::from_str(line).expect("line parses back into Record");
            let reserialized = serde_json::to_string(&parsed).unwrap();
            assert_eq!(reserialized, line, "record re-serializes byte-identically");
        }
    }
}
