---
phase: 2
plan: 1
requirements: [NRM-01, NRM-02, NRM-03, NRM-04]
files:
  - Cargo.toml
  - src/main.rs
  - src/normalize/mod.rs
  - src/normalize/model.rs
  - src/normalize/parse.rs
  - src/normalize/grain.rs
  - src/normalize/extract.rs
  - src/normalize/scrub.rs
  - tests/normalize.rs
  - tests/fixtures/normalize/
---

# Phase 2: Normalize - Plan

## Goal

Turn an archived transcript into Session / Event / Artifact / Mention records with the three-tier
grain and secret scrubbing, emitted as tagged NDJSON on stdout so a person can inspect it with `jq`
and Phase 3 can load the same stream. No SQLite store this phase.

## Must be true when done

- `hindsight normalize <session-dir>` reads the decompressed `.zst` generations under an archived
  session directory (parent plus every nested `subagents/` transcript) and prints NDJSON to stdout,
  one JSON object per line, each carrying a `type` in {session, event, artifact, mention}.
- The file paths and commands in the emitted `mention` records equal the Read/Edit/Write `file_path`
  and Bash argv[0] of the transcript's tool calls, and the `artifact` records equal the Write/Edit
  file bodies (and Bash heredoc / fenced-code bodies) produced in the run.
- A single run over a fixture holding both the nested-split subagent format and a hand-authored
  inline-subagent format exits 0 and files each subagent's events under the correct parent session.
- Every emitted `event` carries exactly one `grain` in {indexed, skeleton, archive-only}, and no
  skeleton (tool-result body / thinking) text and no archive-only line reaches the indexed output.
- A secret seeded into a transcript is absent from the normalized NDJSON (grep = 0 hits) and still
  present byte-for-byte in the archived `.zst` (`zstd -d` = 1 hit).

## Context

Locked decisions (phases/2/CONTEXT.md) bind this plan: D-01 (new `hindsight normalize` subcommand,
NOT wired into the sweep - Phase 6 does that), D-02/D-05 (read decompressed archived generations,
one logical Session = parent + nested `subagents/` generations sharing the parent sessionId),
D-03 (tagged NDJSON to stdout), D-04/D-09 (nested-split live format plus a hand-authored inline
fixture), D-06 (mechanical field mapping from real transcript JSON), D-07 (three-tier grain with the
tool-name-gated skeleton rule), D-08 (fixed-pattern scrub over indexed text only, archive verbatim),
D-10 (file + command mentions only). Field shapes are grounded in the live corpus: lines carry
`type`, `uuid`, `parentUuid`, `sessionId`, `timestamp`, `isSidechain`, `agentId`, `version`,
`gitBranch`, `cwd`, `message`, `attributionSkill`/`attributionAgent`/`attributionPlugin`, `isMeta`,
`toolUseResult`, and `aiTitle`; `message.role` in {user, assistant}; `message.content` is EITHER a bare string (common on real
user lines) OR a block list with block `type` in {text, thinking, tool_use, tool_result} - both shapes
must be handled (see Task 2); a tool_use block is
`{id, name, input}` (Read.input.file_path, Bash.input.command, Write.input.{file_path,content},
Edit.input.{file_path,old_string,new_string}, Agent.input.subagent_type); a tool_result block is
`{tool_use_id, content, is_error?}`; an `ai-title` line carries `aiTitle`. Follow the existing crate
style: `anyhow::Result`, `serde`/`serde_json` (already deps), module tree under `src/`, unit tests in
`#[cfg(test)]`, decompress with `zstd::decode_all` as `archive.rs` does. Out of scope this phase:
the SQLite store/FTS5/sqlite-vec (Phase 3), profiles/embeddings (Phase 4), query/MCP/CLI-search
(Phase 5), sweep wiring and empty-watermark backfill (Phase 6), entropy-based secret detection
(deferred, D-08), and package/symbol/prose mentions (deferred, D-10).

## Tasks

### Task 1: `normalize` subcommand + record model + end-to-end Session emit (tracer bullet)

