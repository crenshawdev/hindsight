---
phase: 4
plan: 1
requirements: [EMB-01, EMB-02]
files:
  - src/store/schema.rs
  - src/store/load.rs
  - src/config.rs
  - src/main.rs
  - Cargo.toml
  - src/embed/mod.rs
  - src/embed/ollama.rs
  - src/embed/profile.rs
  - src/embed/gpu.rs
  - systemd/hindsight-embed.service
  - systemd/hindsight-embed.timer
  - tests/embed_profile.rs
  - docs/diagrams.md
  - docs/STATUS.md
  - docs/DESIGN.md
  - docs/decisions/0004-embedder-and-gpu-scheduling.md
  - docs/decisions/0006-storage-engine-sqlite.md
---

# Phase 4: Fuzzy - Plan

## Goal

Build synthetic profiles mechanically from the loaded SQLite records and embed them via Ollama
`qwen3-embedding:8b` into an extended `vec_embedding` schema, driven by a `hindsight embed`
subcommand that drains a durable resumable queue GPU-opportunistically and is triggered by a systemd
timer.

## Must be true when done

- `hindsight embed` exists as a subcommand and, run against a loaded DB, assembles and drains a queue
  of profile units, then exits 0.
- After a run, `vec_embedding` row count equals the number of queued profile units, every stored
  vector is exactly 4096-dim, and each row carries its `project` plus a `(unit_kind, source_id)`
  mapping that resolves to a real `entity`/`artifact`/`event` record (no orphan vectors).
- The assembled profile text (inspectable via `hindsight embed --dump-profiles`) is built mechanically
  from records and contains no seeded secret and no full-code artifact body.
- A nearest-neighbor query for any stored vector returns its own mapped record as the top hit.
- Re-running `embed` (or resuming after an interruption) does not re-embed already-embedded units, and
  a fresh `hindsight load` resets the ledger so the next `embed` re-embeds the corpus.
- With the GPU forced busy above the configured threshold, the run still lands its vectors (defer then
  CPU fallback) and exits non-failing; a systemd timer, not the capture daemon, triggers the run.

## Context

Locked decisions binding this plan: D-01 (raw HTTP client to Ollama `:11434`, 4096-dim, short
keep-alive), D-02 (`hindsight embed` subcommand draining a queue, matching the `normalize`/`load`
pattern), D-03 (systemd timer triggers it, NOT the capture daemon - the daemon's `embed request`
diagram edge must be corrected), D-04/D-05 (`nvidia-smi` busy detection, defer-then-CPU-fallback,
never fail), D-06 (durable resumable queue; ledger must survive within a run and reset on `load`),
D-07 (profiles from the built store via GROUP BY, not the NDJSON), D-08 (three units: entity profiles,
artifact wrappers, prose chunks; no secrets, no code body), D-09 (two-stage vec0 shape + rowid->record
mapping + materialized `project`), D-10 (full re-embed after a fresh load), D-11 (profile assembly is
a separable Ollama-free stage).

Resolved library facts (verified this session, not assumed): Ollama `POST /api/embed` with
`{"model","input","keep_alive","options"}` returns `embeddings[[...4096 f32...]]`; `options.num_gpu:0`
forces CPU. sqlite-vec `=0.1.9` supports `bit[N]` + `vec_quantize_binary()`, `distance_metric=cosine`
on a float column, filterable metadata columns, and `+aux` columns - no version bump needed. Follow
the existing patterns: `src/store/mod.rs::open_db`, `src/store/schema.rs::apply` (idempotent),
`src/store/load.rs::FRESH_BUILD_TABLES`, `src/config.rs` (serde-default knobs), `src/main.rs` Command
enum, and the `tests/store_load.rs`/`tests/sqlite_vec_linkage.rs` test style. Out of scope (defer,
do not build): incremental/per-session embed trigger and threshold rule (Phase 6), query/RRF/rerank
retrieval and the MCP/CLI search surfaces (Phase 5), an LLM gloss layer.

