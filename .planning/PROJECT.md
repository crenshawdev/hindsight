# Hindsight

## What This Is

Hindsight is a local, cross-session, cross-project memory for Claude Code. It captures session
transcripts before Claude Code's cleanup sweep deletes them, keeps a verbatim archive as the ground
truth, and builds a rebuildable searchable index (exact plus fuzzy) on top so past work is recallable.
It is for a developer, its author first, who wants durable recall across Claude Code sessions without
trusting a transcript directory that ages out in thirty days.

## Core Value

Past Claude Code work stays findable and retrievable, verbatim, long after the cleanup sweep would have
deleted the transcript.

## Requirements

### Validated

(None yet - ship to validate)

### Active

Hypotheses until shipped.

- [ ] Capture every session's transcript reliably before cleanup, no matter how the session ended
- [ ] Snapshot a transcript before Claude Code compacts it in place
- [ ] Keep a verbatim, compressed, write-once archive as the ground truth
- [ ] Normalize transcripts into Session / Event / Artifact / Mention records
- [ ] Apply a three-tier grain (indexed / skeleton / archive-only) to control index signal
- [ ] Scrub secrets from the index while leaving them verbatim in the archive
- [ ] Store records, FTS5 keyword index, and vectors in one SQLite file
- [ ] Embed synthetic profiles via Ollama on a GPU-opportunistic schedule with CPU fallback
- [ ] Answer exact, recall-complete listing queries
- [ ] Answer ranked fuzzy queries via RRF fusion with structural pre-filters
- [ ] Expose recall through an MCP server and operate plus ground-truth search through a CLI
- [ ] Backfill existing history as an empty-watermark sweep, newest-first
- [ ] Cut over from the prior background memory tool

### Out of Scope

- LLM knowledge-graph extraction (cognee-style) - a transcript is a structured log, so entity
  extraction is a parse and not an inference, and the per-chunk model cost is not worth paying
  ([ADR 0003](../docs/decisions/0003-normalize-event-grain.md))
- Absorbing or migrating the prior memory tool's database - it is replaced, its old observations left
  in place ([ADR 0009](../docs/decisions/0009-replace-prior-memory-tool.md))
- A client/server datastore - SQLite is one file with no server to run
  ([ADR 0006](../docs/decisions/0006-storage-engine-sqlite.md))
- Backing up the index - it is rebuildable from the archive by construction, so backing it up is wasted
  effort ([ADR 0001](../docs/decisions/0001-storage-location-and-archive-split.md))
- Disabling Claude Code cleanup outright - not possible, so the retention window is raised instead

## Context

Design is complete, implementation has not started. The full record lives in
[docs/DESIGN.md](../docs/DESIGN.md) (the narrative), twelve accepted ADRs in
[docs/decisions](../docs/decisions), [docs/diagrams.md](../docs/diagrams.md) (the picture), and
[docs/STATUS.md](../docs/STATUS.md) (decided list, remaining open items, build order). This `.planning/`
tree is the execution layer over that design, not a replacement for it.

- Public repo, build-in-the-open. Personal incidentals are generalized in public docs; the technical
  stack stays explicit for reproducibility.
- The data volume is backed up (nightly full plus frequent incrementals), so archive durability is
  already handled and is not re-solved here.
- The retention window is already raised (`cleanupPeriodDays` set to 36500) to stop loss before the
  capture daemon exists. This is a stopgap until the archive is the durability layer.
- Environment: Linux with systemd, Ollama available, an opportunistically-scheduled GPU.

## Constraints

- **Tech stack**: Rust, one static binary with daemon / CLI / MCP subcommands - socket-activation wants
  a small always-on executable, and rusqlite carries SQLite, FTS5, and sqlite-vec as one linked
  dependency ([ADR 0012](../docs/decisions/0012-implementation-language-rust.md))
- **Storage**: verbatim archive plus a rebuildable SQLite index, both under a configurable data-volume
  subdirectory and never the volume root ([ADR 0001](../docs/decisions/0001-storage-location-and-archive-split.md))
- **Capture**: systemd socket-activated daemon, full-tree sweep against a watermark, 15-minute idle
  self-terminate, plus a synchronous PreCompact snapshot
  ([ADR 0002](../docs/decisions/0002-capture-daemon-socket-activation.md),
  [ADR 0011](../docs/decisions/0011-hooks-and-daemon-knobs.md))
- **Embeddings**: qwen3-embedding:8b (Q4_K_M) via Ollama, GPU-opportunistic with CPU fallback, embed
  synthetic profiles rather than raw names or code
  ([ADR 0004](../docs/decisions/0004-embedder-and-gpu-scheduling.md),
  [ADR 0005](../docs/decisions/0005-profile-construction-mechanical.md))
- **Security**: deny-by-default. Secrets scrubbed from the index, left verbatim in the archive
  ([ADR 0008](../docs/decisions/0008-secrets-scrub-index-only.md))
- **Git**: branch plus MR only, a hook hard-blocks direct pushes to main, signed commits required on
  main
- **Privacy**: public repo, so personal incidentals are generalized in public docs

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Verbatim archive is ground truth, index is rebuildable from it | Protect a small slow-growing pile, let the large complicated part regenerate ([ADR 0001](../docs/decisions/0001-storage-location-and-archive-split.md)) | - Pending |
| Capture via socket-activated daemon sweeping vs a watermark, hooks only poke | A sweep catches sessions no matter how they died, unlike a hook with no delivery guarantee ([ADR 0002](../docs/decisions/0002-capture-daemon-socket-activation.md)) | - Pending |
| Normalize is mechanical parse into four record types with three-tier grain | The transcript is a structured log, so structure is extracted for free and more accurately than a model would ([ADR 0003](../docs/decisions/0003-normalize-event-grain.md)) | - Pending |
| One SQLite file with FTS5 and sqlite-vec | One store, no server, keyword and vector recall in the same place ([ADR 0006](../docs/decisions/0006-storage-engine-sqlite.md)) | - Pending |
| Embed synthetic profiles, not raw names or code, via Ollama qwen3 | Better semantic recall and no raw sensitive text in the vector path ([ADR 0004](../docs/decisions/0004-embedder-and-gpu-scheduling.md), [ADR 0005](../docs/decisions/0005-profile-construction-mechanical.md)) | - Pending |
| Implementation language is Rust, one binary with subcommands | Socket-activation deploy shape, rusqlite stack, total parser over untrusted JSON ([ADR 0012](../docs/decisions/0012-implementation-language-rust.md)) | - Pending |

---
*Last updated: 2026-07-20 after project initialization*
