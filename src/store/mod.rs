//! The SQLite store: schema definition and the `open_db` entry point (PLAN-2).
//! The NDJSON loader (`load`) is added in Task 4.
//!
//! One file holds the relational records, an empty `vec0` vector table, and a
//! provenance stamp. sqlite-vec is statically linked into rusqlite's bundled
//! SQLite (PLAN-1) and registered via `sqlite3_auto_extension`, never a runtime
//! `.so`.

pub mod schema;

use std::path::Path;
use std::sync::Once;

use anyhow::{Context, Result};
use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::Connection;

static REGISTER_VEC: Once = Once::new();

/// Register the statically-linked sqlite-vec extension exactly once per process
/// (the pattern PLAN-1 proved in tests/sqlite_vec_linkage.rs). No
/// `load_extension`, no `.so`: `sqlite3_vec_init` is compiled into the binary
/// and installed as an auto-extension so every later connection carries `vec0`.
fn register_sqlite_vec() {
    REGISTER_VEC.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Open (creating if absent) the SQLite index at `path`, register sqlite-vec,
/// and apply the schema. The parent directory is created if missing so the
/// caller only needs `cfg.db_path()` (D-09).
pub fn open_db(path: &Path) -> Result<Connection> {
    register_sqlite_vec();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating index directory {}", parent.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("opening SQLite index at {}", path.display()))?;
    schema::apply(&conn)?;
    Ok(conn)
}
