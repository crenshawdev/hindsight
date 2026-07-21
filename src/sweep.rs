//! Full-tree watermark sweep (ADR 0002, D-08/D-09).
//!
//! Resolves the sweep root (`$CLAUDE_CONFIG_DIR` else `~/.claude`), walks
//! `projects/**/*.jsonl`, and archives every new-or-changed transcript. Change
//! detection is stat-based (mtime + size): an unchanged file is skipped, any
//! other is re-copied as a new generation. The sweep depends on no poke or end
//! hook, so a transcript left by a crashed session is still captured (CAP-01).
//! The watermark is saved after each file so an interrupted sweep resumes
//! without redoing completed files, and the archive writer's sha-dedup prevents
//! any duplicate generation on resume.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use tracing::warn;

use crate::archive::{self, Kind};
use crate::config::Config;
use crate::watermark::{Entry, Watermark};

/// Sweep the live transcript tree (root resolved from the environment).
pub fn run(config: &Config) -> Result<usize> {
    let root = sweep_root()?;
    run_at(config, &root)
}

/// Sweep against an explicit root. Split out so tests drive a temp tree without
/// touching process-global environment.
pub fn run_at(config: &Config, sweep_root: &Path) -> Result<usize> {
    let projects_root = sweep_root.join("projects");
    let mut files = Vec::new();
    collect_jsonl(&projects_root, &mut files)?;
    // Deterministic order so an interrupted-then-resumed sweep makes the same
    // progress and the watermark advances predictably.
    files.sort();

    let mut watermark = Watermark::load(config)?;
    let mut new_generations = 0usize;

    for path in &files {
        let (project, session_id, sub_path) = match config.archive_key(sweep_root, path) {
            Ok(k) => k,
            Err(e) => {
                warn!("skipping malformed transcript path {}: {e:#}", path.display());
                continue;
            }
        };

        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                // File vanished mid-sweep (e.g. cleanup raced us); the next
                // sweep reconciles. Skip rather than fail the whole sweep.
                warn!("skipping unreadable transcript {}: {e}", path.display());
                continue;
            }
        };
        let (mtime_secs, mtime_nanos) = mtime_parts(&meta);
        let size = meta.len();

        // D-08: stat-unchanged files are skipped.
        if let Some(entry) = watermark.get(path) {
            if entry.mtime_secs == mtime_secs
                && entry.mtime_nanos == mtime_nanos
                && entry.size == size
            {
                continue;
            }
        }

        let outcome = archive::write_generation(
            config,
            &project,
            &session_id,
            &sub_path,
            path,
            Kind::Sweep,
        )
        .with_context(|| format!("archiving {}", path.display()))?;

        if outcome.was_written() {
            new_generations += 1;
        }

        watermark.record(
            path,
            Entry {
                mtime_secs,
                mtime_nanos,
                size: outcome.size(),
                sha256: outcome.sha256().to_string(),
            },
        );
        // Save after each file so an interrupted sweep resumes cleanly.
        watermark.save()?;
    }

    Ok(new_generations)
}