## Tasks

### Task 1: Extend the vec0 schema and add the resumable embed ledger

- **Files:** src/store/schema.rs, src/store/load.rs
- **Action:** Replace the bare `vec_embedding` vec0 table in `schema.rs::apply` with the two-stage
  shape (D-09): `CREATE VIRTUAL TABLE IF NOT EXISTS vec_embedding USING vec0(embedding_coarse
  bit[4096], embedding float[4096] distance_metric=cosine, project text, +unit_kind text, +source_id
  text)`. **Migration guard (a reopened Phase-3 DB must not keep the old 1-column shape):** `apply`
  runs on every `open_db` with `CREATE ... IF NOT EXISTS`, and `load.rs` resets the table with `DELETE`,
  not `DROP` - so a DB created under Phase 3 (`vec_embedding USING vec0(embedding float[4096])`) would
  survive untouched and the Task 2 insert would fail with `no such column: embedding_coarse`. Before the
  `CREATE`, read `PRAGMA user_version`; when it is below the new `USER_VERSION` (i.e. an old-shape file),
  `DROP TABLE IF EXISTS vec_embedding` first so the new shape is created. Dropping is safe because
  `vec_embedding` is derived, never authoritative (the archive is ground truth), it is already wiped on
  every `hindsight load`, and D-10 re-embeds the whole corpus after a load. Do NOT drop on a
  same-version reopen - vectors written by a prior `embed` must persist for later query. `embedding_coarse` is the binary-quantized first-pass column (hamming by default),
  `embedding` is the full-precision rescore column, `project` is a filterable metadata column for the
  structural pre-filter, and `unit_kind`/`source_id` are auxiliary mapping columns (`'entity'` /
  `'artifact'` / `'event'` and the entity name / artifact_id / event.id as text) so a KNN rowid hit
  resolves to a record and back to the archive. Add a sibling ledger table `CREATE TABLE IF NOT EXISTS
  embed_ledger (unit_kind TEXT NOT NULL, source_id TEXT NOT NULL, embedder_version TEXT NOT NULL,
  embedded_at TEXT NOT NULL, PRIMARY KEY (unit_kind, source_id))`. Bump `SCHEMA_VERSION` and
  `USER_VERSION` to `"2"`/`2` since the vector shape changed (the comment on `SCHEMA_VERSION` already
  says to bump on shape change). In `load.rs`, extend `FRESH_BUILD_TABLES` to 8 entries adding
  `"embed_ledger"` alongside the existing `"vec_embedding"` so a fresh `hindsight load` wipes both in
  lockstep (D-10: after a load the ledger is empty and the next embed re-embeds; keeping them
  consistent is why ledger-empty can safely mean not-embedded, closing D-06's stale-marker concern).
  Update the `FRESH_BUILD_TABLES` doc comment and the module-header comment in `schema.rs` (currently
  says vectors and the two-stage companion "arrive in Phase 4") to describe the delivered shape.
- **Verify:** `cargo test --test store_load` passes (existing load tests still green against the new
  schema), and `cargo test store::schema` passes including the idempotent re-apply test. Add and run a
  unit test in `schema.rs` asserting `vec_embedding` and `embed_ledger` both appear in `sqlite_master`
  and that `INSERT INTO vec_embedding(embedding_coarse, embedding, project, unit_kind, source_id)
  VALUES (vec_quantize_binary(?), ?, 'p', 'event', '1')` with a 4096-f32 blob succeeds and its row is
  returned by `SELECT source_id FROM vec_embedding WHERE embedding MATCH ? ORDER BY distance LIMIT 1`.

### Task 2: Tracer - `hindsight embed` command, Ollama client, and one end-to-end vector round-trip

- **Files:** Cargo.toml, src/config.rs, src/main.rs, src/embed/mod.rs, src/embed/ollama.rs,
  src/embed/profile.rs
- **Action:** Add `ureq = { version = "2", features = ["json"] }` to `Cargo.toml` (a light blocking
  HTTP client, D-01; no async runtime for a drain-and-exit batch command; not the ollama CLI, not
  ollama-rs). Add an `EmbedConfig` nested struct to `src/config.rs` deserialized from an optional
  `[embed]` TOML table, every field carrying a `#[serde(default = ...)]` so no config file edit is
  required: `ollama_url` (default `"http://127.0.0.1:11434"`), `model` (default
  `"qwen3-embedding:8b"`), `keep_alive` (default `"5m"` - stays warm across a drain, unloads after, per
  ADR 0004's short keep-alive), `gpu_util_busy_pct` (default 30), `gpu_min_free_mib` (default 6144),
  `gpu_defer_poll_secs` (default 60), `gpu_max_defer_secs` (default 1800); expose it as
  `Config.embed` with `#[serde(default)]` on the field so an absent table yields all defaults. Create
  `src/embed/ollama.rs` with `pub enum Placement { Gpu, Cpu }` and `pub fn embed_document(cfg:
  &EmbedConfig, text: &str, place: Placement) -> Result<Vec<f32>>` that POSTs to
  `{ollama_url}/api/embed` a body `{"model": cfg.model, "input": text, "keep_alive": cfg.keep_alive,
  "options": {"num_gpu": 0}}` where `options.num_gpu` is included as 0 only for `Placement::Cpu` and
  omitted otherwise (GPU is Ollama's default). Pin the dimension explicitly per D-01's "explicit-dimension
  pin" rather than only trusting the model default: set the expected width as a named constant
  `EMBED_DIMS: usize = 4096`, request it in the payload where the `/api/embed` surface accepts a
  dimension/`options` field for `qwen3-embedding:8b` (resolve the exact key against the live API - the
  planner confirmed the model returns native 4096), and ALSO parse `embeddings[0]` into a `Vec<f32>` and
  return an error if its length is not exactly `EMBED_DIMS` (the post-response check is the hard
  enforcement backing the ADR 0004 dimension footgun; the request-side pin is the intent). Send the raw
  profile text as `input` with NO instruction prefix - the qwen3 query-side instruction template is a
  Phase 5 query concern; documents embed raw. Create `src/embed/profile.rs` with `pub struct
  ProfileUnit { pub unit_kind: String, pub source_id: String, pub project: String, pub text: String }`
  and `pub fn assemble(conn: &Connection) -> Result<Vec<ProfileUnit>>` that for this tracer builds ONLY
  the prose-chunk unit: one `ProfileUnit { unit_kind: "event", source_id: event.id, project via join to
  session, text: event.text }` per indexed-grain event that has non-null text (the simplest of D-08's
  three units; the other two land in Task 3). Create `src/embed/mod.rs` with `pub mod ollama; pub mod
  profile;` and `pub fn run(cfg: &Config, dump_profiles: bool) -> Result<()>` that opens the DB via
  `store::open_db(&cfg.db_path())`, calls `profile::assemble`, and when `dump_profiles` writes each
  unit as one NDJSON line to stdout and returns without touching Ollama or writing vectors (the
  inspectable Ollama-free sink, D-11); otherwise embeds each unit via `ollama::embed_document(...,
  Placement::Gpu)` and inserts `INSERT INTO vec_embedding(embedding_coarse, embedding, project,
  unit_kind, source_id) VALUES (vec_quantize_binary(?1), ?1, ?2, ?3, ?4)` binding the f32 blob
  (little-endian, as `tests/sqlite_vec_linkage.rs::vector_blob` does). Register `mod embed;` in
  `main.rs` and add `Embed { #[arg(long)] dump_profiles: bool }` to the Command enum wired to
  `report(embed::run(&config::Config::load()?, dump_profiles))`.
- **Verify:** On a DB loaded from a small fixture (reuse `tests/fixtures` via the `hindsight normalize
  <dir> | hindsight load` path, or a fixture DB), `hindsight embed` takes `vec_embedding` from 0 rows to
  a count equal to the indexed-event count (`sqlite3 <db> 'SELECT count(*) FROM vec_embedding'` equals
  `SELECT count(*) FROM event WHERE grain='indexed' AND text IS NOT NULL`), and `sqlite3 <db> 'SELECT
  count(*) FROM vec_embedding WHERE vec_length(embedding)=4096'` equals the same count. A nearest-
  neighbor round-trip returns the probe's own record: pick a probe unit whose `text` is UNIQUE among the
  stored units (two units with byte-identical text embed to identical vectors, so a distance-0 tie would
  be broken by rowid and the query could return a twin's `source_id` - a flaky check; a unique-text
  probe removes the tie), bind its stored vector and confirm `SELECT source_id, distance FROM
  vec_embedding WHERE embedding MATCH ? ORDER BY distance LIMIT 1` returns that probe's own `source_id`
  at `distance` 0. `hindsight embed --dump-profiles` prints one NDJSON `event` unit per indexed event and
  writes no vectors.

### Task 3: Full mechanical profile assembly - entity profiles and artifact wrappers

- **Files:** src/embed/profile.rs, tests/embed_profile.rs
- **Action:** Extend `profile::assemble` to emit all three D-08 units. Entity profiles: `GROUP BY
  entity, entity_type` over `mention` (D-07 - cross-session aggregation is a GROUP BY, not an NDJSON
  re-read), one `ProfileUnit { unit_kind: "entity", source_id: "{entity_type}:{entity}", project: the
  most-frequent project }`. **`source_id` MUST be the composite `{entity_type}:{entity}`, not `entity`
  alone:** normalize emits `entity_type` values `"file"` AND `"command"` (src/normalize/extract.rs),
  so the same surface string (a `build` file and a `build` command) produces two distinct entity units;
  keying `source_id` on `entity` alone collides them on the `embed_ledger` PK `(unit_kind, source_id)`,
  so the second is silently skipped as already-embedded and `vec_embedding` count drops below the
  assembled-unit count (breaking criterion 1). The `text` is assembled mechanically from name + aliases (distinct surface casings
  of the entity) + intro context (the `event.text` of the earliest indexed event joined via
  `mention.event_uuid = event.uuid`) + a bounded set of deduped usage sentences (distinct indexed
  `event.text` values where the entity was mentioned, capped, e.g. 8) + co-occurring entities (distinct
  other `mention.entity` sharing an `event_uuid`) + the distinct set of `mention.project` it appeared
  in. Artifact wrappers: one `ProfileUnit { unit_kind: "artifact", source_id: artifact_id }` per
  `artifact` row. **`project` MUST be derived by joining `artifact.source_event_uuid = event.uuid` then
  `event.session_id = session.session_id` to read `session.project`** (the `artifact` table has NO
  `project` or `session_id` column - only `source_event_uuid` - and this is the same session-resolution
  join `load.rs` already uses for the artifact FTS post-pass; criterion 2 requires every vector,
  artifact units included, to carry a non-empty `project`). Its `text` is the request/explanation (the
  `event.text` joined via `artifact.request_bundle = event.uuid`) + `path` + `language` + a
  mechanically extracted signature -
  only lines of `artifact.content` matching declaration/flag patterns (e.g. `fn`/`def`/`class`/
  `function`/`struct`/`--flag`), never the whole body. The code body (`artifact.content` in full) is
  deliberately excluded from `text` (D-08). Draw text ONLY from already-scrubbed columns
  (`event.text`, `artifact.request_bundle`, `path`, `language`, `mention.entity`, and the whitelisted
  signature lines of the scrubbed `artifact.content`); never emit `artifact.content` verbatim. Keep the
  prose-chunk unit from Task 2. Add `tests/embed_profile.rs` driving `assemble` over an in-memory DB
  seeded with a mention/artifact/event set. Seed a concrete greppable code-body sentinel to make the
  full-code exclusion falsifiable: an `artifact` row whose `content` carries a unique line (e.g.
  `let HINDSIGHT_CODEBODY_SENTINEL = compute();`) plus a matching signature line, then assert no
  `ProfileUnit.text` contains the sentinel line (D-08 code-body exclusion) while the signature line IS
  present, and that entity/artifact/event units are all present with resolvable `source_id`s. The
  seeded-secret half of criterion 3 is an end-to-end check (secrets are scrubbed upstream at normalize,
  not in the store), driven in the Verify below through the real `normalize | load | embed` path, not by
  planting a secret directly in the already-scrubbed store.
- **Verify:** `cargo test --test embed_profile` passes. Against a loaded real-ish DB, `hindsight embed
  --dump-profiles | grep -c '"unit_kind":"entity"'`, `...artifact"'`, and `...event"'` are all > 0.
  End-to-end secret check (criterion 3): seed a real-pattern secret (e.g. a fake
  `AKIA`-prefixed key or a token the normalize scrubber matches) into a transcript fixture, run it
  through `hindsight normalize <fixture> | hindsight load` then `hindsight embed --dump-profiles`, and
  `grep -F <that-exact-seeded-secret-literal>` over the dump returns zero hits; `grep -F 'HINDSIGHT_CODEBODY_SENTINEL'`
  (the unique code-body line) also returns zero hits. Row count after a real `hindsight embed` equals
  the total assembled unit count from `--dump-profiles | wc -l`. No-orphan / project-present check
  (criterion 2): after a real `hindsight embed`, `sqlite3 <db> "SELECT count(*) FROM vec_embedding v
  WHERE (v.unit_kind='event' AND NOT EXISTS (SELECT 1 FROM event e WHERE cast(e.id AS text)=v.source_id))
  OR (v.unit_kind='artifact' AND NOT EXISTS (SELECT 1 FROM artifact a WHERE a.artifact_id=v.source_id))
  OR (v.unit_kind='entity' AND NOT EXISTS (SELECT 1 FROM mention m WHERE m.entity_type ||':'|| m.entity
  = v.source_id))"` returns 0 - the entity arm splits the composite `{entity_type}:{entity}` source_id
  (Task 3), so it matches an existing `mention` row - and `sqlite3 <db> "SELECT count(*) FROM
  vec_embedding WHERE project IS NULL OR project=''"` returns 0.

### Task 4: Resumable ledger drain - skip embedded units, resume on interruption, reset on load

- **Files:** src/embed/mod.rs
- **Action:** Define `pub const PROFILE_SCHEMA_VERSION: &str = "1";` and compute an embedder-version
  stamp per run as `format!("{}/profile-{}", cfg.embed.model, PROFILE_SCHEMA_VERSION)`. In
  `embed::run` (non-dump path), before embedding each `ProfileUnit`, query `embed_ledger` for a row
  with matching `(unit_kind, source_id)` AND `embedder_version` equal to the current stamp; skip the
  unit if present (already embedded under this model+profile version). For each remaining unit, embed
  it, then in ONE transaction insert the `vec_embedding` row (Task 2 SQL) AND `INSERT INTO
  embed_ledger(unit_kind, source_id, embedder_version, embedded_at) VALUES (?,?,?, <RFC3339 now>)`, and
  COMMIT that transaction per unit (or per small batch, e.g. 32) before moving on. **Atomicity, not a
  delete-guard, is what prevents duplicates:** because the vector insert and its ledger stamp commit in
  the same transaction, a crash either lands both or neither - there is no window where a vector exists
  without its stamp, so the resumed run's skip-check (which reads the ledger) is exact and no
  per-unit `DELETE FROM vec_embedding` is needed. Avoid a per-unit pre-delete entirely: a `DELETE ...
  WHERE unit_kind=? AND source_id=?` filters on vec0 auxiliary columns, which vec0 can only satisfy by a
  full table scan, so deleting before every insert makes the drain O(n^2) over the corpus. The only case
  that still needs vectors cleared is a same-file embedder-version bump WITHOUT an intervening `load`
  (rare - D-10 wipes both tables on load); handle that as a single set-based `DELETE FROM vec_embedding
  WHERE (unit_kind, source_id) IN (SELECT ... )` for the changed units once at the start of the drain,
  not per unit. Use `INSERT OR REPLACE` on `embed_ledger` so a re-stamp under a new version is clean. Log
  a per-run summary (units total, skipped-already-embedded, embedded-this-run) via `tracing::info`.
- **Verify:** Run `hindsight embed` twice on the same loaded DB: the first run embeds N units, the
  second embeds 0 (log shows `embedded=0`, `skipped=N`) and `vec_embedding` count is unchanged (no
  duplicates). Interrupt a run partway (SIGINT after some units), re-run, and confirm the final
  `vec_embedding` count equals the total unit count with no duplicate `(unit_kind, source_id)` pairs
  (`SELECT unit_kind, source_id, count(*) FROM vec_embedding GROUP BY 1,2 HAVING count(*)>1` returns no
  rows). Re-run `hindsight load` on the same DB, then `sqlite3 <db> 'SELECT count(*) FROM embed_ledger'`
  is 0 and a following `hindsight embed` re-embeds the full corpus (D-10).

### Task 5: GPU-busy detection with defer-then-CPU-fallback policy

- **Files:** src/embed/gpu.rs, src/embed/mod.rs
- **Action:** Create `src/embed/gpu.rs` with `pub enum GpuState { Free, Busy, Unavailable }` and `pub
  fn gpu_state(cfg: &EmbedConfig) -> GpuState`. **Use a tri-state, not a `bool`:** a `gpu_busy -> bool`
  cannot distinguish "GPU present and idle" from "no GPU here", so a missing `nvidia-smi` would return
  `false` and be read as GPU-available, contradicting the D-05 intent to treat an absent GPU as
  CPU-only. `gpu_state` returns: `Busy` if `HINDSIGHT_EMBED_FORCE_BUSY` is truthy (the test hook that
  makes criterion 5 machine-checkable without a real game); otherwise it runs `nvidia-smi
  --query-gpu=utilization.gpu,memory.free --format=csv,noheader,nounits`, parses the first line, and
  returns `Busy` when utilization > `gpu_util_busy_pct` OR free VRAM (MiB) < `gpu_min_free_mib`, else
  `Free`; if `nvidia-smi` is missing or errors, returns `Unavailable` (never fail). Add `pub fn
  choose_placement(cfg: &EmbedConfig) -> Placement` implementing the D-05 policy: `Free` -> `Gpu`;
  `Unavailable` -> `Cpu` immediately (no GPU to wait for); `Busy` -> sleep `gpu_defer_poll_secs` and
  re-poll, repeating until the state becomes `Free` (`Gpu`) or `Unavailable` (`Cpu`), or the accumulated
  defer time reaches `gpu_max_defer_secs`, at which point fall back to `Cpu`. In `embed::run`, call `choose_placement` once before the drain (and optionally re-check
  before each batch) and pass the chosen `Placement` into `ollama::embed_document`; a busy GPU must
  never abort the run - it defers then embeds on CPU. Wire `mod gpu;` into `src/embed/mod.rs`.
- **Verify:** `HINDSIGHT_EMBED_FORCE_BUSY=1` with `gpu_max_defer_secs` set low (e.g. via an `[embed]`
  test config or a short default in the test) makes `hindsight embed` on a small loaded DB still land
  all its vectors on CPU and exit 0 (`echo $?` is 0, `vec_embedding` count equals the unit count)
  (criterion 5). A unit test on `gpu_state` with `HINDSIGHT_EMBED_FORCE_BUSY=1` returns `GpuState::Busy`,
  and `choose_placement` maps a forced-busy state (with a low `gpu_max_defer_secs`) to `Placement::Cpu`.
  Real-GPU
  contention (an actual game holding the card) is human-verify: with a game running, `hindsight embed`
  logs a defer and does not contend for the card - observe the deferral in the journal/stderr
  (criterion 6, needs live GPU contention).

### Task 6: systemd timer and service units for scheduled embedding

- **Files:** systemd/hindsight-embed.service, systemd/hindsight-embed.timer
- **Action:** Add a user-level oneshot service `hindsight-embed.service` mirroring the style of
  `systemd/hindsight.service` (`Type=oneshot`, `ExecStart=%h/.local/bin/hindsight embed`, a
  `Description`, and the same commented note that `ExecStart` must be the installed binary path). Add
  `hindsight-embed.timer` with a `[Timer]` running on a schedule (`OnCalendar=` a sensible cadence such
  as hourly with `Persistent=true` so a missed run catches up) and `[Install] WantedBy=timers.target`.
  This keeps embedding on a timer and OUT of the capture daemon (D-03), protecting the daemon's
  15-minute idle self-terminate contract. Do not add a socket or hook edge; the timer is the only
  trigger, manual `hindsight embed` is the dev path.
- **Verify:** `systemd-analyze verify systemd/hindsight-embed.service systemd/hindsight-embed.timer`
  reports no errors (human-verify if `systemd-analyze` is unavailable in the execution environment:
  confirm the timer has `OnCalendar`/`WantedBy=timers.target` and the service `ExecStart=... hindsight
  embed` by inspection).

### Task 7: Documentation sync - correct the daemon-embeds diagram edge and record the resolutions

- **Files:** docs/diagrams.md, docs/STATUS.md, docs/DESIGN.md,
  docs/decisions/0004-embedder-and-gpu-scheduling.md, docs/decisions/0006-storage-engine-sqlite.md
- **Action:** In `docs/diagrams.md`, correct the component diagram (D-03, standing rule 1): remove the
  `daemon -- embed request --> ollama` edge and the `daemon -- normalize + scrub --> index` /
  `ollama -- vectors --> index` implication that the daemon embeds; replace with an `embed job`
  node triggered by a systemd timer (`timer -- triggers --> embed`, `embed -- read records --> index`,
  `embed -- embed request --> ollama`, `ollama -- vectors --> index`). In the capture-sequence diagram,
  remove the `D->>O: embed profiles` and `O-->>X: vectors` steps from the daemon's sequence (the daemon
  no longer embeds) and note embedding is a separate timer-driven job. Amend ADR 0004 with a
  dated amendment recording the resolved specifics: transport is `ureq` POST to `/api/embed` with
  `keep_alive` and `options.num_gpu` for CPU fallback; the durable queue is the `embed_ledger` table
  (wiped in lockstep with `vec_embedding` on load); the GPU policy is `nvidia-smi` util/free-VRAM
  thresholds, defer-poll then CPU fallback after a bound. Amend ADR 0006 with a dated note recording
  the concrete two-stage vec0 shape delivered (`embedding_coarse bit[4096]` + `embedding float[4096]
  distance_metric=cosine` + `project` metadata + `+unit_kind`/`+source_id` aux mapping) and that
  sqlite-vec `=0.1.9` carries it with no version bump. Update `docs/DESIGN.md`'s embedder narrative to
  reflect the timer-driven subcommand and the ledger. Update `docs/STATUS.md`: move Phase 4 to built,
  update the build-order list. Keep all doc prose in John's voice (crenshaw-voice technical register,
  no em-dashes) and generalize personal incidentals (no exact GPU model, no game name, no private
  paths) since this is a public repo.
- **Verify:** `grep -n "embed request" docs/diagrams.md` shows the edge now originates from the embed
  job / timer, not `daemon`, and `grep -n "daemon" docs/diagrams.md` shows no daemon->ollama edge. ADR
  0004 and ADR 0006 each contain a new dated amendment section naming the ledger/vec0-shape
  resolutions. `docs/STATUS.md` lists Phase 4 as built. `grep -n "—" docs/diagrams.md docs/STATUS.md
  docs/DESIGN.md docs/decisions/0004*.md docs/decisions/0006*.md` returns no em-dashes in the edited
  regions.

## Notes

- Deviation from the CONTEXT `Plan shape` directive (recorded per plan_output rule): the directive
  asked for multiple ordered plans with "the schema plan gating the writes." I produced ONE PLAN.md.
  The slices fail the parallel-independence test the split mechanism requires: the write/embed slices
  are not independently verifiable without the schema slice (the directive itself names a hard
  schema-gates-writes ordering), and the profile-assembly, Ollama-client, GPU-defer, and ledger slices
  all share the `src/embed/` module tree and `src/main.rs`, so they cannot be file-disjoint plans.
  Cadence splits feed a parallel path and require no cross-slice ordering and no shared files; neither
  holds here. The gating is expressed as task ordering instead (Task 1 schema gates Tasks 2-5 writes),
  and skeleton-first ordering lets the end-to-end tracer round-trip a real vector at Task 2 rather than
  stranding a schema-only plan with no observable end-to-end value.
- Environment facts confirmed live this session: Ollama 0.32.1 running at `127.0.0.1:11434` with
  `qwen3-embedding:8b` pulled (returns native 4096-dim), `nvidia-smi` present at `/usr/bin/nvidia-smi`,
  sqlite-vec `=0.1.9` source carries `bit[N]`, `vec_quantize_binary`, `distance_metric`, metadata and
  auxiliary columns. So the Task 2-5 embed and round-trip verifies are runnable commands, not
  human-verify; only criterion 6 (real game GPU contention) is human-verify.
- Recalled prior art (phases/3/CONTEXT.md): the sqlite-vec linkage spike already ran and passed in
  Phase 3 (`tests/sqlite_vec_linkage.rs`), so vec0 is proven to link and round-trip; this phase re-uses
  that registration path via `store::open_db` and only extends the table shape.
- Plan-review adjudication (fired the `plan` trigger, adjudicated gate; claude-subagent + gpt-5.3-codex
  + gemini-3.1-pro-preview): six grounded findings were folded into the tasks above - the vec0 migration
  guard (Task 1), composite entity `source_id` to avoid the `file`/`command` ledger collision (Task 3),
  artifact `project` derivation via the session join (Task 3), a concrete seeded-secret/code-body verify
  (Task 3), a unique-text/distance-0 NN round-trip check (Task 2), the explicit dimension pin (Task 2),
  the tri-state GPU detection (Task 5), and single-transaction ledger atomicity replacing the O(n^2)
  delete-guard (Task 4). One reviewer finding was rejected as a false positive (a claim that a plain
  `project text` metadata column must be `+project`; empirically it creates and filters fine on
  sqlite-vec 0.1.9).
- OPEN, flagged for John (not auto-applied - it is a schema-modeling decision, not a plan-mechanics
  fix): an entity that appears across multiple projects gets ONE `project` on its vector (the
  most-frequent one), which satisfies criterion 2 (non-empty project) but means the structural
  pre-filter cannot narrow a cross-project entity to its other projects in Phase 5. The full project set
  still lives in the entity profile TEXT (D-08). If per-project retrieval of a shared entity matters, the
  alternatives are one entity vector per (entity, project) or a multi-valued project index - both change
  the D-09 shape and belong in CONTEXT, so this is left as-is pending your call.
