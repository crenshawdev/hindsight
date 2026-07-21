# normalize fixtures

Hand-authored transcript fixtures for the `normalize` integration test.

- `nested_split_parent.jsonl` + `nested_split_subagent.jsonl` — the live
  nested-split subagent format (D-04): the subagent turns live in a separate
  file under `subagents/agent-<id>/`, sharing the parent `sessionId` and
  carrying `isSidechain:true` / `agentId`. The parent references the agent via
  an `Agent` tool_use whose `input.subagent_type` names the agent.

- `inline_subagent.jsonl` — the hand-authored inline-subagent format
  reconstructed from ADR 0003 (D-09): the subagent turns are inlined in a single
  file with `isSidechain:true` / `agentId` set and the spawning `Agent` tool_use
  in the same file. The live tree has zero inline sessions left, so this
  approximates the historical shape rather than reproducing a real sample.

Both fixtures seed the secret `sk-SEEDEDSECRET0123456789` into an indexed
assistant text block (must be scrubbed from the NDJSON, kept verbatim in the
archive) and the marker `SKELETON_BODY_MARKER` into a Read tool_result body
(skeleton grain, must never reach the indexed output).
