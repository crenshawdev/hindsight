---
phase: 3
plan: 2
requirements: [STO-01, STO-03]
files: [src/config.rs, src/store/mod.rs, src/store/schema.rs, src/store/load.rs, src/main.rs, src/normalize/model.rs, src/normalize/mod.rs, tests/store_load.rs]
---

# Phase 3: Store - Plan 2 (schema, loader, `hindsight load`)

## Goal

Persist a normalized tagged-NDJSON stream into a single SQLite file - four
relational tables, an empty 4096-dim `vec0` table, and a stamped
schema/provenance version - via a new `hindsight load` subcommand, on a
fresh-build load whose per-type row counts equal the emitted records.

## Must be true when done

- `hindsight normalize <archived-session> | hindsight load` loads the stream
  into a SQLite file under the configured `base_dir` index subdirectory.
- The `session` / `event` / `artifact` / `mention` table row counts each equal
  the count of NDJSON lines of that `type` in the same stream.
- The DB carries the empty `vec0` table (dimension 4096) and a non-zero stamped
  schema/provenance version readable at open time.
- Every file the loader creates sits under `base_dir/index/`; nothing is written
  at the data-volume root (ARC-02 continuity).
- Mention rows use a synthetic autoincrement key so duplicate references (two
  reads of one path in one event) both survive.

## Context

- Depends on PLAN-1 (sqlite-vec linkage proven). Reuse the `sqlite3_auto_extension`
  + `sqlite3_vec_init` registration pattern PLAN-1 proved.
- D-03: the loader consumes the EXACT serde output of the normalize `Record` enum
  (type in {session,event,artifact,mention} lowercase, snake_case fields, grain
  kebab-case, `None` -> null). Reuse the existing `normalize::model` types as the
  one source of truth rather than re-declaring the shape.
- D-05/D-06 table topology: Session by `session_id`, Event by `uuid`, Artifact by
  `artifact_id`, Mention by synthetic autoincrement rowid carrying denormalized
  `session_id` + `project`.
- D-08 (from CONTEXT): store code lives in a new `src/store/` module; subcommand
  reads NDJSON on stdin (mirror `precompact` reading stdin and `normalize`'s
  `run_to<W: Write>` sink pattern). Do NOT re-parse archives.
- D-09: DB under a new `Config` helper (`index_dir()`), never the volume root.
- D-10: fresh-build load only - empty/truncated DB, one pass. Incremental upsert
  is Phase 6, OUT of scope.
- D-11: stamp `PRAGMA user_version` (non-zero) and a `meta` table.
- D-07: stand up ONE empty `vec0` table; do NOT populate vectors (Phase 4).
- FTS5 population is PLAN-3, OUT of scope here.

## Tasks

### Task 1: Add the index-directory config helper

- **Files:** src/config.rs
- **Action:** Add `pub fn index_dir(&self) -> PathBuf` returning
  `self.base_dir.join("index")` and `pub fn db_path(&self) -> PathBuf` returning
  `self.index_dir().join("hindsight.db")`, alongside the existing `archive_dir()`
  / `state_dir()` helpers. The `index` subdir name is this phase's choice (D-09,
  ARC-02: never the volume root); Phase 5 and Phase 6 must open this same path.
  Add a unit test `index_dir_is_base_slash_index` asserting `index_dir()` equals
  `base_dir/index` and `db_path()` equals `base_dir/index/hindsight.db`, mirroring
  the existing `archive_dir_is_base_slash_archive` test.
- **Verify:** `cargo test config::` passes including the new test.

### Task 2: Create the store module, schema, and version stamp

