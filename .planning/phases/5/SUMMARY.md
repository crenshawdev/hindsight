---
phase: 5
status: complete
completed: 2026-07-22
---

# Phase 5: Embed delivery - Summary

Retired the Phase-4 systemd embed timer and GPU-opportunistic/CPU-fallback scheduling in favor of a
hook-triggered `hindsight embed --detach` entrypoint that runs unconditionally on the GPU, single-flighted
by a `state_dir()` flock, resumable and continue-on-error, with a DB-backed `embed_run` + per-unit
`embed_ledger` status read by `hindsight embed --status`.

## What shipped

- GPU-only embedding - `src/embed/gpu.rs` deleted; `Placement`/CPU path and every `gpu_*` config knob
  removed; a single unconditional `EmbedOptions{num_gpu:999}` sent on every request (`src/embed/ollama.rs`,
  `src/config.rs`)
- Status schema - `embed_ledger.status/attempts/last_error` columns + new `embed_run` table;
  `SCHEMA_VERSION`/`USER_VERSION` 2->3 with a lockstep drop block; `embed_run` added to
  `FRESH_BUILD_TABLES` (`src/store/schema.rs`, `src/store/load.rs`)
- Single-flight drain - non-blocking `libc::flock` advisory lock under `state_dir()/embed.lock`, fd-scoped
  release; a blocked concurrent run exits `Ok(())` without draining (`src/embed/mod.rs`, `Cargo.toml`)
- Continue-on-error drain - testable `drain` core, `ON CONFLICT` upsert accumulating `attempts`,
  `MAX_EMBED_ATTEMPTS=5` give-up cap, per-unit heartbeat + run counts on `embed_run` (`src/embed/mod.rs`)
- `--detach` entrypoint - `setsid` self-daemonize with stdio nulled so the parent returns before opening
  the DB or taking the lock (does not hold the hook's stdout pipe) (`src/main.rs`, `src/embed/mod.rs`)
- `--status` reporter - classifier with running / stalled / done / done-with-failures / not-yet
  precedence, a live run outranking a stale per-unit failure (`src/embed/status.rs`)
- Doc-sync - new ADR `0013-embed-delivery-hook-gpu.md` (accepted, supersedes 0004's Phase-4 amendment);
  DESIGN.md and diagrams.md updated; both systemd embed units `git rm`'d

## Commits

| Plan | Task | Commit | Description |
|---|---|---|---|
| 1 | 1 | 644fd39 | Collapse embedding to GPU-only, delete the scheduling machinery (D-05) |
| 1 | 2 | cc69b56 | Per-unit status columns + `embed_run` table, schema v3 (D-07) |
| 1 | 3 | 59970ce | Single-flight the drain with a `state_dir` flock (D-03) |
| 1 | 4 | 70777cd | Continue-on-error drain with run record + heartbeat (D-06, D-07) |
| 1 | 5 | 3277267 | `--detach` self-daemonize entrypoint (D-01, D-02) |
| 1 | 6 | 3797672 | `hindsight embed --status` reporter (D-07) |
| 1 | 7 | e7ac49f | Doc-sync: ADR 0013, DESIGN, diagrams, remove the timer (D-09) |

All seven commits GPG-signed (`693AB15F91734B0C`, John Crenshaw <john@jcrenshaw.dev>); 80 tests pass, 0 fail.

## Deviations

- [deviation] Task 1 (644fd39), pre-flagged and adjudicated in plan review: honored the plan's
  letter-deviation from D-05 by keeping a single unconditional `EmbedOptions{num_gpu:999}` on every
  request instead of deleting `EmbedOptions` entirely - deleting it would hand placement to Ollama's auto
  heuristic (partial CPU offload under VRAM pressure), the exact CPU path D-05 forbids. Honors D-05's
  locked intent (always-GPU, never CPU).
- [deviation] Task 7 (e7ac49f): the `opportunistic` grep is intentionally non-empty because it matches
  ADR 0004's original Decision/Alternatives prose (immutable historical record, marked superseded) and
  `docs/STATUS.md:28` (not in this plan's file list). Every live-doc target D-09 named was removed (the
  diagram `timer` node and edge, the "GPU opportunistic / CPU floor" node, "CPU floor" now absent, the
  "embed timer unit" label, both systemd unit files).
- [deviation] Sequencing, no coverage loss: the `schema.rs` stale CPU-fallback comment (Task 1 grep) was
  corrected in Task 2 per the plan's own Task 2 action; the `--detach + --status` rejection test (Task 5
  verify) was added in Task 6, where `run` gained the `status` parameter.
- [deviation] Process, self-corrected: chained commit commands in Tasks 1 and 7 failed `git add` on paths
  already staged via `git rm`; amended each. Final 644fd39 and e7ac49f are complete (verified `git show --stat`).

## Open items

- [medium, advisory diff review] `attempts` accumulates across `embedder_version` changes while the
  give-up skip-check is version-scoped (`src/embed/mod.rs`): a unit that failed 5x under model A can be
  retired after a single attempt under model B when re-embedding without a `hindsight load` in between.
  Consider resetting `attempts` when the ledger row's `embedder_version` differs from the current one.
- [human-verify, needs Ollama + qwen3-embedding:8b + GPU + loaded DB] Task 5(b): `time hindsight embed
  --detach` fired from a real session hook exits ~1s while the detached child (reparented to PID 1, new
  session) survives and `vec_embedding` grows after the parent returned - confirm from a hook invocation,
  not an interactive shell.
- [human-verify] Goal criterion 3: a full drain lands every assembled unit's vector (`vec_embedding` count
  equals the `--dump-profiles` line count, every vector 4096-dim) with the model GPU-resident (`ollama ps`
  shows the GPU processor).
