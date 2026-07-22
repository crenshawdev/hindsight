# Phase 4: Fuzzy - Context

Gathered: 2026-07-21
Feeds: /cad-plan 4

## Scope boundary

In: The batch re-embed capability. A new `hindsight embed` subcommand that (1) assembles the three
synthetic profile units mechanically from the built SQLite store, (2) embeds them via Ollama
`qwen3-embedding:8b` over the local HTTP API, GPU-opportunistically with `nvidia-smi` busy-detection and
CPU fallback, and (3) writes 4096-dim vectors into an extended `vec_embedding` schema (two-stage
coarse + rescore, a rowid->record mapping, a materialized `project` pre-filter column), backed by a
durable resumable queue so a deferred or interrupted run resumes without re-embedding. Driven
automatically by a systemd timer. Serves EMB-01, EMB-02.
Out: The per-session incremental embed trigger and the incremental re-embed threshold rule (Phase 6,
where the incremental load exists); the query core, RRF fusion, and two-stage rerank retrieval that
consumes these vectors (Phase 5); the MCP server and CLI search surfaces (Phase 5); the historical
backfill run (Phase 6).
Deferred: Incremental threshold re-embed (mention-count crossing or new project) plus the per-session
embed trigger - Phase 6, riding the same embed core with the incremental load it depends on. Swapping
the raw HTTP client for an `ollama-rs`-style client - backlog. A language-model gloss per entity -
ADR 0005's additive enrichment layer, later.
Plan shape: Big - multiple plans, same phase. /cad-plan breaks the six criteria into ordered plans
(e.g. vec0 schema extension, then mechanical profile assembly, then the Ollama embed job with
GPU-defer + resumable queue), the schema plan gating the writes.

## Decisions

- D-01 (Embedder transport): Embeddings are requested over Ollama's local HTTP API (a light raw client;
  `ureq` vs `reqwest` is the planner's call) at `:11434`, requesting `qwen3-embedding:8b` with an
  explicit 4096-dim expectation and a short `keep_alive`. Not the `ollama` CLI, not the `ollama-rs`
  crate. Evidence: docs/decisions/0004-embedder-and-gpu-scheduling.md (keep-alive, explicit-dimension
  pin, CPU/GPU control - all request options), Cargo.toml (no HTTP client present today); user decision.
- D-02 (Embed process): Embedding is a new `hindsight embed` subcommand that drains a queue and exits -
  the batch re-embed process - matching the standalone `normalize`/`load` subcommand pattern, not folded
  into the capture daemon. Evidence: src/main.rs (Command enum: daemon/precompact/poke/normalize/load),
  docs/STATUS.md build order; user decision.
- D-03 (Trigger): `hindsight embed` is triggered automatically by a systemd timer on a schedule; manual
  invocation is the dev path. It is kept out of the capture daemon to protect the daemon's 15-minute
  idle self-terminate contract (a multi-hour deferring embed job would fight it). Doc-sync (standing
  rule 1): docs/diagrams.md currently draws `daemon -- embed request --> ollama` and must be amended -
  the daemon is not the embedder. Evidence: docs/decisions/0004 (embedding is deferrable, nothing waits
  on it), src/daemon.rs (idle-exit lifecycle), docs/diagrams.md; user decision.
- D-04 (GPU-busy detection): Detect a busy GPU by polling `nvidia-smi` (utilization and/or free VRAM
  against a configured threshold), deferring while busy and using the GPU when free. Evidence:
  docs/decisions/0004 ("defers when a game is holding the card"), `nvidia-smi` present at
  /usr/bin/nvidia-smi; user decision.
- D-05 (Deferral behavior): When the GPU is busy the job defers or falls back to CPU, never fails
  (serves success criterion 3). The precise policy - how long to defer before falling back to CPU, and
  the utilization/VRAM threshold value - is the planner's call. Evidence: docs/decisions/0004 ("queuing
  the work ... and draining when the card frees ... falls back to the CPU otherwise").
- D-06 (Resumable queue): A durable embed queue records which units are already embedded so a deferred,
  interrupted, or CPU-fallback run resumes without re-embedding. "Table empty" cannot stand in for "not
  embedded" because the loader wipes `vec_embedding` on every `hindsight load`. The mechanism (a
  separate embed-watermark file, a DB table stamping each embedded unit + embedder version, or a
  derive-by-diff each run) is the planner's call. Evidence: src/watermark.rs (path-keyed transcript
  watermark, no embed concept), src/store/load.rs (`FRESH_BUILD_TABLES` truncates `vec_embedding`),
  docs/decisions/0004 ("queue the work in the watermark").
