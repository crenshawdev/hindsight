//! `hindsight embed` (D-02): assemble synthetic profile units from the loaded
//! store and embed them via Ollama into the two-stage `vec_embedding` table. A
//! drain-and-exit batch command matching the `normalize`/`load` pattern, driven by
//! a systemd timer (D-03), never folded into the capture daemon.
//!
//! `--dump-profiles` is the Ollama-free inspection sink (D-11): it prints the
//! assembled units as NDJSON and writes no vectors, so profile assembly is
//! machine-checkable without an embedder.
//!
//! The drain is resumable (D-06): each embedded unit is stamped in `embed_ledger`
//! in the SAME transaction as its vector insert, so a deferred, interrupted, or
//! CPU-fallback run resumes exactly - the ledger skip-check never skips a unit
//! whose vector did not land, and never re-embeds one that did.

pub mod gpu;
pub mod ollama;
pub mod profile;

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use crate::config::Config;
use crate::store::open_db;

/// The profile-construction contract the stored vectors were built under. Bumped
/// when the mechanical assembly in `profile.rs` changes shape, so a re-embed under
/// a new profile version re-stamps the ledger and clears stale vectors.
pub const PROFILE_SCHEMA_VERSION: &str = "1";

/// Open the store, assemble profile units, and either dump them (D-11) or drain
/// the queue into `vec_embedding`, skipping already-embedded units (D-06).
pub fn run(cfg: &Config, dump_profiles: bool) -> Result<()> {
    let mut conn = open_db(&cfg.db_path())?;
    let units = profile::assemble(&conn)?;

    if dump_profiles {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for unit in &units {
            let line = serde_json::to_string(unit).context("serializing profile unit to JSON")?;
            writeln!(out, "{line}").context("writing profile NDJSON line")?;
        }
        return Ok(());
    }

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

    // Choose GPU vs CPU once before the drain (D-05): a free card runs on the
    // GPU, a busy card defers then falls back to CPU, an absent card runs on CPU
    // immediately. A busy GPU never aborts the run.
    let placement = gpu::choose_placement(&cfg.embed);
    tracing::info!(?placement, "embed placement chosen");

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

        let vector = ollama::embed_document(&cfg.embed, &unit.text, placement)
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
