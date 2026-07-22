# Phase 6: Query and surfaces - Context

Gathered: 2026-07-22
Feeds: /cad-plan 6

## Scope boundary

In: The two-path query core over the Phase 3-5 store, plus its two surfaces. Exact listing (QRY-01)
runs recall-complete off the `mention` inventory table. Ranked search (QRY-02) builds the fresh
two-stage vector read (binary-coarse hamming then full-precision cosine rescore for unfiltered queries,
filter-then-exact-rerank for anchored ones), a query-side Ollama embed with the qwen3 query instruction
prefix, and RRF fusion of the keyword and vector rankings with `project`/time structural pre-filters.
Archive resolution (QRY-03) adds a shared archive-read primitive to `archive.rs` and resolves a hit to
the pinpointed record's verbatim bytes. Surfaces: an MCP server on the official `rmcp` SDK exposing the
recall tools (IFC-01) and new CLI `Command` variants for a no-model ground-truth search plus the MCP
serve entrypoint (IFC-02). Serves QRY-01, QRY-02, QRY-03, IFC-01, IFC-02.
Out: The historical transcript-ingest backfill and empty-watermark sweep (Phase 7, MIG-01); live
SessionStart/SessionEnd hook wiring and retiring the prior memory tool (Phase 7, MIG-02); any new
operator commands beyond query (backfill, capture-health, index-rebuild - `hindsight load` already
rebuilds). The optional query-time local re-rank of the top fuzzy hits (ADR 0007 additive enrichment).
Deferred: None.
Plan shape: Big - multiple plans, same phase. Indicative split: (A) query core - exact listing + FTS
keyword + two-stage vector + RRF fusion + structural pre-filters; (B) archive resolution - shared read
primitive + pinpoint re-normalize; (C) surfaces - rmcp MCP server + CLI no-model search.

## Decisions

- D-01 (Exact listing): The recall-complete listing runs directly off the `mention` table, filtering
  its `entity` / `entity_type` / `project` / `timestamp` columns and returning the whole `session_id`
  set unranked or time-ordered, no join required. Evidence: src/store/schema.rs (mention carries
  `entity, entity_type, event_uuid, session_id, project, timestamp` per row), src/embed/profile.rs
  (mention is the denormalized inventory for listing without joins),
  docs/decisions/0007-query-interface.md, phases/3/CONTEXT.md D-06.
- D-02 (Two-stage vector read): Built fresh in Phase 6 - the schema already carries the columns and
  inserts quantize on write, but no two-stage read exists yet. Unfiltered queries run binary-coarse
  hamming then full-precision cosine rescore over the vector set; anchored queries filter first then
  exact-rerank the survivors. Evidence: src/store/schema.rs (`vec_embedding` has
  `embedding_coarse bit[4096]`, `embedding float[4096] distance_metric=cosine`), src/embed/mod.rs
  (insert uses `vec_quantize_binary`), docs/decisions/0007-query-interface.md amendment,
  docs/decisions/0006-storage-engine-sqlite.md.
- D-03 (RRF id-space reconciliation): RRF fusion must reconcile two id spaces before fusing - FTS keys
  events by `event.uuid` and artifacts by `artifact_id` (both carry `session_id`), while `vec_embedding`
  keys events by the synthetic `event.id`, adds `entity`/`artifact` units, and carries no `session_id`
  column (only `project`/`unit_kind`/`source_id`). A per-unit-kind mapping join is required so the same
  event is not fused as two unrelated hits. The exact fusion granularity (canonical record key vs
  session granularity) is the planner's call. Evidence: src/store/load.rs (FTS event row uses `e.uuid`,
  artifact uses `a.artifact_id`), src/embed/profile.rs (vector event `source_id` is
  `CAST(e.id AS TEXT)`, plus entity/artifact units), src/store/schema.rs (vec_embedding metadata).
- D-04 (Query-side embed): A distinct query-embed entry point sends the query text with the qwen3
  query-side instruction prefix, separate from `embed_document`, which sends raw text with no prefix.
  Evidence: src/embed/ollama.rs (comment: query-side instruction template is a query concern, asymmetry
  lives on the query side; `embed_document` always sends `num_gpu: 999`),
  docs/decisions/0007-query-interface.md.
- D-05 (Fuzzy fallback): The fuzzy path degrades to keyword-only when the query-embed call fails - the
  vector arm is skipped, FTS keyword results are still returned, and the degradation is reported, rather
  than the query erroring. Evidence: user decision; src/embed/ollama.rs (query-embed can fail when the
  GPU/Ollama is busy or down).