- **Files:** src/main.rs, src/normalize/mod.rs, src/normalize/model.rs
- **Action:** Add a `Normalize { session_dir: PathBuf }` variant to the `Command` enum in main.rs
  (doc comment: "Normalize an archived session directory to tagged NDJSON on stdout") and dispatch it
  to `normalize::run` via `report(...)`. Declare `mod normalize;`. In `src/normalize/model.rs` define
  the four record structs matching the data-model diagram, each `#[derive(Serialize)]` and tagged for
  NDJSON via serde: `Session{session_id, project, git_branch, cc_version, started_at, ended_at,
  end_reason, title, archive_refs: Vec<String> (generation filenames in sorted order)}`, `Event{uuid, parent_uuid, session_id, role, kind, timestamp,
  text, tool_name, is_error: Option<bool>, attribution, is_sidechain, agent_id, agent_type: Option<String>, grain}`
  (`is_error` carries the tool_result error flag that skeleton grain must retain per Task 3),
  `Artifact{artifact_id,
  kind, path, language, content, request_bundle, source_event_uuid}`, `Mention{entity, entity_type,
  event_uuid, session_id, project, timestamp}`. Add a `Record` enum with `#[serde(tag = "type",
  rename_all = "lowercase")]` wrapping the four so each serializes with a `type` field; provide
  `write_ndjson<W: Write>(records, w)` that writes one compact `serde_json` line per record. In
  `src/normalize/mod.rs` implement `run(session_dir: &Path) -> anyhow::Result<()>`: derive `project`
  = parent dir name from `session_dir`, and take `session_id` mechanically from the transcript's
  `sessionId` field (D-06), not the directory name (the two are equal by the archive layout, but the
  JSON field is the authoritative mapping and survives a renamed/moved dir); scan the directory for
  `NNNN.zst` generation files (numeric stem, `.zst` suffix, skip dotfiles and `meta.json`, mirroring
  `archive::scan_generations`), decompress each with `zstd::decode_all`, split into non-empty lines,
  and parse each line as `serde_json::Value`. Build a minimal `Session` from the parsed lines
  (session_id, gitBranch->git_branch, version->cc_version, min/max `timestamp` -> started_at/ended_at,
  `aiTitle` -> title, end_reason left None per the flagged assumption, archive_refs = the generation
  filenames read) and emit exactly that one session line to `std::io::stdout()`. Do not read the live
  `~/.claude` tree and do not touch the sweep or config archive path resolution - the argument is a
  direct archive directory path so the command is inspectable without a config file (D-01, D-02).
- **Verify:** `cargo build` succeeds and `cargo test normalize::` passes a new unit test in
  `src/normalize/mod.rs` that uses `archive::write_generation` to write a small hand-built transcript
  (containing `sessionId`, `gitBranch`, `version`, two differing `timestamp` lines, and an `aiTitle`
  line) into a `tempfile` archive dir, calls `run` capturing stdout, and asserts stdout is exactly one
  line that `serde_json` parses to an object with `"type":"session"` and the expected `title` and
  `git_branch`.

### Task 2: Parse lines into Events with mechanical field mapping, generation union, and session assembly

- **Files:** src/normalize/parse.rs, src/normalize/mod.rs
- **Action:** In `src/normalize/parse.rs` implement the line-to-Event mapping (D-06). Collect lines
  from every generation of every transcript belonging to the session - the parent directory's
  generations plus every nested `subagents/agent-<id>/` subdirectory's generations under the same
  session dir (D-05) - and union them, deduplicating by line `uuid` (keep first seen) so a `precompact`
  generation's pre-compaction turns survive alongside a later `sweep` generation (the flagged
  reconciliation assumption; taking only the latest generation would silently drop them and defeat
  CAP-03). Emit events ONLY from message lines - a positive whitelist of `type` in {user, assistant}
  (the concrete transcript line-type values, NOT a literal `"message"`); every other `type` produces no
  event and is left to the archive (Task 3's noise list is illustrative, not an exhaustive blacklist,
  so unlisted/future types like `permission-mode` never leak). Expand each whitelisted message line into
  Events, handling BOTH content shapes: `message.content` is often a bare string on real user lines
  (roughly one in eight user messages in the live corpus, e.g. `"yes"`), and is a block list on
  assistant lines and richer user lines. When `content` is a string, emit ONE `kind = text` Event with
  `text` = that string; when it is a block list, emit one Event per block. For every Event
  set `uuid` = line uuid, `parent_uuid` = parentUuid, `session_id`, `role` = message.role, `kind` =
  the block type (thinking/text/tool_use/tool_result) or `text` for string content, `timestamp`,
  `is_sidechain` = isSidechain, `agent_id` = agentId, `is_error` = the tool_result `is_error` flag when
  present (else None), `attribution` = whichever of attributionSkill/attributionAgent/attributionPlugin
  is present. For `text`/`thinking` blocks set `text` to the block string. For `tool_use` set
  `tool_name` = block.name and `text` to a compact one-line summary of `input` (the extracted args,
  e.g. the file_path or command), never the raw tool_result. For `tool_result` set `text` to the
  result body and resolve `tool_name` to the answering tool by matching block.tool_use_id against the
  tool_use blocks seen in the session (build a `tool_use_id -> name` map during the first pass); if the
  tool_use_id is not found in the map, set `tool_name` to None. For
  sidechain events populate `agent_type` from the spawning Agent tool_use's `input.subagent_type`:
  build an `agent_id -> set of seen subagent_type values` map, and for each sidechain event use the
  single type when the set holds exactly one, else set `agent_type` None (empty or multiple distinct
  types = ambiguous), same best-effort posture as end_reason. Because both the split format (nested files) and the inline
  format (sidechain blocks in the parent file) produce events carrying `is_sidechain`/`agent_id`, the
  same expansion handles both formats in one run (NRM-03). Wire `mod.rs::run` to emit all events after
  the session line, ordered by their generation/line order.
