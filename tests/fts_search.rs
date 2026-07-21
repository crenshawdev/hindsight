//! Integration test for the FTS5 BM25 index (PLAN-3, CONTEXT criteria 3 and 4).
//!
//! Drives the real compiled binary end to end: archive a synthetic transcript,
//! `hindsight normalize` it to NDJSON, pipe that stream into `hindsight load`,
//! then open the resulting SQLite file and query the `fts` table directly.
//!
//! Same harness as tests/store_load.rs: this crate is binary-only, so the tests
//! shell out via `CARGO_BIN_EXE_hindsight` with a temp `XDG_CONFIG_HOME`/`base_dir`
//! and read the loaded DB with rusqlite (a normal dependency, available to
//! integration tests).
//!
//! Positive (Task 1): a bareword term in an indexed-grain event returns the
//! session that contains it. Negative (Task 2): a sentinel present ONLY in a
//! skeleton tool_result body returns zero rows, and no Mention record ever
//! produces an FTS row.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Write `bytes` as the single `0000.zst` generation of an archive directory.
fn write_generation(dir: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    let compressed = zstd::encode_all(bytes, 3).unwrap();
    std::fs::write(dir.join("0000.zst"), compressed).unwrap();
}

/// Archive `transcript` under `<tmp>/archived/projF/<session_id>`, normalize +
/// load it against a fresh DB rooted at `<tmp>/data`, and return the DB path.
/// The whole pipeline uses one temp `XDG_CONFIG_HOME`/`base_dir` per call.
fn normalize_and_load(tmp: &Path, session_id: &str, transcript: &str) -> std::path::PathBuf {
    let cfg_home = tmp.join("cfg");
    let data_dir = tmp.join("data");
    let cfg_file_dir = cfg_home.join("hindsight");
    std::fs::create_dir_all(&cfg_file_dir).unwrap();
    std::fs::write(
        cfg_file_dir.join("config.toml"),
        format!("base_dir = {:?}\n", data_dir),
    )
    .unwrap();

    let session_dir = tmp.join("archived").join("projF").join(session_id);
    write_generation(&session_dir, transcript.as_bytes());

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

    data_dir.join("index").join("hindsight.db")
}

/// A transcript whose assistant text event carries a unique bareword term
/// (`zylophonics`), a Write artifact whose content carries `xylobyteword`, and a
/// Read whose tool_result body carries the skeleton-only sentinel
/// `qwertysentinel42`. The sentinel is kept out of every indexed field: not in
/// any event text, not in the Write content, and not in a tool_use summary
/// (file_path/command), so its only home is the blanked tool_result body.
fn transcript(session_id: &str) -> String {
    format!(
        concat!(
            r#"{{"type":"user","uuid":"f-u1","sessionId":"{sid}","gitBranch":"main","version":"1.2.3","timestamp":"2026-07-21T10:00:00Z","message":{{"role":"user","content":"please read the file and take notes"}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"f-a1","parentUuid":"f-u1","sessionId":"{sid}","timestamp":"2026-07-21T10:00:01Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Here is the plan involving zylophonics for the review."}}]}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"f-a2","parentUuid":"f-a1","sessionId":"{sid}","timestamp":"2026-07-21T10:00:02Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"tool-read-1","name":"Read","input":{{"file_path":"/repo/notes.txt"}}}}]}}}}"#,
            "\n",
            r#"{{"type":"user","uuid":"f-u2","parentUuid":"f-a2","sessionId":"{sid}","timestamp":"2026-07-21T10:00:03Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tool-read-1","content":"qwertysentinel42 the full file body that must never reach the index","is_error":false}}]}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"f-a3","parentUuid":"f-u2","sessionId":"{sid}","timestamp":"2026-07-21T10:00:04Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"tool-write-1","name":"Write","input":{{"file_path":"/repo/out.txt","content":"artifact body carrying xylobyteword content"}}}}]}}}}"#,
            "\n"
        ),
        sid = session_id
    )
}

#[test]
fn indexed_event_and_artifact_terms_return_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let session_id = "sessF";
    let db_path = normalize_and_load(tmp.path(), session_id, &transcript(session_id));

    let conn = rusqlite::Connection::open(&db_path).expect("opening loaded DB");

    // Criterion 3 (event): the indexed assistant-text bareword returns the
    // session containing it.
    let hits: Vec<String> = conn
        .prepare("SELECT session_id FROM fts WHERE fts MATCH 'zylophonics'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        hits,
        vec![session_id.to_string()],
        "indexed event term must return exactly the loaded session"
    );

    // Criterion 3 (artifact): the Write artifact content bareword returns the
    // session, resolved through the artifact->event join (D-04, DISTINCT).
    let art_hits: Vec<String> = conn
        .prepare(
            "SELECT session_id FROM fts WHERE fts MATCH 'xylobyteword' AND source_type = 'artifact'",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        art_hits,
        vec![session_id.to_string()],
        "indexed artifact term must return exactly the loaded session, once"
    );
}

#[test]
fn skeleton_body_and_mentions_never_enter_fts() {
    let tmp = tempfile::tempdir().unwrap();
    let session_id = "sessF";
    let db_path = normalize_and_load(tmp.path(), session_id, &transcript(session_id));

    let conn = rusqlite::Connection::open(&db_path).expect("opening loaded DB");

    // Honesty check: an indexed term IS present, so a zero below proves exclusion
    // rather than an empty index.
    let indexed: i64 = conn
        .query_row(
            "SELECT count(*) FROM fts WHERE fts MATCH 'zylophonics'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(indexed > 0, "the indexed sentinel must match, or the test is vacuous");

    // Criterion 4 (skeleton, string proof): a sentinel present ONLY in a blanked
    // tool_result body never enters FTS.
    let skeleton: i64 = conn
        .query_row(
            "SELECT count(*) FROM fts WHERE fts MATCH 'qwertysentinel42'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        skeleton, 0,
        "a skeleton-only tool_result body must produce no FTS row"
    );

    // Mention exclusion (structural proof): the loader only ever writes a
    // source_type of 'event' or 'artifact', so no FTS row can carry 'mention'.
    // This is collision-proof, unlike MATCHing a Mention's file path (which also
    // appears as a tool_use event's indexed text).
    let mention_rows: i64 = conn
        .query_row(
            "SELECT count(*) FROM fts WHERE source_type = 'mention'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mention_rows, 0, "Mention records must produce no FTS row");
}