/// Resolve the sweep root: `$CLAUDE_CONFIG_DIR` else `~/.claude` (D-09).
fn sweep_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|x| !x.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .context("neither CLAUDE_CONFIG_DIR nor HOME is set; cannot locate the transcript tree")?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// Collect `*.jsonl` files under `dir`, recursing into real subdirectories.
/// Symlinks are not followed (they report as neither dir nor regular file via
/// `file_type`), so the walk cannot loop.
fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in rd {
        let entry = entry.with_context(|| format!("reading an entry under {}", dir.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("statting {}", entry.path().display()))?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_jsonl(&path, out)?;
        } else if file_type.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

/// Decompose a file's mtime into `(secs, nanos)` for exact-equality comparison.
fn mtime_parts(meta: &Metadata) -> (i64, u32) {
    match meta.modified() {
        Ok(mtime) => match mtime.duration_since(UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
            // Pre-epoch mtime (not expected for transcripts); represent as negative.
            Err(e) => {
                let d = e.duration();
                (-(d.as_secs() as i64), d.subsec_nanos())
            }
        },
        // Platform without mtime support: treat as always-changed (0,0 never
        // matches a real mtime, so the file is re-archived; sha-dedup prevents
        // duplicates).
        Err(_) => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config(base: &Path) -> Config {
        Config::from_toml_str(&format!("base_dir = {:?}\nidle_timeout_secs = 5\n", base)).unwrap()
    }

    /// Build a transcript under `root/projects/<rel>` with given bytes.
    fn write_transcript(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join("projects").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn gen_count(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        let n = e.file_name();
                        let n = n.to_string_lossy();
                        n.ends_with(".zst") && !n.starts_with('.')
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn first_sweep_archives_each_jsonl_including_nested_subagent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let root = tmp.path().join("claude");
        let cfg = test_config(&base);

        write_transcript(&root, "projA/sess1.jsonl", b"top-level-transcript");
        // A nested subagent transcript (no poke of its own) must stay under its
        // real project/session, not a bogus "subagents" project.
        write_transcript(
            &root,
            "projA/sess1/subagents/agent-xyz.jsonl",
            b"nested-subagent-transcript",
        );

        let new = run_at(&cfg, &root).unwrap();
        assert_eq!(new, 2, "both transcripts archived on the first sweep");

        assert_eq!(gen_count(&base.join("archive/projA/sess1")), 1);
        assert_eq!(
            gen_count(&base.join("archive/projA/sess1/subagents/agent-xyz")),
            1,
            "nested transcript filed under its session, not a subagents project"
        );
        assert!(
            !base.join("archive/subagents").exists(),
            "no bogus subagents project directory"
        );
    }

    #[test]
    fn second_sweep_over_unchanged_tree_archives_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let root = tmp.path().join("claude");
        let cfg = test_config(&base);

        write_transcript(&root, "p/s.jsonl", b"unchanged");
        assert_eq!(run_at(&cfg, &root).unwrap(), 1);
        assert_eq!(
            run_at(&cfg, &root).unwrap(),
            0,
            "unchanged tree yields zero new generations"
        );
        assert_eq!(gen_count(&base.join("archive/p/s")), 1);
    }

    #[test]
    fn resume_after_crash_before_watermark_save_writes_no_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let root = tmp.path().join("claude");
        let cfg = test_config(&base);

        let f1 = write_transcript(&root, "p/s1.jsonl", b"file-one-bytes");
        write_transcript(&root, "p/s2.jsonl", b"file-two-bytes");

        // Simulate a sweep killed after archiving f1 but before its watermark
        // save: f1 has a generation on disk, but the watermark is still empty.
        archive::write_generation(&cfg, "p", "s1", "", &f1, Kind::Sweep).unwrap();
        assert_eq!(gen_count(&base.join("archive/p/s1")), 1);

        // Re-run: f1 is not in the watermark, so it is re-offered to the writer,
        // which sha-dedups it (no duplicate); f2 is newly archived.
        let new = run_at(&cfg, &root).unwrap();
        assert_eq!(new, 1, "only f2 is a newly written generation");
        assert_eq!(gen_count(&base.join("archive/p/s1")), 1, "no duplicate for f1");
        assert_eq!(gen_count(&base.join("archive/p/s2")), 1);
    }

    #[test]
    fn grown_file_is_rearchived_as_a_new_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let root = tmp.path().join("claude");
        let cfg = test_config(&base);

        let f = write_transcript(&root, "p/s.jsonl", b"line-one\n");
        assert_eq!(run_at(&cfg, &root).unwrap(), 1);

        // Append content (grows the file -> stat changes -> re-archived).
        std::fs::write(&f, b"line-one\nline-two\n").unwrap();
        assert_eq!(run_at(&cfg, &root).unwrap(), 1, "grown file re-archived");
        assert_eq!(
            gen_count(&base.join("archive/p/s")),
            2,
            "two generations kept (write-once)"
        );
    }
}
