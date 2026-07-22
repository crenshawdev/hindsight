---
phase: 6
plan: 1
requirements: [QRY-01, QRY-02, QRY-03, IFC-01, IFC-02]
files:
  - src/query/mod.rs
  - src/query/exact.rs
  - src/query/keyword.rs
  - src/query/vector.rs
  - src/query/ranked.rs
  - src/query/resolve.rs
  - src/embed/ollama.rs
  - src/archive.rs
  - src/normalize/mod.rs
  - src/mcp/mod.rs
  - src/main.rs
  - Cargo.toml
---

# Phase 6: Query and surfaces - Plan

## Goal

The two-path query core (recall-complete exact listing plus RRF-fused ranked search) built over the
Phase 3-5 SQLite store, exposed through an `rmcp` MCP server for in-session recall and a CLI for
operating and no-model ground-truth search, with every hit resolvable to its verbatim archived bytes.

## Must be true when done

- `hindsight search --exact <file>` (with Ollama not running) lists every `session_id` whose `mention`
  rows reference that file, and the returned count equals a direct `sqlite3 COUNT` over `mention`.
- `hindsight search <terms>` (with Ollama not running) returns FTS5 keyword plus exact-listing rows,
  proving the CLI ground-truth path has no embedder or GPU dependency.
- A ranked fuzzy query returns results fused by RRF from both the FTS keyword arm and the sqlite-vec
  vector arm, and adding `--project` (or a time window) narrows the result to a strict subset.
- When the query-side embed is unavailable, a fuzzy query degrades to keyword-only (nonzero for a known
  term) and reports the degradation rather than erroring.
- Resolving an event/artifact hit returns verbatim bytes that appear byte-for-byte in the `zstd -d` of
  the source `<project>/<session-id>/<ref>` generation.
- Claude Code connects to the `hindsight mcp` server (rmcp over stdio) and its recall tools return
  results for a seeded query, with the tokio runtime confined to the `hindsight mcp` subcommand.

## Context

Locked decisions D-01..D-10 (CONTEXT.md) bind this plan. The store schema is fixed: `mention` carries
`entity/entity_type/session_id/project/timestamp` (D-01); `vec_embedding` is `vec0(embedding_coarse
bit[4096], embedding float[4096] distance_metric=cosine, project text, +unit_kind, +source_id)` (D-02);
`fts` carries `content` plus UNINDEXED `session_id/source_type/source_id`, and the loader already stamps
`session_id` on both event and artifact FTS rows (D-03). Relational timestamp columns
(`event.timestamp`, `session.started_at/ended_at`, `mention.timestamp`) are RFC3339 TEXT that sorts
chronologically, so the time pre-filter is a string range with no schema change (D-06, confirmed against
schema.rs). `normalize::parse::assemble_events` and `normalize::extract::extract` are pure functions over
decompressed generation `Vec<Vec<Value>>`, and `scrub_indexed` is a *separate* pass applied after them,
so a pinpoint re-parse yields verbatim (unscrubbed) content (D-08, confirmed against normalize/mod.rs).
Follow established patterns: `report()`/`open_db()`/clap-derive `Command` (main.rs), the injectable
`embed_fn` closure seam (embed/mod.rs `drain`), the `vec_quantize_binary(?1)` insert and little-endian
`vector_blob` (embed/mod.rs), and the `check_segment` archive path guard (archive.rs). Out of scope:
backfill / empty-watermark sweep, hook wiring, retiring the prior memory tool (Phase 7), and the
optional query-time local re-rank (ADR 0007 additive enrichment).

RRF fusion granularity (flagged decision, my call): **fuse at `session_id` granularity.** Both arms and
the exact-listing path already speak `session_id`, and an `entity` vector unit is a cross-session
aggregate with no single record key, so a canonical-record-key fusion would have to special-case or drop
entity hits; reconciling every arm to `session_id` accommodates all three unit kinds uniformly and
matches the recall use case (which sessions are relevant). The cost, lost within-session ordering, is
recovered by the resolve path, which re-pinpoints the specific record. The per-unit-kind mapping join
D-03 requires is still done, mapping each vector `source_id` to its `session_id(s)` (see Task 5).

