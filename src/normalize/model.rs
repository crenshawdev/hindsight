//! The four normalized record types and the tagged-NDJSON emit primitive.
//!
//! Normalize turns an archived transcript into `Session` / `Event` / `Artifact`
//! / `Mention` records (the data-model diagram). Each serializes as one JSON
//! object per line carrying a `type` field so `hindsight normalize <dir> | jq`
//! inspects the stream and Phase 3 loads the same bytes (D-03).

use std::io::Write;

use anyhow::{Context, Result};
use serde::Serialize;

/// Three-tier grain (D-07). Controls how much of an event reaches the index:
/// `indexed` keeps full text, `skeleton` keeps only structural signal (the body
/// blanked), `archive-only` produces no record at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grain {
    Indexed,
    Skeleton,
    ArchiveOnly,
}

/// One logical session (D-05): the parent generation plus every nested
/// `subagents/` generation sharing the parent `sessionId`.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    Session(Session),
    Event(Event),
    Artifact(Artifact),
    Mention(Mention),
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
