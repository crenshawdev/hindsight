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
pub mod status;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
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

/// Spawn a detached child that re-execs this binary as `hindsight embed` and
/// return immediately (D-01, D-02). The child calls `setsid` so it leads a new
/// session and process group and survives the hook's process-group reaping, and its
/// three standard streams are redirected to `/dev/null` BEFORE the spawn: a session
/// hook's stdout is a pipe Claude Code reads to EOF for the hook JSON, so a child
/// that inherited that write-end would hold it open for the whole multi-minute
/// drain and block the session. With stdio nulled and the child handle dropped, no
/// descriptor keeps the hook's pipe open and the parent returns at once.
fn spawn_detached(program: &Path, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and the closure touches no shared
    // state; it only detaches the child into a new session before exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning detached {}", program.display()))?;
    // Drop the handle without waiting: the child is reparented and runs on its own.
    drop(child);
    Ok(())
}

/// Open the store, assemble profile units, and either dump them (D-11), report
/// drain status (D-07), or drain the queue into `vec_embedding`, skipping
/// already-embedded units (D-06). With `detach` set, self-detach a child to run the
/// drain and return immediately (D-01).
pub fn run(cfg: &Config, dump_profiles: bool, detach: bool, status: bool) -> Result<()> {
    // `--status` is a read-only reporter; combining it with a mode that writes or
    // detaches is a contradiction, so reject it up front (D-07).
    if status && (detach || dump_profiles) {
        bail!("--status cannot be combined with --detach or --dump-profiles (it only reads)");
    }
    if status {
        return status::report(cfg);
    }

    // `--dump-profiles` is a foreground inspection sink; detaching it would send its
    // NDJSON to /dev/null in a child, which is never what the caller wants.
    if detach && dump_profiles {
        bail!("--detach cannot be combined with --dump-profiles (dump is a foreground sink)");
    }

    // Detach path (D-01, D-02): spawn the drain as a new-session child and return
    // before opening the DB or taking the lock, so a session hook is not blocked.
    // The child is a plain `hindsight embed`, which takes the single-flight lock.
    if detach {
        let exe = std::env::current_exe().context("resolving current executable for --detach")?;
        spawn_detached(&exe, &["embed"])?;
        tracing::info!("spawned detached embed drain; parent returning");
        return Ok(());
    }

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

    let total = units.len();

    // Open a run record (D-07): `--status` reads the latest row to tell a live
    // *running* drain (fresh heartbeat) from a *stalled* one (stale heartbeat) and
    // reports progress. `pid` is this process id so a killed drain is nameable.
    let now = now_secs();
    conn.execute(
        "INSERT INTO embed_run
            (started_at, heartbeat_at, pid, state, total, embedded, skipped, failed)
         VALUES (?1, ?1, ?2, 'running', ?3, 0, 0, 0)",
        rusqlite::params![now, std::process::id() as i64, total as i64],
    )
    .context("inserting embed_run record")?;
    let run_id = conn.last_insert_rowid();

    // Continue-on-error drain (D-06): a batch embed failure is caught and the batch
    // is retried unit-by-unit so one poison input fails alone instead of failing
    // (and accruing give-up attempts on) its whole batch. A unit that still fails
    // on its own is a real per-unit failure, recorded and counted; the drain
    // proceeds rather than aborting.
    let counts = drain(&mut conn, &units, &embedder_version, run_id, |batch| {
        let texts: Vec<&str> = batch.iter().map(|u| u.text.as_str()).collect();
        let results: Vec<Result<Vec<f32>>> = match ollama::embed_documents(&cfg.embed, &texts) {
            Ok(vectors) => vectors.into_iter().map(Ok).collect(),
            Err(batch_err) => batch
                .iter()
                .map(|u| {
                    ollama::embed_document(&cfg.embed, &u.text).map_err(|e| {
                        anyhow::anyhow!("{e:#} (isolated after batch error: {batch_err:#})")
                    })
                })
                .collect(),
        };
        results
    })?;

    // Terminal state for the run (D-07): done with final counts. A `--status` read
    // after this classifies as done (or "done with N failed" if `failed > 0`).
    conn.execute(
        "UPDATE embed_run SET state = 'done', heartbeat_at = ?1,
             embedded = ?2, skipped = ?3, failed = ?4 WHERE id = ?5",
        rusqlite::params![
            now_secs(),
            counts.embedded as i64,
            counts.skipped as i64,
            counts.failed as i64,
            run_id
        ],
    )
    .context("marking embed_run done")?;

    tracing::info!(
        total,
        skipped = counts.skipped,
        embedded = counts.embedded,
        failed = counts.failed,
        stale_cleared,
        "embed drain complete"
    );
    Ok(())
}

