//! `hindsight embed` (D-02, ADR 0013): assemble synthetic profile units from the
//! loaded store and embed them via Ollama into the two-stage `vec_embedding` table.
//! A drain-and-exit batch command matching the `normalize`/`load` pattern, triggered
//! by the session-lifecycle hooks as a detached (`setsid`) process that runs
//! unconditionally on the GPU with no CPU path, never folded into the capture daemon.
//!
//! `--dump-profiles` is the Ollama-free inspection sink (D-11): it prints the
//! assembled units as NDJSON and writes no vectors, so profile assembly is
//! machine-checkable without an embedder.
//!
//! The drain is resumable (D-06): each embedded unit is stamped in `embed_ledger`
//! in the SAME transaction as its vector insert, so an interrupted run resumes
//! exactly - the ledger skip-check never skips a unit whose vector did not land,
//! and never re-embeds one that did.

pub mod ollama;
pub mod profile;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use crate::config::Config;
use crate::store::open_db;

/// The profile-construction contract the stored vectors were built under. Bumped
/// when the mechanical assembly in `profile.rs` changes shape, so a re-embed under
/// a new profile version re-stamps the ledger and clears stale vectors.
pub const PROFILE_SCHEMA_VERSION: &str = "1";

/// The single-flight lock file under `state_dir()` (D-03). One drain at a time:
/// an advisory `flock` on this file, released by the kernel on any process exit.
const LOCK_FILE: &str = "embed.lock";

/// The outcome of trying to take the single-flight drain lock (D-03).
enum LockOutcome {
    /// The lock was taken; the guard holds it for the process lifetime.
    Acquired(DrainLock),
    /// Another drain already holds the lock; this invocation must exit cleanly.
    AlreadyHeld,
}

/// Guard owning the open lock file. The advisory `flock` is fd-scoped, so the
/// kernel releases it when this `File` is dropped on normal exit OR when the
/// process dies (crash, kill) - no PID-file staleness to reconcile (D-03).
struct DrainLock {
    _file: File,
}

/// Take the single-flight drain lock (D-03): create `state_dir()` if absent, open
/// (create + write) `state_dir()/embed.lock`, and take a non-blocking exclusive
/// advisory `flock`. A held lock returns `AlreadyHeld` (not an error) so a second
/// concurrent drain exits cleanly without ever double-embedding.
fn acquire_lock(state_dir: &Path) -> Result<LockOutcome> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let path = state_dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening drain lock {}", path.display()))?;

    // SAFETY: `flock` takes the raw fd of a file we own and keep alive in `file`.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(LockOutcome::Acquired(DrainLock { _file: file }));
    }
    let err = std::io::Error::last_os_error();
    // EWOULDBLOCK (== EAGAIN on Linux) is the "another drain holds it" signal.
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
            Ok(LockOutcome::AlreadyHeld)
        }
        _ => Err(err).with_context(|| format!("locking drain lock {}", path.display())),
    }
}

/// Open the store, assemble profile units, and either dump them (D-11) or drain
/// the queue into `vec_embedding`, skipping already-embedded units (D-06).
pub fn run(cfg: &Config, dump_profiles: bool) -> Result<()> {
    let mut conn = open_db(&cfg.db_path())?;

    // `--dump-profiles` is a foreground inspection sink (D-11): it writes no
    // vectors, so it does NOT take the single-flight lock and can run alongside a
    // real drain.
    if dump_profiles {
        let units = profile::assemble(&conn)?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for unit in &units {
            let line = serde_json::to_string(unit).context("serializing profile unit to JSON")?;
            writeln!(out, "{line}").context("writing profile NDJSON line")?;
        }
        return Ok(());
    }

    // Single-flight (D-03): take the drain lock BEFORE assembling so a second
    // concurrent invocation exits without redoing the assembly work or embedding.
    let _lock = match acquire_lock(&cfg.state_dir())? {
        LockOutcome::Acquired(guard) => guard,
        LockOutcome::AlreadyHeld => {
            tracing::info!("an embed drain is already running; exiting without draining");
            return Ok(());
        }
    };

    let units = profile::assemble(&conn)?;

    // The version stamp every landed vector is keyed to: model + profile version.
    // A change to either means prior vectors are stale under the new contract.
    let embedder_version = format!("{}/profile-{}", cfg.embed.model, PROFILE_SCHEMA_VERSION);

    // Version-bump cleanup (D-06): clear vectors for units whose ledger stamp is
    // under a DIFFERENT embedder_version - the only case a same-file re-embed
    // without an intervening `load` can leave a stale vector. Done ONCE, set-based
    // (a per-unit pre-delete would scan the vec0 aux columns each time, O(n^2)).
    // Same-version resume matches nothing here and clears zero.
    let stale_cleared = conn
        .execute(
            "DELETE FROM vec_embedding WHERE (unit_kind, source_id) IN
               (SELECT unit_kind, source_id FROM embed_ledger WHERE embedder_version <> ?1)",
            rusqlite::params![embedder_version],
        )
        .context("clearing stale-version vectors before drain")?;

    let embedded_at = now_rfc3339();
    let total = units.len();
    let mut skipped = 0usize;
    let mut embedded = 0usize;

    for unit in &units {
        // Skip if this unit is already embedded under the CURRENT version. The
        // atomic vector+ledger commit below guarantees this check is exact: a
        // ledger stamp exists only if its vector landed.
        let already: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM embed_ledger
                 WHERE unit_kind = ?1 AND source_id = ?2 AND embedder_version = ?3",
                rusqlite::params![unit.unit_kind, unit.source_id, embedder_version],
                |r| r.get(0),
            )
            .optional()
            .context("checking embed_ledger for an already-embedded unit")?;
        if already.is_some() {
            skipped += 1;
            continue;
        }

        let vector = ollama::embed_document(&cfg.embed, &unit.text)
            .with_context(|| format!("embedding {} unit {}", unit.unit_kind, unit.source_id))?;
        let blob = vector_blob(&vector);

        // Vector insert and ledger stamp commit together: a crash lands both or
        // neither, so there is no window where a vector exists without its stamp.
        let tx = conn.transaction().context("beginning per-unit embed tx")?;
        tx.execute(
            "INSERT INTO vec_embedding(embedding_coarse, embedding, project, unit_kind, source_id)
             VALUES (vec_quantize_binary(?1), ?1, ?2, ?3, ?4)",
            rusqlite::params![blob, unit.project, unit.unit_kind, unit.source_id],
        )
        .context("inserting vec_embedding row")?;
        tx.execute(
            "INSERT OR REPLACE INTO embed_ledger(unit_kind, source_id, embedder_version, embedded_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![unit.unit_kind, unit.source_id, embedder_version, embedded_at],
        )
        .context("stamping embed_ledger row")?;
        tx.commit().context("committing per-unit embed tx")?;
        embedded += 1;
    }

    tracing::info!(
        total,
        skipped,
        embedded,
        stale_cleared,
        "embed drain complete"
    );
    Ok(())
}