- **Files:** src/store/schema.rs, src/store/mod.rs, src/main.rs
- **Action:** Add `mod store;` to src/main.rs. In `src/store/mod.rs` add
  `pub fn open_db(path: &Path) -> Result<rusqlite::Connection>` that: registers
  the sqlite-vec extension once (the `std::sync::Once` + `sqlite3_auto_extension`
  + `sqlite3_vec_init` pattern from PLAN-1), creates the parent directory if
  absent, opens the connection, and applies the schema from `schema.rs`. In
  `src/store/schema.rs` define `pub fn apply(conn: &Connection) -> Result<()>`
  creating: `session(session_id TEXT PRIMARY KEY, project TEXT NOT NULL,
  git_branch TEXT, cc_version TEXT, started_at TEXT, ended_at TEXT, end_reason
  TEXT, title TEXT, archive_refs TEXT)` (archive_refs stored as a JSON array
  string); `event(uuid TEXT PRIMARY KEY, parent_uuid TEXT, session_id TEXT NOT
  NULL, role TEXT, kind TEXT, timestamp TEXT, text TEXT, tool_name TEXT, is_error
  INTEGER, attribution TEXT, is_sidechain INTEGER NOT NULL, agent_id TEXT,
  agent_type TEXT, grain TEXT NOT NULL)`; `artifact(artifact_id TEXT PRIMARY KEY,
  kind TEXT, path TEXT, language TEXT, content TEXT NOT NULL, request_bundle TEXT,
  source_event_uuid TEXT NOT NULL)`; `mention(id INTEGER PRIMARY KEY
  AUTOINCREMENT, entity TEXT NOT NULL, entity_type TEXT NOT NULL, event_uuid
  TEXT, session_id TEXT NOT NULL, project TEXT NOT NULL, timestamp TEXT)` - no
  UNIQUE on entity so duplicate references survive (D-05). Create the empty
  vector table `CREATE VIRTUAL TABLE IF NOT EXISTS vec_embedding USING
  vec0(embedding float[4096])` (empty this phase, D-07 - do NOT insert rows).
  Stamp provenance: `PRAGMA user_version = 1` (non-zero, D-11) and a
  `meta(key TEXT PRIMARY KEY, value TEXT)` table seeded with `schema_version`,
  `parser_version`, and `scrub_ruleset_version` rows (use simple constant version
  strings defined in the module). The seed MUST be idempotent - `INSERT OR REPLACE
  INTO meta` (or `ON CONFLICT(key) DO UPDATE`) - because `open_db` re-applies the
  schema on every run and `meta` is deliberately NOT in the loader's fresh-build
  DELETE set; a plain `INSERT` would raise `UNIQUE constraint failed: meta.key` on
  the second `hindsight load` against the same DB (the criterion-2 CLI check and
  any reload both reopen an existing file). Do NOT create the FTS5 table here -
  PLAN-3 owns it.
- **Verify:** A unit test in `schema.rs` opens `open_db` against a `tempfile`
  path, then asserts via `sqlite_master`/pragma queries that tables `session`,
  `event`, `artifact`, `mention`, `vec_embedding`, `meta` all exist, that
  `PRAGMA user_version` returns a non-zero value, and that `SELECT value FROM
  meta WHERE key='schema_version'` returns a row. `cargo test store::` passes.

### Task 3: Make the normalize record types deserializable and reachable

- **Files:** src/normalize/model.rs, src/normalize/mod.rs
- **Action:** Add `#[derive(Deserialize)]` (with `use serde::Deserialize`)
  alongside the existing `Serialize` on `Grain`, `Session`, `Event`, `Artifact`,
  `Mention`, and the `Record` enum, so a line emitted by `write_ndjson` parses
  back into the same type (D-03). The existing serde attributes (`Grain`
  kebab-case, `Record` internally-tagged `tag = "type"` + lowercase) already
  define the contract - Deserialize must honor them, so change no attribute.
  Expose the module to the rest of the crate: change `mod model;` in
  `src/normalize/mod.rs` to `pub(crate) mod model;` (and re-export
  `pub(crate) use model::Record;` if convenient) so `src/store/` can name the
  types. Keep normalize's own use of the types unchanged. This reuse keeps ONE
  definition of the record shape so the loader cannot drift from what normalize
  emits. (Do NOT try to expose `normalize::run_to` for the `tests/` crate: this
  package is binary-only - Cargo.toml has `[[bin]]`, no `[lib]` - so `tests/*.rs`
  compile as separate crates and cannot reach any internal item; `pub(crate)`
  never crosses that boundary. The store integration tests drive the built binary
  via `CARGO_BIN_EXE_hindsight`, exactly like tests/normalize.rs - see Task 4.)
- **Verify:** Add a round-trip unit test (in model.rs) that builds one of each
  record variant, `write_ndjson`s them to a buffer, parses each line with
  `serde_json::from_str::<Record>`, and asserts the parsed value re-serializes
  to a byte-identical line. `cargo test normalize::model` passes.

### Task 4: Implement the loader and the `hindsight load` subcommand

