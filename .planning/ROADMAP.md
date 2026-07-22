# Roadmap: Hindsight

## Overview

The journey is from nothing to durable recall, built bottom-up because the architecture is bottom-up.
The archive cannot be built after the fact, every day without capture running loses thirty-day-old
sessions for good, so capture comes first. Everything else is derived from the archive and is
rebuildable, so each later phase is independently testable against it. The order is dependency-driven
rather than a set of vertical user-facing slices, which is honest to a design where the ground truth
has to exist before anything can be indexed or asked. Scaffolding (the Cargo project and the
one-binary-with-subcommands skeleton) folds into Phase 1. The one unproven dependency, sqlite-vec
linking against the bundled SQLite, is validated at the start of Phase 3 before the store is built on
it.

## Phases

- [x] **Phase 1: Capture** - socket-activated daemon that archives every session verbatim before cleanup
- [x] **Phase 2: Normalize** - parse archived transcripts into the four record types with grain and scrubbing
- [x] **Phase 3: Store** - persist records into one SQLite file with FTS5 and sqlite-vec
- [ ] **Phase 4: Fuzzy** - synthetic profiles embedded via Ollama into sqlite-vec
- [ ] **Phase 5: Embed delivery** - hook-triggered, always-GPU embedding, single-flighted and resumable
- [ ] **Phase 6: Query and surfaces** - two-path recall over an MCP server and a CLI
- [ ] **Phase 7: Backfill and cutover** - ingest existing history and go live

## Phase Details

### Phase 1: Capture
**Goal:** A systemd socket-activated daemon that archives every session transcript verbatim before the
cleanup sweep can take it. Includes the repo scaffold and the one-binary skeleton.
**Depends on:** Nothing (first phase)
**Requirements:** CAP-01, CAP-02, CAP-03, CAP-04, ARC-01, ARC-02
**Success Criteria:**
1. Poking the systemd socket starts the daemon, and with no further pokes it exits on its own after 15
   minutes (visible in the journal).
2. A sweep copies every new-or-changed transcript in the tree into the archive, including a session
   killed with no SessionEnd hook firing.
3. An immediate second sweep archives nothing new, and interrupting a sweep then re-running it resumes
   without duplicating.
4. Archived files decompress byte-identical to the source transcript, and they live under the
   configured subdirectory rather than the volume root.
5. Triggering a compaction fires the PreCompact snapshot and the pre-compaction transcript is in the
   archive.

### Phase 2: Normalize
**Goal:** Turn an archived transcript into Session / Event / Artifact / Mention records with the
three-tier grain and secret scrubbing, emitted to an inspectable form (no store yet).
**Depends on:** Phase 1
**Requirements:** NRM-01, NRM-02, NRM-03, NRM-04
**Success Criteria:**
1. Parsing an archived session emits records whose files, commands, and artifacts match the
   transcript's actual tool calls.
2. Both historical transcript formats parse in one run with no errors.
3. Every event carries exactly one grain, and skeleton and archive-only bodies are absent from the
   indexed output.
4. A known secret seeded into a transcript is absent from the normalized index output and present in
   the archived copy.

### Phase 3: Store
**Goal:** Persist normalized records into a single SQLite database with FTS5 keyword search and
sqlite-vec, after proving sqlite-vec links against the bundled SQLite build.
**Depends on:** Phase 2
**Requirements:** STO-01, STO-02, STO-03
**Success Criteria:**
1. sqlite-vec loads against the bundled SQLite and a vector round-trips (insert then nearest-neighbor)
   in the same database file.
2. Normalized records load into the schema and row counts match the emitted records.
3. An FTS5 BM25 query over indexed content returns the expected sessions.

### Phase 4: Fuzzy
**Goal:** Build synthetic profiles from records and embed them via Ollama into sqlite-vec, on a
GPU-opportunistic schedule.
**Depends on:** Phase 3
**Requirements:** EMB-01, EMB-02
**Success Criteria:**
1. Profiles are constructed mechanically from records and contain no raw secrets or full-code payloads.
2. Embedding runs against Ollama qwen3-embedding and stores its vectors in sqlite-vec.
3. Embedding lands its vectors on the GPU, and an interrupted run resumes against the ledger without
   re-embedding units that already landed.

### Phase 5: Embed delivery
**Goal:** Replace the timer trigger and GPU-opportunistic scheduling with embedding triggered by the
session-lifecycle hooks, running unconditionally on the GPU, single-flighted and resumable, with a
one-time backfill then incremental cutover.
**Depends on:** Phase 4
**Requirements:** EMB-01, EMB-02
**Success Criteria:**
1. A session hook triggers a detached embed drain that returns without blocking the session, and no
   timer remains.
2. Embedding runs unconditionally on the GPU with no CPU-fallback path present in the code.
3. Concurrent triggers never double-embed a unit, and a session landing mid-drain is embedded without
   lag.
4. An interrupted drain resumes from the ledger and re-embeds only units that did not land, and one
   Ollama error does not abort the run.
5. `hindsight embed --status` reports drain progress and health, distinguishing done, running,
   stalled, and failed.

### Phase 6: Query and surfaces
**Goal:** The two-path query core (exact listing and RRF-fused ranked search) exposed through an MCP
server for recall and a CLI for operating and ground-truth search.
**Depends on:** Phase 5
**Requirements:** QRY-01, QRY-02, QRY-03, IFC-01, IFC-02
**Success Criteria:**
1. An exact listing query (for example every session that touched a given file) returns all matches
   with no ranking omissions.
2. A fuzzy search returns RRF-fused keyword and vector results, and a structural pre-filter (project or
   time) narrows them.
3. A result resolves to its verbatim archived bytes.
4. Claude Code calls the MCP server's recall tools and gets results, and the CLI runs the same search
   and the operational commands.

### Phase 7: Backfill and cutover
**Goal:** Ingest all existing transcript history through the normal pipeline, then wire the hooks and
retire the prior memory tool.
**Depends on:** Phase 6
**Requirements:** MIG-01, MIG-02
**Success Criteria:**
1. A first run with an empty watermark backfills existing history newest-first, and interrupting then
   resuming does not re-ingest archived sessions.
2. Exact and keyword recall over historical sessions is live before the embedding phase finishes
   draining.
3. After cutover the session hooks poke the daemon and the prior background memory tool is disabled.
