---
status: testing
phase: 5
started: 2026-07-22
updated: 2026-07-22
---

## Items

### 1. Cold-start: fresh load builds schema v3
expected: From a clean state, `hindsight load` builds the DB and `PRAGMA user_version` reports 3, with `embed_ledger` carrying status/attempts/last_error columns and an `embed_run` table present.
status: pass
first_pass: pass
source: verifier
evidence: Real cold-start via `hindsight load` (empty stdin): user_version=3, embed_ledger has status/attempts/last_error, embed_run present. src/store/schema.rs:28,41,156-163,193-228; test open_db_creates_tables_and_stamps_version passes.

### 2. --detach returns fast, child survives
expected: `hindsight embed --detach` exits 0 within about one second; the detached child keeps running afterward (its PID stays alive, reparented to PID 1) and `vec_embedding` row count rises after the parent returned.
status: pass
first_pass: pass
reported: time embed --detach: 0.001s total, exit 0; detached binary pid 121541 alive after parent returned; --status went 0 -> 25/25 on its own
reason: Installed ~/.local/bin/hindsight is a pre-Phase-5 build (no --detach/--status); source has both (src/main.rs:56,59). Needs rebuild + reinstall before runtime walk.

### 3. GPU-only collapse, no residue
expected: `git grep` finds no CPU-fallback path, no `nvidia-smi`, no `Placement::Cpu`, no `gpu_*` config knob, no `HINDSIGHT_EMBED_FORCE_BUSY`; `src/embed/gpu.rs` does not exist; no `hindsight-embed.timer` or `hindsight-embed.service` remains in the tree.
status: pass
first_pass: pass
source: verifier
evidence: src/embed/gpu.rs ABSENT; git grep over src/ for nvidia-smi|Placement|gpu_*|choose_placement|HINDSIGHT_EMBED_FORCE_BUSY returns nothing; config.rs has no gpu_ fields (src/config.rs:33-42); no systemd/hindsight-embed.timer or .service. Single num_gpu:999 by design (src/embed/ollama.rs:27,63).

### 4. Full drain lands every vector, GPU-resident
expected: A full `hindsight embed` drain against a loaded DB lands every assembled unit's vector (`vec_embedding` count equals the assembled unit count, every vector 4096-dim) with the model GPU-resident during the run (`ollama ps` shows the GPU processor).
status: pass
first_pass: pass
reported: drain landed 25/25 units (embed_ledger 25 done, 6 entity + 19 event = all assembled); ollama ps shows qwen3-embedding:8b 100% GPU; 4096-dim enforced at ollama.rs:83-89, all units done not failed

### 5. Single-flight: no double-embed, late unit next run
expected: Two `hindsight embed` runs started concurrently produce no duplicate `(unit_kind, source_id)` rows and no duplicate vectors, the second exits without error, and a unit added after the drain releases its lock is embedded by the next `hindsight embed` run.
status: pass
first_pass: pass
reported: Concurrent race: loser logged 'an embed drain is already running; exiting without draining' exit 0, winner drained total=25 embedded=25 failed=0 exit 0, no double-embed. Re-run against complete DB: skipped=25 embedded=0 (landed units skipped). Fresh assemble each drain + ON CONFLICT upsert => new unit embedded next run, no dup (unit_kind,source_id) rows.

### 6. Resume + continue-on-error
expected: Interrupting a drain mid-way then re-running embeds only the units that did not land (landed units skipped); a run in which one unit's Ollama request fails completes the remaining units rather than aborting on the first error.
status: pass
first_pass: pass
source: verifier
evidence: Test drain_records_a_failure_and_continues passes: failing unit gets status='failed'+last_error, no vector, other units done; second drain skips done units (0 re-embed). src/embed/mod.rs:307-339 (Err arm records & continues, not ?-propagated), :289-298 (skip check).

### 7. --status distinguishes done/running/stalled/failed
expected: `hindsight embed --status` shows running with progress counts during a live drain, done with the full count after completion, stalled for a killed or stale run, and the failed unit after a seeded failure.
status: pass
first_pass: pass
source: verifier
evidence: Four classify tests pass: running_with_fresh_heartbeat, running_with_stale_heartbeat_is_stalled, done_when_ledger_covers_total, failed_row_reports_done_with_failures. src/embed/status.rs:58-134; wired src/main.rs:74, src/embed/mod.rs:128-130.

## Summary

total: 7
passed: 7
failed: 0
pending: 0
skipped: 0
blocked: 0
reworked: 0
