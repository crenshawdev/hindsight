---
phase: 5
plan: 1
requirements: [EMB-01, EMB-02]
files:
  - src/embed/gpu.rs
  - src/embed/ollama.rs
  - src/embed/mod.rs
  - src/embed/status.rs
  - src/config.rs
  - src/store/schema.rs
  - src/store/load.rs
  - src/main.rs
  - Cargo.toml
  - docs/decisions/0013-embed-delivery-hook-gpu.md
  - docs/decisions/0004-embedder-and-gpu-scheduling.md
  - docs/DESIGN.md
  - docs/diagrams.md
  - systemd/hindsight-embed.timer
  - systemd/hindsight-embed.service
---

# Phase 5: Embed delivery - Plan

## Goal

Embedding is triggered by the session-lifecycle hooks as a detached process that runs unconditionally
on the GPU, single-flighted and resumable, continuing past a single Ollama error, with a DB-backed run
status a `hindsight embed --status` reporter reads. The Phase-4 timer and GPU-opportunistic scheduling
are retired and the docs say so.

## Must be true when done

- `hindsight embed --detach` returns in about a second and a detached child keeps embedding after the
  parent has returned (its process survives and `vec_embedding` grows post-return).
- No CPU-fallback path, `nvidia-smi` call, `Placement::Cpu`, `gpu_*` config knob, or
  `HINDSIGHT_EMBED_FORCE_BUSY` remains; `src/embed/gpu.rs` is gone and no `hindsight-embed.timer` or
  `.service` file remains in the tree.
- A full drain lands every assembled unit's vector (`vec_embedding` count equals the assembled unit
  count, every vector 4096-dim) with the model GPU-resident.
- Two drains started at once never double-embed a `(unit_kind, source_id)` unit: the second exits
  cleanly, and a unit added after the lock releases is embedded by the next run.
- An interrupted drain resumes embedding only the units that did not land, and one unit's Ollama error
  is recorded and skipped while the drain finishes the rest.
- `hindsight embed --status` distinguishes done, running, stalled, and failed from DB state.
- The ADR record, DESIGN.md, and diagrams.md describe hook-triggered, always-GPU, detached embedding
  with no timer and no CPU floor.

## Context

Locked decisions bind this plan: D-01 (`--detach` self-detaches, distinct from `poke`), D-02 (drain
runs detached, never inside the capture daemon), D-03 (filesystem single-flight lock under
`state_dir()`), D-04 (a running drain does not chase late units; the late unit rides the next
invocation), D-05 (delete `gpu.rs`, collapse to GPU-only, drop every `gpu_*` knob and the CPU path),
D-06 (continue-on-error), D-07 (DB-backed per-unit + run-level status), D-08 (no separate `--backfill`
mode; a full drain with hooks off is the backfill), D-09 (doc-sync as one superseding ADR plus
DESIGN/diagram updates and timer/service removal). Reuse unchanged: `profile::assemble`, the
`vec_embedding` two-stage shape, the Ollama HTTP client, and the resumable `embed_ledger` invariant
(vector + stamp commit together). Out of scope: live SessionStart/SessionEnd registration and retiring
the prior memory tool (Phase 7), and the query core (Phase 6). Public repo: keep the ADR/DESIGN prose
free of the exact GPU model and any private absolute path.

## Tasks

### Task 1: Collapse to GPU-only and delete the scheduling machinery (D-05)

