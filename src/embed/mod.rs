//! `hindsight embed` (D-02): assemble synthetic profile units from the loaded
//! store and embed them via Ollama into the two-stage `vec_embedding` table. A
//! drain-and-exit batch command matching the `normalize`/`load` pattern, driven by
//! a systemd timer (D-03), never folded into the capture daemon.
//!
//! `--dump-profiles` is the Ollama-free inspection sink (D-11): it prints the
//! assembled units as NDJSON and writes no vectors, so profile assembly is
//! machine-checkable without an embedder.

pub mod ollama;
pub mod profile;

use std::io::Write;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config::Config;
use crate::store::open_db;
use ollama::Placement;
use profile::ProfileUnit;

/// Open the store, assemble profile units, and either dump them (D-11) or embed
/// each and write its vector into `vec_embedding`.
pub fn run(cfg: &Config, dump_profiles: bool) -> Result<()> {
    let conn = open_db(&cfg.db_path())?;
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

    for unit in &units {
        let vector = ollama::embed_document(&cfg.embed, &unit.text, Placement::Gpu)
            .with_context(|| format!("embedding {} unit {}", unit.unit_kind, unit.source_id))?;
        insert_vector(&conn, unit, &vector)
            .with_context(|| format!("storing vector for {} {}", unit.unit_kind, unit.source_id))?;
    }
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

/// Insert one profile unit's vector: the coarse companion is quantized from the
/// same full-precision blob, and `project`/`unit_kind`/`source_id` carry the
/// pre-filter column and the mapping back to the source record (D-09).
fn insert_vector(conn: &Connection, unit: &ProfileUnit, vector: &[f32]) -> Result<()> {
    let blob = vector_blob(vector);
    conn.execute(
        "INSERT INTO vec_embedding(embedding_coarse, embedding, project, unit_kind, source_id)
         VALUES (vec_quantize_binary(?1), ?1, ?2, ?3, ?4)",
        rusqlite::params![blob, unit.project, unit.unit_kind, unit.source_id],
    )
    .context("inserting vec_embedding row")?;
    Ok(())
}