Two id-space facts bind Tasks 5 and 7 (verified against the loaded schema and the loader/profile code, so
the plan does not silently mis-key a hit):
- The `event` unit lives in **two** id spaces. FTS keys an event by `event.uuid` (load.rs stamps
  `e.uuid` into the FTS `source_id`), but the vector arm keys the same event by the synthetic
  `CAST(event.id AS TEXT)` (profile.rs prose chunk). A fused session's resolve annotation must therefore
  carry a **canonical resolvable key**, not the arm-native `source_id`: the Task 5 mapping join translates
  a vector event `source_id` (synthetic `event.id`) to its `event.uuid` before annotating, so Task 7
  always resolves against a uuid.
- The `entity` unit has no record-level key, but `mention` carries `event_uuid` (D-01), so an
  entity-ranked session resolves to a **representative `mention.event_uuid`** in that session rather than
  being unresolvable. The annotation an entity hit produces is `(source_type=event, uuid=<that
  mention.event_uuid>)`, never a bare `{type}:{name}`.
Strict-subset invariant (governs the vector-hit -> session remap): the mapping join in Task 5 MUST carry
the active `project`/time predicate. Mapping an entity to **all** its `mention.session_id` rows would
re-widen past a `--project`/time anchor that Task 4 already applied on the vector arm, reintroducing
out-of-anchor sessions and breaking the strict-subset acceptance criterion. The remap therefore joins only
`mention` rows that themselves satisfy the filter.

## Tasks

### Task 1: Query module skeleton, exact listing, and the CLI `search` tracer

- **Files:** src/query/mod.rs, src/query/exact.rs, src/main.rs
- **Action:** Create a `query` module (declare `mod query;` in main.rs alongside the existing modules).
  In `exact.rs` add `pub fn exact_listing(conn: &Connection, entity: &str, entity_type: Option<&str>,
  project: Option<&str>, since: Option<&str>, until: Option<&str>) -> Result<Vec<String>>` that selects
  `DISTINCT session_id` from `mention` filtered by `entity` (exact match) and any supplied
  `entity_type`/`project` and `timestamp >= since` / `timestamp <= until` (RFC3339 string range, D-01,
  D-06), ordered by the earliest `mention.timestamp` per session so the listing is time-ordered and
  recall-complete, no join required. In `mod.rs` re-export it and hold shared helpers. Add a
  `Command::Search { query: Option<String>, #[arg(long)] exact: Option<String>, #[arg(long)] entity_type:
  Option<String>, #[arg(long)] project: Option<String>, #[arg(long)] since: Option<String>, #[arg(long)]
  until: Option<String> }` variant wired through `report(...)` to a `query::run_search(cfg, ...)` that
  opens the DB via `open_db(&cfg.db_path())` and, when `--exact` is set, prints one `session_id` per
  line. This is the end-to-end tracer: store to CLI is runnable after this task. Do not add ranking or
  any embedder here.
- **Verify:** `cargo test query::exact` passes a test that seeds two `mention` rows for one file across
  two sessions plus a decoy file, asserts `exact_listing` returns exactly the two `session_id`s, and
  asserts the count equals `SELECT COUNT(DISTINCT session_id) FROM mention WHERE entity = ?`. Also
  `cargo build` and `hindsight search --exact <file>` against a seeded DB prints the session ids.

### Task 2: FTS5 keyword arm and the no-model CLI ground-truth search