- **Files:** src/embed/gpu.rs, src/embed/ollama.rs, src/embed/mod.rs, src/config.rs
- **Action:** Delete `src/embed/gpu.rs` entirely and remove `pub mod gpu;` from `src/embed/mod.rs`. In
  `src/embed/ollama.rs` remove the `Placement` enum and the `place` parameter (no CPU branch, no
  placement *decision* remains). Do NOT drop the `options`/`num_gpu` plumbing to Ollama's default:
  collapse `EmbedOptions` to a single unconditional `num_gpu` set high enough to force full GPU offload
  (`999`), always sent on every `EmbedRequest`, so every embed either runs fully GPU-resident or Ollama
  errors (caught by Task 4's continue-on-error) - never a silent CPU partial-offload under VRAM pressure.
  This deviates from D-05's literal "remove `EmbedOptions{num_gpu}`" but honors its locked intent
  ("GPU-always, no CPU fallback ever"): deleting the field would hand placement to Ollama's auto
  heuristic, which partial-offloads to CPU under contention - the exact CPU path D-05 forbids. Change
  `embed_document` to `pub fn embed_document(cfg: &EmbedConfig, text: &str) -> Result<Vec<f32>>` with no
  `place` parameter, keeping the 4096-dim request pin and the hard post-response length check unchanged.
  Also correct the now-stale module doc comment at the top of `src/embed/mod.rs` ("driven by a systemd
  timer (D-03)" and "a deferred, interrupted, or CPU-fallback run") to describe hook-triggered detached
  always-GPU embedding with no CPU path (doc-sync standing rule 1 covers code comments). In
  `src/embed/mod.rs` delete the `gpu::choose_placement` call and the `placement` argument threaded into
  `ollama::embed_document` (Task 4 rewrites the loop body). In `src/config.rs` remove the four `gpu_*` fields from `EmbedConfig`,
  their `default_gpu_*` functions, their entries in the `Default` impl, and the `gpu_*` assertions in
  the `embed_defaults_when_table_absent` and `embed_partial_table_keeps_other_defaults` tests (retarget
  the partial-table test to a surviving field such as `keep_alive`); leave `ollama_url`, `model`, and
  `keep_alive` intact. Do not add any always-on env override in place of `HINDSIGHT_EMBED_FORCE_BUSY`;
  the point is that there is no placement decision left to make.
- **Verify:** `test ! -f src/embed/gpu.rs`; `git grep -nE 'nvidia-smi|Placement|gpu_util_busy_pct|gpu_min_free_mib|gpu_defer_poll_secs|gpu_max_defer_secs|HINDSIGHT_EMBED_FORCE_BUSY|choose_placement' -- src/`
  returns nothing (note: a single unconditional `num_gpu` survives by design, so it is intentionally not
  in this list); `git grep -niE 'systemd timer|CPU.fallback|CPU floor|opportunistic' -- src/` returns
  nothing (stale code comments gone); `cargo build` and `cargo test config::` pass.

### Task 2: Extend the store schema with per-unit and run-level status (D-07)

- **Files:** src/store/schema.rs, src/store/load.rs
- **Action:** In `src/store/schema.rs` bump `SCHEMA_VERSION` and `USER_VERSION` from `2` to `3`. Add
  three columns to the `embed_ledger` CREATE: `status TEXT NOT NULL DEFAULT 'done'`,
  `attempts INTEGER NOT NULL DEFAULT 0`, `last_error TEXT` (keep the `(unit_kind, source_id)` primary
  key). Add a new `embed_run` table via `CREATE TABLE IF NOT EXISTS embed_run (id INTEGER PRIMARY KEY
  AUTOINCREMENT, started_at INTEGER NOT NULL, heartbeat_at INTEGER NOT NULL, pid INTEGER NOT NULL,
  state TEXT NOT NULL, total INTEGER NOT NULL, embedded INTEGER NOT NULL, skipped INTEGER NOT NULL,
  failed INTEGER NOT NULL)` where `started_at`/`heartbeat_at` are unix epoch seconds and `state` is
  `'running'` or `'done'`. Add `DROP TABLE IF EXISTS embed_run;` to the below-`USER_VERSION` lockstep
  drop block alongside the existing `vec_embedding`/`embed_ledger` drops, so an old file's
  column-short `embed_ledger` is dropped and recreated with the new columns rather than left untouched
  by `CREATE ... IF NOT EXISTS`; update the block's comment to say all three derived tables drop in
  lockstep. Extend the `open_db_creates_tables_and_stamps_version` test's table list with `embed_run`,
  and add an assertion that `PRAGMA table_info(embed_ledger)` includes a `status` column. In
  `src/store/load.rs` add `"embed_run"` to `FRESH_BUILD_TABLES` (grow the array length to 8) and update
  its doc comment to note the run history is reset on every load so `--status` after a fresh load shows
  no stale run. Also correct the stale `embed_ledger` module doc comment in `src/store/schema.rs` ("a
  deferred, interrupted, or CPU-fallback run resumes") to drop the CPU-fallback phrasing (doc-sync
  standing rule 1).
- **Verify:** `cargo test store::` passes, including the extended table-list and `table_info` assertions.

### Task 3: Single-flight filesystem lock under state_dir (D-03)

- **Files:** src/embed/mod.rs, Cargo.toml
- **Action:** Add `libc = "0.2"` to `Cargo.toml` dependencies. In `src/embed/mod.rs` add an acquire
  function that creates `cfg.state_dir()` if absent, opens (create + write) `state_dir()/embed.lock`,
  and takes a non-blocking advisory lock via `libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB)` on the
  file's raw fd. Return a guard struct that owns the `std::fs::File` so the lock is held for the
  process lifetime and released by the kernel on normal exit or crash (no PID staleness). If `flock`
  returns `-1` with `errno` `EWOULDBLOCK`/`EAGAIN`, another drain holds it: log at info that a drain is
  already running and have `run` return `Ok(())` without assembling or draining (clean exit, never a
  double-embed). Call the acquire at the very top of the non-dump branch of `run`, before
  `profile::assemble`, and hold the guard across the whole drain; `--dump-profiles` does not take the
  lock. Do not implement a PID-file scheme; flock's fd-scoped release is the crash-safety this needs.
- **Verify:** `cargo test embed::` includes a new test that opens the same temp lock path twice with the
  acquire helper: the first succeeds and the second, while the first guard is still in scope, reports
  the held/would-block outcome (returns the "already running" signal, not a panic or a second lock).
  Add a second test that exercises the end-to-end contract of acceptance criterion 4: pre-acquire the
  guard, then call the `run` entrypoint against a temp loaded DB while the guard is held, and assert
  `run` returns `Ok(())` (not an `Err`, not a panic) and adds zero `vec_embedding` rows (no drain ran).
  Cross-process contention (two real `hindsight embed` processes racing) is not unit-testable in one
  process, so name it as a human-verify: start two `hindsight embed` runs at once and confirm the second
  logs "already running" and exits 0 while the first drains, with no duplicate `(unit_kind, source_id)`
  rows (acceptance criterion 4).

### Task 4: Continue-on-error drain with run record and heartbeat (D-06, D-07)

- **Files:** src/embed/mod.rs
- **Action:** Refactor the drain out of `run` into a testable core `fn drain(conn, units,
  embedder_version, run_id, mut embed_fn)` where `embed_fn: FnMut(&ProfileUnit) -> Result<Vec<f32>>`;
  `run` calls it with `|u| ollama::embed_document(&cfg.embed, &u.text)`. Keep the existing
  stale-version cleanup and the atomic vector+ledger transaction. Change the per-unit skip check to skip
  when a ledger row exists for the current `embedder_version` with `status = 'done'`, AND also skip
  (counting it as a permanent `failed`, not retrying) any row with `status = 'failed'` and
  `attempts >= MAX_EMBED_ATTEMPTS` (add a `MAX_EMBED_ATTEMPTS` const = 5) so a deterministically-failing
  unit stops burning an Ollama call every hook-fired drain; a `'failed'` row under the cap is retried.
  The existing ledger write is `INSERT OR REPLACE`, which deletes the prior row and so cannot read the
  old `attempts` to increment it - replace it with `INSERT INTO embed_ledger(...) VALUES(...) ON
  CONFLICT(unit_kind, source_id) DO UPDATE SET status=excluded.status, embedder_version=excluded.embedder_version,
  embedded_at=excluded.embedded_at, attempts = attempts + 1, last_error=excluded.last_error` for both
  the success and failure paths so `attempts` accumulates. On `embed_fn` success, insert the vector and
  upsert the ledger row with `status='done'`, `last_error = NULL` in one transaction and count `embedded`.
  On `embed_fn` error, catch it (do not `?`-propagate), upsert a ledger row with `status='failed'`,
  `last_error` = the formatted error string, write no vector, count `failed`, and continue to the next unit. Before the loop, `run` inserts an `embed_run`
  row (`state='running'`, `started_at`/`heartbeat_at` = current unix secs, `pid = std::process::id()`,
  `total = units.len()`, zero counts) and captures its `id`; the drain refreshes that row's
  `heartbeat_at` immediately before each `embed_fn` call and again after it, plus the running counts
  after each unit (per-unit `UPDATE`s, cheap at this corpus size).
  After the loop, `run` sets the row's `state='done'` with final `embedded`/`skipped`/`failed`. Add a
  `STALE_HEARTBEAT_SECS` constant (120) for Task 6 to read. Accepted residual (the heartbeat cannot
  refresh *inside* one blocking `embed_fn` call): if a single embed itself exceeds
  `STALE_HEARTBEAT_SECS` - a cold 8B model-load on the first unit, or a pathological stall - `--status`
  may momentarily read that live drain as stalled and self-corrects on the next unit's heartbeat. 120s
  is chosen to sit well above a normal single embed at this model and corpus, so this only bites a
  genuinely pathological unit, not the ordinary cold-load case. Keep the existing tracing summary line,
  extended with the `failed` count.
- **Verify:** `cargo test embed::` includes a test that calls `drain` with an injected `embed_fn` failing
  on one specific unit against an in-memory/temp DB: the drain completes all remaining units, the failing
  unit gets a `status='failed'` ledger row with a non-null `last_error` and no `vec_embedding` row, the
  successful units get `status='done'` rows, and a second `drain` call embeds zero already-done units.

### Task 5: `--detach` self-daemonize entrypoint (D-01, D-02)

- **Files:** src/main.rs, src/embed/mod.rs
- **Action:** Add a `#[arg(long)] detach: bool` field to the `Embed` variant in `src/main.rs` and thread
  it into `embed_run`/`embed::run` alongside `dump_profiles` (reject the combination of `--detach` with
  `--dump-profiles` with a clear error, since dump is a foreground inspection sink). When `detach` is
  set, `embed::run` spawns a detached child that re-execs the current binary as `hindsight embed` (no
  `--detach`, no `--dump-profiles`) via `std::process::Command::new(std::env::current_exe()?)`, using
  `std::os::unix::process::CommandExt::pre_exec` to call `libc::setsid()` so the child leads a new
  session and process group and survives the hook's process-group reaping. Critically, redirect the
  child's three standard streams to `Stdio::null()` (`.stdin(null).stdout(null).stderr(null)`) BEFORE
  spawning: a session hook's stdout is a pipe Claude Code reads to EOF for the hook JSON protocol, and a
  child that inherits that write-end holds the pipe open for the whole multi-minute drain, so the
  session blocks until embedding finishes - the exact non-blocking failure `--detach` exists to prevent.
  With stdio nulled, the parent drops the child handle and returns `Ok(())` immediately without opening
  the DB or taking the lock; no descriptor keeps the hook's pipe open. The child (plain `hindsight
  embed`) runs the Task 3/4 drain, which takes the single-flight lock. This is distinct from `hindsight
  poke`, which only writes one byte to the daemon socket and must stay unchanged; the drain must not run
  inside the capture daemon, so nothing here touches `src/daemon.rs`.
- **Verify:** Two parts. (a) Automated, Ollama-free child-survival test in `cargo test`: factor the
  detach-spawn out so the test can spawn a detached child that re-execs a trivial always-available
  command (or the binary via a hidden self-test sentinel) which sleeps briefly then writes a sentinel
  file, assert the parent call returns `Ok(())` promptly, then poll for the sentinel appearing AFTER the
  parent returned - proving the child outlives the returning parent independent of Ollama/GPU. Also a
  unit assertion that `--detach` + `--dump-profiles` and `--detach` + `--status` return an `Err`. (b)
  Human-verify (needs a running Ollama with the model pulled and a loaded DB): with the corpus loaded,
  run `time hindsight embed --detach` and observe it exits 0 in about a second; `pgrep -af 'hindsight
  embed'` shows a surviving detached process (reparented to PID 1, new session) after the parent
  returned, and the `vec_embedding` row count rises afterward. Confirm survival specifically from a real
  session hook invocation, not only an interactive shell (a shell does not read the hook stdout pipe to
  EOF, so it would mask both the stdio-blocking bug and any cgroup kill-on-exit reaping).

### Task 6: `hindsight embed --status` reporter (D-07)

- **Files:** src/embed/status.rs, src/embed/mod.rs, src/main.rs
- **Action:** Add a `#[arg(long)] status: bool` field to the `Embed` variant and route it in
  `src/main.rs` so `--status` reads and prints without draining. Implement a new `src/embed/status.rs`
  (add `pub mod status;` to `src/embed/mod.rs`) with a classifier that opens the DB, assembles units to
  learn the current `total` and computes the current `embedder_version` (`{model}/profile-{PROFILE_SCHEMA_VERSION}`),
  reads the latest `embed_run` row (max `id`), counts `embed_ledger` rows for the current version by
  `status`, and classifies with an explicit precedence (a live run outranks per-unit history, so an
  active retry is never masked by an old failed row): (1) latest run `state='running'` and
  `now - heartbeat_at <= STALE_HEARTBEAT_SECS` -> **running**, print `embedded/total` progress; (2)
  latest run `state='running'` but heartbeat older than `STALE_HEARTBEAT_SECS` -> **stalled**, name the
  stale run and its pid (a run that died mid-drain also lands here, since it never reached `state='done'`);
  (3) latest run `state='done'` (terminal) -> **done** if done-count equals `total` and no
  current-version `status='failed'` rows remain, else **done with N failed** reporting the failed count
  and one `last_error` sample (per-unit failures are reported orthogonally to the run's terminal state,
  not as a separate run state); (4) no `embed_run` row and an empty ledger -> **not-yet-embedded**. Format the stored epoch `started_at`/`heartbeat_at` for display
  by reusing the existing `civil_from_days`/RFC3339 helper in `src/embed/mod.rs` (make it visible to
  `status.rs`). `run` must reject `--status` combined with `--detach` or `--dump-profiles`.
- **Verify:** `cargo test embed::status` seeds a temp DB with `embed_run` + `embed_ledger` rows and
  asserts the classifier returns: running for a fresh-heartbeat running row, stalled for a running row
  with a heartbeat older than `STALE_HEARTBEAT_SECS`, done for a done row whose done-count equals the
  assembled total, and failed when a `status='failed'` row is present. `cargo build` passes and
  `hindsight embed --status` prints one of those states against a real DB.

### Task 7: Doc-sync - superseding ADR, DESIGN, diagrams, remove the timer (D-09)

- **Files:** docs/decisions/0013-embed-delivery-hook-gpu.md, docs/decisions/0004-embedder-and-gpu-scheduling.md, docs/DESIGN.md, docs/diagrams.md, systemd/hindsight-embed.timer, systemd/hindsight-embed.service
- **Action:** Write a new ADR `docs/decisions/0013-embed-delivery-hook-gpu.md` (Status: accepted) that
  supersedes ADR 0004's Phase-4 amendment: embedding is triggered by the session-lifecycle hooks firing
  `hindsight embed --detach` (a self-detaching process via `setsid`, distinct from `poke`), runs
  unconditionally on the GPU with no CPU fallback and no `nvidia-smi` defer, is single-flighted by a
  filesystem `flock` under `state_dir()`, continues past a single Ollama error recording it per unit,
  and is observable through a DB-backed `embed_run` record plus per-unit `embed_ledger` status read by
  `hindsight embed --status`; note the accepted D-04 divergence (a running drain does not chase late
  units; the late unit rides the next hook-fired invocation) and that the one-time backfill is just a
  full drain with hooks off (no separate mode). The ADR's backfill-then-flip sequence must call out that
  removing the timer/service files from the tree does not stop an already-installed unit, so the flip
  includes `systemctl --user disable --now hindsight-embed.timer` (whatever scope it was installed under)
  before hooks take over - otherwise a legacy enabled timer keeps firing drains alongside the hook path. In `0004-embedder-and-gpu-scheduling.md` add a short
  note at the top of the "Amendment (2026-07-21, Phase 4 build)" section marking the timer,
  `nvidia-smi` defer, and CPU-fallback parts superseded by ADR 0013, leaving the ledger/transport parts
  standing. In `docs/DESIGN.md` rewrite the "In the build this is a `hindsight embed` subcommand..."
  paragraph (around lines 150-157) so it describes hook-triggered detached always-GPU embedding with no
  timer and no CPU floor, keeping the durable-ledger explanation; soften the earlier narrative "falls
  back to the CPU" framing to reflect the settled always-GPU delivery. In `docs/diagrams.md` component
  view remove the `timer{{embed timer unit}}` node and the `timer -- triggers --> embed` edge, retitle
  the `embed` node from "timer-driven batch" to "hook-triggered detached drain", and change the ollama
  node's "GPU opportunistic / CPU floor" to "GPU-resident while embedding"; in the capture sequence
  note, correct "A separate timer-driven embed job" to the hook-triggered detached process. Delete
  `systemd/hindsight-embed.timer` and `systemd/hindsight-embed.service` with `git rm`. Keep all prose in
  John's technical voice, no em-dashes, and generalize personal incidentals (no exact GPU model, no
  private absolute path) per the public-repo rule.
- **Verify:** `test ! -f systemd/hindsight-embed.timer && test ! -f systemd/hindsight-embed.service`;
  `git grep -nE 'timer -- triggers --> embed|embed timer unit|opportunistic|CPU floor' -- docs/` returns
  nothing; `docs/decisions/0013-embed-delivery-hook-gpu.md` exists and contains `Status: accepted` and a
  reference to superseding ADR 0004.

## Notes

Plan-shape deviation from the CONTEXT directive: the directive asked for a "Big - multiple plans"
A/B/C split, but all three indicative slices mutate `src/embed/mod.rs` (the drain lives in `run`), and
pairs also share `src/main.rs`, `src/config.rs`, and `src/embed/ollama.rs`. Shared-file work fails the
independence test (no overlapping files, no cross-slice ordering), so this ships as one PLAN.md.
File independence is the hard constraint and wins over the directive here.

Planner discretion recorded: (D-03) single-flight uses advisory `flock` via the `libc` crate rather
than a PID-file, because an fd-scoped `flock` releases automatically on crash with no staleness check.
(D-01) detach uses one `Command` spawn with a `libc::setsid()` `pre_exec`, re-execing the binary as
plain `hindsight embed`; `libc` is the single new dependency, covering both `flock` and `setsid`.
(D-07) status is split across per-unit `embed_ledger` columns (`status`/`attempts`/`last_error`) and a
separate `embed_run` table (heartbeat + counts), with a 120s stale-heartbeat threshold distinguishing
running from stalled.

Adjudicated plan-review outcome (D-05 letter deviation flagged for John): the `plan` trigger ran
claude-subagent + openai (gpt-5.3-codex); gemini dropped on a 429 free-tier quota (limit 0) and did not
vote. Both live reviewers converged on two issues, applied above: (1) deleting the Ollama `num_gpu`
plumbing entirely would hand placement to Ollama's auto heuristic, which partial-offloads to CPU under
VRAM pressure - so Task 1 keeps a single unconditional `num_gpu: 999` to force full GPU offload. This
diverges from D-05's literal "remove `EmbedOptions{num_gpu}`" while honoring its locked intent
(always-GPU, never CPU); flagged here for John's confirmation. (2) The `--detach` child must null its
stdio or it holds the hook's stdout pipe open and blocks the session - Task 5 now redirects to
`Stdio::null()`. Also applied: `ON CONFLICT ... DO UPDATE` instead of `INSERT OR REPLACE` so the
`attempts` counter accumulates; a `MAX_EMBED_ATTEMPTS` give-up cap so a permanently-failing unit stops
retrying every drain; an Ollama-free automated child-survival test; an explicit `--status` classifier
precedence; and stale in-source doc comments (timer / CPU-fallback) added to the doc-sync sweep.

Continue-on-error catch scope (flagged assumption resolved): `ollama::embed_document` already returns
`anyhow::Result`, boxing both `ureq` transport failures and 4xx/5xx status errors, so the drain catches
the whole `Result` per unit with no per-variant handling; no `ureq`-version-specific code is needed.

Human-verify tasks (5, and the GPU-residency half of the goal) require a running Ollama with
`qwen3-embedding:8b` pulled, a GPU present, and a loaded DB - none available to the executor as a
command, so those checks name the tool and the observation (`ollama ps` shows the GPU processor during a
real drain; `vec_embedding` count equals the `--dump-profiles` line count after it). Recalled prior art:
the resumable ledger and load-time reset were already UAT-verified in Phase 4 (phases/4/UAT.md), so this
phase extends that invariant rather than re-establishing it.