/// Give-up cap (D-06): a unit whose ledger row is `'failed'` with this many
/// attempts is treated as a permanent failure and skipped, so a deterministically
/// failing unit stops burning an Ollama call on every hook-fired drain. A
/// `'failed'` row under the cap is retried.
const MAX_EMBED_ATTEMPTS: i64 = 5;

/// Stale-heartbeat threshold in seconds (D-07): a `running` run whose
/// `heartbeat_at` is older than this reads as *stalled* rather than *running*. Read
/// by the `--status` classifier (status.rs). Chosen well above a normal single
/// embed at this model and corpus, so only a pathological stall trips it.
pub const STALE_HEARTBEAT_SECS: i64 = 120;

/// Running tally a drain returns for the run record's terminal counts.
struct DrainCounts {
    embedded: usize,
    skipped: usize,
    failed: usize,
}

/// The testable drain core (D-06, D-07): skip already-done and permanently-failed
/// units, then embed the rest in batches of `EMBED_BATCH_SIZE`. `embed_fn` takes a
/// batch of pending units and returns one result per unit, in order; each result is
/// landed in the ledger - a vector plus a `'done'` stamp on success, a `'failed'`
/// row with the error string on failure - with the whole batch committed in one
/// transaction and the run's heartbeat and counts refreshed around each batch.
/// `embed_fn` is injected so a test can drive the error path without Ollama, and so
/// the batch-then-isolate fallback lives in the caller.
fn drain(
    conn: &mut rusqlite::Connection,
    units: &[profile::ProfileUnit],
    embedder_version: &str,
    run_id: i64,
    mut embed_fn: impl FnMut(&[&profile::ProfileUnit]) -> Vec<Result<Vec<f32>>>,
) -> Result<DrainCounts> {
    let mut skipped = 0usize;
    let mut embedded = 0usize;
    let mut failed = 0usize;

    // First pass - skip check (D-06): a `'done'` row under the current version is
    // already embedded (the atomic vector+ledger commit makes this exact), and a
    // `'failed'` row at the attempts cap is a permanent failure counted as such and
    // not retried. A `'failed'` row under the cap falls through into `pending`.
    // Partitioning up front means only truly-pending units are batched to Ollama.
    let mut pending: Vec<&profile::ProfileUnit> = Vec::new();
    for unit in units {
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT status, attempts FROM embed_ledger
                 WHERE unit_kind = ?1 AND source_id = ?2 AND embedder_version = ?3",
                rusqlite::params![unit.unit_kind, unit.source_id, embedder_version],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("checking embed_ledger for unit status")?;
        if let Some((status, attempts)) = &existing {
            if status == "done" {
                skipped += 1;
                continue;
            }
            if status == "failed" && *attempts >= MAX_EMBED_ATTEMPTS {
                failed += 1;
                continue;
            }
        }
        pending.push(unit);
    }
    // Reflect the skip/permanent-fail tally before the first batch lands, so an
    // early `--status` read after a resume shows the skipped count immediately.
    update_run_counts(conn, run_id, embedded, skipped, failed)?;

    for batch in pending.chunks(ollama::EMBED_BATCH_SIZE) {
        // Heartbeat immediately before and after the (blocking) batch embed so a
        // live drain reads as running even across a slow batch (D-07).
        touch_heartbeat(conn, run_id)?;
        let results = embed_fn(batch);
        touch_heartbeat(conn, run_id)?;

        // The injected `embed_fn` contract is one result per input unit, in order;
        // a mismatch would misalign vectors onto units, so fail loud rather than zip.
        if results.len() != batch.len() {
            bail!(
                "embed_fn returned {} results for a {}-unit batch",
                results.len(),
                batch.len()
            );
        }

        let embedded_at = now_rfc3339();
        // One transaction per batch: every vector+ledger stamp in the batch lands or
        // none does, so a crash mid-batch rolls the batch back and it re-embeds next
        // run (resumable, D-06) - never a vector without its stamp.
        let tx = conn.transaction().context("beginning per-batch embed tx")?;
        for (unit, result) in batch.iter().zip(results) {
            match result {
                Ok(vector) => {
                    let blob = vector_blob(&vector);
                    tx.execute(
                        "INSERT INTO vec_embedding
                            (embedding_coarse, embedding, project, unit_kind, source_id)
                         VALUES (vec_quantize_binary(?1), ?1, ?2, ?3, ?4)",
                        rusqlite::params![blob, unit.project, unit.unit_kind, unit.source_id],
                    )
                    .context("inserting vec_embedding row")?;
                    upsert_ledger(&tx, unit, embedder_version, &embedded_at, "done", None)?;
                    embedded += 1;
                }
                Err(e) => {
                    // Continue-on-error (D-06): record the failure, write no vector.
                    // The `attempts` counter accumulates via the upsert so the
                    // give-up cap can retire a deterministically failing unit.
                    let msg = format!("{e:#}");
                    upsert_ledger(&tx, unit, embedder_version, &embedded_at, "failed", Some(&msg))?;
                    failed += 1;
                }
            }
        }
        tx.commit().context("committing per-batch embed tx")?;

        update_run_counts(conn, run_id, embedded, skipped, failed)?;
    }

    Ok(DrainCounts {
        embedded,
        skipped,
        failed,
    })
}