- [human-verify] Criterion 4 cross-process: two concurrent `hindsight embed` runs produce no duplicate
  `(unit_kind, source_id)` rows, the second logs "already running" and exits 0, and a unit added after the
  lock releases is embedded by the next run.
- [accepted residual, documented in plan Task 4] The heartbeat is not refreshed inside a single blocking
  `embed_fn` call, so a cold 8B model-load exceeding `STALE_HEARTBEAT_SECS` (120s) can momentarily read a
  live drain as `stalled`; it self-corrects on the next unit and the single-flight lock keeps a
  re-invocation harmless.
- [out-of-scope find] `docs/STATUS.md:28` still summarizes ADR 0004 as "opportunistic GPU schedule",
  now stale against ADR 0013's hook-triggered always-GPU delivery. Not in this plan's file list; flag for
  a STATUS refresh.
- [pre-existing, unrelated] `src/archive.rs:57` dead-code warning (`Outcome` variant field `path` never
  read); surfaced by the build, not introduced by this phase.

## Goal check

The seven commits plausibly deliver the phase goal. GPU-only is real and evidenced: `src/embed/gpu.rs` is
gone (`test ! -f`), and `git grep` for `nvidia-smi|Placement|choose_placement|gpu_*|HINDSIGHT_EMBED_FORCE_BUSY`
over `src/` returns nothing, with a single unconditional `num_gpu:999` surviving by design. The status
substrate is in place: schema is at v3 with `embed_ledger.status/attempts/last_error` and an `embed_run`
table, single-flight is a `libc::flock` under `state_dir()`, and the drain is continue-on-error with an
`ON CONFLICT` attempts upsert and a heartbeat (all under `cargo test embed::`/`store::`, 80 tests pass).
`--detach` and `--status` are wired in `src/main.rs:56,59` and route through `embed_run`. Doc-sync landed:
ADR `0013-embed-delivery-hook-gpu.md` is `Status: accepted` and supersedes 0004's Phase-4 amendment, and
both systemd embed units are removed. What is NOT closed here, honestly: the three human-verify criteria
(detached-child survival from a real hook, full-drain GPU-resident vector landing, cross-process no-double-
embed) require a running Ollama with the model pulled and a GPU, none available to the executor, so they
are implemented with Ollama-free automated tests but not runtime-confirmed. Live SessionStart/SessionEnd
hook registration and the actual one-time backfill run are Phase 7 by design; Phase 5 ships the entrypoint
and the backfill-then-flip sequence in the ADR, not the live wiring. Net: the code and docs deliver
hook-triggered, always-GPU, single-flighted, resumable, observable embedding; the runtime proof is deferred
to the named human-verify checks.
