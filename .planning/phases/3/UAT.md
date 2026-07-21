---
status: testing
phase: 3
started: 2026-07-21
updated: 2026-07-21
---

## Items

### 1. Cold-start smoke test
expected: From a clean state (empty/truncated DB), building the binary and piping `hindsight normalize <archived-session> | hindsight load` completes with no error: the schema is created from scratch, load finishes, and a COUNT(*) query against a loaded table returns real rows.
status: pass
first_pass: pass
source: verifier
evidence: Manual run against built binary: normalize exit 0, load exit 0 on fresh tempdir (no pre-existing DB); counts session 1/event 9/artifact 1/mention 3. open_db creates parent dir + applies schema (src/store/mod.rs:36-46). tests/store_load.rs:32 passes.

### 2. vec0 vector round-trip from single binary
expected: From the one built binary with no separately-shipped extension file present, inserting a known 4096-dim vector into a vec0 table and running a nearest-neighbor query returns that vector's row as the top match, all in one DB file.
status: pass
first_pass: pass
source: verifier
evidence: tests/sqlite_vec_linkage.rs:41 passes: 4096-dim insert, NN returns target rowid as top over decoy. Registration via sqlite3_auto_extension(sqlite3_vec_init) (src/store/mod.rs:25-31). ldd shows no sqlite/vec dynamic lib; production binary creates vec_embedding vec0(float[4096]) in the real DB (would raise 'no such module: vec0' if unlinked).

### 3. Row counts match NDJSON type tally
expected: Piping `hindsight normalize <archived-session>` into the loader against an empty DB, the row count of each of session/event/artifact/mention equals the count of NDJSON lines with that `type` in the same stream (jq tally vs sqlite3 COUNT(*)).
status: pass
first_pass: pass
source: verifier
evidence: Manual: NDJSON tally {session:1,event:9,artifact:1,mention:3} == DB COUNT(*) exactly. tests/store_load.rs:126-137 asserts equality per type against the emitted stream.

### 4. FTS5 BM25 returns the containing session
expected: After loading, an FTS5 BM25 query for a term appearing in an indexed-grain event or an artifact returns the session that contains it.
status: pass
first_pass: pass
source: verifier
evidence: tests/fts_search.rs:107 passes: zylophonics (event) and xylobyteword (artifact) each return sessF. Loader populates event FTS on grain==Indexed (src/store/load.rs:137-146), artifact FTS via DISTINCT join (load.rs:68-74).

### 5. FTS5 excludes skeleton/archive-only bodies
expected: After loading, an FTS5 query for a string appearing only in a skeleton or archive-only body (e.g. a tool-result body) returns zero rows.
status: pass
first_pass: pass
source: verifier
evidence: tests/fts_search.rs:148 passes: qwertysentinel42 (skeleton-only tool_result) returns 0 while indexed term > 0; mention rows 0. Guard if e.grain==Grain::Indexed (load.rs:137); archive-only emits no Event (src/normalize/model.rs:14-15).

### 6. All files under base_dir index subdir (ARC-02)
expected: Every file the loader creates sits under the configured base_dir index subdirectory; none is written at the data-volume root.
status: pass
first_pass: pass
source: verifier
evidence: Manual find under DATA lists only DATA/index/hindsight.db. Loader writes solely to cfg.db_path()=base_dir/index/hindsight.db (src/config.rs:91-98). tests/store_load.rs:117-124 asserts no .db at base_dir root; config validate() ARC-02 guard (config.rs:65-76).

### 7. Non-zero stamped schema/provenance version
expected: Opening the loaded DB shows a non-zero stamped schema/provenance version (PRAGMA user_version or a meta-table row).
status: pass
first_pass: pass
source: verifier
evidence: Manual: PRAGMA user_version=1, meta rows schema_version=1/parser_version=1/scrub_ruleset_version=1. Stamped src/store/schema.rs:119-140; open_db_creates_tables_and_stamps_version test (schema.rs:150) asserts non-zero.

## Summary

total: 7
passed: 7
failed: 0
pending: 0
skipped: 0
blocked: 0
reworked: 0
