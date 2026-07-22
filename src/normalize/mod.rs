//! `hindsight normalize`: read an archived session directory's generations and
//! emit tagged NDJSON (Session / Event / Artifact / Mention) to stdout (D-01,
//! D-03). Not wired into the sweep this phase (Phase 6 does that); the argument
//! is a direct archive directory path so the command is inspectable without a
//! config file.

mod extract;
mod grain;
pub(crate) mod model;
mod parse;
mod scrub;

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use model::{Record, Session};

/// A decompressed generation: its filename and its parsed non-empty lines.
struct Generation {
    filename: String,
    lines: Vec<Value>,
}

/// Entry point: normalize the archived session at `session_dir` to stdout.
pub fn run(session_dir: &Path) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    run_to(session_dir, &mut lock)
}

/// Inner form writing to an arbitrary sink, so tests can capture the stream.
pub(crate) fn run_to<W: Write>(session_dir: &Path, w: &mut W) -> Result<()> {
    let project = session_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            anyhow!(
                "cannot derive project from session dir {}",
                session_dir.display()
            )
        })?
        .to_string();

    // One logical Session = the parent generations plus every nested
    // `subagents/` generation under the same session dir (D-05).
    let generations = collect_generations(session_dir)?;

    let session = build_session(&project, &generations)?;
    let session_id = session.session_id.clone();

    let gen_lines: Vec<Vec<Value>> = generations.into_iter().map(|g| g.lines).collect();
    let events = parse::assemble_events(&gen_lines, &session_id);
    let (mentions, artifacts) = extract::extract(&gen_lines, &session_id, &project);

    let mut records = Vec::with_capacity(1 + events.len() + artifacts.len() + mentions.len());
    records.push(Record::Session(session));
    records.extend(events.into_iter().map(Record::Event));
    records.extend(artifacts.into_iter().map(Record::Artifact));
    records.extend(mentions.into_iter().map(Record::Mention));

    // Scrub fixed-pattern secrets from indexed free-text before emission (D-08).
    model::scrub_indexed(&mut records);
    model::write_ndjson(&records, w)
}

/// Pinpoint the verbatim bytes of the transcript line whose `uuid` matches, for
/// hit resolution (D-08, QRY-03). Returns the ORIGINAL raw line bytes, never a
/// parsed-then-reserialized value.
///
/// VERBATIM CONSTRAINT: this crate's `serde_json` has no `preserve_order`, so a
/// `serde_json::Value` object is a `BTreeMap` that re-emits keys alphabetically -
/// a Claude Code transcript line (`type`/`uuid`/`timestamp`/`message` order) would
/// round-trip to DIFFERENT bytes and fail the byte-for-byte QRY-03 criterion. So
/// each line is parsed ONLY far enough to read its `uuid`, and the untouched raw
/// slice of the first matching line is returned. No `scrub_indexed` is ever
/// applied, so the bytes are verbatim (unscrubbed).
pub fn pinpoint(generation: &[u8], uuid: &str) -> Option<Vec<u8>> {
    /// Reads just the `uuid` field; every other transcript field is ignored.
    #[derive(serde::Deserialize)]
    struct UuidOnly {
        uuid: Option<String>,
    }

    for line in generation.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(UuidOnly { uuid: Some(line_uuid) }) = serde_json::from_slice::<UuidOnly>(line) {
            if line_uuid == uuid {
                return Some(line.to_vec());
            }
        }
    }
    None
}

/// Read the parent session dir's generations plus every nested subagent
/// directory's generations (`subagents/<agent>/NNNN.zst`), parent first.
fn collect_generations(session_dir: &Path) -> Result<Vec<Generation>> {
    let mut generations = read_generations(session_dir, "")?;

    let subagents = session_dir.join("subagents");
    if subagents.is_dir() {
        let mut subdirs: Vec<std::path::PathBuf> = std::fs::read_dir(&subagents)
            .with_context(|| format!("reading {}", subagents.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        subdirs.sort();
        for subdir in subdirs {
            let name = subdir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let prefix = format!("subagents/{name}");
            generations.extend(read_generations(&subdir, &prefix)?);
        }
    }
    Ok(generations)
}

/// Decompress and parse every `NNNN.zst` generation in `dir`, sorted by
/// filename. Skips dotfiles and `meta.json` (mirrors `archive::scan_generations`).
/// `rel_prefix` labels each generation's `filename` for `archive_refs` (empty
/// for the parent dir, `subagents/<agent>` for a nested one).
fn read_generations(dir: &Path, rel_prefix: &str) -> Result<Vec<Generation>> {
    let mut names: Vec<String> = Vec::new();
    let rd = std::fs::read_dir(dir)
        .with_context(|| format!("reading session dir {}", dir.display()))?;
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') || !name.ends_with(".zst") {
            continue;
        }
        let stem = &name[..name.len() - ".zst".len()];
        if stem.parse::<u64>().is_err() {
            continue;
        }
        names.push(name.to_string());
    }
    names.sort();

    let mut generations = Vec::with_capacity(names.len());
    for filename in names {
        let path = dir.join(&filename);
        let file = std::fs::File::open(&path)
            .with_context(|| format!("opening generation {}", path.display()))?;
        let bytes = zstd::decode_all(file)
            .with_context(|| format!("decompressing generation {}", path.display()))?;
        let text = String::from_utf8(bytes)
            .with_context(|| format!("generation {} is not UTF-8", path.display()))?;
        let mut lines = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(line).with_context(|| {
                format!("parsing line {} of generation {}", i + 1, path.display())
            })?;
            lines.push(value);
        }
        let label = if rel_prefix.is_empty() {
            filename
        } else {
            format!("{rel_prefix}/{filename}")
        };
        generations.push(Generation {
            filename: label,
            lines,
        });
    }
    Ok(generations)
}