- **Files:** src/query/keyword.rs, src/query/mod.rs, src/main.rs
- **Action:** In `keyword.rs` add `pub fn keyword_search(conn: &Connection, query: &str, project:
  Option<&str>, since: Option<&str>, until: Option<&str>) -> Result<Vec<KeywordHit>>` where
  `KeywordHit { session_id, source_type, source_id, rank }`. Run an FTS5 MATCH over the `fts.content`
  column ordered by `bm25(fts)` ascending, reading the UNINDEXED `session_id/source_type/source_id`
  columns for the mapping (D-03). Apply the `project` filter by joining `session` on `session_id`, and
  the time window by joining the mapped source record's timestamp (`event.timestamp` via
  `source_type='event'` source_id=uuid, or the artifact's source event timestamp) as an RFC3339 range
  (D-06). Escape/quote the user query for FTS5 so a bare term or phrase is safe. In `mod.rs`, make
  `run_search` (from Task 1) dispatch on the two distinct ground-truth modes, not blend them per call
  (D-10): a positional `query` runs `keyword_search` and prints its hits (session_id plus a
  source_type/source_id column); `--exact <entity>` runs the Task 1 `exact_listing`. Both modes are
  model-free - "FTS5 keyword plus exact-listing rows" in Must-be-true #2 means the CLI ground-truth surface
  offers **both** paths, each embedder-free, not that one invocation returns both. Take no Ollama
  dependency in this file.
- **Verify:** `cargo test query::keyword` passes a test that loads a small fixture (indexed event text
  plus an artifact) and asserts `keyword_search` for an indexed term returns the containing session with
  nonzero results, and that a `project` filter narrows it. Then, with Ollama stopped, `hindsight search
  <known-term>` prints keyword rows and exits 0, **and** `hindsight search --exact <file>` prints the
  exact-listing session ids and exits 0, against a loaded fixture DB - proving both CLI modes run with no
  embedder/GPU dependency.

### Task 3: Query-side embed with the qwen3 instruction prefix

- **Files:** src/embed/ollama.rs
- **Action:** Add `pub fn embed_query(cfg: &EmbedConfig, query: &str) -> Result<Vec<f32>>` distinct from
  `embed_document` (D-04): it wraps `query` in the qwen3 query-side instruction template (an
  "Instruct: {task} \nQuery: {query}" style prefix, the asymmetry the ollama.rs header comment reserves
  for the query side) via a small pure helper `fn query_input(query: &str) -> String`, then issues the
  same `/api/embed` POST as `embed_document` (same model, `num_gpu` full-GPU pin, `dimensions` pin, and
  the same post-response `EMBED_DIMS` length enforcement). Documents still embed raw through
  `embed_document`; only the query side adds the prefix. Do not change `embed_document`.
- **Verify:** `cargo test embed::ollama` passes a test asserting `query_input("find the deploy script")`
  contains the instruction prefix and the raw query text, and that `embed_document`'s input path is
  unchanged (no prefix). The live HTTP call is exercised by the ranked-search UAT with Ollama up.

### Task 4: Two-stage vector read with project and time pre-filters

- **Files:** src/query/vector.rs, src/query/mod.rs
- **Action:** In `vector.rs` add `pub fn vector_search(conn: &Connection, query_vec: &[f32], project:
  Option<&str>, time_ids: Option<&TimeFilter>, k: usize) -> Result<Vec<VectorHit>>` where `VectorHit {
  unit_kind, source_id, project, distance }`. Serialize `query_vec` with the existing little-endian
  `vector_blob` shape. Unfiltered path (D-02, the governing case): stage one runs a binary-coarse KNN
  `WHERE embedding_coarse MATCH vec_quantize_binary(:q) ORDER BY distance LIMIT :coarse_k` (hamming) to
  collect a candidate rowid pool (coarse_k a few times k), stage two rescscores those rowids by full
  precision with `vec_distance_cosine(embedding, :q)` ordered ascending, limited to k. Anchored path
  (D-06): `project` filters inside the vec0 MATCH as a metadata column (`AND project = :project`); a time
  window filters *outside* the MATCH via filter-then-exact-rerank, computing a candidate `source_id` set
  per unit_kind from the relational timestamp columns (event: `CAST(id AS TEXT)` where `event.timestamp`
  in range; artifact: `artifact_id` whose source event is in range; entity: `{entity_type}:{entity}`
  groups with any `mention.timestamp` in range) and running the exact cosine rescore only over those
  survivors, no KNN over the whole set. Add a `TimeFilter` type carrying the computed candidate set.
  Keep `k`, `coarse_k` as named constants. Take no embedder dependency here: the caller supplies
  `query_vec` (mirrors the injectable `embed_fn` seam in embed/mod.rs) so this is testable without Ollama.
