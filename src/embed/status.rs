//! `hindsight embed --status` (D-07): a read-only reporter that classifies the
//! drain state from the DB - the `embed_run` run record plus per-unit `embed_ledger`
//! status - and prints it. No draining, no Ollama, no lock.
//!
//! The classifier has an explicit precedence so a live run is never masked by stale
//! per-unit history (an active retry outranks an old failed row): a fresh-heartbeat
//! `running` run reports *running* with progress; a `running` run whose heartbeat
//! has gone stale (killed or hung) reports *stalled*; a terminal `done` run reports
//! *done* (or *done with N failed* when current-version failures remain); and a DB
//! with no run and an empty ledger reports *not-yet-embedded*.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::config::Config;
use crate::store::open_db;

use super::{now_secs, profile, rfc3339_from_secs, PROFILE_SCHEMA_VERSION, STALE_HEARTBEAT_SECS};

/// The classified drain state (D-07). `total` is the current assembled unit count;
/// `embedded`/`done` are current-embedder-version ledger counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedStatus {
    /// No run record and an empty ledger: nothing has been embedded yet.
    NotYetEmbedded { total: i64 },
    /// The latest run is `running` with a fresh heartbeat.
    Running { embedded: i64, total: i64 },
    /// The latest run is `running` but its heartbeat has gone stale (killed/hung).
    Stalled {
        pid: i64,
        started_at: i64,
        heartbeat_at: i64,
    },
    /// The latest run is terminal and every assembled unit is embedded, no failures.
    Done { done: i64, total: i64 },
    /// Terminal run with current-version failures recorded (reported orthogonally
    /// to the run's terminal state).
    DoneWithFailures {
        failed: i64,
        sample_error: Option<String>,
    },
}

/// Open the store, learn the current `total` and embedder version, classify, and
/// print. The read-only entry point behind `hindsight embed --status`.
pub fn report(cfg: &Config) -> Result<()> {
    let conn = open_db(&cfg.db_path())?;
    let total = profile::assemble(&conn)?.len() as i64;
    let embedder_version = format!("{}/profile-{}", cfg.embed.model, PROFILE_SCHEMA_VERSION);
    let status = classify(&conn, total, &embedder_version, now_secs())?;
    println!("{}", render(&status));
    Ok(())
}

/// Classify the drain state from the DB (D-07). `now` is injected (unix epoch
/// seconds) so a test can drive the running/stalled heartbeat boundary without
/// waiting real time.
pub fn classify(
    conn: &Connection,
    total: i64,
    embedder_version: &str,
    now: i64,
) -> Result<EmbedStatus> {
    // The latest run record (max id): the run this DB most recently opened.
    let latest: Option<(String, i64, i64, i64, i64)> = conn
        .query_row(
            "SELECT state, heartbeat_at, started_at, pid, embedded
             FROM embed_run ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .context("reading latest embed_run row")?;

    let done_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM embed_ledger WHERE embedder_version = ?1 AND status = 'done'",
            rusqlite::params![embedder_version],
            |r| r.get(0),
        )
        .context("counting done ledger rows")?;
    let failed_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM embed_ledger WHERE embedder_version = ?1 AND status = 'failed'",
            rusqlite::params![embedder_version],
            |r| r.get(0),
        )
        .context("counting failed ledger rows")?;
    let ledger_total: i64 = conn
        .query_row("SELECT count(*) FROM embed_ledger", [], |r| r.get(0))
        .context("counting ledger rows")?;

    // Precedence (D-07): a live run outranks per-unit history.
    if let Some((state, heartbeat_at, started_at, pid, embedded)) = latest {
        if state == "running" {
            if now - heartbeat_at <= STALE_HEARTBEAT_SECS {
                return Ok(EmbedStatus::Running { embedded, total });
            }
            return Ok(EmbedStatus::Stalled {
                pid,
                started_at,
                heartbeat_at,
            });
        }
        // Terminal run: done unless current-version failures remain or the done
        // count does not cover the assembled total.
        if failed_count == 0 && done_count == total {
            return Ok(EmbedStatus::Done {
                done: done_count,
                total,
            });
        }
        return Ok(EmbedStatus::DoneWithFailures {
            failed: failed_count,
            sample_error: sample_error(conn, embedder_version)?,
        });
    }

    // No run record. An empty ledger is a never-embedded DB; a populated ledger with
    // no run row (e.g. a legacy file) is classified by its counts.
    if ledger_total == 0 {
        return Ok(EmbedStatus::NotYetEmbedded { total });
    }
    if failed_count == 0 && done_count == total {
        return Ok(EmbedStatus::Done {
            done: done_count,
            total,
        });
    }
    Ok(EmbedStatus::DoneWithFailures {
        failed: failed_count,
        sample_error: sample_error(conn, embedder_version)?,
    })
}