- **Verify:** `cargo test normalize::parse` passes unit tests that feed in-memory JSON lines and assert:
  (a) a two-generation input where both generations share a uuid emits that event once; (b) an
  assistant line with text + tool_use + thinking blocks emits three events with the right `kind` and
  `tool_name`; (c) a tool_result block resolves `tool_name` to the tool named by its matching tool_use;
  (d) a nested-subagent generation's events carry `is_sidechain=true` and the parent's `agentId`;
  (e) a user line whose `message.content` is a bare string (not an array) emits exactly one `kind=text`
  Event whose `text` equals that string; (f) a non-message line type (e.g. `system`, `permission-mode`)
  emits no Event.

### Task 3: Three-tier grain assignment with the tool-name gate

- **Files:** src/normalize/grain.rs, src/normalize/parse.rs
- **Action:** In `src/normalize/grain.rs` implement `assign_grain` as a pure function of an event plus
  its resolved answering-tool name, returning `indexed | skeleton | archive-only` (D-07): user prompts,
  assistant `text`, `tool_use` invocations (with extracted args), and artifacts are `indexed`;
  assistant `thinking` blocks and `tool_result` bodies are `skeleton` EXCEPT when the answering tool is
  WebFetch or WebSearch (those stay `indexed`), with the skeleton gate applied for local-content tools
  Read/Bash/Grep/Glob (and any other non-web tool). For skeleton tool_result events keep only
  `is_error` and the answering `tool_name` and blank the body `text` so no result body reaches the
  indexed output; for skeleton thinking events blank the `text`. Machine-noise line types
  (`system`, `attachment`, `mode`, `file-history-snapshot`, `last-prompt`, `queue-operation`,
  `ai-title`, and any line with `isMeta:true` or hook chatter such as `hookInfos`/`hookErrors`) are
  `archive-only`: emit no Event record for them at all (the `ai-title` value already fed
  Session.title in Task 1). Set `Event.grain` during expansion in parse.rs so every emitted event
  carries exactly one grain, and drop archive-only lines before emission.
- **Verify:** `cargo test normalize::grain` passes unit tests asserting: a user text event and a
  tool_use event are `indexed`; a Read tool_result is `skeleton` with a blanked body but preserved
  `is_error`+`tool_name`; a WebFetch tool_result is `indexed` with its body kept; a thinking event is
  `skeleton` with blanked text; and a `system`/`isMeta` line produces no event. Add an assertion that
  every emitted event's `grain` is one of the three literals.

### Task 4: Artifact and Mention extraction from tool-call inputs and answer text

