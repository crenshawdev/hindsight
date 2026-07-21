//! `hindsight normalize`: read an archived session directory's generations and
//! emit tagged NDJSON (Session / Event / Artifact / Mention) to stdout (D-01,
//! D-03). Not wired into the sweep this phase (Phase 6 does that); the argument
//! is a direct archive directory path so the command is inspectable without a
//! config file.

mod model;

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
fn run_to<W: Write>(session_dir: &Path, w: &mut W) -> Result<()> {
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

    // Parent generations only for Task 1's minimal session; nested subagents
    // join in Task 2.
    let generations = read_generations(session_dir)?;

    let session = build_session(&project, &generations)?;

    let records = vec![Record::Session(session)];
    model::write_ndjson(&records, w)
}

/// Decompress and parse every `NNNN.zst` generation in `dir`, sorted by
/// filename. Skips dotfiles and `meta.json` (mirrors `archive::scan_generations`).
fn read_generations(dir: &Path) -> Result<Vec<Generation>> {
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
        generations.push(Generation { filename, lines });
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
        assert_eq!(lines.len(), 1, "exactly one line emitted");

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
