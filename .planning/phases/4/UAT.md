---
status: testing
phase: 4
started: 2026-07-22
updated: 2026-07-22
---

## Items

### 1. Cold-start: fresh load + embed boots clean
expected: On a clean DB (or a Phase-3 old-shape DB), `hindsight load` then `hindsight embed` runs to completion from scratch: the vec0 migration guard drops any old single-column shape, the two-stage `vec_embedding` + `embed_ledger` tables are (re)created, and embed exits 0 with real vectors written. No `no such column: embedding_coarse` error.
status: pass
first_pass: pass
source: verifier
evidence: Fresh-DB `hindsight embed`: embedded=7, EXIT=0, no error. Migration guard: built old single-col vec_embedding + user_version=1, reopened via binary -> columns [embedding_coarse,embedding,project,unit_kind,source_id], user_version=2, embed_ledger created, no `no such column` (src/store/schema.rs:46-48,149-155)

### 2. Row count equals unit count, 4096-dim
expected: Running `hindsight embed` against a loaded DB takes `vec_embedding` from 0 rows to a count equal to the number of queued profile units, and every stored vector is exactly 4096-dim (`SELECT count(*) FROM vec_embedding WHERE vec_length(embedding)=4096` equals the row count).
status: pass
first_pass: pass
source: verifier
evidence: --dump-profiles=7 units; total_vectors=7, dim4096=7. src/embed/ollama.rs:82-88 hard-bails on non-4096

### 3. No orphan vectors, project present
expected: Every stored vector's `(unit_kind, source_id)` mapping resolves to an existing entity/artifact/event record (the no-orphan SQL returns 0), and no vector has a null/empty `project` (`SELECT count(*) FROM vec_embedding WHERE project IS NULL OR project=''` returns 0).
status: pass
first_pass: pass
source: verifier
evidence: orphans=0, null_or_empty_project=0 (composite {entity_type}:{entity} resolves against mention)

### 4. Secret + full-code body excluded from profile text
expected: After `normalize | load | embed --dump-profiles`, grepping the dumped profile text for a seeded real-pattern secret returns zero hits, and grepping for a known full-code artifact body line (the code-body sentinel) returns zero hits. Signature lines may appear; the body must not.
status: pass
first_pass: pass
source: verifier
evidence: End-to-end normalize(fixture with sk-SEEDEDSECRET)->load->embed --dump-profiles: SEEDEDSECRET=0 hits (1 [REDACTED]); code-body sentinel=0 hits, `fn compute` signature=1 hit. tests/embed_profile.rs passes; scrub at src/normalize/mod.rs:66, model.rs:110-117

### 5. Nearest-neighbor round-trip returns own record
expected: A nearest-neighbor query for a stored profile's own vector (using a unique-text probe) returns that profile's own mapped `source_id` as the top match at distance 0.
status: pass
first_pass: pass
source: verifier
evidence: All 7 stored vectors queried via embedding MATCH -> each returns its own source_id at dist=0. Unit test vec_embedding_two_stage_insert_and_knn_round_trip passes

### 6. Forced GPU-busy still lands vectors on CPU
expected: With `HINDSIGHT_EMBED_FORCE_BUSY=1` and a low `gpu_max_defer_secs`, `hindsight embed` on a small loaded DB still lands all its vectors (defer then CPU fallback) and exits 0; `vec_embedding` count equals the unit count.
status: pass
first_pass: pass
source: verifier
evidence: HINDSIGHT_EMBED_FORCE_BUSY=1 + gpu_max_defer_secs=0: placement=Cpu, embedded=7, EXIT=0, all 4096-dim, dup_pairs=0. Unit test forced_busy_reads_busy_and_falls_back_to_cpu passes

### 7. Resumable + reset-on-load
expected: Re-running `hindsight embed` on the same DB embeds 0 (log shows skipped=N) with no duplicate `(unit_kind, source_id)` rows; interrupting a run and re-running yields the full count with no duplicates; a fresh `hindsight load` empties `embed_ledger` and the next `embed` re-embeds the whole corpus.
status: pass
first_pass: pass
source: verifier
evidence: Re-run: skipped=7 embedded=0, ledger unchanged, dup_pairs=0. `hindsight load` -> embed_ledger=0 -> next embed re-embeds embedded=7. Per-unit atomic vector+ledger tx at src/embed/mod.rs:101-114

### 8. Real GPU contention defers (human-verify)
expected: With an actual game holding the card, `hindsight embed` logs a deferral and does not contend for the GPU. Needs live GPU contention; cannot be machine-checked here.
status: pass
first_pass: pass
reported: Live run: 'GPU busy, deferring deferred_secs=0 poll=60' then 'placement=Gpu' once free; did not contend. (corpus empty so embedded=0)

## Summary

total: 8
passed: 8
failed: 0
pending: 0
skipped: 0
blocked: 0
reworked: 0
