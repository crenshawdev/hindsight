//! Integration test for mechanical profile assembly (Phase 4, D-05/D-07/D-08).
//!
//! This package is binary-only (no `[lib]`), so a test cannot call
//! `embed::profile::assemble` directly (the same constraint tests/store_load.rs
//! documents). Instead it drives the real compiled binary: pipe a crafted NDJSON
//! record set into `hindsight load`, then run `hindsight embed --dump-profiles`
//! (the Ollama-free inspection sink, D-11) and assert on the emitted units.
//!
//! What it proves: the full-code artifact body is excluded while its whitelisted
//! signature line survives (D-08 code-body exclusion), all three unit kinds are
//! present, and the composite `{entity_type}:{entity}` source_id keeps a `file`
//! and a `command` sharing one surface string as two distinct entity units.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// A unique line in an artifact body that must never reach a profile text.
const CODEBODY_SENTINEL: &str = "let HINDSIGHT_CODEBODY_SENTINEL = compute();";
/// A declaration line in the same body that MUST survive as the signature.
const SIGNATURE_LINE: &str = "fn compute() -> u32 {";

fn ndjson_fixture() -> String {
    let records = vec![
        json!({
            "type": "session",
            "session_id": "sess1", "project": "projX",
            "git_branch": null, "cc_version": null,
            "started_at": "2026-07-21T10:00:00Z", "ended_at": "2026-07-21T10:05:00Z",
            "end_reason": null, "title": null, "archive_refs": []
        }),
        json!({
            "type": "event",
            "uuid": "u1", "parent_uuid": null, "session_id": "sess1",
            "role": "user", "kind": "text", "timestamp": "2026-07-21T10:00:00Z",
            "text": "please build the widget", "tool_name": null, "is_error": null,
            "attribution": null, "is_sidechain": false, "agent_id": null,
            "agent_type": null, "grain": "indexed"
        }),
        json!({
            "type": "event",
            "uuid": "u2", "parent_uuid": "u1", "session_id": "sess1",
            "role": "assistant", "kind": "text", "timestamp": "2026-07-21T10:01:00Z",
            "text": "wrote the compute function", "tool_name": null, "is_error": null,
            "attribution": null, "is_sidechain": false, "agent_id": null,
            "agent_type": null, "grain": "indexed"
        }),
        json!({
            "type": "artifact",
            "artifact_id": "art1", "kind": "file", "path": "src/lib.rs",
            "language": "rust",
            "content": format!("{SIGNATURE_LINE}\n    {CODEBODY_SENTINEL}\n}}"),
            "request_bundle": "u1", "source_event_uuid": "u2"
        }),
        // Same surface string "build", two entity types: must not collide.
        json!({
            "type": "mention",
            "entity": "build", "entity_type": "file", "event_uuid": "u1",
            "session_id": "sess1", "project": "projX", "timestamp": "2026-07-21T10:00:00Z"
        }),
        json!({
            "type": "mention",
            "entity": "build", "entity_type": "command", "event_uuid": "u2",
            "session_id": "sess1", "project": "projX", "timestamp": "2026-07-21T10:01:00Z"
        }),
    ];
    records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn assemble_excludes_code_body_keeps_signature_and_splits_entity_types() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_home = tmp.path().join("cfg");
    let data_dir = tmp.path().join("data");
    let cfg_file_dir = cfg_home.join("hindsight");
    std::fs::create_dir_all(&cfg_file_dir).unwrap();
    std::fs::write(
        cfg_file_dir.join("config.toml"),
        format!("base_dir = {:?}\n", data_dir),
    )
    .unwrap();

    // Pipe the crafted NDJSON into `hindsight load`.
    let mut load = Command::new(env!("CARGO_BIN_EXE_hindsight"))
        .arg("load")
        .env("XDG_CONFIG_HOME", &cfg_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    load.stdin
        .take()
        .unwrap()
        .write_all(ndjson_fixture().as_bytes())
        .unwrap();
    let load_out = load.wait_with_output().unwrap();
    assert!(
        load_out.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&load_out.stderr)
    );

    // Dump assembled profiles (no Ollama).
    let dump = Command::new(env!("CARGO_BIN_EXE_hindsight"))
        .arg("embed")
        .arg("--dump-profiles")
        .env("XDG_CONFIG_HOME", &cfg_home)
        .output()
        .unwrap();
    assert!(
        dump.status.success(),
        "dump-profiles failed: {}",
        String::from_utf8_lossy(&dump.stderr)
    );
    let dump_stdout = String::from_utf8(dump.stdout).unwrap();

    let units: Vec<Value> = dump_stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("unit line parses"))
        .collect();
    assert!(!units.is_empty(), "at least one unit emitted");

    // D-08 code-body exclusion: the sentinel line never reaches any text, but the
    // whitelisted signature line does.
    let mut saw_signature = false;
    for u in &units {
        let text = u["text"].as_str().unwrap();
        assert!(
            !text.contains("HINDSIGHT_CODEBODY_SENTINEL"),
            "code body leaked into unit {}: {text}",
            u["source_id"]
        );
        if text.contains(SIGNATURE_LINE.trim_end_matches(" {").trim()) {
            saw_signature = true;
        }
    }
    assert!(saw_signature, "the signature line must survive in some unit");

    // All three unit kinds present.
    let kinds: std::collections::HashSet<&str> =
        units.iter().map(|u| u["unit_kind"].as_str().unwrap()).collect();
    for kind in ["entity", "artifact", "event"] {
        assert!(kinds.contains(kind), "missing unit_kind {kind}");
    }

    // Composite source_id keeps the file and command "build" as distinct units.
    let entity_ids: std::collections::HashSet<&str> = units
        .iter()
        .filter(|u| u["unit_kind"] == "entity")
        .map(|u| u["source_id"].as_str().unwrap())
        .collect();
    assert!(entity_ids.contains("file:build"), "missing file:build entity");
    assert!(
        entity_ids.contains("command:build"),
        "missing command:build entity"
    );

    // Every unit carries a non-empty project (criterion 2 precondition).
    for u in &units {
        assert!(
            !u["project"].as_str().unwrap().is_empty(),
            "unit {} has empty project",
            u["source_id"]
        );
    }
}
