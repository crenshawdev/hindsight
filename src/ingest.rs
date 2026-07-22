//! `hindsight ingest` (Phase 7): the incremental capture -> index -> embed pipeline
//! the session hooks drive. One synchronous sweep archives new/changed transcripts
//! so the archive is current, then every archived session whose generations changed
//! since last ingest is re-normalized and session-scoped-replaced into the index
//! (tracked in `ingest_ledger`), and finally a detached embed drain is fired iff
//! something changed.
//!
//! It is idempotent and single-flight, which is what lets both session hooks call
//! the same command: SessionStart picks up any session left un-indexed by a missing
//! end hook or a crash, SessionEnd folds in the session that just closed, and two
//! overlapping Claude Code sessions cannot race because only one ingest holds the
//! lock at a time (the others exit cleanly, their work already covered).
//!
//! Unlike a full `hindsight load`, ingest never wipes the index or the embed ledger,
//! so already-landed vectors survive and only genuinely-new units are drained. A
//! session that grew and re-ingested keeps its stable-id vectors until the next full
//! re-embed - an accepted reconciliation, since a full re-embed is cheap to re-run.

use std::fs::OpenOptions;
use std::io::Cursor;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::store::{load, open_db};
use crate::{embed, normalize, sweep};

/// Single-flight lock file under `state_dir()` (mirrors embed's `embed.lock`): one
/// ingest at a time so overlapping session hooks never race the sweep watermark or
/// the per-session index writes.
const LOCK_FILE: &str = "ingest.lock";

/// An archived session on disk: its directory, id (the directory name), and project
/// (the parent directory name), matching the archive layout
/// `archive/<project>/<session-id>/`.
struct ArchivedSession {
    dir: PathBuf,
    session_id: String,
    project: String,
}

/// Run one incremental ingest pass. Returns cleanly (a no-op) if another ingest
/// already holds the lock.
pub fn run(cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(cfg.state_dir())
        .with_context(|| format!("creating state dir {}", cfg.state_dir().display()))?;
    let lock_path = cfg.state_dir().join(LOCK_FILE);
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening ingest lock {}", lock_path.display()))?;
    // SAFETY: `flock` takes the raw fd of a file we own and keep alive in `lock`.
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        tracing::info!("an ingest is already running; exiting without re-ingesting");
        return Ok(());
    }

    // 1) Archive new/changed transcripts synchronously, so the archive we index in
    //    step 2 is current as of this moment.
    let archived = sweep::run(cfg).context("sweep during ingest")?;

    // 2) Reconcile the index against the archive, one session at a time. A session
    //    whose generation fingerprint matches the ledger is already current and
    //    skipped; a new or grown session is re-normalized and session-scoped-replaced.
    let mut conn = open_db(&cfg.db_path())?;
    let sessions = enumerate_sessions(&cfg.archive_dir())?;
    let mut reindexed = 0usize;
    for sess in &sessions {
        let fp = fingerprint(&sess.dir)
            .with_context(|| format!("fingerprinting {}", sess.dir.display()))?;
        let prior: Option<String> = conn
            .query_row(
                "SELECT fingerprint FROM ingest_ledger WHERE session_id = ?1",
                params![sess.session_id],
                |r| r.get(0),
            )
            .optional()
            .context("reading ingest_ledger")?;
        if prior.as_deref() == Some(fp.as_str()) {
            continue;
        }

        // Normalize this session to an in-memory NDJSON buffer, then replace just
        // this session's rows in the index.
        let mut buf = Vec::new();
        normalize::run_to(&sess.dir, &mut buf)
            .with_context(|| format!("normalizing {}", sess.dir.display()))?;
        let session_id = load::ingest_session(&mut conn, Cursor::new(buf))
            .with_context(|| format!("indexing {}", sess.dir.display()))?;

        conn.execute(
            "INSERT INTO ingest_ledger (session_id, project, fingerprint, ingested_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET
                 project = excluded.project,
                 fingerprint = excluded.fingerprint,
                 ingested_at = excluded.ingested_at",
            params![session_id, sess.project, fp, now_secs()],
        )
        .context("updating ingest_ledger")?;
        reindexed += 1;
    }

    tracing::info!(
        archived,
        sessions = sessions.len(),
        reindexed,
        "ingest reconcile complete"
    );

    // 3) Only if something changed: fire a detached embed drain. The embed path
    //    self-detaches and takes its own single-flight lock, so a drain already
    //    running just makes this a clean no-op. Nothing changed -> no drain spawned.
    if reindexed > 0 {
        embed::run(cfg, false, true, false).context("triggering embed drain after ingest")?;
    }
    Ok(())
}

/// Enumerate archived sessions at `archive/<project>/<session-id>/`. A missing
/// archive dir (nothing captured yet) yields an empty list rather than an error.
fn enumerate_sessions(archive_dir: &Path) -> Result<Vec<ArchivedSession>> {
    let mut out = Vec::new();
    let projects = match std::fs::read_dir(archive_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(e).with_context(|| format!("reading archive dir {}", archive_dir.display()))
        }
    };
    for project in projects {
        let project = project.context("reading archive project entry")?;
        if !project.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let project_name = project.file_name().to_string_lossy().into_owned();
        for session in std::fs::read_dir(project.path())
            .with_context(|| format!("reading project dir {}", project.path().display()))?
        {
            let session = session.context("reading archive session entry")?;
            if !session.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            out.push(ArchivedSession {
                session_id: session.file_name().to_string_lossy().into_owned(),
                project: project_name.clone(),
                dir: session.path(),
            });
        }
    }
    Ok(out)
}

/// A stable content-fingerprint of a session's generations: the sha256 of the
/// sorted `relpath|size|mtime` triples of every `*.zst` under the session dir. Any
/// generation added or rewritten (a grown session, a PreCompact snapshot) changes
/// the set and so the fingerprint; an unchanged session hashes identically and is
/// skipped. Stat-only - no decompression - so it stays cheap enough for a hook.
fn fingerprint(session_dir: &Path) -> Result<String> {
    let mut entries: Vec<String> = Vec::new();
    collect_zst(session_dir, session_dir, &mut entries)?;
    entries.sort();
    let mut hasher = Sha256::new();
    for e in &entries {
        hasher.update(e.as_bytes());
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Recursively collect `relpath|size|mtime_secs.nanos` for each `*.zst` under
/// `dir`, relative to `root`, into `out`.
fn collect_zst(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading session dir {}", dir.display()))?
    {
        let entry = entry.context("reading session dir entry")?;
        let path = entry.path();
        let file_type = entry.file_type().context("stat-ing session dir entry")?;
        if file_type.is_dir() {
            collect_zst(root, &path, out)?;
        } else if path.extension().map(|e| e == "zst").unwrap_or(false) {
            let meta = entry.metadata().context("reading generation metadata")?;
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
            let (secs, nanos) = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| (d.as_secs(), d.subsec_nanos()))
                .unwrap_or((0, 0));
            out.push(format!("{rel}|{}|{secs}.{nanos}", meta.len()));
        }
    }
    Ok(())
}

/// Current unix epoch seconds; the `ingest_ledger.ingested_at` provenance stamp.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
