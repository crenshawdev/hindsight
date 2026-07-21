---
phase: 3
status: complete
completed: 2026-07-21
---

# Phase 3: Store - Summary

A single SQLite index (`base_dir/index/hindsight.db`) holding four relational tables, an FTS5
BM25 term index, and an empty 4096-dim sqlite-vec table, populated from normalize's tagged NDJSON
by a new `hindsight load` subcommand - with sqlite-vec proven to link statically into rusqlite's
bundled SQLite first.

## What shipped

- sqlite-vec static-linkage proof - `tests/sqlite_vec_linkage.rs`, deps in `Cargo.toml` (rusqlite
  0.40 bundled, sqlite-vec =0.1.9), registered via `sqlite3_auto_extension`, no runtime `.so`.
- Store module and schema - `src/store/mod.rs` (`open_db`), `src/store/schema.rs` (session / event /
  artifact / mention / meta tables, empty `vec_embedding` vec0[4096], `PRAGMA user_version=1` + `meta`
  provenance stamp, FTS5 `fts` table).
- `hindsight load` loader - `src/store/load.rs`, wired as the `Load` subcommand in `src/main.rs`;
  fresh-build load in one transaction, per-type inserts, FTS5 populated in the same pass.
- Normalize records made loadable - `Deserialize` on the `Record` types, `pub(crate) mod model`
  (`src/normalize/model.rs`, `src/normalize/mod.rs`), so the loader reuses normalize's one record shape.
- Index-directory config helpers - `src/config.rs` `index_dir()` / `db_path()`.
- Tests - `tests/store_load.rs` (row-count + ARC-02) and `tests/fts_search.rs` (positive + negative
  D-04). Full suite: 65 passed / 0 failed.

## Commits

| Plan | Task | Commit | Description |
|---|---|---|---|
| 1 | 1 | 2dcd38b | Add rusqlite (bundled) + sqlite-vec dependencies |
| 1 | 2 | 0d2dbea | Prove sqlite-vec static linkage with 4096-dim round-trip |
| 2 | 1 | 17e27ee | Add `index_dir` / `db_path` config helpers |
| 2 | 2 | 3210b08 | Add store module, SQLite schema, and version stamp |
| 2 | 3 | af47a97 | Make normalize record types deserializable |
| 2 | 4 | 0e924eb | Add loader and `hindsight load` subcommand |
| 3 | 1+2 | 0c35e13 | Add FTS5 BM25 index over indexed events and artifacts (both tasks, one atomic commit) |

## Deviations

- [deviation] PLAN-1: `rusqlite` pinned "0.40" (plan said "0.3x"; 0.40 is current). The rusqlite
  `fts5` cargo feature does not exist in any current rusqlite version, so it was dropped; FTS5 SQL is
  available because the bundled libsqlite3-sys is compiled with `-DSQLITE_ENABLE_FTS5`. `sqlite-vec`
  pinned "=0.1.9" because the 0.1.10 alpha line fails to compile (missing `sqlite-vec-diskann.c`).
  Commits 2dcd38b / 0d2dbea.
- [deviation] PLAN-2 Task 2: `event` is keyed by a synthetic `id INTEGER PRIMARY KEY AUTOINCREMENT`
  with `uuid TEXT NOT NULL` a non-unique indexed column (plus an `event_uuid` index), NOT the
  plan-as-written `event(uuid TEXT PRIMARY KEY)`. A blocking risk_surface review confirmed the uuid PK
  rejects any multi-block assistant turn, since normalize emits one Event per content block all sharing
  the source line's uuid (`src/normalize/parse.rs:157`, test `assistant_blocks_expand_to_three_events`).
  Resolved by amending D-05 (`.planning/phases/3/CONTEXT.md`) and the ER diagram (`docs/diagrams.md`).
  Commit 3210b08.
- [deviation] PLAN-2 Task 4: `main.rs` uses a small `load_stream()` helper rather than the plan's
  literal `report(store::load::run(&Config::load()?))`, because `main` returns `ExitCode` and cannot
  host `?`; behavior is identical. Commit 0e924eb.
- [deviation] PLAN-3: both tasks landed in one atomic commit (0c35e13) rather than one commit per task,
  because they are one FTS5 change over shared files (`schema.rs` + `load.rs`).

## Open items

- Two LOW risk_surface findings on the artifact FTS5 inner-join (`src/store/load.rs`, the artifact
  post-pass), intentionally not addressed this phase - the input is trusted serde-produced NDJSON:
  - An artifact whose `source_event_uuid` matches no `event` row is silently dropped from FTS by the
    INNER JOIN (D-04 says "every Artifact.content"). A LEFT JOIN would preserve it with a null
    session_id.
  - A malformed line missing its `uuid` field (extract sets `source_event_uuid=""`, parse emits
    empty-uuid events, empty uuids exempt from dedup) could fan the artifact out across every empty-uuid
    event, producing several misattributed FTS rows for one artifact. Reachable only on malformed input
    plus a multi-session load; `hindsight load` consumes one session's stream today.
- Doc-sync (standing rule 1): D-05 and the ER diagram were amended for the event synthetic-key change,
  but there is still no dedicated store-schema ADR - the Phase-3 schema lives only in CONTEXT decisions
  D-05..D-11. Consider promoting the store schema to an ADR before Phase 5/6 build on it.

## Goal check

The seven commits deliver the phase goal. Criterion 1 (sqlite-vec links against the bundled SQLite and
a vector round-trips) is met by `tests/sqlite_vec_linkage.rs`, which registers via
`sqlite3_auto_extension` and asserts a 4096-dim KNN returns the inserted rowid over a decoy, with no
`.so` on disk (commit 0d2dbea). Criterion 2 (records load, row counts match) is met by
`tests/store_load.rs::load_row_counts_match_ndjson_types_and_db_stays_under_index`, which pipes
`hindsight normalize | hindsight load` and asserts each of session/event/artifact/mention COUNT(*)
equals the emitted NDJSON line count for that `type`, and that no DB file sits outside `base_dir/index/`
(ARC-02; commit 0e924eb). Criterion 3 (FTS5 BM25 returns the expected session) is met by the positive
`tests/fts_search.rs` test (`zylophonics` in an indexed event and `xylobyteword` in an artifact each
return the loaded session), and the negative test confirms a skeleton-only body (`qwertysentinel42`)
and `source_type='mention'` both return zero rows, proving D-04 exclusion (commit 0c35e13). Full suite:
65 passed / 0 failed. Nothing in the goal is unmet; the only gaps are the two LOW malformed-input FTS
edges recorded as open items, which do not affect well-formed normalize output.
