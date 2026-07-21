//! Integration test for `hindsight normalize` over both transcript formats.
//!
//! Drives the real compiled binary (`CARGO_BIN_EXE_hindsight`) against archived
//! `.zst` fixtures and checks the five Phase 2 acceptance criteria end to end:
//! mention/artifact equality with the tool-call inputs, both-format subagent
//! filing under the parent session, exactly-one-grain with no skeleton body
//! leaking, secret scrubbing, and a valid tagged-NDJSON stream.
//!
//! Fixture A is the live nested-split format (parent file plus a nested
//! `subagents/agent-<id>/` generation). Fixture B is the hand-authored inline
//! format reconstructed from ADR 0003 (D-09); it approximates the historical
//! shape, which has no live sample.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

const FIXTURE_A_PARENT: &str = include_str!("fixtures/normalize/nested_split_parent.jsonl");
const FIXTURE_A_SUBAGENT: &str = include_str!("fixtures/normalize/nested_split_subagent.jsonl");
const FIXTURE_B_INLINE: &str = include_str!("fixtures/normalize/inline_subagent.jsonl");

const SECRET: &str = "sk-SEEDEDSECRET0123456789";
const SKELETON_MARKER: &str = "SKELETON_BODY_MARKER";
const GRAINS: [&str; 3] = ["indexed", "skeleton", "archive-only"];

/// Write `bytes` as the single `0000.zst` generation of an archive directory.
fn write_generation(dir: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    let compressed = zstd::encode_all(bytes, 3).unwrap();
    std::fs::write(dir.join("0000.zst"), compressed).unwrap();
}

/// Run `hindsight normalize <session_dir>`; return (stdout, success).
fn run_normalize(session_dir: &Path) -> (String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_hindsight"))
        .arg("normalize")
        .arg(session_dir)
        .output()
        .expect("spawning hindsight normalize");
    (
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        output.status.success(),
    )
}

/// Parse each non-empty stdout line into a JSON value (criterion 5's parse step).
fn parse_lines(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("line not JSON: {l}: {e}")))
        .collect()
}

/// Assert the five acceptance criteria over one fixture's normalized output.
///
/// `session_id` is the logical session's id, `expected_files` the Read/Edit/Write
/// `file_path` inputs, `expected_commands` the Bash argv[0]s, and
/// `expected_artifacts` the Write/Edit content bodies that must appear verbatim.
fn assert_acceptance(
    stdout: &str,
    success: bool,
    session_id: &str,
    expected_files: &[&str],
    expected_commands: &[&str],
    expected_artifacts: &[&str],
) {
    // Criterion 2 (part): the run exits 0.
    assert!(success, "normalize must exit 0");

    let records = parse_lines(stdout);
    assert!(!records.is_empty(), "at least the session line is emitted");

    // Criterion 5: every line is JSON with a type in the tagged set.
    for r in &records {
        let ty = r["type"].as_str().expect("record has a string type");
        assert!(
            matches!(ty, "session" | "event" | "artifact" | "mention"),
            "unexpected record type {ty}"
        );
    }

    // Criterion 4: the seeded secret is absent from the whole stream.
    assert!(
        !stdout.contains(SECRET),
        "secret {SECRET} must be scrubbed from the NDJSON"
    );
    // Criterion 3 (part): no skeleton/tool-result body text reaches the output.
    assert!(
        !stdout.contains(SKELETON_MARKER),
        "skeleton body marker must not appear in the indexed output"
    );

    let events: Vec<&Value> = records.iter().filter(|r| r["type"] == "event").collect();
    assert!(!events.is_empty(), "events were emitted");

    // Criterion 3 (part): every event carries exactly one grain in the set.
    for e in &events {
        let grain = e["grain"].as_str().expect("event has a grain");
        assert!(GRAINS.contains(&grain), "grain {grain} not one of the three");
    }

    // Criterion 2: subagent events are filed under the parent session. Assert a
    // positive count so parent-filing is not vacuously true when subagents drop.
    let sidechain = events
        .iter()
        .filter(|e| e["is_sidechain"].as_bool() == Some(true))
        .count();
    assert!(sidechain > 0, "at least one sidechain event was filed");
    for e in &events {
        if e["is_sidechain"].as_bool() == Some(true) {
            assert_eq!(
                e["session_id"].as_str(),
                Some(session_id),
                "sidechain event filed under the parent session id"
            );
        }
    }

    // Criterion 1: mention file/command entities equal the tool-call inputs.
    let files: Vec<&str> = records
        .iter()
        .filter(|r| r["type"] == "mention" && r["entity_type"] == "file")
        .map(|r| r["entity"].as_str().unwrap())
        .collect();
    let commands: Vec<&str> = records
        .iter()
        .filter(|r| r["type"] == "mention" && r["entity_type"] == "command")
        .map(|r| r["entity"].as_str().unwrap())
        .collect();
    for f in expected_files {
        assert!(files.contains(f), "expected file mention {f}, got {files:?}");
    }
    for c in expected_commands {
        assert!(
            commands.contains(c),
            "expected command mention {c}, got {commands:?}"
        );
    }

    // Criterion 1: artifact contents equal the Write/Edit inputs.
    let contents: Vec<&str> = records
        .iter()
        .filter(|r| r["type"] == "artifact")
        .map(|r| r["content"].as_str().unwrap())
        .collect();
    for a in expected_artifacts {
        assert!(
            contents.contains(a),
            "expected artifact content {a:?}, got {contents:?}"
        );
    }
}

#[test]
fn nested_split_fixture_meets_acceptance_criteria() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("projA").join("sessA");
    write_generation(&session_dir, FIXTURE_A_PARENT.as_bytes());
    write_generation(
        &session_dir.join("subagents").join("agent-rev1"),
        FIXTURE_A_SUBAGENT.as_bytes(),
    );

    let (stdout, success) = run_normalize(&session_dir);
    assert_acceptance(
        &stdout,
        success,
        "sessA",
        &["/repo/src/main.rs", "/repo/notes.txt"],
        &["cargo"],
        &["first line of notes\nsecond line of notes"],
    );
}

#[test]
fn inline_subagent_fixture_meets_acceptance_criteria() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("projB").join("sessB");
    write_generation(&session_dir, FIXTURE_B_INLINE.as_bytes());

    let (stdout, success) = run_normalize(&session_dir);
    assert_acceptance(
        &stdout,
        success,
        "sessB",
        &["/repo/README.md", "/repo/out.md"],
        &["git"],
        &["inline artifact body content"],
    );
}