- D-06 (Structural pre-filter): `project` filters on `vec_embedding` directly. A time pre-filter
  computes a candidate `source_id` set from the relational timestamp columns
  (`event.timestamp` / `session.started_at` / `mention.timestamp`) and constrains the query, applied
  outside the vec0 MATCH - no timestamp column is added to the vector/FTS tables and no re-embed is
  needed. Evidence: src/store/schema.rs (vec_embedding metadata column is `project` only; FTS UNINDEXED
  columns are `session_id`/`source_type`/`source_id`; timestamps live on the relational tables),
  src/embed/profile.rs (only `project` materialized onto vectors).
- D-07 (Archive read primitive): Add a shared archive-read primitive to `archive.rs` (write-only today;
  the only decompress path, `read_generations`, is private to normalize). A hit resolves via
  `session.archive_refs` (JSON array of generation labels like `0000.zst`, `subagents/a/0000.zst`) plus
  `project` + `session_id` to `archive_dir/<project>/<session-id>/<ref>`, decompressed. Evidence:
  src/archive.rs (only `write_generation`, no reader), src/normalize/mod.rs (`read_generations`,
  private), src/store/schema.rs + src/store/load.rs (`session.archive_refs` JSON array), src/config.rs
  (`archive_dir()`), phases/1/CONTEXT.md.
- D-08 (Resolution granularity): Resolution pinpoints the record - the resolved generation is
  re-normalized on the fly to extract the specific hit event/artifact bytes, not the whole session
  transcript. Evidence: user decision; src/archive.rs (a generation is one whole transcript file),
  src/store/schema.rs (`archive_refs` is per-session while hits are event/artifact/entity granularity),
  src/normalize/ (the on-the-fly re-parse reuses the existing normalizer).
- D-09 (MCP server): The MCP server is built on the official `rmcp` Rust SDK, exposing the recall tools
  (exact listing, fuzzy ranked search, resolve). Its tokio async runtime is scoped to the
  `hindsight mcp` subcommand entrypoint so the rest of the deliberately-synchronous binary
  (ureq/rusqlite blocking) is untouched. Evidence: user decision; Cargo.toml (no MCP/tokio/jsonrpc deps
  today), src/main.rs (Command enum, synchronous subcommands), docs/STATUS.md (MCP tool surface open,
  not yet decided), docs/decisions/0007-query-interface.md (MCP is the recall surface).
- D-10 (CLI surface): The CLI gains new `Command` variants for the `hindsight mcp` serve entrypoint and
  a no-model ground-truth search (FTS5 keyword + exact listing, no embedder and no Ollama/GPU
  dependency), reusing the established clap-derive `report()` / `open_db()` pattern. The MCP surface
  owns the fuzzy vector path; the CLI search is the ground-truth, no-ranking-opinion view. Evidence:
  user decision; src/main.rs (Command enum, `report`/`open_db`/`embed_run` pattern),
  docs/decisions/0007-query-interface.md (CLI is operator + plain no-model search), ROADMAP criterion 4.

## Acceptance criteria

- [ ] An exact-listing query for a file present in the store returns every `session_id` whose `mention`
      rows reference that file, and the count equals a direct `sqlite3 COUNT` over the `mention` table
      for that entity (no omissions, countable).
- [ ] A fuzzy ranked query returns results drawn from both the FTS5 keyword arm and the sqlite-vec
      vector arm fused by RRF, and adding a `--project` (or time-window) pre-filter returns a strict
      subset narrowed to that anchor.
- [ ] With Ollama unreachable, a fuzzy query still returns keyword results for a known term (nonzero)
      rather than erroring - the vector arm is skipped and the degradation is reported.
- [ ] Resolving a specific event/artifact hit returns verbatim bytes that match that record's content
      in the archived generation (the returned bytes appear byte-for-byte in the `zstd -d` of the
      source `<project>/<session-id>/<ref>` generation).
- [ ] The CLI ground-truth search (keyword + exact) returns the expected rows with Ollama stopped,
      proving no embedder/GPU dependency in that path.
- [ ] Claude Code connects to the `hindsight mcp` server and a recall tool call returns results for a
      seeded query. (human-verify: needs a live Claude Code MCP client)

## Flagged assumptions

- RRF fusion granularity (D-03): whether hits fuse at a canonical record key (event/artifact) or at
  session granularity is left to the planner; the id-space reconciliation join is required either way.
  If session granularity is chosen, within-session ordering is lost.
- `rmcp` SDK version, tool-registration shape, and Claude Code's local stdio transport contract are a
  library/protocol fact the planner resolves against current MCP docs - not readable off this repo
  (no MCP dependency exists yet).
- `mention` timestamp fidelity for the time pre-filter (D-06): the relational time columns must carry
  usable per-row timestamps for the candidate-set narrowing; confirmed at plan time against the loaded
  schema.
- On-the-fly re-normalize for pinpoint resolution (D-08): reusing the normalizer to re-extract a single
  event/artifact block from a resolved generation assumes the parser can be driven for one record; the
  planner confirms the re-parse entry point.