- **Verify:** `cargo test query::vector` passes a test that inserts three known `vec_embedding` rows
  (distinct coarse+full vectors, distinct `project`/`unit_kind`/`source_id`, matching the schema.rs
  round-trip pattern), calls `vector_search` with `query_vec` equal to one row's vector, and asserts that
  row ranks first; a second assertion passes `project = Some(...)` and confirms the result set is the
  strict subset carrying that project. A third assertion guards the two-stage recall (the coarse arm is
  approximate by design, D-02): insert a row whose full-precision vector is the true nearest neighbor of
  the query but is NOT bit-identical, and assert it survives the coarse stage into the rescore (i.e.
  `coarse_k` is set wide enough that the exact-match test is not the only case exercised); if a true
  neighbor with imperfect Hamming proximity is dropped, the assertion fails and flags `coarse_k` as too
  tight. Note in code that binary-coarse recall is a locked-design tradeoff, not a defect - the constant is
  the tuning knob.

### Task 5: RRF fusion at session granularity with keyword-only fallback

- **Files:** src/query/ranked.rs, src/query/mod.rs
- **Action:** In `ranked.rs` add `pub fn ranked_search<F>(conn: &Connection, query: &str, project:
  Option<&str>, since: Option<&str>, until: Option<&str>, embed_query: F) -> Result<RankedResult>` where
  `F: FnOnce(&str) -> Result<Vec<f32>>` (the injectable query-embed seam, so tests and the fallback path
  drive it without Ollama). Run the keyword arm (Task 2) and, when the embed closure succeeds, the vector
  arm (Task 4). Reconcile both arms to `session_id` (D-03): keyword hits carry `session_id` directly;
  map each vector hit's `source_id` to `session_id(s)` per `unit_kind` (event: synthetic `event.id` ->
  `event.session_id`; artifact: `artifact_id` -> `source_event_uuid` -> `event.session_id`; entity: split
  `{entity_type}:{entity}` -> `mention.session_id`, one entity may contribute several sessions). The
  mapping join MUST carry the active `project`/time predicate (Context, strict-subset invariant): join only
  `mention` rows satisfying `--project` and the time window, so an entity never re-widens the result past
  the anchor Task 4 applied on the vector arm. Collapse each arm to a per-session best (lowest) rank, then
  fuse with reciprocal-rank fusion `score(session) = sum_arms 1/(RRF_K + rank)` (RRF_K = 60), sorted
  descending. Annotate each fused session with a **canonical resolvable target** derived in the same
  mapping join (Context, id-space facts), never the arm-native `source_id`: keyword event -> `(event,
  e.uuid)`; keyword/vector artifact -> `(artifact, artifact_id)`; vector event -> translate synthetic
  `event.id` to `(event, e.uuid)`; entity -> `(event, <a representative mention.event_uuid in that
  session>)`. So Task 7 always resolves against a uuid/artifact_id it can pinpoint. Fuzzy fallback (D-05):
  if `embed_query` returns `Err`, skip the vector arm, fuse
  the keyword arm alone (RRF over one list), and set a `degraded: true` flag with the reason on
  `RankedResult` rather than propagating the error. Do NOT wire `ranked_search` into the CLI
  `run_search`: the CLI positional `search` stays on the Task-2 keyword+exact ground-truth path with no
  embedder touched (D-10 - the CLI is the no-ranking-opinion view; the MCP surface owns the fuzzy vector
  path). `ranked_search` is reached only from the MCP `ranked_search` tool (Task 8), which supplies
  `ollama::embed_query` as the closure; the fuzzy fallback (D-05) therefore lives on the MCP path.
