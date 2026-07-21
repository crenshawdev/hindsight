---
phase: 3
plan: 1
requirements: [STO-03]
files: [Cargo.toml, tests/sqlite_vec_linkage.rs]
---

# Phase 3: Store - Plan 1 (sqlite-vec linkage spike, GATING)

## Goal

Prove sqlite-vec links statically into rusqlite's bundled SQLite and a 4096-dim
vector round-trips (insert, then nearest-neighbor returns it) in one DB file,
with no separately-shipped extension, before any store code is written.

## Must be true when done

- `cargo build` compiles the binary with `rusqlite` (bundled amalgamation, FTS5)
  and `sqlite-vec` as linked dependencies, no system SQLite required.
- A test opens a single SQLite connection, creates a `vec0` table of dimension
  4096, inserts a known vector, and a nearest-neighbor query returns that exact
  row as the top match.
- The round-trip works with no `.so`/`.dylib` extension file present or loaded:
  sqlite-vec is compiled into the binary, not loaded at runtime.
- No `src/store/` code exists yet; this plan only proves the dependency and
  gates the rest of the phase.

## Context

- D-01/D-02 (CONTEXT.md): add `rusqlite` bundled + fts5, link sqlite-vec
  statically into that same SQLite, and prove the round-trip FIRST. Static-or-bust:
  a genuine failure to link statically is a surfaced blocker (return `## PHASE
  TOO BIG` reason "missing information" / stop and report), never a runtime `.so`
  fallback.
- Registration pattern is the sqlite-vec crate's own Rust example:
  `rusqlite::ffi::sqlite3_auto_extension` with `sqlite_vec::sqlite3_vec_init`.
- Out of scope: the real schema, the loader, FTS5 population, populating vectors
  (Ollama is Phase 4). This plan stands up nothing persistent beyond the test.
- Cargo.toml currently has no `rusqlite` (see it: clap/serde/zstd/... only).

## Tasks

### Task 1: Add rusqlite (bundled + fts5) and sqlite-vec dependencies

- **Files:** Cargo.toml
- **Action:** Add to `[dependencies]`: `rusqlite` with features `["bundled",
  "fts5"]` (bundled compiles the SQLite amalgamation into the binary with FTS5
  enabled, so there is no system-SQLite hunt and FTS5 is available for Plan 3;
  fts5 exposes rusqlite's FTS5 API surface), and `sqlite-vec` (the FFI-binding
  crate whose build.rs compiles the extension C source into the binary). Use the
  current released versions (`rusqlite` 0.3x, `sqlite-vec` 0.1.x). If the bundled
  SQLite version is below sqlite-vec's minimum and the round-trip in Task 2 fails
  to link or load, pin compatible `rusqlite`/`sqlite-vec`/`libsqlite3-sys`
  versions rather than reaching for a runtime `.so` (D-01 static-or-bust). Do NOT
  add any vector store as a second dependency (ADR 0006: one SQLite file).
- **Verify:** `cargo build` succeeds, and `cargo tree -i rusqlite` shows the
  `bundled` build (a `libsqlite3-sys` node) and `cargo tree -i sqlite-vec`
  resolves; `find "${CARGO_TARGET_DIR:-target}" -name '*vec0*.so' -o -name
  'vec.so'` prints nothing.

### Task 2: Linkage spike - 4096-dim vector round-trip in one DB file

- **Files:** tests/sqlite_vec_linkage.rs
- **Action:** Write an integration test that, exactly once per process (guard
  with `std::sync::Once`), registers the statically-linked extension by calling
  `rusqlite::ffi::sqlite3_auto_extension` with `sqlite_vec::sqlite3_vec_init`
  transmuted to the expected extension-init function-pointer type (the pattern
  from the sqlite-vec crate's Rust example). Then open one
  `rusqlite::Connection` (in-memory or a `tempfile` DB - one file, one
  connection), create a virtual table `USING vec0(embedding float[4096])`,
  insert a known 4096-dim vector at a known rowid (serialize the `f32` slice to
  the byte blob format sqlite-vec expects, or bind via the crate's helper), then
  run a `WHERE embedding MATCH ?1 ORDER BY distance LIMIT 1` KNN query with that
  same vector and assert the returned rowid equals the inserted one. Do NOT call
  `Connection::load_extension` and do NOT reference any `.so` - a passing test
  with `sqlite3_auto_extension` registration IS the static-linkage proof. Insert
  a second, clearly-different vector so "top match" is a real ranking assertion,
  not a single-row trick.
- **Verify:** `cargo test --test sqlite_vec_linkage` passes; temporarily
  breaking the assertion (asserting the wrong rowid) makes it fail, confirming
  the KNN query is actually exercised.

## Notes

- This plan is the first of three sequentially-dependent plans in phase 3
  (PLAN-1 gates PLAN-2 gates PLAN-3). It shares `Cargo.toml` with the later
  plans by design - these are ordered slices within one phase, not independent
  parallel splits, so the template's "split plans must not overlap" file rule is
  intentionally relaxed here per the CONTEXT plan-shape directive.
- D-02 requires this plan to PASS before any `src/store/` code (PLAN-2) is
  written. If Task 2 cannot be made to link/round-trip statically, stop and
  surface it - do not proceed to PLAN-2 and do not fall back to a runtime `.so`.
