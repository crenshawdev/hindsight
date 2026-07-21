//! PreCompact subcommand: snapshot a transcript before Claude Code compacts it
//! in place (D-12, D-05).
//!
//! Reads the PreCompact payload as JSON on stdin, derives the archive
//! coordinates from `transcript_path` via the same `archive_key` the sweep uses
//! (so a subagent-triggered compaction files correctly), and writes a
//! `precompact` generation directly (D-04). Any failure to read, parse, or
//! write is fail-loud-and-block: `run` returns an error and `main` maps it to
//! exit code 2, vetoing the compaction so no pre-compaction bytes are lost.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::archive::{self, Kind, Outcome};
use crate::config::Config;
use crate::sweep::sweep_root;

/// The PreCompact hook stdin schema (fields we use; extras are ignored).
#[derive(Debug, Deserialize)]
struct PreCompactPayload {
    session_id: String,
    transcript_path: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    trigger: String,
}

pub fn run() -> Result<()> {
    let config = Config::load()?;

    let mut raw = Vec::new();
    std::io::stdin()
        .read_to_end(&mut raw)
        .context("reading the PreCompact payload from stdin")?;
    let payload: PreCompactPayload =
        serde_json::from_slice(&raw).context("parsing the PreCompact JSON payload")?;

    let root = sweep_root()?;
    match snapshot(&config, &root, &payload)? {
        Some(outcome) => tracing::info!(
            was_written = outcome.was_written(),
            trigger = %payload.trigger,
            "precompact snapshot"
        ),
        None => tracing::warn!(
            transcript = %payload.transcript_path,
            trigger = %payload.trigger,
            "precompact: source transcript not on disk; nothing to snapshot, allowing compaction"
        ),
    }
    Ok(())
}

/// Archive the current bytes of the payload's transcript as a `precompact`
/// generation. Split from `run` so tests exercise it without global stdin/env.
///
/// `Ok(None)` means the source transcript was not on disk, so there was nothing
/// to snapshot and the compaction is allowed (see the D-05 refinement below).
fn snapshot(
    config: &Config,
    sweep_root: &Path,
    payload: &PreCompactPayload,
) -> Result<Option<Outcome>> {
    let transcript_path = Path::new(&payload.transcript_path);

    // D-05 refinement: the fail-loud veto (exit 2) exists to protect the
    // pre-compaction bytes we are about to lose. A source transcript that is not
    // on disk - e.g. a fresh session compacted before Claude Code flushed its
    // transcript - puts zero bytes at risk, so allow the compaction rather than
    // vetoing it. A failure once the source IS present (bytes read but not
    // persisted) still vetoes.
    if !transcript_path.try_exists().unwrap_or(false) {
        return Ok(None);
    }

    let (project, session_id, sub_path) = match config.archive_key(sweep_root, transcript_path) {
        Ok(key) => key,
        Err(_) => {
            // Ambiguous path (not under the sweep root's projects tree): fall
            // back to the authoritative payload session_id and a best-effort
            // project derived from cwd.
            let project = project_from_cwd(&payload.cwd).context(
                "PreCompact transcript_path is not under the transcript tree and cwd is empty; \
                 cannot locate the archive coordinates",
            )?;
            (project, payload.session_id.clone(), String::new())
        }
    };

    let outcome = archive::write_generation(
        config,
        &project,
        &session_id,
        &sub_path,
        transcript_path,
        Kind::Precompact,
    )
    .context("writing the precompact generation")?;
    Ok(Some(outcome))
}

/// Best-effort project encoding for the fallback path: Claude Code names a
/// project directory as the cwd with `/` replaced by `-`. Only used when the
/// transcript path is not under the transcript tree.
fn project_from_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    Some(cwd.replace('/', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config(base: &Path) -> Config {
        Config::from_toml_str(&format!("base_dir = {:?}\nidle_timeout_secs = 5\n", base)).unwrap()
    }

    #[test]
    fn snapshot_writes_precompact_generation_with_preinvocation_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let root = tmp.path().join("claude");
        let cfg = test_config(&base);

        let tpath = root.join("projects").join("proj").join("sess.jsonl");
        std::fs::create_dir_all(tpath.parent().unwrap()).unwrap();
        let bytes = b"pre-invocation-transcript-bytes\n";
        std::fs::write(&tpath, bytes).unwrap();

        let payload = PreCompactPayload {
            session_id: "sess".into(),
            transcript_path: tpath.to_string_lossy().into_owned(),
            cwd: "/proj".into(),
            trigger: "manual".into(),
        };
        let outcome = snapshot(&cfg, &root, &payload)
            .unwrap()
            .expect("source present -> generation written");
        assert!(outcome.was_written());

        let gen_dir = base.join("archive/proj/sess");
        let round = zstd::decode_all(std::fs::File::open(gen_dir.join("0000.zst")).unwrap()).unwrap();
        assert_eq!(round, bytes, "generation holds the pre-invocation bytes");

        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(gen_dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["generations"][0]["kind"], "precompact");
    }

    #[test]
    fn snapshot_falls_back_to_payload_session_when_path_not_in_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let root = tmp.path().join("claude");
        let cfg = test_config(&base);

        // transcript_path is NOT under root/projects, so archive_key fails and
        // the fallback (cwd-derived project + payload session_id) is used.
        let tpath = tmp.path().join("loose.jsonl");
        std::fs::write(&tpath, b"loose-bytes").unwrap();
        let payload = PreCompactPayload {
            session_id: "fallback-sess".into(),
            transcript_path: tpath.to_string_lossy().into_owned(),
            cwd: "/data/code/proj".into(),
            trigger: "auto".into(),
        };
        let outcome = snapshot(&cfg, &root, &payload)
            .unwrap()
            .expect("source present -> generation written");
        assert!(outcome.was_written());
        assert!(base
            .join("archive/-data-code-proj/fallback-sess/0000.zst")
            .exists());
    }

    #[test]
    fn snapshot_allows_compaction_when_source_transcript_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let root = tmp.path().join("claude");
        let cfg = test_config(&base);

        // The payload points at a transcript that was never written to disk - a
        // fresh session compacted before Claude Code flushed it. D-05 refinement:
        // no bytes are at risk, so snapshot returns None (allow compaction) and
        // writes nothing, rather than erroring into an exit-2 veto.
        let tpath = root.join("projects").join("proj").join("ghost.jsonl");
        let payload = PreCompactPayload {
            session_id: "ghost".into(),
            transcript_path: tpath.to_string_lossy().into_owned(),
            cwd: "/proj".into(),
            trigger: "manual".into(),
        };
        let outcome = snapshot(&cfg, &root, &payload).unwrap();
        assert!(
            outcome.is_none(),
            "absent source transcript -> nothing to snapshot, compaction allowed"
        );
        assert!(
            !base.join("archive/proj/ghost").exists(),
            "no generation written for an absent source"
        );
    }
}
