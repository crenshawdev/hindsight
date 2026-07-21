# Phase 2: Normalize - Context

Gathered: 2026-07-21
Feeds: /cad-plan 2

## Scope boundary

In: A `hindsight normalize` subcommand that reads an archived transcript generation and emits Session /
Event / Artifact / Mention records to tagged NDJSON on stdout; the three-tier grain (indexed / skeleton /
archive-only) on every event; a fixed-pattern secret scrub over indexed text; and a parser that handles
both historical transcript formats (the live nested-split subagent format plus a hand-authored
inline-subagent fixture). Serves NRM-01, NRM-02, NRM-03, NRM-04.
Out: The SQLite store, FTS5, and sqlite-vec (Phase 3); synthetic profiles and Ollama embeddings
(Phase 4); the query core, MCP server, and CLI search (Phase 5); wiring normalize into the daemon sweep
and the empty-watermark backfill (Phase 6).
Deferred: Entropy-based secret detection - a later hardening pass on top of the fixed pattern set (D-08).
Package / symbol / prose Mention extraction - Phase 2 emits only `file` and `command` mentions (D-10).
Plan shape: Big - multiple plans, same phase (/cad-plan breaks the five criteria into ordered plans,
e.g. parse + record types, then grain + scrub, then NDJSON output + both-format fixtures).

## Decisions

- D-01 (Invocation): Normalize is a new `hindsight normalize` subcommand emitting to an inspectable sink;
  it is NOT wired into the daemon sweep this phase. Evidence: src/main.rs (Command enum is
  Daemon/Precompact/Poke), .planning/ROADMAP.md (Phase 2 "no store yet"), docs/STATUS.md (build order).
- D-02 (Input): Normalize reads decompressed archived generations under
  `<base>/archive/<project>/<session-id>/NNNN.zst` (the ground truth), not the live `~/.claude` tree.
  Evidence: docs/decisions/0003-normalize-event-grain.md (re-runnable over the archive, the format-drift
  firewall), src/archive.rs, docs/DESIGN.md.
- D-03 (Output): Emit tagged NDJSON to stdout - one JSON object per line, each carrying a `type` in
  {session, event, artifact, mention} - so `hindsight normalize <session> | jq` inspects it and Phase 3
  loads the same stream. Evidence: .planning/ROADMAP.md (Phase 2 inspectable form / Phase 3 load into
  schema), Cargo.toml (serde_json already a dependency).
- D-04 (Split format): Subagent turns live in nested `subagents/agent-<agentId>.jsonl` files sharing the
  parent `sessionId`, with `isSidechain:true` and `agentId` set; the parent references them via an `Agent`
  tool_use whose `input.subagent_type` is the agent type. This is the only subagent format in the live
  corpus. Evidence: live scan (827 sidechain files, all nested; zero Task-tool sessions), src/config.rs
  (`archive_key` sub-path handling), src/sweep.rs.
- D-05 (Session assembly): One logical Session is the parent generation plus every nested `subagents/`
  generation under the same `<project>/<session-id>/` archive directory (all share the parent sessionId).
  Evidence: nested subagent files carry the parent sessionId, archive layout on disk, src/config.rs.
- D-06 (Field mapping): Event / Session / Artifact fields map mechanically from JSON - Event{uuid,
  parentUuid, role, content-block kind (thinking/text/tool_use/tool_result), timestamp, tool name,
  isSidechain, agentId, attributionAgent/Skill/Plugin, agent_type from the spawning Agent call};
  Session{sessionId, project from the archive segment, gitBranch, version->cc_version, first/last
  timestamp, `ai-title`->title}; Artifacts and Mentions come from tool-call INPUTS (Write/Edit/Bash) and
  answer text, never tool_result bodies. Evidence: live line-key analysis, docs/decisions/0003 and 0005,
  docs/diagrams.md data-model.
- D-07 (Grain): indexed = user prompts, assistant text, tool_use invocations with extracted args, and
  artifacts; skeleton = assistant thinking blocks and tool_result bodies (keep only `is_error` + which
  tool), tool-name-gated so Read/Bash/Grep/Glob results skeleton while WebFetch/WebSearch results stay
  indexed; archive-only = duplicated/injected payloads and machine noise (`system`, `attachment`,
  `isMeta`, hook chatter) with no record; `ai-title` feeds Session.title only. Evidence:
  docs/decisions/0003-normalize-event-grain.md (lines 32-43, the tool-name gate), live type census.
- D-08 (Scrub scope): A fixed pattern set now (tokens, private keys, connection strings, auth headers,
  config-file values) applied to indexed text only; the archive stays verbatim. Entropy detection is
  deferred to a later hardening pass. Evidence: docs/decisions/0008-secrets-scrub-index-only.md,
  docs/STATUS.md (patterns/thresholds flagged open).
- D-09 (Inline format): Hand-author an inline-subagent transcript fixture from ADR 0003's description to
  exercise the inline parser path, since the live tree has zero inline sessions left (all subagents are
  the D-04 split format). Evidence: live scan (oldest transcript 2026-06-23, no Task-tool sessions),
  docs/decisions/0003-normalize-event-grain.md (both formats existed in the author's corpus).
- D-10 (Mentions): High-confidence extraction only - `entity_type` in {file, command}, files from
  Read/Edit/Write `file_path` and commands from Bash argv[0]; package / symbol / prose mining deferred.
  Evidence: docs/diagrams.md (Mention has entity/entity_type, no enum), docs/DESIGN.md (file/command/
  package/symbol given as examples, not a closed set).

## Acceptance criteria

- [ ] Running `hindsight normalize` on an archived session emits NDJSON whose file paths and commands
      equal the Read/Edit/Write/Bash tool-call inputs in that transcript, and whose artifacts equal the
      Write/Edit content.
- [ ] A single normalize run over a fixture holding both the nested-split format and a hand-authored
      inline-subagent format exits 0 and files each subagent's events under the correct parent session.
- [ ] Every emitted Event carries exactly one `grain` in {indexed, skeleton, archive-only}, and no
      skeleton or archive-only body text appears in the indexed output (grep for a known tool-result body
      string against the indexed stream returns zero hits).
- [ ] A secret seeded into a transcript is absent from the normalized NDJSON output (grep = 0 hits) and
      present byte-for-byte in the archived `.zst` (`zstd -d` = 1 hit).
- [ ] Every output line parses as JSON and carries a `type` in {session, event, artifact, mention}
      (`jq` validation over the full stream).

## Flagged assumptions

- Multi-generation reconciliation: when a session has a `precompact` snapshot plus a later `sweep`
  generation, union events across all generations and dedupe by event `uuid` so pre-compaction turns are
  not dropped; if wrong, taking only the latest generation silently loses the turns PreCompact captured
  (defeats CAP-03). Planner's default unless a plan revisits it.
- `end_reason` has no obvious single source field in the transcript; if wrong, it is guessed from the
  wrong signal - may be left null in Phase 2 and populated later.
- The inline-format's exact field layout is reconstructed from ADR 0003 with no live sample, so the
  hand-authored fixture (D-09) approximates rather than reproduces the real historical shape.
