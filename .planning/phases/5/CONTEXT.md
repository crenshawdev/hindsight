# Phase 5: Embed delivery - Context

Gathered: 2026-07-22
Feeds: /cad-plan 5

## Scope boundary

In: Replace Phase 4's timer trigger and GPU-opportunistic scheduling with hook-triggered, always-GPU
embedding as a detached process. Adds a `--detach` mode to `hindsight embed` (the binary self-detaches
so a session hook returns without blocking), a filesystem single-flight lock, a continue-on-error drain
posture, a DB-backed run/unit status model with a `hindsight embed --status` reporter, and a documented
one-time backfill-then-flip sequence. Deletes the GPU-scheduling machinery (`src/embed/gpu.rs`, CPU
fallback, `nvidia-smi` defer, the `gpu_*` knobs) and retires the systemd embed timer/service. Syncs the
docs (new ADR superseding ADR 0004's Phase-4 amendment, DESIGN.md, diagrams.md). Keeps and reuses the
Phase-4 `embed_ledger`, profile assembly, `vec_embedding` two-stage schema, and Ollama HTTP client.
Serves EMB-01, EMB-02.
Out: Live SessionStart/SessionEnd hook registration and retiring the prior memory tool (Phase 7,
MIG-02); the query core, RRF fusion, archive resolution, MCP server, and CLI search (Phase 6); the
transcript-ingest historical backfill (Phase 7, MIG-01 - distinct from this phase's embed backfill).
Deferred: None.
Plan shape: Big - multiple plans, same phase. Indicative split: (A) GPU-only collapse + retire timer +
doc-sync ADR; (B) `--detach` self-daemonize + single-flight lock; (C) continue-on-error + ledger/run
status schema + `--status`.

## Decisions

- D-01 (Detach trigger): A session hook fires `hindsight embed --detach`; the binary self-detaches
  (setsid / double-fork) and the parent returns immediately so the session is not blocked. Distinct
  from `hindsight poke`, which only pokes the capture-daemon socket. Evidence: locked user decision;
  src/poke.rs (poke is one byte to the daemon socket, no spawn/detach path), src/main.rs (Command enum;
  `Embed` carries only `--dump-profiles` today); no fork/setsid machinery in src/ yet.
- D-02 (Placement): The drain runs as that detached process, NOT inside the capture daemon; the
  daemon's 15-minute idle self-terminate contract is untouched. Evidence: locked user decision;
  src/daemon.rs:34-87 (idle self-terminate loop a multi-minute drain must not reset),
  docs/decisions/0011-hooks-and-daemon-knobs.md.
- D-03 (Single-flight): A filesystem lock under `config.state_dir()` allows one drain at a time; a
  second invocation that finds the lock held exits cleanly, never double-embedding. The lock
  crate/mechanism (advisory flock via a new dependency vs a PID-file + liveness check) is the planner's
  call. Evidence: locked user decision; src/config.rs:152-155 (`state_dir`), src/watermark.rs (state_dir
  already holds daemon state); no lock machinery in src/ or Cargo.toml today.
- D-04 (Mid-drain freshness): The running drain does NOT chase units that land after it started; the
  blocked concurrent trigger exits and the late unit is embedded on the next hook-fired invocation.
  This diverges from ROADMAP Phase-5 success criterion 3's "without lag" phrasing and is the accepted
  behavior (single-flight still prevents double-embed; lag is bounded by the next session event).
  Evidence: user decision; src/embed/mod.rs (`profile::assemble` runs once at drain start).
- D-05 (GPU-only collapse): Delete `src/embed/gpu.rs` entirely, collapse `Placement` to GPU-only, and
  remove the four `gpu_*` config knobs and their default tests, the `nvidia-smi` polling, the defer
  loop, the CPU-fallback request path, and `HINDSIGHT_EMBED_FORCE_BUSY`. Evidence: locked user decision
  (GPU-always, no CPU fallback ever); src/embed/gpu.rs (whole file), src/embed/ollama.rs:19-58
  (`Placement::Cpu`, `EmbedOptions{num_gpu}`), src/embed/mod.rs:66-70 (`choose_placement` threading),
  src/config.rs:25-78,268-287 (the `gpu_*` fields, defaults, and tests).
- D-06 (Error posture): Change the drain from fail-fast to continue-on-error - a single Ollama failure
  is caught, recorded, counted, and the drain proceeds to the next unit. Evidence: src/embed/mod.rs:95-96
  (`ollama::embed_document(...)?` propagates and aborts the whole run on the first error today).
- D-07 (Status, DB-backed): `hindsight embed --status` reads state from the DB. Per-unit
  `status`/`attempts`/`last_error` on `embed_ledger` (a failed unit gets a recorded row, not only
  successes) plus a run-level record (started / heartbeat / counts) so *running* vs *stalled* is
  distinguishable. Exact split (ledger columns vs a separate `embed_run` table) is the planner's call.
  State map: done = every assembled unit stamped for the current embedder version; running/stalled =
  run record present with a fresh vs stale heartbeat; failed = recorded unit failures. Evidence:
  src/store/schema.rs:181-190 (ledger is `unit_kind, source_id, embedder_version, embedded_at` only, a
  row lands only inside the successful transaction); user decision.
- D-08 (Scope): Phase 5 ships `--detach`, `--status`, continue-on-error, single-flight, the GPU-only
  collapse, and a documented backfill-then-flip sequence - no separate `--backfill` code mode, because a
  full drain with hooks off IS the backfill (`hindsight load` already wipes `embed_ledger` and the drain
  is full-corpus). Criterion 1 is verified by invoking the entrypoint the way a hook would. Live
  SessionStart/SessionEnd registration and retiring the prior memory tool stay in Phase 7 (MIG-02).
  Evidence: user decision; src/store/load.rs:30-54 (`FRESH_BUILD_TABLES` wipes vec_embedding +
  embed_ledger on every load), src/embed/mod.rs:35-126 (full-corpus drain), ROADMAP Phase 7.
- D-09 (Doc-sync, standing rule 1): A new ADR supersedes ADR 0004's Phase-4 amendment (timer,
  `nvidia-smi` defer, and CPU fallback all reversed); DESIGN.md and diagrams.md are updated (drop
  `timer -- triggers --> embed` and the "GPU opportunistic / CPU floor" note, and correct the daemon
  "embedding is NOT here" note to the detached-process reality); `systemd/hindsight-embed.timer` and
  `.service` are removed from the tree. Evidence: docs/decisions/0004-embedder-and-gpu-scheduling.md:68-92
  (the now-reversed amendment), docs/diagrams.md:26-47,79, CLAUDE.md standing rule 1, memory note
  `embed-trigger-gpu-redesign`.

## Acceptance criteria

- [ ] `hindsight embed --detach` exits 0 within about one second and a detached child keeps running
      afterward: its PID stays alive and the `vec_embedding` row count rises after the parent has
      returned.
- [ ] `git grep` finds no CPU-fallback path, no `nvidia-smi` call, no `Placement::Cpu`, no `gpu_*`
      config knob, and no `HINDSIGHT_EMBED_FORCE_BUSY`; `src/embed/gpu.rs` does not exist; and no
      `hindsight-embed.timer` or `hindsight-embed.service` file remains in the tree.
- [ ] A full `hindsight embed` drain against a loaded DB lands every assembled unit's vector
      (`vec_embedding` count equals the assembled unit count, every vector 4096-dim) with the model
      GPU-resident during the run (`ollama ps` shows the GPU processor).
- [ ] Two `hindsight embed` runs started concurrently produce no duplicate `(unit_kind, source_id)`
      rows and no duplicate vectors, the second exits without error, and a unit added to the store after
      a drain releases its lock is embedded by the next `hindsight embed` run.
- [ ] Interrupting a drain mid-way then re-running embeds only the units that did not land (already
      landed units are skipped), and a run in which one unit's Ollama request fails completes the
      remaining units rather than aborting on the first error.
- [ ] `hindsight embed --status` distinguishes done / running / stalled / failed: running with progress
      counts during a live drain, done with the full count after completion, stalled for a killed or
      stale run, and the failed unit after a seeded failure.

## Flagged assumptions

- Claude Code hook child-process lifetime - whether a hook-spawned `hindsight embed --detach` child
  survives after the hook returns and Claude Code reaps the hook's process group. The `--detach`
  (setsid/double-fork) choice is meant to guarantee this; the planner confirms the exact hook-JSON
  invocation and detach sequence. If wrong, the child dies with the session despite the parent returning.
- Ollama behavior under GPU contention with no CPU fallback (blocks/queues vs errors) - with the
  fallback deleted, this decides whether ordinary contention surfaces as a per-unit error (exercising
  continue-on-error) or a stall. A runtime fact confirmed at plan/UAT time.
- `ureq` v2 error surface - whether a 4xx/5xx or a connection failure returns `Err` vs a status to
  inspect - sets the exact catch scope for continue-on-error in src/embed/ollama.rs. Library fact the
  planner resolves.
- Single-flight lock crate/mechanism (D-03) and the DB status persistence shape (D-07, ledger columns
  vs a separate `embed_run` table) are left to the planner.