/// Upsert one `embed_ledger` row (D-06, D-07). `INSERT ... ON CONFLICT DO UPDATE`
/// (not `INSERT OR REPLACE`, which deletes the prior row and loses the count) so
/// `attempts` accumulates across retries: a fresh insert records attempt 1, a
/// conflict increments the stored count. `last_error` is NULL on success.
fn upsert_ledger(
    conn: &rusqlite::Connection,
    unit: &profile::ProfileUnit,
    embedder_version: &str,
    embedded_at: &str,
    status: &str,
    last_error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO embed_ledger
            (unit_kind, source_id, embedder_version, embedded_at, status, attempts, last_error)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
         ON CONFLICT(unit_kind, source_id) DO UPDATE SET
             status = excluded.status,
             embedder_version = excluded.embedder_version,
             embedded_at = excluded.embedded_at,
             attempts = attempts + 1,
             last_error = excluded.last_error",
        rusqlite::params![
            unit.unit_kind,
            unit.source_id,
            embedder_version,
            embedded_at,
            status,
            last_error
        ],
    )
    .context("upserting embed_ledger row")?;
    Ok(())
}

/// Refresh a run's heartbeat to now (D-07). A no-op UPDATE if `run_id` is absent.
fn touch_heartbeat(conn: &rusqlite::Connection, run_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE embed_run SET heartbeat_at = ?1 WHERE id = ?2",
        rusqlite::params![now_secs(), run_id],
    )
    .context("refreshing embed_run heartbeat")?;
    Ok(())
}

/// Write the running counts onto a run record (D-07), cheap at this corpus size.
fn update_run_counts(
    conn: &rusqlite::Connection,
    run_id: i64,
    embedded: usize,
    skipped: usize,
    failed: usize,
) -> Result<()> {
    conn.execute(
        "UPDATE embed_run SET embedded = ?1, skipped = ?2, failed = ?3 WHERE id = ?4",
        rusqlite::params![embedded as i64, skipped as i64, failed as i64, run_id],
    )
    .context("updating embed_run counts")?;
    Ok(())
}

/// Current unix epoch seconds. The `embed_run` heartbeat/started stamps are integer
/// epoch seconds so the `--status` classifier can do plain arithmetic on them.
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_from_secs(secs)
}

