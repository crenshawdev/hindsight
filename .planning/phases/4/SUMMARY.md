---
phase: 4
status: complete
completed: 2026-07-22
---

# Phase 4: Fuzzy - Summary

Synthetic profiles assembled mechanically from the loaded SQLite records and embedded via Ollama
`qwen3-embedding:8b` into a two-stage `vec_embedding` vec0 table, driven by a resumable `hindsight embed`
subcommand.

## What shipped

- Two-stage vec0 schema + `embed_ledger` - `src/store/schema.rs` (`vec_embedding` with `embedding_coarse
  bit[4096]` + `embedding float[4096] distance_metric=cosine` + `project` + `+unit_kind`/`+source_id`,
  `SCHEMA_VERSION`/`USER_VERSION` bumped to 2 with a drop-on-old-version migration guard), wiped in
  lockstep on load via `FRESH_BUILD_TABLES` in `src/store/load.rs`.
- `hindsight embed` subcommand - `src/main.rs:49,64` (`Embed { --dump-profiles }`), `src/embed/mod.rs`
  (`run`), `src/embed/ollama.rs` (`ureq` POST to `/api/embed`, `Placement::{Gpu,Cpu}`, 4096-dim pin).
- Mechanical profile assembly (three D-08 units) - `src/embed/profile.rs` (entity profiles via GROUP BY
  with composite `{entity_type}:{entity}` source_id, artifact wrappers with session-join project,
  prose chunks; no secrets, no code body), tests in `tests/embed_profile.rs`.
- Resumable ledger drain - `src/embed/mod.rs` (single-transaction vector+ledger stamp, skip already-
  embedded, reset on load).
- GPU-busy tri-state detection + defer-then-CPU-fallback - `src/embed/gpu.rs` (`GpuState`,
  `choose_placement`, `HINDSIGHT_EMBED_FORCE_BUSY` test hook).
- systemd timer + oneshot service - `systemd/hindsight-embed.{service,timer}`.
- Docs sync - `d309950` corrected the daemon-embeds diagram edge to a timer-driven embed job and amended
  ADR 0004/0006 and STATUS/DESIGN.

## Commits

| Plan | Task | Commit | Description |
|---|---|---|---|
| 1 | 1 | e408fc6 | Extend vec0 schema with two-stage shape and resumable embed ledger |
| 1 | 2 | b7c7001 | Add `hindsight embed` command with Ollama client and vector round-trip |
| 1 | 3 | d0f6175 | Assemble entity profiles and artifact wrappers mechanically |
| 1 | 4 | 5a23ab1 | Drain the embed queue resumably against the embed_ledger |
| 1 | 5 | 3a0247f | Defer embedding on a busy GPU and fall back to CPU |
| 1 | 6 | 4e30729 | Add systemd timer and service to schedule embedding |
| 1 | 7 | d309950 | Sync docs to the timer-driven embed job and delivered vec0 shape |

## Deviations

- [plan shape] The CONTEXT `Plan shape` directive asked for multiple ordered plans (schema plan gating
  the writes); one `PLAN.md` was produced instead because the slices are not independently verifiable
  (schema gates writes) and all share `src/embed/` + `src/main.rs`, failing Cadence's file-disjoint
  parallel-split test. Gating expressed as task ordering (Task 1 gates Tasks 2-5). Recorded in PLAN.md
  Notes.
- [execution] Phase closed by cad-progress/cad-execute recovery, not fresh executor dispatch: all seven
  tasks were already committed as `feat(4-1)` on `feat/phase-4-fuzzy-plan` before this run, so no
  executors were dispatched; this SUMMARY records the already-landed commits rather than re-running them.

## Open items

- Delivery mechanism superseded by the embed redesign: Task 5 (CPU fallback) and Task 6 (systemd timer
  trigger) were built to ADR 0004, but the locked embed-trigger redesign (hook-triggered, always-GPU,
  never CPU, no timer/socket) supersedes both and is now scoped to the newly-inserted **Phase 5: Embed
  delivery**. The timer + CPU-fallback code shipped here is expected to be relocated or replaced in Phase
  5; it is not the final delivery path. Confirm the redesign lands there before treating this as final.
- Cross-project entity carries one `project` (the most-frequent) on its vector, satisfying criterion 2
  (non-empty project) but leaving Phase 6's structural pre-filter unable to narrow a shared entity to its
  other projects. Full project set still lives in the entity profile TEXT. Per-project retrieval would
  need one vector per (entity, project) or a multi-valued project index, both changing the D-09 shape -
  flagged for John in PLAN.md Notes, left as-is pending decision.

## Goal check

The seven commits plausibly deliver the phase goal (synthetic profiles built from records and embedded
via Ollama into an extended `vec_embedding`, drained GPU-opportunistically by a `hindsight embed`
subcommand). Evidence: the two-stage vec0 shape and `embed_ledger` are live in `src/store/schema.rs:24`
(`SCHEMA_VERSION = "2"`) and `schema.rs:167` (the `CREATE VIRTUAL TABLE ... vec0(embedding_coarse
bit[4096], embedding float[4096] distance_metric=cosine, ...)`), with the old-shape migration drop guarded
by `USER_VERSION` at `schema.rs:149`; the `Embed` command is wired in `src/main.rs:49,64`; assembly,
Ollama client, resumable drain, and GPU tri-state policy live in `src/embed/{profile,ollama,mod,gpu}.rs`;
the timer/service units exist under `systemd/`; docs were synced in `d309950`. Two caveats, both open
items above rather than gaps in the phase-as-planned: the timer + CPU-fallback delivery path built here is
superseded by the Phase 5 embed redesign, and a cross-project entity gets a single `project` on its
vector. The runnable end-to-end verifies (row-count equals unit-count, 4096-dim, NN round-trip, no-orphan,
secret/code-body exclusion, twice-run idempotence, forced-busy CPU landing) are asserted by the plan's
Verify blocks and `tests/embed_profile.rs` but were not re-run in this recovery pass - `/cad-verify 4`
owns falsifying them against the live tree.