- **Verify:** `cargo test query::ranked` passes (unit tests only, no CLI assertion): (a) a fusion test
  seeding FTS rows and `vec_embedding` rows that map to two sessions, calling `ranked_search` with an
  injected closure returning a known vector, asserting the result draws from both arms and a `project`
  filter yields a strict subset; (b) a fallback test whose injected closure returns `Err`, asserting the
  result is nonzero keyword-only, `degraded == true`, and `ranked_search` returns `Ok`; (c) a strict-subset
  leak test seeding one `entity` vector unit whose `mention` rows span session A (inside a `--project`/time
  anchor) and session B (outside it), asserting the anchored query returns A and **not** B (the remap does
  not re-widen); and (d) an annotation-resolvability assertion that a session ranked best by the vector
  `event` arm carries a `(event, <uuid>)` target whose uuid exists in `event`, and an entity-ranked session
  carries a `(event, <mention.event_uuid>)` target (never a bare `{type}:{name}`), so Task 7 can pinpoint it.

### Task 6: Shared archive-read primitive

- **Files:** src/archive.rs
- **Action:** Add `pub fn read_generation(config: &Config, project: &str, session_id: &str, gen_ref:
  &str) -> Result<Vec<u8>>` (D-07): resolve `archive_dir()/<project>/<session-id>/<gen_ref>` where
  `gen_ref` is an `archive_refs` label such as `0000.zst` or `subagents/agent-x/0000.zst`, validating
  every path segment with the existing `check_segment` guard (split `gen_ref` on `/`) and confirming the
  resolved path stays under `archive_dir()` (the same ARC-02 guard `resolve_session_dir` applies), then
  `zstd::decode_all` the file and return the decompressed bytes. This is the first public read path on
  archive.rs (writer-only today); do not disturb `write_generation`.
- **Verify:** `cargo test archive::` passes a test that writes a generation via `write_generation`, reads
  it back through `read_generation` using the returned generation's ref label, and asserts the bytes
  equal the original source bytes; a second assertion confirms a `../escape` `gen_ref` is rejected.

### Task 7: Pinpoint re-normalize and hit resolution to verbatim bytes

