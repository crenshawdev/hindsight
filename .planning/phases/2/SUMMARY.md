---
phase: 2
status: complete
completed: 2026-07-21
---

# Phase 2: Normalize - Summary

A `hindsight normalize <session-dir>` subcommand that turns archived `.zst` transcript generations into
tagged NDJSON Session / Event / Artifact / Mention records with three-tier grain and a fixed-pattern
secret scrub over indexed text only.

## What shipped

- `normalize` subcommand - src/main.rs (`Command::Normalize`), src/normalize/mod.rs (`run`: scans
  `NNNN.zst` generations, decompresses via `zstd::decode_all`, assembles one logical Session).
- Four record types + tagged NDJSON emit - src/normalize/model.rs (`Session/Event/Artifact/Mention`,
  `Record` enum `#[serde(tag = "type")]`, `write_ndjson`, `scrub_indexed`).
- Line-to-Event mapping with generation union + dedup-by-uuid, both content shapes (bare string and
  block list), tool_use_id->tool_name resolution, sidechain/agent handling - src/normalize/parse.rs.
- Three-tier grain with the tool-name gate (Read/Bash/Grep/Glob results skeleton; WebFetch/WebSearch
  indexed; skeleton bodies blanked keeping is_error+tool_name; archive-only lines drop) -
  src/normalize/grain.rs.
- File/command Mentions (Bash argv[0] with env-prefix skipping, path kept) and file-body Artifacts
  (Write/Edit content, Bash heredoc, fenced code) - src/normalize/extract.rs.
- Fixed-pattern secret scrub over indexed text only, archive verbatim - src/normalize/scrub.rs
  (regex dep added).
- Both-format fixtures + integration test over the five acceptance criteria - tests/normalize.rs,
  tests/fixtures/normalize/.

## Commits

| Plan | Task | Commit | Description |
|---|---|---|---|
| 1 | 1 | e25df98 | normalize subcommand, record model, session emit |
| 1 | 2 | 7ad800a | parse lines into events, generation union, session assembly |
| 1 | 3 | 716b0e5 | three-tier grain with the tool-name gate |
| 1 | 4 | a25e625 | file/command mentions and file-body artifacts |
| 1 | 5 | 094a2d4 | scrub fixed-pattern secrets from indexed text |
| 1 | 6 | c4d5f83 | both-format fixtures and normalize integration test |

## Deviations

- [deviation] Task 2 (7ad800a) - `agent_type` linkage: no live field reliably links a spawning `Agent`
  tool_use's `subagent_type` to a specific spawned `agentId`, so `agent_type` is derived from a
  session-scoped set of observed `subagent_type` values (assigned when exactly one distinct type was
  seen, else None) rather than the plan's per-`agent_id` map. Preserves the CONTEXT flagged
  best-effort/nullable posture; no verify asserts `agent_type`.
- [deviation] Task 1 test (7ad800a) - relaxed the "exactly one line emitted" assertion to "the session
  is the first emitted record" once Task 2 began emitting events. Expected cross-task evolution, fields
  unchanged.
- [deviation] Task 5 (094a2d4) - the scrub pattern set was hardened beyond the plan's literal
  enumeration per the adjudicated risk_surface gate (user chose "targeted fix now"): added provider
  prefixes (Stripe `sk_live_`/`sk_test_`/`rk_live_`, Google `AIza`, GitLab `glpat-`, npm `npm_`);
  broadened secret-named keys (`pass`, `pwd`, `credential`, and any compound `*_KEY`/`*-key`); made the
  assignment key tolerate optional surrounding quotes so quoted JSON fields (`{"access_token": "..."}`)
  are caught; and gated the assignment value to a quoted-or-length->=12 token shape so ordinary prose
  (`the big secret: it was a lie`) and code (`access_key = os.environ[...]`) stay byte-identical.
  Entropy detection still deferred per D-08. Full MUST/MUST-NOT matrix added as scrub.rs unit tests.
- [deviation] Task 6 (c4d5f83) - the crate is binary-only, so `archive::write_generation` is not
  reachable from an integration test; used the plan's allowed alternative (write fixture bytes as
  `0000.zst`, drive the real binary via `CARGO_BIN_EXE_hindsight`). The D-09 inline-format
  approximation note lives in the fixtures README and the test doc-comment (per-line JSON parse forbids
  comment lines in the `.jsonl`).

## Open items

- [diff review, high] `collect_generations` (src/normalize/mod.rs) descends only one hardcoded
  `subagents/<agent>/` level, but `archive_key` files nested transcripts at an arbitrary sub-path
  (`segments[2..]`). The confirmed-live subagent shape is handled (integration tests green, positive
  sidechain count), but deeper nesting (subagent-within-subagent) or workflow transcript dirs that ADR
  0001's "and below" allows would be silently dropped. Intersects Phase 6 (sweep wiring) and D-05's
  `subagents/`-only framing - decide whether the walk should recurse for every `NNNN.zst` under the
  session dir.
- [diff review, medium] `read_generations` (src/normalize/mod.rs) aborts the whole session on one
  invalid JSON line; a mid-append truncated final line would make an otherwise-complete session emit
  zero records. Consider skip-and-continue per line.
- [diff review, medium] `extract.rs` heredoc close matches `trim() == delim`, so an indented
  delimiter-lookalike inside a plain (non `<<-`) heredoc closes the body early and truncates the
  artifact.
- [diff review, low] `extract.rs` fenced-block detection breaks on an inner ``` line or an unclosed
  opening fence, truncating or dropping subsequent fenced artifacts in the same message.
- [build] `Grain::ArchiveOnly` is never constructed (archive-only lines emit no Event by design,
  Task 3) - a harmless `dead_code` warning.

## Goal check

The six commits plausibly deliver the phase goal. `hindsight normalize <session-dir>` (e25df98,
src/main.rs `Command::Normalize`) reads the decompressed `.zst` generations and emits tagged NDJSON:
run against the nested-split fixture it exits 0 and prints session/event/artifact/mention lines whose
`mention` entities (`/repo/src/main.rs`, `cargo`, `/repo/notes.txt`) equal the fixture's Read/Bash/Write
inputs (manual `jq` run - criterion 1). Both transcript formats parse in one run: the two integration
tests (c4d5f83, tests/normalize.rs `nested_split_..._criteria` and `inline_subagent_..._criteria`) both
pass with a positive `is_sidechain=true` count, each such event carrying the parent `session_id`
(criterion 2). Grain (716b0e5, grain.rs) assigns exactly one of indexed/skeleton/archive-only per event,
and the manual run shows `SKELETON_BODY_MARKER` absent from stdout (`grep -c` = 0) with skeleton bodies
blanked (criterion 3). The hardened scrub (094a2d4) removes the seeded `sk-SEEDEDSECRET0123456789` from
the NDJSON (`grep -c` = 0) while `zstd -dc` of the archive still finds it once (criterion 4). Every
emitted line parses as JSON with a `type` in {session,event,artifact,mention} (`jq` validation, no
BADLINE - criterion 5). 56 unit + 2 integration tests green (`cargo test`). Honest gap: the directory
walk handles the confirmed-live `subagents/<agent>/` layout but not the deeper-nested or workflow
sub-paths `archive_key` can produce (the high open item above); the phase's own acceptance criteria do
not exercise it, but it is a real completeness risk to weigh before Phase 6 wires normalize into the
sweep.
