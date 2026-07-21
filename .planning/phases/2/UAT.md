---
status: testing
phase: 2
started: 2026-07-21
updated: 2026-07-21
---

## Items

### 1. Mentions and artifacts equal tool-call inputs
expected: Running `hindsight normalize` on an archived session emits NDJSON whose file-path and command mentions equal the Read/Edit/Write/Bash tool-call inputs in that transcript, and whose artifacts equal the Write/Edit content.
status: pass
first_pass: pass
source: verifier
evidence: Real binary run: mentions `file /repo/src/main.rs`, `command cargo`, `file /repo/notes.txt` equal fixture Read/Bash/Write inputs; artifact content equals the Write input. Bash mention is argv[0]. extract.rs:116-199

### 2. Both transcript formats in one run
expected: A single normalize run over a fixture holding both the nested-split format and a hand-authored inline-subagent format exits 0 and files each subagent's events under the correct parent session.
status: pass
first_pass: pass
source: verifier
evidence: Both `cargo test --test normalize` cases pass; manual runs exit 0: nested-split 2 sidechain events under session_id=sessA, inline 3 under sessB. collect_generations mod.rs:72-94, parse.rs:140-150

### 3. Exactly one grain per event, no leakage
expected: Every emitted Event carries exactly one `grain` in {indexed, skeleton, archive-only}, and no skeleton or archive-only body text appears in the indexed output (grep for a known tool-result body string against the indexed stream returns zero hits).
status: pass
first_pass: pass
source: verifier
evidence: Every event has a grain field, census {indexed,skeleton} both fixtures. Read tool_result -> skeleton with text:null, is_error/tool_name kept. grep -c SKELETON_BODY_MARKER = 0 both. grain.rs:17-41

### 4. Secret scrubbed from index, verbatim in archive
expected: A secret seeded into a transcript is absent from the normalized NDJSON output (grep = 0 hits) and present byte-for-byte in the archived `.zst` (`zstd -d` = 1 hit).
status: pass
first_pass: pass
source: verifier
evidence: grep -c sk-SEEDEDSECRET0123456789 on stdout = 0 both fixtures (text shows [REDACTED]); zstd -dc 0000.zst | grep -c = 1. model.rs:107-121, scrub.rs:90-96

### 5. Every line is JSON with a valid type
expected: Every output line parses as JSON and carries a `type` in {session, event, artifact, mention} (`jq` validation over the full stream).
status: pass
first_pass: pass
source: verifier
evidence: All 14 nested-split lines + inline output parse as JSON with valid types; Record enum #[serde(tag="type", rename_all="lowercase")]. model.rs:94-101

## Summary

total: 5
passed: 5
failed: 0
pending: 0
skipped: 0
blocked: 0
reworked: 0