/// Serialize an f32 slice to the little-endian byte blob sqlite-vec expects for a
/// `float[N]` vector column (matches tests/sqlite_vec_linkage.rs::vector_blob).
fn vector_blob(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

/// Current wall-clock time as an RFC3339 UTC string (`YYYY-MM-DDTHH:MM:SSZ`). A
/// provenance stamp only - nothing reads it back for logic, so second precision is
/// enough. Uses the same proleptic-Gregorian math as archive.rs's timestamp.
fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Days since 1970-01-01 to a (year, month, day) in the proleptic Gregorian
/// calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, EmbedConfig};

    /// A `Config` rooted at a temp dir so `db_path()` and `state_dir()` land under it.
    fn test_config(base: &Path) -> Config {
        Config {
            base_dir: base.to_path_buf(),
            idle_timeout_secs: 900,
            embed: EmbedConfig::default(),
        }
    }

    /// Seed one indexed-grain event so `assemble` yields exactly one embeddable
    /// unit: a real drain would then call Ollama, so a run that writes zero vectors
    /// proves it short-circuited before draining rather than an empty corpus.
    fn seed_one_event_unit(cfg: &Config) {
        let conn = open_db(&cfg.db_path()).unwrap();
        conn.execute(
            "INSERT INTO session(session_id, project) VALUES ('s1', 'proj')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO event(uuid, session_id, is_sidechain, grain, text)
             VALUES ('u1', 's1', 0, 'indexed', 'hello world')",
            [],
        )
        .unwrap();
    }

    /// D-03: a second acquire against a lock still held reports `AlreadyHeld`, not a
    /// panic or a second exclusive lock (two `open()`s are distinct open file
    /// descriptions, so `flock` conflicts even within one process).
    #[test]
    fn second_acquire_reports_already_held() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");

        let first = acquire_lock(&state).unwrap();
        assert!(
            matches!(first, LockOutcome::Acquired(_)),
            "first acquire takes the lock"
        );
        let second = acquire_lock(&state).unwrap();
        assert!(
            matches!(second, LockOutcome::AlreadyHeld),
            "second acquire, first still held, reports already-held"
        );
        drop(first);
    }

    /// Acceptance criterion 4 (in-process half): with the lock pre-held, `run`
    /// returns `Ok(())` and writes zero vectors - it never assembles or drains, so
    /// no Ollama call is made even though a unit is queued.
    #[test]
    fn run_is_a_clean_noop_when_lock_is_held() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        seed_one_event_unit(&cfg);

        let guard = match acquire_lock(&cfg.state_dir()).unwrap() {
            LockOutcome::Acquired(g) => g,
            LockOutcome::AlreadyHeld => panic!("expected to acquire the lock first"),
        };

        run(&cfg, false).expect("run returns Ok(()) when the drain lock is held");

        let conn = open_db(&cfg.db_path()).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM vec_embedding", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "no vectors written while the lock was held");
        drop(guard);
    }
}