/// One sample `last_error` for the current embedder version, for the failure report.
fn sample_error(conn: &Connection, embedder_version: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT last_error FROM embed_ledger
         WHERE embedder_version = ?1 AND status = 'failed' AND last_error IS NOT NULL
         LIMIT 1",
        rusqlite::params![embedder_version],
        |r| r.get(0),
    )
    .optional()
    .context("sampling a failure last_error")
}

/// Render a classified status as the one-line message `--status` prints.
fn render(status: &EmbedStatus) -> String {
    match status {
        EmbedStatus::NotYetEmbedded { total } => {
            format!("not-yet-embedded: 0 of {total} assembled units embedded")
        }
        EmbedStatus::Running { embedded, total } => {
            format!("running: {embedded}/{total} units embedded")
        }
        EmbedStatus::Stalled {
            pid,
            started_at,
            heartbeat_at,
        } => format!(
            "stalled: run pid {pid} started {}, last heartbeat {} (past the stale threshold)",
            rfc3339_from_secs(*started_at),
            rfc3339_from_secs(*heartbeat_at),
        ),
        EmbedStatus::Done { done, total } => {
            format!("done: {done}/{total} units embedded")
        }
        EmbedStatus::DoneWithFailures {
            failed,
            sample_error,
        } => match sample_error {
            Some(err) => format!("done with {failed} failed: sample error: {err}"),
            None => format!("done with {failed} failed"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open_db;

    /// The embedder version the classifier keys ledger counts on, matching what a
    /// real drain would stamp for the default model.
    const EV: &str = "qwen3-embedding:8b/profile-1";

    fn open(tmp: &tempfile::TempDir) -> Connection {
        open_db(&tmp.path().join("hs.db")).unwrap()
    }

    fn insert_run(conn: &Connection, state: &str, heartbeat_at: i64, embedded: i64, total: i64) {
        conn.execute(
            "INSERT INTO embed_run
                (started_at, heartbeat_at, pid, state, total, embedded, skipped, failed)
             VALUES (0, ?1, 4242, ?2, ?3, ?4, 0, 0)",
            rusqlite::params![heartbeat_at, state, total, embedded],
        )
        .unwrap();
    }

    fn insert_ledger(conn: &Connection, source_id: &str, status: &str, last_error: Option<&str>) {
        conn.execute(
            "INSERT INTO embed_ledger
                (unit_kind, source_id, embedder_version, embedded_at, status, attempts, last_error)
             VALUES ('event', ?1, ?2, '2026-01-01T00:00:00Z', ?3, 1, ?4)",
            rusqlite::params![source_id, EV, status, last_error],
        )
        .unwrap();
    }

    /// A fresh-heartbeat running run classifies as running with progress (D-07).
    #[test]
    fn running_with_fresh_heartbeat() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open(&tmp);
        let now = 1_000_000;
        insert_run(&conn, "running", now - 5, 3, 10);
        let s = classify(&conn, 10, EV, now).unwrap();
        assert_eq!(s, EmbedStatus::Running {
            embedded: 3,
            total: 10
        });
    }

    /// A running run whose heartbeat is older than the stale threshold is stalled.
    #[test]
    fn running_with_stale_heartbeat_is_stalled() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open(&tmp);
        let now = 1_000_000;
        let hb = now - (STALE_HEARTBEAT_SECS + 30);
        insert_run(&conn, "running", hb, 2, 10);
        let s = classify(&conn, 10, EV, now).unwrap();
        assert_eq!(s, EmbedStatus::Stalled {
            pid: 4242,
            started_at: 0,
            heartbeat_at: hb
        });
    }

    /// A terminal run whose done-count covers the assembled total, no failures, is
    /// done.
    #[test]
    fn done_when_ledger_covers_total() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open(&tmp);
        insert_run(&conn, "done", 0, 2, 2);
        insert_ledger(&conn, "1", "done", None);
        insert_ledger(&conn, "2", "done", None);
        let s = classify(&conn, 2, EV, 1_000_000).unwrap();
        assert_eq!(s, EmbedStatus::Done { done: 2, total: 2 });
    }

    /// A terminal run with a current-version failed row reports done-with-failures
    /// and surfaces one last_error sample.
    #[test]
    fn failed_row_reports_done_with_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open(&tmp);
        insert_run(&conn, "done", 0, 1, 2);
        insert_ledger(&conn, "1", "done", None);
        insert_ledger(&conn, "2", "failed", Some("simulated ollama failure"));
        let s = classify(&conn, 2, EV, 1_000_000).unwrap();
        assert_eq!(
            s,
            EmbedStatus::DoneWithFailures {
                failed: 1,
                sample_error: Some("simulated ollama failure".to_string()),
            }
        );
    }

    /// No run record and an empty ledger is not-yet-embedded.
    #[test]
    fn empty_db_is_not_yet_embedded() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open(&tmp);
        let s = classify(&conn, 7, EV, 1_000_000).unwrap();
        assert_eq!(s, EmbedStatus::NotYetEmbedded { total: 7 });
    }
}
