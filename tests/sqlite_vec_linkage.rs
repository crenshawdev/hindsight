//! Linkage spike (Phase 3, Plan 1, GATING): prove sqlite-vec is statically
//! linked into rusqlite's bundled SQLite and that a 4096-dim vector round-trips
//! (insert, then nearest-neighbor returns it) in one DB file.
//!
//! The proof of static linkage is that registration goes through
//! `sqlite3_auto_extension` with the compiled-in `sqlite3_vec_init` symbol.
//! There is no `Connection::load_extension` call and no `.so`/`.dylib` on disk:
//! the extension C source is compiled into the test binary by sqlite-vec's
//! build.rs and linked against the bundled SQLite amalgamation.

use std::sync::Once;

use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::Connection;

/// The vector dimension the store will use (ADR 0004: qwen3-embedding, 4096 dims).
const DIM: usize = 4096;

static REGISTER: Once = Once::new();

/// Register the statically-linked sqlite-vec extension exactly once per process.
fn register_sqlite_vec() {
    REGISTER.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Serialize an f32 slice to the little-endian byte blob sqlite-vec expects for
/// a `float[N]` vector column.
fn vector_blob(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

#[test]
fn vec0_round_trip_returns_nearest_vector() {
    register_sqlite_vec();

    // One DB file, one connection. In-memory is still a single SQLite DB; no
    // extension file is loaded at runtime.
    let conn = Connection::open_in_memory().expect("open in-memory db");

    // Sanity: the vec extension is actually registered and compiled in.
    let vec_version: String = conn
        .query_row("SELECT vec_version()", [], |r| r.get(0))
        .expect("vec_version() should resolve via the linked extension");
    assert!(
        vec_version.starts_with('v'),
        "unexpected vec_version: {vec_version}"
    );

    conn.execute(
        &format!("CREATE VIRTUAL TABLE vec_items USING vec0(embedding float[{DIM}])"),
        [],
    )
    .expect("create vec0 table");

    // Two clearly different known vectors at known rowids.
    let target: Vec<f32> = vec![0.25_f32; DIM];
    let decoy: Vec<f32> = vec![0.90_f32; DIM];
    let target_rowid: i64 = 1;
    let decoy_rowid: i64 = 2;

    conn.execute(
        "INSERT INTO vec_items(rowid, embedding) VALUES (?1, ?2)",
        rusqlite::params![target_rowid, vector_blob(&target)],
    )
    .expect("insert target vector");
    conn.execute(
        "INSERT INTO vec_items(rowid, embedding) VALUES (?1, ?2)",
        rusqlite::params![decoy_rowid, vector_blob(&decoy)],
    )
    .expect("insert decoy vector");

    // Nearest-neighbor query with the target vector itself: it must rank first.
    let top_rowid: i64 = conn
        .query_row(
            "SELECT rowid FROM vec_items WHERE embedding MATCH ?1 ORDER BY distance LIMIT 1",
            rusqlite::params![vector_blob(&target)],
            |r| r.get(0),
        )
        .expect("knn query returns a row");

    assert_eq!(
        top_rowid, target_rowid,
        "nearest neighbor of the target vector should be the target row"
    );

    // The ranking is real: the decoy is present but must not win against its
    // own probe over the target's probe. Probe with the decoy and confirm it
    // ranks the decoy first, so the query is genuinely ordering by distance.
    let top_for_decoy: i64 = conn
        .query_row(
            "SELECT rowid FROM vec_items WHERE embedding MATCH ?1 ORDER BY distance LIMIT 1",
            rusqlite::params![vector_blob(&decoy)],
            |r| r.get(0),
        )
        .expect("knn query returns a row for decoy probe");
    assert_eq!(
        top_for_decoy, decoy_rowid,
        "nearest neighbor of the decoy vector should be the decoy row"
    );
}
