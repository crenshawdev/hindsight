//! Integration test for `hindsight load` (PLAN-2, criterion 2 and 5).
//!
//! Drives the real compiled binary end to end: archive a fixture session,
//! `hindsight normalize` it to NDJSON, pipe that stream into `hindsight load`,
//! then open the resulting SQLite file and assert each relational table's row
//! count equals the count of NDJSON lines carrying that `type` (criterion 2),
//! and that the DB sits under `base_dir/index/` with nothing written at the
//! stand-in volume root (ARC-02, criterion 5).
//!
//! This package is binary-only (no `[lib]`), so the test cannot call internals;
//! it uses the `CARGO_BIN_EXE_hindsight` + local `write_generation` pattern from
//! tests/normalize.rs and reads the loaded DB with rusqlite (a normal dependency,
//! available to integration tests).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

const FIXTURE_PARENT: &str = include_str!("fixtures/normalize/nested_split_parent.jsonl");
const FIXTURE_SUBAGENT: &str = include_str!("fixtures/normalize/nested_split_subagent.jsonl");

/// Write `bytes` as the single `0000.zst` generation of an archive directory.
fn write_generation(dir: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    let compressed = zstd::encode_all(bytes, 3).unwrap();
    std::fs::write(dir.join("0000.zst"), compressed).unwrap();
}

#[test]
fn load_row_counts_match_ndjson_types_and_db_stays_under_index() {
    let tmp = tempfile::tempdir().unwrap();

    // Config the binary will read: base_dir under the tempdir (the stand-in
    // volume root), config.toml under a separate XDG_CONFIG_HOME.
    let cfg_home = tmp.path().join("cfg");
    let data_dir = tmp.path().join("data");
    let cfg_file_dir = cfg_home.join("hindsight");
    std::fs::create_dir_all(&cfg_file_dir).unwrap();
    std::fs::write(
        cfg_file_dir.join("config.toml"),
        format!("base_dir = {:?}\n", data_dir),
    )
    .unwrap();

    // Archive a fixture session (parent plus a nested subagent generation) so
    // normalize emits a rich multi-type stream.
    let session_dir = tmp.path().join("archived").join("projA").join("sessA");
    write_generation(&session_dir, FIXTURE_PARENT.as_bytes());
    write_generation(
        &session_dir.join("subagents").join("agent-rev1"),
        FIXTURE_SUBAGENT.as_bytes(),
    );

    // `hindsight normalize <session_dir>` -> tagged NDJSON on stdout.
    let normalize_out = Command::new(env!("CARGO_BIN_EXE_hindsight"))
        .arg("normalize")
        .arg(&session_dir)
        .env("XDG_CONFIG_HOME", &cfg_home)
        .output()
        .expect("spawning hindsight normalize");
    assert!(
        normalize_out.status.success(),
        "normalize must exit 0: {}",
        String::from_utf8_lossy(&normalize_out.stderr)
    );
    let ndjson = normalize_out.stdout;

    // Tally emitted records by `type` (criterion 2's `jq -r .type | uniq -c`).
    let mut expected = std::collections::HashMap::<String, i64>::new();
    for line in String::from_utf8(ndjson.clone()).unwrap().lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).expect("emitted line is JSON");
        let ty = v["type"].as_str().expect("record has a type").to_string();
        *expected.entry(ty).or_insert(0) += 1;
    }
    // The stream carries at least one of each type, or the equality check below
    // would be vacuous for a missing table.
    for ty in ["session", "event", "artifact", "mention"] {
        assert!(
            expected.get(ty).copied().unwrap_or(0) > 0,
            "fixture stream should carry at least one {ty} record, got {expected:?}"
        );
    }

    // `hindsight load` with the captured NDJSON on stdin.
    let mut child = Command::new(env!("CARGO_BIN_EXE_hindsight"))
        .arg("load")
        .env("XDG_CONFIG_HOME", &cfg_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning hindsight load");
    child
        .stdin
        .take()
        .expect("load stdin piped")
        .write_all(&ndjson)
        .expect("writing NDJSON to load stdin");
    let load_out = child.wait_with_output().expect("waiting on hindsight load");
    assert!(
        load_out.status.success(),
        "load must exit 0: {}",
        String::from_utf8_lossy(&load_out.stderr)
    );

    // The DB must sit at base_dir/index/hindsight.db (D-09).
    let db_path = data_dir.join("index").join("hindsight.db");
    assert!(db_path.exists(), "DB must exist at {}", db_path.display());

    // ARC-02 (criterion 5): nothing DB-like is written at the stand-in volume
    // root (tempdir); the only DB file lives under index/.
    for entry in std::fs::read_dir(&data_dir).unwrap() {
        let path = entry.unwrap().path();
        assert!(
            !(path.is_file() && path.extension().is_some_and(|e| e == "db")),
            "no DB file may sit at the base_dir root, found {}",
            path.display()
        );
    }

    // Criterion 2: per-table row counts equal the emitted per-type line counts.
    let conn = rusqlite::Connection::open(&db_path).expect("opening loaded DB");
    for table in ["session", "event", "artifact", "mention"] {
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count,
            expected.get(table).copied().unwrap_or(0),
            "row count of {table} must equal emitted {table} records"
        );
    }
}