/// Format an arbitrary unix-epoch-seconds value as an RFC3339 UTC string. Shared
/// with `status.rs` so it can render an `embed_run`'s stored `started_at` /
/// `heartbeat_at` stamps for display (D-07).
pub(crate) fn rfc3339_from_secs(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86_400;
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
    use std::time::{Duration, Instant};

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

        run(&cfg, false, false, false).expect("run returns Ok(()) when the drain lock is held");

        let conn = open_db(&cfg.db_path()).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM vec_embedding", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "no vectors written while the lock was held");
        drop(guard);
    }

    /// A hand-built profile unit; `drain` takes `&[ProfileUnit]` so a test can
    /// exercise the loop without going through `assemble` or a loaded corpus.
    fn unit(kind: &str, id: &str) -> profile::ProfileUnit {
        profile::ProfileUnit {
            unit_kind: kind.to_string(),
            source_id: id.to_string(),
            project: "proj".to_string(),
            text: format!("text {id}"),
        }
    }

    /// Insert a `running` run record and return its id so `drain`'s heartbeat and
    /// count UPDATEs have a row to hit.
    fn open_run(conn: &rusqlite::Connection, total: i64) -> i64 {
        conn.execute(
            "INSERT INTO embed_run
                (started_at, heartbeat_at, pid, state, total, embedded, skipped, failed)
             VALUES (0, 0, 0, 'running', ?1, 0, 0, 0)",
            rusqlite::params![total],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// D-06: a single unit's embed error is caught, recorded, and skipped while the
    /// drain finishes the rest; a second drain re-embeds none of the already-done
    /// units. (Acceptance criterion 5, in-process half.)
    #[test]
    fn drain_records_a_failure_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let mut conn = open_db(&cfg.db_path()).unwrap();

        let units = vec![unit("event", "1"), unit("event", "2"), unit("event", "3")];
        let ev = "m/profile-1";
        let run_id = open_run(&conn, units.len() as i64);

        // embed_fn fails only on source_id "2"; the other two succeed with a
        // full-width fake vector. The batch closure returns one result per unit.
        let counts = drain(&mut conn, &units, ev, run_id, |batch| {
            batch
                .iter()
                .map(|u| {
                    if u.source_id == "2" {
                        Err(anyhow::anyhow!("simulated ollama failure"))
                    } else {
                        Ok(vec![0.1_f32; ollama::EMBED_DIMS])
                    }
                })
                .collect()
        })
        .unwrap();

        assert_eq!(counts.embedded, 2, "two units embedded");
        assert_eq!(counts.failed, 1, "one unit failed");

        // The failing unit: a `failed` ledger row with a non-null error and NO vector.
        let (status, last_error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_error FROM embed_ledger WHERE source_id = '2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert!(last_error.is_some(), "failed unit records last_error");
        let vec2: i64 = conn
            .query_row(
                "SELECT count(*) FROM vec_embedding WHERE source_id = '2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vec2, 0, "failed unit wrote no vector");

        // The successful units carry `done` rows.
        for id in ["1", "3"] {
            let s: String = conn
                .query_row(
                    "SELECT status FROM embed_ledger WHERE source_id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(s, "done", "unit {id} is done");
        }

        // A second drain never calls `embed_fn` for the already-done units.
        let run2 = open_run(&conn, units.len() as i64);
        let mut called = Vec::new();
        let counts2 = drain(&mut conn, &units, ev, run2, |batch| {
            batch
                .iter()
                .map(|u| {
                    called.push(u.source_id.clone());
                    Ok(vec![0.2_f32; ollama::EMBED_DIMS])
                })
                .collect()
        })
        .unwrap();
        assert_eq!(counts2.skipped, 2, "the two done units are skipped");
        assert!(
            !called.contains(&"1".to_string()) && !called.contains(&"3".to_string()),
            "embed_fn is not called for already-done units"
        );
    }

    /// D-01, Ollama-free: `spawn_detached` returns promptly and the detached child
    /// runs on to completion AFTER the spawn returned, proving the child outlives
    /// the returning parent. A trivial `sh` child (sleep, then touch a sentinel)
    /// stands in for the real `hindsight embed` drain so no Ollama/GPU is needed.
    #[test]
    fn detached_child_outlives_the_spawn_call() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("done");
        let script = format!("sleep 0.4; : > '{}'", sentinel.display());

        let start = Instant::now();
        spawn_detached(Path::new("sh"), &["-c", &script]).expect("spawn returns Ok");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(250),
            "spawn returned promptly ({elapsed:?}), did not wait for the child"
        );
        assert!(
            !sentinel.exists(),
            "child has not finished its sleep when the spawn returned"
        );

        let mut appeared = false;
        for _ in 0..100 {
            if sentinel.exists() {
                appeared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            appeared,
            "the detached child completed its work after the spawn call returned"
        );
    }

    /// D-01: `--detach` with `--dump-profiles` is rejected before any spawn or DB
    /// open (dump is a foreground sink).
    #[test]
    fn detach_with_dump_profiles_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        assert!(
            run(&cfg, true, true, false).is_err(),
            "--detach + --dump-profiles must error"
        );
    }

    /// D-07: `--status` combined with `--detach` (or `--dump-profiles`) is rejected;
    /// the reporter only reads.
    #[test]
    fn status_with_other_modes_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        assert!(
            run(&cfg, false, true, true).is_err(),
            "--status + --detach must error"
        );
        assert!(
            run(&cfg, true, false, true).is_err(),
            "--status + --dump-profiles must error"
        );
    }
}