- D-07 (Profile source): Profiles are assembled by querying the built SQLite store
  (`mention`/`artifact`/`event`), not by re-reading the per-session normalize NDJSON, because
  cross-session aggregation (deduped usage sentences, co-occurring entities, the set of projects an
  entity appears in) is a GROUP BY over `mention`. Evidence:
  docs/decisions/0005-profile-construction-mechanical.md, src/store/schema.rs (denormalized
  `project`/`session_id` on `mention`), src/normalize/mod.rs (emits one session at a time).
- D-08 (Three embedded units): entity profiles (name + aliases + intro context + deduped usage +
  co-occurring entities + projects); artifact wrappers (request + explanation + path + language +
  mechanically extracted signature, code body deliberately excluded); prose chunks from indexed-grain
  events. Profile text carries no raw secrets (already scrubbed at normalize) and no full-code payloads.
  Serves EMB-01. Evidence: docs/decisions/0005, docs/STATUS.md, src/normalize/model.rs
  (Artifact.request_bundle/path/language present, `content` is the excluded code body),
  src/store/load.rs (indexed-grain gate).
- D-09 (vec0 schema extension): `vec_embedding` gains the two-stage retrieval shape (a bit-quantized
  coarse companion plus a full-precision rescore path), a rowid->record mapping tying each vector to its
  source unit (entity / artifact_id / event.id) so a KNN hit resolves to a record and back to the
  archive, and at least a `project` column materialized on the vector/mapping table for the structural
  pre-filter (vec0 narrows on in-table columns, it cannot take a restricted id set from a join).
  Evidence: docs/decisions/0006-storage-engine-sqlite.md (binary-coarse then float-rescore amendment;
  filter-locality), docs/decisions/0007-query-interface.md (a hit resolves to the archive),
  src/store/schema.rs (bare `embedding` column, `project` denormalized on `mention`), Phase 3 D-07
  deferral.
- D-10 (Re-embed posture): A full re-embed of the corpus runs after a fresh load; the incremental
  threshold rule (re-embed on a mention-count crossing or a new project) and the per-session trigger
  defer to Phase 6 with the incremental load they hang on. Evidence: src/store/load.rs (fresh-build,
  wipe-and-reload; incremental upsert is Phase 6), docs/decisions/0005 (threshold rule),
  .planning/ROADMAP.md (Phase 6 cutover); user decision.
- D-11 (Separation of concerns, standing): Phase 4 delivers the batch re-embed process only; the
  per-session incremental embed rides the same embed core but is Phase 6 (D-10). Profile assembly is a
  separable, inspectable stage feeding the embedder, so it is testable without Ollama (this is what
  makes criterion 3 machine-checkable). This encodes John's standing design bias toward surgical,
  single-purpose increments; the durable global version (baking it into cad-planner's methodology
  upstream) is tracked at crenshawdev/cadence issue #32.

## Acceptance criteria

- [ ] Running `hindsight embed` against a loaded DB takes `vec_embedding` from 0 rows to a row count
      equal to the number of queued profile units, and every stored vector has exactly 4096 dimensions.
- [ ] Every stored vector's rowid->record mapping resolves to an existing `entity`/`artifact`/`event`
      record (no orphan vectors), and each carries its `project` on the vector/mapping table.
- [ ] Grepping the assembled profile text (what is sent to Ollama, emitted to an inspectable sink) for a
      seeded secret returns zero hits, and for a known full-code artifact body returns zero hits.
- [ ] A nearest-neighbor query for a stored profile's own vector returns that profile's mapped record as
      the top match.
- [ ] With `nvidia-smi` forced to report the GPU busy (above the configured threshold), `hindsight
      embed` still lands its vectors by deferring or running on CPU and exits non-failing.
- [ ] Deferral holds against real GPU contention: an actual game holding the card makes the run defer
      rather than contend (human-verify: needs live GPU contention).

## Flagged assumptions

- Ollama embeddings API shape - the exact endpoint, and how `keep_alive`, CPU-vs-GPU placement, and the
  4096-dim output are requested for `qwen3-embedding:8b` so the write matches `float[4096]` - is a
  library fact the planner resolves via Context7; the repo has no Ollama client to read from.
- qwen3's instruction-aware document-vs-query prompt template (ADR 0005's "describe it, get the name"
  asymmetry) is not encoded anywhere in the repo; the planner sets it from the model's docs.
- Whether `sqlite-vec =0.1.9` (Cargo.toml pin) supports the `bit[N]` coarse column and metadata/partition
  columns the two-stage schema needs; if not, the coarse companion needs a version bump or a plain
  second float table. Resolved at plan time against the crate's surface.
- Deferral-vs-CPU-fallback policy and the busy threshold value (D-05) are left to the planner.
- Embed queue mechanism (D-06) - embed-watermark file vs DB stamp vs derive-by-diff - is the planner's
  call.