- **Files:** src/normalize/mod.rs, src/query/resolve.rs, src/query/mod.rs
- **Action:** VERBATIM CONSTRAINT (blocker fix): the resolved bytes must be the **original line bytes**
  from the decompressed generation, never a `serde_json::Value` that was parsed and re-serialized. This
  crate's `serde_json = "1"` has no `preserve_order` feature, so a `Value` object is a `BTreeMap` that
  re-emits keys alphabetically; a Claude Code transcript line (`type`/`uuid`/`timestamp`/`message` order)
  would round-trip to different bytes and fail the byte-for-byte QRY-03 criterion. So pinpoint locates the
  line but returns its raw bytes. In `normalize/mod.rs` add `pub fn pinpoint(generation: &[u8], uuid:
  &str) -> Option<Vec<u8>>` (D-08): split `generation` on `\n` into raw line slices, and for each line
  parse **only enough to read its `uuid` field** (a `serde_json::from_slice::<PartialLine>` reading just
  `uuid`, or a `Value` read used solely for the uuid comparison), returning the **original raw line bytes**
  (the untouched slice) of the first line whose `uuid` matches - never a re-serialized value. No
  `scrub_indexed` is ever applied, so the bytes are verbatim. In `resolve.rs` add `pub fn resolve(conn:
  &Connection, cfg: &Config, session_id: &str, source_type: &str, source_id: &str) -> Result<Vec<u8>>`
  which first resolves the target to an **event uuid** (id-space translation, Context): `source_type ==
  "event"` -> `source_id` is already the `event.uuid` (the Task 5 annotation guarantees a uuid here, having
  already translated any synthetic `event.id`); `source_type == "artifact"` -> `SELECT source_event_uuid
  FROM artifact WHERE artifact_id = ?1` (a whole line carrying the tool_use satisfies "appears byte-for-byte
  in the source"). Then read `project` and `archive_refs` (JSON array) from the `session` row, and for each
  ref call `archive::read_generation` and `pinpoint(&bytes, &uuid)`; return the raw bytes of the first
  match. Resolve against `archive_refs` **as stored**, never by re-walking the archive tree (per the Phase 2
  `collect_generations` nesting caveat). Wire a `RankedResult`'s annotated top hit through to this on the
  CLI (a `--resolve` affordance is optional here; the MCP `resolve` tool in Task 8 is the primary caller).
- **Verify:** `cargo test query::resolve` passes an end-to-end test: write a transcript generation whose
  first object line has keys in transcript order (`type` before `uuid`, i.e. NOT alphabetical) and contains
  a known Write tool_use, `normalize | load` it into a temp DB, then `resolve` the artifact hit and assert
  the returned bytes appear **byte-for-byte** inside `zstd::decode_all` of the source generation file
  (`generation.windows(returned.len()).any(|w| w == returned)`). A second assertion re-serializes the same
  line through `serde_json::to_vec(&Value)` and confirms it does **not** byte-match the source (proving the
  raw-bytes path is load-bearing, not incidental).

### Task 8: rmcp MCP server and the `hindsight mcp` subcommand (tokio-scoped)

- **Files:** Cargo.toml, src/mcp/mod.rs, src/main.rs
- **Action:** Add dependencies (D-09): `rmcp` (the official modelcontextprotocol Rust SDK) with its
  server, stdio-transport, and macros features; `tokio` with `rt-multi-thread`, `macros`, `io-std`; and
  `schemars` for the tool-argument `JsonSchema` derive. Create an `mcp` module (declare `mod mcp;` in
  main.rs) defining a `HindsightServer` struct holding the resolved `Config`. Register three tools with
  rmcp's attribute-macro shape (`#[tool_router]` on the impl block, `#[tool(description = "...")]` on
  each async method taking a `Parameters<Args>` where `Args: Deserialize + JsonSchema`, and
  `#[tool_handler]` on the `ServerHandler` impl with a `get_info()` advertising the tools): `exact_listing`
  (calls `query::exact::exact_listing`), `ranked_search` (calls `query::ranked::ranked_search` with
  `ollama::embed_query`), and `resolve` (calls `query::resolve::resolve`). Because rusqlite is blocking,
  run each tool's DB work inside `tokio::task::spawn_blocking` so the async runtime is not stalled. Add a
  `Command::Mcp` variant; its handler calls `mcp::run(&Config::load()?)`, which builds a tokio runtime
  *inside the subcommand* (a `tokio::runtime::Runtime` `block_on`, never `#[tokio::main]` on `main`, so
  the rest of the synchronous binary is untouched, D-09) and serves the server over stdio
  (`serve`/`stdio()` then await completion). Keep all logging on stderr (init_tracing already writes
  stderr) so stdout carries only the JSON-RPC stream. FIXED CONTRACT (not latitude): the three tool
  names and arities (`exact_listing`, `ranked_search`, `resolve`) and the stdio JSON-RPC transport are
  fixed and must not change. Only crate-internal identifier spellings (macro attribute names, the
  argument-wrapper type, the transport constructor) may be adjusted to match the resolved `rmcp`
  version's actual API; the tool surface, its three names/arities, and the JSON-RPC-over-stdio contract
  stay exactly as specified, and the `cargo test mcp::` handler-invocation assertion must pass unchanged.
