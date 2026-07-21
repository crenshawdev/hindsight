# Phase 3: Store - Context

Gathered: 2026-07-21
Feeds: /cad-plan 3

## Scope boundary

In: A single SQLite database that persists the normalized records and answers keyword search. Adds
`rusqlite` (bundled amalgamation + fts5) and links sqlite-vec statically into that same bundled SQLite;
proves the linkage with a vector round-trip before building on it. A new `src/store/` module and a
subcommand that loads the tagged-NDJSON stream from `hindsight normalize` on stdin into the schema:
four relational tables (Session / Event / Artifact / Mention), an FTS5 (BM25) index over indexed
content, an empty 4096-dim float `vec0` table, and a stamped schema/provenance version. Serves STO-01,
STO-02, STO-03.
Out: Building synthetic profiles and populating vectors via Ollama (Phase 4); the binary-coarse +
two-stage rerank vector design (Phase 4); the query core, RRF fusion, archive resolution, MCP server,
and CLI search (Phase 5); wiring the loader into the daemon sweep and the incremental/backfill run
(Phase 6).
Deferred: The vec0 two-stage retrieval schema (bit-quantized coarse companion, full-precision rescore,
rowid->record mapping) - stood up in Phase 4 where real vectors exercise it. Incremental idempotent
upsert over grown/re-swept sessions - Phase 6.
Plan shape: Big - multiple plans, same phase. /cad-plan breaks the six criteria into ordered plans
(e.g. prove-linkage spike, then schema + loader, then FTS5 wiring + verification), the linkage spike
gating the rest.

## Decisions

- D-01 (Dependency): Add `rusqlite` with the `bundled` amalgamation and the `fts5` feature; link
  sqlite-vec statically into that same bundled SQLite (the `sqlite-vec` crate's `sqlite3_auto_extension`
  registration, or vendor its C source), preserving the single static binary. Static-or-bust: a true
  failure to link statically is a surfaced blocker, not a runtime `.so`. Evidence:
  docs/decisions/0012-implementation-language-rust.md (bundled amalgamation, FTS5 compile flag,
  "sqlite-vec loads as an extension"), Cargo.toml (rusqlite absent today); user decision.
- D-02 (Spike first): Prove sqlite-vec loads against the bundled SQLite and a vector round-trips
  (insert then nearest-neighbor) in one DB file before any store code is written. Evidence:
  .planning/ROADMAP.md (validate the one unproven dependency at the start of Phase 3).
- D-03 (Loader contract): The loader consumes the exact serde output of the `Record` enum - one JSON
  object per line, `type` in {session, event, artifact, mention} (lowercase), fields snake_case,
  `grain` kebab-case, `None` -> JSON `null`, stable key set per type. Evidence: src/normalize/model.rs
  (serde tags, no skip attributes), src/normalize/mod.rs round-trip test.
- D-04 (FTS5 content): FTS5 indexes exactly indexed-grain `Event.text` plus every `Artifact.content`;
  skeleton and archive-only bodies and Mention entities never feed FTS5. Evidence: src/normalize/grain.rs
  (skeleton text blanked to None), src/normalize/model.rs `scrub_indexed`,
  docs/decisions/0003-normalize-event-grain.md.
- D-05 (Table topology): Each type gets its own table keyed on its natural id - Session by `session_id`,
  Event by `uuid`, Artifact by `artifact_id`. Mention gets a synthetic autoincrement rowid (no natural
  unique key; genuine duplicate references, e.g. two Read blocks on one path in one event, must survive).
  Evidence: src/normalize/model.rs (Session/Event/Artifact ids; Mention none), src/normalize/extract.rs
  (one Mention per Read/Edit/Write block).
- D-06 (Mention inventory): Mention is the standalone entity-inventory table carrying denormalized
  `session_id` + `project` per row, so exact-listing ("every session that touched file X") runs off it
  directly without joining Event/Session. Evidence: src/normalize/model.rs (Mention fields),
  docs/decisions/0007-query-interface.md, docs/diagrams.md.
