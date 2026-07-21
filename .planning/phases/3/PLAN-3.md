---
phase: 3
plan: 3
requirements: [STO-02]
files: [src/store/schema.rs, src/store/load.rs, tests/fts_search.rs]
---

# Phase 3: Store - Plan 3 (FTS5 BM25 wiring and verification)

## Goal

Give the store an FTS5 (BM25) index over exactly indexed-grain `Event.text` and
every `Artifact.content`, so a keyword query returns the session that contains
the term and a string present only in a skeleton or archive-only body returns
zero rows.

## Must be true when done

- After a load, a BM25 query for a term that appears in an indexed-grain event or
  an artifact returns the `session_id` that contains it.
- A query for a string that appears only in a skeleton body (blanked tool_result)
  or archive-only content returns zero rows - those bodies never enter FTS5.
- Mention entities and skeleton/archive-only bodies are absent from the FTS5
  index; only indexed-grain `Event.text` and all `Artifact.content` feed it.
- A fresh reload rebuilds the FTS5 content in step with the relational tables
  (no stale rows from a prior load).

## Context

- Depends on PLAN-2 (schema + loader exist). This plan extends
  `src/store/schema.rs` (add the FTS5 table) and `src/store/load.rs` (populate it
  in the same load pass and clear it on the fresh-build DELETE).
- D-04: FTS5 indexes EXACTLY indexed-grain `Event.text` plus every
  `Artifact.content`. Skeleton/archive-only bodies (already blanked to `None` by
  normalize) and Mention entities never feed FTS5.
- The negative acceptance test (skeleton/archive-only-only string returns zero
  rows) is a required, falsifiable verification (CONTEXT criterion 4).
- Out of scope: vector/embedding search (Phase 4), RRF fusion and the query core
  (Phase 5).

## Tasks

### Task 1: Create and populate the FTS5 BM25 index

- **Files:** src/store/schema.rs, src/store/load.rs
- **Action:** In `schema.rs` `apply(...)`, create `CREATE VIRTUAL TABLE IF NOT
  EXISTS fts USING fts5(content, session_id UNINDEXED, source_type UNINDEXED,
  source_id UNINDEXED)` - `content` is the only tokenized column so BM25 ranks on
  it, and the UNINDEXED columns carry the mapping back to the source session and
  record without polluting the term index. In `load.rs`, add `fts` to the
  fresh-build `DELETE FROM` set so a reload starts clean, and populate it exactly
  per D-04:
  - **Events (inline, in the insert loop):** for a `Record::Event` whose `grain`
    is `Indexed` AND whose `text` is `Some`, insert `(text, session_id, 'event',
    uuid)` - the Event record carries `session_id` (model.rs:48) so this is direct.
  - **Artifacts (post-pass, after all relational inserts):** `Record::Artifact`
    has NO `session_id` field (model.rs:69-77 carries only `source_event_uuid`),
    and events are not guaranteed to precede their artifact in the stream, so the
    artifact FTS `session_id` must be resolved by a set-based join AFTER the record
    loop: `INSERT INTO fts(content, session_id, source_type, source_id) SELECT
    a.content, e.session_id, 'artifact', a.artifact_id FROM artifact a JOIN event e
    ON e.uuid = a.source_event_uuid;`. This is order-independent and gives every
    artifact hit a correct session_id (criterion 3).
  Insert NOTHING for skeleton/archive-only events (their `text` is already `None`
  in the stream - also guard on `grain == Indexed` so a future non-blanked
  skeleton body still cannot leak in) and NOTHING for `Mention` records. Keep the
  event inserts and the artifact post-pass in the same transaction as the
  relational inserts.
- **Verify:** `tests/fts_search.rs` (positive, criterion 3) drives the built
  binary (binary-only crate - shell out via `CARGO_BIN_EXE_hindsight` with a temp
  `XDG_CONFIG_HOME`/`base_dir`, same harness as tests/store_load.rs / PLAN-2
  Task 4): build a synthetic transcript with an assistant text event containing a
  unique single-token alphanumeric term (e.g. `zylophonics` - no hyphens/slashes,
  so it is a clean FTS5 bareword), normalize + load it, then open the DB and query
  `SELECT session_id FROM fts WHERE fts MATCH 'zylophonics'` and assert it returns
  the loaded session's id. (Any match term containing `-`, `/`, or other
  non-alphanumeric characters MUST be wrapped as an FTS5 phrase in double quotes,
  e.g. `fts MATCH '"a/b-c"'`, or FTS5 raises a syntax error near the punctuation.)
  `cargo test --test fts_search` passes.

### Task 2: Prove skeleton/archive-only bodies never enter FTS5 (negative)

- **Files:** tests/fts_search.rs
- **Action:** Add a negative test asserting D-04's exclusion. Two independent
  claims, proven two different ways:
  - **Skeleton/archive-only bodies never enter FTS (string proof):** build a
    synthetic transcript with a unique single-token alphanumeric sentinel (e.g.
    `qwertysentinel42` - NO hyphens/slashes so it is a clean FTS5 bareword; a
    hyphenated token would need double-quote phrase wrapping) placed ONLY inside a
    skeleton-grain body - a Read/Bash `tool_result` content block, which normalize
    blanks to `None` - and NOT in any indexed event text, artifact, OR tool_use
    summary (the tool_use `input`'s file_path/command becomes an Indexed event's
    text, parse.rs:208+226, so keep the sentinel out of those too). Normalize +
    load, then assert `SELECT count(*) FROM fts WHERE fts MATCH 'qwertysentinel42'`
    returns 0. To keep the test honest, include one indexed term in the same
    transcript and assert IT matches, so a zero result proves exclusion rather than
    an empty index.
  - **Mention entities never feed FTS (structural proof):** do NOT try to prove
    this by MATCHing the Mention's path string - a Read/Edit/Write Mention's
    `entity` is the file_path, which is ALSO the produced tool_use event's indexed
    `text` (parse.rs:208, grain.rs:19 marks tool_use `Indexed`), so that path
    legitimately appears in FTS via the event row and a path MATCH can never
    return 0. Instead assert directly on the source tag:
    `SELECT count(*) FROM fts WHERE source_type = 'mention'` returns 0. Since the
    loader only ever writes `source_type` of `'event'` or `'artifact'` (Task 1),
    this is an unambiguous, collision-proof proof that Mention records produce no
    FTS row.
- **Verify:** `cargo test --test fts_search` passes both the positive and
  negative tests; temporarily seeding the sentinel into an indexed event text
  makes the skeleton negative assertion fail, confirming the test actually
  discriminates.

## Notes

- Third of three sequentially-dependent plans: PLAN-1 gates PLAN-2 gates this
  plan. It shares `src/store/schema.rs` and `src/store/load.rs` with PLAN-2 by
  design (this plan extends both with the FTS5 table and its population) - ordered
  slices in one phase, not independent parallel splits, so the file overlap is
  intentional per the CONTEXT plan-shape directive.
- FTS5 is available because PLAN-1 enabled rusqlite's `bundled` + `fts5` features;
  if `CREATE VIRTUAL TABLE ... USING fts5` errors with "no such module: fts5",
  that is a features gap to fix in Cargo.toml, not a schema problem.
