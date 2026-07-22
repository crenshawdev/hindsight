---
status: testing
phase: 6
started: 2026-07-22
updated: 2026-07-22
---

## Items

### 1. Exact listing is recall-complete
expected: An exact-listing query (hindsight search --exact <file>) for a file present in the store returns every session_id whose mention rows reference that file; the count equals a direct sqlite3 COUNT(DISTINCT session_id) over the mention table for that entity - no omissions.
status: pass
first_pass: pass
source: verifier
evidence: src/query/exact.rs:17-60 (DISTINCT session_id via GROUP BY over mention alone, no join); test returns_every_session_for_a_file_and_count_matches asserts result == SELECT COUNT(DISTINCT session_id) FROM mention (exact.rs:103-110), passed

### 2. Fuzzy ranked search fuses FTS+vector and pre-filters to a strict subset
expected: A fuzzy ranked query returns results drawn from both the FTS5 keyword arm and the sqlite-vec vector arm fused by RRF; adding a --project (or time-window) pre-filter returns a strict subset narrowed to that anchor.
status: pass
first_pass: pass
source: verifier
evidence: src/query/ranked.rs:77-114 (RRF_K=60, per-session best rank, fuse both arms); keyword FTS5 MATCH+bm25 (keyword.rs:49-70); vector two-stage coarse-hamming->cosine-rescore vs real vec_embedding (vector.rs:139-267); test fuses_both_arms_and_project_narrows passed

### 3. Ollama-unreachable fuzzy query degrades to keyword-only
expected: With Ollama unreachable, a fuzzy query still returns keyword results for a known term (nonzero) rather than erroring - the vector arm is skipped and the degradation is reported (degraded: true).
status: pass
first_pass: pass
source: verifier
evidence: src/query/ranked.rs:94-106 (embed_query Err -> skip vector, degraded=true, Ok); embed_failure_degrades_to_keyword_only + mcp handler test drive real embed_query at unreachable 127.0.0.1:1 asserting degraded:true with sess-1 returned, passed

### 4. Hit resolution returns verbatim archived bytes
expected: Resolving a specific event/artifact hit returns verbatim bytes that appear byte-for-byte in the zstd -d of the source <project>/<session-id>/<ref> generation.
status: pass
first_pass: pass
source: verifier
evidence: src/normalize/mod.rs:81-99 (pinpoint returns raw line.to_vec(), no re-serialize); resolve.rs:23-71 (archive_refs -> read_generation -> pinpoint); test resolves_artifact_to_verbatim_source_bytes asserts generation.windows(len).any(==) AND re-serialized Value does NOT match, passed

### 5. CLI ground-truth search has no embedder/GPU dependency
expected: The CLI ground-truth search (keyword + exact) returns the expected rows with Ollama stopped, proving no embedder/GPU dependency in that path.
status: pass
first_pass: pass
source: verifier
evidence: src/query/mod.rs:24-65 run_search calls only exact_listing/keyword_search; grep confirms zero ollama::/use crate::embed in mod.rs/exact.rs/keyword.rs; main.rs:102-109 routes Command::Search->run_search, no embed init

### 6. Claude Code connects to the MCP server
expected: Claude Code connects to the hindsight mcp server and a recall tool call returns results for a seeded query. (human-verify: needs a live Claude Code MCP client)
status: pass
first_pass: pass
reported: got 1 session listed via live Claude Code MCP exact_listing call

## Summary

total: 6
passed: 6
failed: 0
pending: 0
skipped: 0
blocked: 0
reworked: 0