- D-07 (Vector table scope): Phase 3 stands up a single empty 4096-dim float `vec0` table, just enough
  to prove the round-trip and hold real vectors in Phase 4. The binary-coarse companion, two-stage
  rerank, and rowid->record mapping design are deferred to Phase 4. Evidence:
  docs/decisions/0006-storage-engine-sqlite.md (amendment, two-stage design is Phase 4's concern),
  docs/decisions/0004-embedder-and-gpu-scheduling.md (4096 dims); user decision.
- D-08 (Module + subcommand): Store code lives in a new `src/store/` module; a new subcommand reads
  tagged-NDJSON on stdin and loads it (`hindsight normalize <dir> | hindsight <load>`), reusing
  normalize's output rather than re-parsing archives. Evidence: src/main.rs (subcommand enum; Precompact
  already reads stdin, Normalize writes stdout), src/normalize/mod.rs (`run_to<W: Write>` sink pattern),
  docs/STATUS.md (build order).
- D-09 (DB location): The DB lives under a new `base_dir` subdirectory (e.g. `base_dir/index/`), added
  as a `Config` helper alongside `archive_dir()` / `state_dir()`, never the volume root (ARC-02).
  Evidence: src/config.rs (archive_dir/state_dir, ARC-02 validate() guard),
  docs/decisions/0001-storage-location-and-archive-split.md. Exact subdir name is the planner's call.
- D-10 (Load posture): Fresh-build load - a normalize stream loads into an empty/truncated DB in one
  pass, and row counts match emitted records on that fresh load. Incremental idempotent upsert over
  grown/re-swept sessions is Phase 6. Evidence: docs/decisions/0001-storage-location-and-archive-split.md
  and 0006 (disposable index, rebuilt from the archive in a single pass); user decision.
- D-11 (Version stamp): The DB stamps a schema/provenance version (`PRAGMA user_version` and/or a small
  meta table carrying the scrub-ruleset + parser versions) so rebuilds are reproducible and schema drift
  is detectable at open time. Evidence: docs/decisions/0001-storage-location-and-archive-split.md
  (amendment - stamp scrub-ruleset/parser/embedder versions).

## Acceptance criteria

- [ ] From the single built binary, with no separately-shipped extension file present, a round-trip
      check inserts a known 4096-dim vector into a `vec0` table and a nearest-neighbor query returns
      that vector's row as the top match, all in one DB file.
- [ ] Piping `hindsight normalize <archived-session>` into the store loader against an empty DB, the row
      count of each of the `session` / `event` / `artifact` / `mention` tables equals the count of
      NDJSON lines with that `type` in the same stream (`jq` tally vs `sqlite3 COUNT(*)`).
- [ ] After loading, an FTS5 BM25 query for a term that appears in an indexed-grain event or an artifact
      returns the session that contains it.
- [ ] After loading, an FTS5 query for a string that appears only in a skeleton or archive-only body
      (for example a tool-result body) returns zero rows.
- [ ] Every file the loader creates sits under the configured `base_dir` index subdirectory, and none is
      written at the data-volume root (ARC-02 continuity).
- [ ] Opening the loaded DB shows a non-zero stamped schema/provenance version (`PRAGMA user_version` or
      a meta-table row).

## Flagged assumptions

- Loader per-line robustness (abort vs skip-and-continue): the loader consumes trusted serde-produced
  NDJSON, so default to abort-on-error matching normalize's upstream parse pattern; if wrong, a partial
  upstream stream loads partially without signal. Planner's default unless a plan revisits it.
- The bundled SQLite version meeting sqlite-vec's minimum and shipping FTS5 is resolved by the D-02
  spike; if the bundled version is too old, pin the rusqlite / sqlite-vec versions accordingly (a
  library-compat fact not readable off this repo, since rusqlite is not yet a dependency).
- vec0's supported quantization/KNN surface (bit-quantized coarse + rescore) matters only in Phase 4;
  Phase 3's minimal float table does not depend on it.
- The index subdirectory name under `base_dir` is the planner's call within ARC-02 (never the volume
  root); Phase 5 (query) and Phase 6 (backfill rebuild) must open the same path the loader writes.