- **Files:** src/normalize/extract.rs, src/normalize/mod.rs
- **Action:** In `src/normalize/extract.rs` implement mechanical extraction from tool-call INPUTS and
  answer text only, never tool_result bodies (D-06, D-10). Mentions (`entity_type` in {file, command}
  only): a `file` mention for each Read/Edit/Write `input.file_path`, and a `command` mention equal to
  argv[0] of each Bash `input.command` - the first whitespace-delimited shell token AFTER skipping any
  leading `VAR=value` environment-assignment prefixes, keeping the token's full path (do NOT strip a
  leading directory), so the entity equals argv[0] exactly as the acceptance criterion requires (e.g.
  `FOO=1 /usr/bin/make test` -> `/usr/bin/make`, not `FOO=1` and not `make`); each
  Mention carries `entity`, `entity_type`, `event_uuid` (the tool_use event), `session_id`, `project`,
  `timestamp`. Artifacts (file bodies produced in the run): from Write emit an Artifact with
  `path`=file_path, `content`=input.content; from Edit emit `path`=file_path, `content`=input.new_string;
  from a Bash command containing a heredoc (`<<'?EOF'? ... EOF` mechanically detected) emit the heredoc
  body as content; and from fenced triple-backtick code blocks in assistant `text` blocks emit the
  fenced body with `language` = the fence tag. Set `kind` (file vs snippet), `language` from the path
  extension when present, `source_event_uuid` = the producing event's uuid, `artifact_id` formatted as
  `{source_event_uuid}-{index}` where index is a zero-based counter per source event (stable across
  runs), and `request_bundle` = the uuid of the nearest preceding user-prompt event - track this by
  maintaining, during the ordered walk over events, a mutable "last user prompt" holding the uuid of
  the most recent event with role=user and kind=text, and set `request_bundle` to it (None if no user
  prompt has been seen yet). Wire `mod.rs::run` to emit mention and artifact records after the
  events. Package/symbol/prose mentions are out of scope this phase per D-10.
- **Verify:** `cargo test normalize::extract` passes unit tests asserting: a Write tool_use yields a
  file Mention with the exact file_path and an Artifact whose content equals input.content; a
  `Bash{command:"cargo test foo"}` yields a command Mention with entity `cargo`, and
  `Bash{command:"FOO=1 /usr/bin/make test"}` yields entity `/usr/bin/make` (env prefix skipped, path
  kept); a Bash heredoc and a fenced code block in assistant text each yield an Artifact with the body
  content; and no Mention or Artifact is produced from any tool_result body.

### Task 5: Fixed-pattern secret scrub over indexed text only

- **Files:** src/normalize/scrub.rs, src/normalize/model.rs
- **Action:** In `src/normalize/scrub.rs` implement `scrub(text: &str) -> String` applying a fixed
  pattern set (D-08): common token shapes (e.g. `sk-`/`ghp_`/`AKIA` prefixed keys and long
  base64/hex bearer tokens), PEM private-key blocks (`-----BEGIN ... PRIVATE KEY-----` ... `END`),
  database/connection strings with embedded credentials (`scheme://user:pass@host`), `Authorization:`
  header values, and `KEY=VALUE` style config-file secret assignments, each replaced with the fixed
  redaction marker `[REDACTED]`. Apply the scrub in the NDJSON emit path (model.rs / mod.rs)
  to the free-text indexed fields only - Event `text` on indexed events and Artifact `content` - and
  NOT to `Mention.entity` (file paths and command names are structural identifiers that acceptance
  criterion 1 requires to equal the tool-call inputs byte-for-byte; scrubbing a path/command that
  happened to match a pattern would break that equality, and argv[0]/file_path do not carry secrets),
  NOT to skeleton bodies (already blanked), and NOT to any file
  under the archive (the archive stays verbatim; this command never writes the archive). Add the
  `regex` crate to Cargo.toml for the patterns. Entropy-based detection is deferred to a later
  hardening pass per D-08.
- **Verify:** `cargo test normalize::scrub` passes unit tests asserting each pattern class (a token,
  a PEM block, a `user:pass@host` connection string, an `Authorization` header, a `KEY=secret`
  assignment) is replaced by the redaction marker and that ordinary prose is left unchanged; and a
  test that an Event whose source text contains a seeded `sk-` token emits with the token replaced.

### Task 6: Both-format fixtures + integration test covering the acceptance criteria