- **Verify:** `cargo build` succeeds with the new deps and `cargo test mcp::` passes a test constructing
  `HindsightServer` over a seeded temp DB and invoking **all three** tool handlers directly (in a
  `tokio::runtime` built in the test): `exact_listing` returns the seeded session ids; `ranked_search`
  (driven with an injected/known query vector, or exercising the D-05 keyword-only fallback so the test
  needs no live Ollama) returns fused rows; and `resolve` returns verbatim bytes for a seeded hit - so a
  missing or mis-signatured tool fails the test, not just the build. Plus a runtime-confinement assertion:
  `grep -L '#\[tokio::main\]' src/main.rs` (or an equivalent check) confirms `main` is not annotated
  `#[tokio::main]` and the only `tokio::runtime` construction is inside `mcp::run`, so the async runtime
  does not leak into the synchronous binary (D-09).
  human-verify: register the built binary with Claude Code (`claude mcp add hindsight -- <path>/hindsight
  mcp`), restart the session, and confirm a recall tool call returns results for a seeded query. Needs a
  live Claude Code MCP client.

## Notes

- Plan-shape deviation from CONTEXT: the directive asked for "Big - multiple plans" with a 3-way A/B/C
  split (query core / archive resolution / surfaces). File-independence analysis contradicts it, so this
  is one plan. The surfaces slice (MCP + CLI) imports the query-core and resolve functions and adds the
  `Command` variants in `src/main.rs`, a hard cross-slice ordering dependency that makes it unverifiable
  independently; and the query-core and archive-resolution slices would share `src/query/mod.rs`. Per the
  hard rule (file independence wins, never split shared-file work) these are one sequential plan.
- rmcp library fact (flagged assumption 2): context7 was unavailable in the planning environment, so the
  rmcp API shape above (`#[tool_router]` / `#[tool]` / `#[tool_handler]` macros, `Parameters<T>` args,
  `ServerHandler::get_info`, stdio transport via `serve(stdio())`, and a subcommand-local tokio runtime)
  is from prior knowledge of the SDK. The executor must confirm the exact resolved `rmcp` version and its
  macro/transport names against the installed crate's docs/examples at `cargo add` time and adjust
  identifiers to match; the transport contract with Claude Code is JSON-RPC 2.0 over the subprocess's
  stdin/stdout, which the stdio transport handles.
- Cross-project entity limitation (recalled, Phase 4): a shared entity's vector carries one `project`
  (most-frequent), so a `project` pre-filter on the vector arm cannot *recall* a shared entity under its
  other projects; per-project completeness would need one vector per (entity, project). This is a recall
  (coverage) gap, not a precision one - and the strict-subset invariant (Context) closes the precision side:
  the Task 5 remap join carries the `project`/time predicate, so a `--project P1` query never emits a P2
  session even via a shared entity. The exact listing (which reads `mention` directly) is unaffected and
  remains the recall-complete per-project path.
- Plan review (adjudicated, fired after the checker gate): the `plan` trigger ran a two-model adversarial
  panel (`cad-reviewer` + `openai gpt-5.3-codex`; `gemini` dropped on a provider 429 quota error, reported
  not silently skipped). Adjudication grounded each finding against the real code and applied the survivors
  to this plan directly: (1) BLOCKER - resolve must return raw generation-line bytes, not a re-serialized
  `serde_json::Value` (this crate's `serde_json` has no `preserve_order`, so a `Value` re-emits keys
  alphabetically and breaks the byte-for-byte QRY-03 criterion); fixed in Task 7. (2) BLOCKER - the vector
  arm keys events by the synthetic `event.id` while FTS/pinpoint key by `event.uuid`, and entity units have
  no record key; fixed by making Task 5 annotate a canonical resolvable target (translate `event.id` ->
  `event.uuid`, resolve entity via a representative `mention.event_uuid`) so Task 7 always pinpoints a uuid.
  (3) HIGH - the entity -> session remap re-widened past a `project`/time anchor; fixed by carrying the
  filter predicate into the remap join (strict-subset invariant). (4-5) MEDIUM - clarified the CLI
  keyword-vs-exact dispatch (Task 2) and strengthened the MCP verify to exercise all three tool handlers
  plus a `#[tokio::main]`-absence check (Task 8). (6) LOW - added a near-neighbor recall case to the
  two-stage vector test (Task 4). One finding was killed on grounding: the exact-listing count check is
  correctly `COUNT(DISTINCT session_id)` (recall-completeness is over sessions, not raw `mention` rows).
