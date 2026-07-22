# Phase 7: Backfill and cutover - Context

Gathered: 2026-07-22
Feeds: /cad-plan 7

## Scope boundary

In: The one-time first fill and the switch-over to Hindsight. A single `hindsight backfill` operation
that takes all existing history to fully usable in three waves - archive every session, build the whole
keyword/exact index in one fast pass, then drain embeddings in the background newest-first. Plus the
cutover: wiring the live SessionStart/SessionEnd hooks (capture poke + embed trigger) into Claude Code
settings with a documented install step, and retiring the prior LLM-observer memory tool (and the
leftover legacy embed timer). Serves MIG-01, MIG-02.
Out: Any new query features (Phase 6 delivered them). Reworking the embed/GPU internals beyond what
cutover needs. Migrating or deleting the old tool's stored observations (ADR 0009 leaves them in place).
Dialing `cleanupPeriodDays` back down after backfill (operational, not a repo deliverable).
Deferred: None.
Plan shape: One plan (user call, 2026-07-22). The two pieces - backfill command and cutover - are
right-sized for a single plan rather than split.

## Decisions

- D-01 (Backfill is one command, three waves): `hindsight backfill` runs the whole first-time fill in
  order - (1) archive every existing session via the empty-watermark sweep, (2) build the keyword/exact
  index for the entire corpus in one fast normalize+load pass so all-history keyword/exact search is
  live within minutes, (3) launch the embedding drain (`hindsight embed --detach`) in the background so
  semantic recall fills in over hours. One operation to fully usable, not three manual steps. Evidence:
  docs/decisions/0010-backfill-coldstart-sweep.md (backfill = empty-watermark sweep; mechanical phase
  then embedding phase), docs/decisions/0013-embed-delivery-hook-gpu.md (`hindsight load` wipes the
  vector table + ledger so the next `embed` is a full-corpus drain; cold start = raise retention, load,
  embed with hooks off); analyzer: the sweep only archives today, normalize is not wired into the
  pipeline (src/normalize/mod.rs docstring), and no backfill command exists (src/main.rs Command enum).

- D-02 (Newest-first where it is observable): the archive step and the embedding drain both process the
  newest sessions first, so recent work becomes searchable and semantically recallable before old work.
  The keyword index is one whole-corpus pass - fast enough that per-session ordering there buys nothing.
  Evidence: docs/decisions/0010-backfill-coldstart-sweep.md ("both run newest-first, because recent work
  is the most likely to be recalled"), ROADMAP Phase 7 criterion 1; analyzer: src/sweep.rs currently
  sorts lexically (`files.sort()`), so newest-first ordering must be added.

- D-03 (Interruptible without redoing work): stopping backfill partway and re-running does not duplicate
  - the archive step skips already-archived sessions (watermark advanced per file + sha256 content
  dedup), and the embedding drain resumes from its `embed_ledger`. The keyword-index pass is
  whole-corpus and simply re-runs if interrupted (cheap, minutes). Evidence:
  docs/decisions/0010-backfill-coldstart-sweep.md (idempotent/resumable/replayable inherited from the
  watermark), docs/decisions/0013-embed-delivery-hook-gpu.md (`embed_ledger` resumable drain,
  single-flight flock); analyzer: src/sweep.rs saves the watermark after each file, archive writer
  dedups on sha256, sweep tests `resume_after_crash_before_watermark_save_writes_no_duplicate` and
  `second_sweep_over_unchanged_tree_archives_nothing`.

- D-04 (Live hooks wire both capture and embedding): cutover adds SessionStart and SessionEnd entries to
  Claude Code's user settings so each session start/end pokes the capture daemon (`hindsight poke`) and
  triggers an embed drain (`hindsight embed --detach`); PreCompact is already wired from Phase 1. These
  live in external Claude Code settings, documented in the repo with an install step; John runs the
  actual registration (same pattern as the MCP setup). Evidence:
  docs/decisions/0011-hooks-and-daemon-knobs.md (poke-only session hooks, PreCompact already the only
  hook wired), docs/decisions/0013-embed-delivery-hook-gpu.md (a session hook fires `embed --detach`,
  detached so it does not block the hook or ride the daemon), README.md (SessionStart/SessionEnd pokes
  noted as Phase 6/7 work).

- D-05 (Retire the old tool, keep its notes): after backfill drains, turn off the prior LLM-observer
  memory tool so it stops observing sessions and spending tokens per tool call; its existing
  observations stay in place - not migrated, not deleted. Also disable the leftover legacy embed timer
  if still installed so it does not fire drains alongside the hooks. The repo documents the disable
  step; John executes it on his machine. Evidence:
  docs/decisions/0009-replace-prior-memory-tool.md (replace it, leave its observations, the observer
  gets turned off once Hindsight is capturing), docs/decisions/0013-embed-delivery-hook-gpu.md (disable
  the legacy `hindsight-embed.timer` before hooks take over).

- D-06 (Cutover order, hooks off during the long fill): the one-time sequence is retention already
  raised -> run `hindsight backfill` (archive + index live in minutes, embeddings drain over hours) ->
  once the drain finishes, turn the hooks ON -> disable the old tool + legacy timer. This order means
  nothing races and both memory systems are never running at once. Evidence:
  docs/decisions/0010-backfill-coldstart-sweep.md (raise the retention window before the first run),
  docs/decisions/0013-embed-delivery-hook-gpu.md (backfill is a full drain with hooks off, then wire the
  hooks); docs/STATUS.md (retention already raised to `cleanupPeriodDays: 36500`).

## Acceptance criteria

- [ ] Running `hindsight backfill` over existing history with an empty watermark archives every existing
      session, and a second run archives nothing new (no duplicate generations).
- [ ] After the backfill index pass finishes, a keyword search and an exact listing return rows drawn
      from historical sessions, while the embedding drain is still running (semantic recall still filling
      in).
- [ ] Interrupting `hindsight backfill` partway and re-running it re-archives no already-archived session
      and does not double-embed (the ledger resumes).
- [ ] The archive step and the embedding drain both process sessions newest-first (a newer session is
      archived / embedded before an older one).
- [ ] With the hooks wired, starting and ending a Claude Code session archives the new session and
      triggers an embed drain. (human-verify: needs a live Claude Code session)
- [ ] After cutover, the prior memory tool's observer no longer runs (no new observations, no
      per-tool-call token spend) and the legacy embed timer is disabled. (human-verify: needs the live
      machine / operator config)

## Flagged assumptions

- The exact disable mechanism and command for the prior memory tool are John's to supply (his install),
  not discoverable from the repo (ADR 0009 never names the tool or records how it is installed); the
  planner documents a disable step and John fills the specific command. If wrong: the plan documents a
  disable against the wrong surface.
- The Claude Code SessionStart/SessionEnd hook `settings.json` block shape and any stdin payload are a
  Claude Code contract the planner resolves at plan time; only the PreCompact block shape is documented
  in README today. If wrong: the authored hook wiring does not fire correctly.
- The backfill command must supply new orchestration connecting archive -> normalize -> load, since the
  sweep only archives today and normalize is not wired into the pipeline; the planner owns that
  implementation. If wrong: running the daemon produces an archive with no index and criterion 2 is not
  met.
- Newest-first ordering must be added to the archive step (the sweep sorts lexically today); the planner
  confirms and wires mtime-descending ordering. If wrong: criterion 4 fails verification.