- **Files:** tests/normalize.rs, tests/fixtures/normalize/
- **Action:** Create two hand-authored transcript fixtures under `tests/fixtures/normalize/`. Fixture
  A is the live nested-split format: a parent `<session>.jsonl` with user/assistant turns (including at
  least one user line whose `message.content` is a bare string, not a block array, to exercise the
  real dominant user-prompt shape), a Read, a
  Bash, a Write (with content), and an Agent tool_use whose `input.subagent_type` names an agent, plus
  a nested `subagents/agent-<id>.jsonl` whose lines carry `isSidechain:true`, the parent `sessionId`,
  and that `agentId` (D-04). Fixture B is the hand-authored inline-subagent format reconstructed from
  ADR 0003: subagent turns inlined in a single file with `isSidechain:true`/`agentId` set and the
  spawning Agent tool_use in the same file (D-09; note in a header comment that this approximates the
  historical shape, which has no live sample). Seed a known secret (e.g. `sk-SEEDEDSECRET0123456789`)
  into one assistant text block and a distinctive tool-result body string (e.g.
  `SKELETON_BODY_MARKER`) into one Read tool_result in each fixture. In `tests/normalize.rs` write an
  integration test that, for each fixture, uses `archive::write_generation` (or a small helper that
  writes the fixture bytes as `0000.zst`) to build an archived session dir, runs the normalize path
  capturing stdout, and asserts the five acceptance criteria: (1) the emitted `mention`/`artifact`
  records' file paths, commands, and artifact contents equal the fixture's tool-call inputs; (2) the
  run exits 0 and a positive number of `is_sidechain=true` events appear, every one carrying the parent
  `session_id` (assert count > 0 so the check is not vacuously true when subagents are dropped); (3) every event line
  has exactly one `grain` and `SKELETON_BODY_MARKER` does not appear anywhere in stdout; (4)
  `sk-SEEDEDSECRET0123456789` does not appear in stdout; (5) every stdout line parses as JSON with a
  `type` in {session, event, artifact, mention}.
- **Verify:** `cargo test --test normalize` passes; then, as an executor-run manual confirmation of
  the acceptance criteria with the real tools, build one fixture's `.zst` and run
  `hindsight normalize <dir> | jq -c 'select(.type=="mention")'` and confirm the file/command entities
  match the fixture, `hindsight normalize <dir> | grep -c SKELETON_BODY_MARKER` prints 0,
  `hindsight normalize <dir> | grep -c sk-SEEDEDSECRET0123456789` prints 0, and
  `zstd -dc <dir>/0000.zst | grep -c sk-SEEDEDSECRET0123456789` prints 1.

## Notes

Plan-shape deviation: the phase-2 CONTEXT `Plan shape` line directs "Big - multiple plans, same
phase" as ordered slices. File-independence analysis contradicts it - every slice touches the shared
`src/normalize/*` module tree and the slices are strictly ordered (parser before grain, grain and
scrub before the emit path, all before the fixture integration test), so they fail the split test
(non-overlapping `files`, independently verifiable) and are correctly one plan. Per the dispatch
instruction, file independence wins over the directive; recorded here and in the return marker.

Flagged assumptions carried forward from CONTEXT: multi-generation reconciliation is union + dedupe by
line `uuid` (Task 2); `end_reason` is left null this phase (Task 1); the inline fixture approximates
the historical shape from ADR 0003 (Task 6); `agent_type` is best-effort from the spawning Agent call
and may be null when the linkage is ambiguous (Task 2).

Review adjudication (plan trigger, adjudicated gate): the plan-checker passed with 8 clarity WARNINGs,
all folded in (field types, unmatched-tool_use_id -> None, agent_type ambiguity rule, artifact_id
format, request_bundle tracking, fixed `[REDACTED]` marker). The cross-model + subagent review then
surfaced grounded defects, applied here: `message.content` can be a bare string (not always a block
list) and must produce a text Event, or ~1-in-8 real user prompts are silently dropped (Task 2, Context
field-shapes, Task 6 fixture); `Event` needed an `is_error` field for Task 3's skeleton rule (Task 1);
event emission is now a positive {user, assistant} message-type whitelist rather than a leaky noise
blacklist (Task 2/3); `session_id` maps from the transcript `sessionId` field per D-06, not the dir name
(Task 1); command mentions equal argv[0] with the path kept and env-assignment prefixes skipped, per
acceptance criterion 1 (Task 4); the secret scrub no longer touches `Mention.entity` so path/command
equality holds (Task 5); `Cargo.toml` added to file scope for the `regex` dep; the integration test now
asserts a positive sidechain-event count so parent-filing is not vacuously true (Task 6). Gemini
(`gemini-3.1-pro-preview`) was unavailable (HTTP 503) and dropped from the reviewer set; openai
(`gpt-5.3-codex`) and the `cad-reviewer` subagent both ran.