/// Build the minimal Session from the parsed generation lines (D-06): session_id
/// from the transcript `sessionId` field (authoritative mapping, not the dir
/// name), git_branch, cc_version, first/last timestamp, and `aiTitle` -> title.
fn build_session(project: &str, generations: &[Generation]) -> Result<Session> {
    let mut session_id: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut cc_version: Option<String> = None;
    let mut title: Option<String> = None;
    let mut min_ts: Option<String> = None;
    let mut max_ts: Option<String> = None;
    let mut archive_refs: Vec<String> = Vec::new();

    for generation in generations {
        archive_refs.push(generation.filename.clone());
        for line in &generation.lines {
            if session_id.is_none() {
                if let Some(s) = line.get("sessionId").and_then(Value::as_str) {
                    session_id = Some(s.to_string());
                }
            }
            if git_branch.is_none() {
                if let Some(s) = line.get("gitBranch").and_then(Value::as_str) {
                    if !s.is_empty() {
                        git_branch = Some(s.to_string());
                    }
                }
            }
            if cc_version.is_none() {
                if let Some(s) = line.get("version").and_then(Value::as_str) {
                    cc_version = Some(s.to_string());
                }
            }
            if title.is_none() {
                if let Some(s) = line.get("aiTitle").and_then(Value::as_str) {
                    title = Some(s.to_string());
                }
            }
            if let Some(ts) = line.get("timestamp").and_then(Value::as_str) {
                if min_ts.as_deref().map_or(true, |m| ts < m) {
                    min_ts = Some(ts.to_string());
                }
                if max_ts.as_deref().map_or(true, |m| ts > m) {
                    max_ts = Some(ts.to_string());
                }
            }
        }
    }

    let session_id = session_id
        .ok_or_else(|| anyhow!("no sessionId field found in any generation of {}", project))?;

    Ok(Session {
        session_id,
        project: project.to_string(),
        git_branch,
        cc_version,
        started_at: min_ts,
        ended_at: max_ts,
        end_reason: None,
        title,
        archive_refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{self, Kind};
    use crate::config::Config;

    fn test_config(base: &Path) -> Config {
        Config::from_toml_str(&format!("base_dir = {:?}\nidle_timeout_secs = 5\n", base)).unwrap()
    }

    #[test]
    fn emits_one_session_line_with_title_and_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());

        // Hand-built transcript: sessionId, gitBranch, version, two differing
        // timestamps, and an aiTitle line.
        let transcript = concat!(
            r#"{"type":"user","sessionId":"sess-1","gitBranch":"feat/x","version":"1.2.3","timestamp":"2026-07-21T10:00:00Z","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-07-21T10:05:00Z","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
            "\n",
            r#"{"type":"ai-title","sessionId":"sess-1","aiTitle":"A Test Session"}"#,
            "\n",
        );
        let src = tmp.path().join("source.jsonl");
        std::fs::write(&src, transcript).unwrap();
        archive::write_generation(&cfg, "proj", "sess-1", "", &src, Kind::Sweep).unwrap();

        let session_dir = cfg.archive_dir().join("proj").join("sess-1");
        let mut out: Vec<u8> = Vec::new();
        run_to(&session_dir, &mut out).unwrap();

        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(!lines.is_empty(), "at least the session line emitted");

        // The session is always the first emitted record.
        let value: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(value["type"], "session");
        assert_eq!(value["session_id"], "sess-1");
        assert_eq!(value["project"], "proj");
        assert_eq!(value["title"], "A Test Session");
        assert_eq!(value["git_branch"], "feat/x");
        assert_eq!(value["cc_version"], "1.2.3");
        assert_eq!(value["started_at"], "2026-07-21T10:00:00Z");
        assert_eq!(value["ended_at"], "2026-07-21T10:05:00Z");
        assert!(value["end_reason"].is_null());
    }
}