- **Files:** src/store/load.rs, src/store/mod.rs, src/main.rs, tests/store_load.rs
- **Action:** In `src/store/load.rs` add `pub fn run(cfg: &Config) -> Result<()>`
  that opens the DB at `cfg.db_path()` via `open_db` and delegates to an inner
  `fn run_from<R: Read>(cfg: &Config, reader: R) -> Result<()>`, calling it with
  `std::io::stdin().lock()` (a buffer-injectable core with a thin stdin wrapper -
  the same shape as normalize's `run_to<W: Write>` sink, and reachable from an
  in-crate `#[cfg(test)]` unit test if one is wanted; the acceptance verification
  below drives the binary instead). `run_from` reads NDJSON from `reader` and
  loads it in a single transaction. Fresh-build posture (D-10): at the start of
  the transaction `DELETE FROM` `session`, `event`, `artifact`, `mention`,
  `vec_embedding` (and the FTS table once PLAN-3 adds it) so a load always
  rebuilds truly from empty - `vec_embedding` is included now so a Phase-4 reload
  cannot leave orphaned vectors behind stale relational rows, even though this
  phase never inserts a vector. For each non-blank line, parse with
  `serde_json::from_str::<Record>`; on a parse error, return `Err` with the line
  number in context and let the transaction roll back - abort-on-error, matching
  normalize's upstream `read_generations` parse pattern (prior art: CAPTURE.md,
  phase 2, flagged that one bad line aborts a session; the loader consumes
  trusted serde output so abort is the right default here, see Notes). Insert
  each record into its table: Session serializing `archive_refs` to a JSON array
  string, booleans (`is_sidechain`, `is_error`) as integers, `grain` as its
  kebab-case string; Mention inserted without an id so AUTOINCREMENT assigns it
  (duplicates both persist). Do NOT touch `vec_embedding`. Commit at end. Add a
  `Load` variant to the `Command` enum in `src/main.rs` (doc: "Load a normalize
  NDJSON stream from stdin into the SQLite index.") wired through `report(...)`
  to `store::load::run(&Config::load()?)`. Add `pub mod load;` to
  `src/store/mod.rs`.
- **Verify:** `tests/store_load.rs` drives the built binary (this package is
  binary-only - no `[lib]` - so integration tests cannot call internals; follow
  the `CARGO_BIN_EXE_hindsight` + local `write_generation` pattern already in
  tests/normalize.rs). The binary reads its `base_dir` from
  `$XDG_CONFIG_HOME/hindsight/config.toml` (Config::config_path, config.rs:31-42),
  so the test creates a `tempfile::tempdir()`, writes
  `<tmp>/cfg/hindsight/config.toml` containing `base_dir = "<tmp>/data"`, and sets
  `XDG_CONFIG_HOME=<tmp>/cfg` on BOTH child `Command`s (normalize and load) via
  `.env("XDG_CONFIG_HOME", ...)`. Archive a fixture session generation under a
  session dir, run `hindsight normalize <session_dir>` capturing stdout, then
  spawn `hindsight load` with that captured stdout written to its stdin. Assert the process succeeds, then
  open `base_dir/index/hindsight.db` (rusqlite in the test, a dev-dependency, or
  parse `sqlite3` CLI output) and assert `COUNT(*)` of each of
  `session`/`event`/`artifact`/`mention` equals the number of captured NDJSON
  lines whose `type` field matches that table (tally the captured stdout by
  parsing each line's `type`). Also assert the DB file exists under
  `base_dir/index/` and that the tempdir root (stand-in volume root) holds no DB
  file outside `index/` (ARC-02, criterion 5). `cargo test --test store_load`
  passes. This is exactly criterion 2's CLI shape
  (`hindsight normalize <dir> | hindsight load`, then `jq -r .type | sort |
  uniq -c` vs `sqlite3 ... 'SELECT count(*) FROM event;'`), run in-test.

## Notes

- Sequentially dependent: PLAN-1 gates this plan, and this plan gates PLAN-3.
  This plan shares `src/store/schema.rs` and `src/store/load.rs` with PLAN-3 by
  design (PLAN-3 extends both with FTS5) - ordered slices in one phase, not
  independent parallel splits, so the file-overlap is intentional per the CONTEXT
  plan-shape directive.
- Abort-vs-skip (flagged assumption): default is abort-on-error because the
  loader's input is trusted serde-produced NDJSON, matching the upstream
  `read_generations` precedent (CAPTURE.md, phase 2). Consequence, accepted for
  this phase: a partial/truncated upstream stream loads nothing and signals the
  parse error rather than loading partially in silence.
