# Status

**Phases 1 through 6 are built and merged to main; Phase 7 (backfill and cutover) is in flight.** The
architecture is settled end to end and recorded in the [decision records](decisions), narrated in
[DESIGN.md](DESIGN.md), and drawn in [diagrams.md](diagrams.md). What has changed since the design
conversation is that the whole pipeline is code now, from capturing a transcript to recalling it inside a
live session.

Phase 1 (Capture) shipped the socket-activated daemon, the poke path, the systemd units, the watermark
and full-tree sweep, the verbatim zstd archive writer, and the synchronous PreCompact hook. Phase 2
(Normalize) shipped `hindsight normalize`: it reads the archived `.zst` generations and emits tagged
NDJSON Session / Event / Artifact / Mention records with the three-tier grain and a fixed-pattern secret
scrub over indexed text only. Phase 3 (Store) shipped `hindsight load` and the SQLite index: the
relational schema, the loader that drains normalized NDJSON into it, and the FTS5 BM25 term index over
indexed events and artifacts. Phase 4 (Fuzzy) shipped `hindsight embed`: mechanical profile assembly
from the loaded records (entity profiles, artifact wrappers, and prose chunks, carrying no secrets and no
full-code bodies), a `ureq` Ollama client that embeds each profile as a 4096-dim vector, and the
two-stage sqlite-vec table backed by a resumable `embed_ledger` drain.

Phase 5 (Embed delivery) reworked how embedding is triggered and where it runs. The Phase-4 delivery
mechanics, a systemd timer firing the drain on a schedule, `nvidia-smi` GPU-busy polling, and a CPU
fallback, were all removed. Embedding is now triggered by a session hook firing `hindsight embed
--detach`, runs unconditionally on the GPU with no CPU path, self-detaches with `setsid` so it outlives
the hook, takes a single-flight `flock`, continues past a per-unit failure, and reports drain state
through an `embed_run` record and `hindsight embed --status`. This is [ADR 0013](decisions/0013-embed-delivery-hook-gpu.md),
which supersedes the delivery specifics of [ADR 0004](decisions/0004-embedder-and-gpu-scheduling.md). The
`gpu.rs` placement code and the timer and service units are gone from the tree.

Phase 6 (Query and surfaces) shipped the two-path query core and both surfaces. Exact listing is
recall-complete and unranked; ranked search fuses keyword (BM25) and semantic (sqlite-vec) results with
RRF, narrowed first by structural pre-filters (project, time window). It surfaces as an MCP recall server
over stdio with three tools, `exact_listing`, `ranked_search`, and `resolve` (which returns verbatim
archive bytes), and as `hindsight search` on the CLI for no-model ground-truth lookups. `hindsight mcp`
serves the recall server.

Phase 7 (Backfill and cutover) is on `feat/phase-7-backfill`. The backfill ran: 715 sessions archived,
fully indexed, and embedded (63k-plus vectors) under the incremental pipeline. The pipeline itself,
`hindsight ingest`, is the phase's core, a single command that sweeps, session-scoped-replaces any
new-or-changed session into the index against an `ingest_ledger` fingerprint, and fires an embed drain
only when something changed. The session hooks now run `hindsight ingest`, and the prior background
memory tool is masked. See [ADR 0014](decisions/0014-incremental-ingest-and-cutover.md). The docs and
planning records are being reconciled to this state; the human-verify that the live hooks fire on a real
session start and end is the one open acceptance item.

## Decided

All fourteen ADRs are accepted. In short: socket-activated capture daemon; verbatim archive plus a
rebuildable SQLite index under a configurable data-volume subdirectory; mechanical normalize into
Session / Event / Artifact / Mention with a three-tier grain; secrets scrubbed from the index only;
qwen3-embedding:8b via Ollama, hook-triggered and always on the GPU, sent to `/api/embed` in batches;
SQLite with FTS5 and sqlite-vec as the one store; a two-path query interface (exact listing plus
RRF-fused ranked search) over an MCP server and a CLI; backfill and steady-state indexing both carried by
`hindsight ingest`; a synchronous PreCompact snapshot and a 15-minute idle daemon; and the whole system
built in Rust as one static binary with daemon, CLI, and MCP subcommands.

Three ADRs were amended on 2026-07-21 after a stress test on the real corpus and an adversarial review:
the store stays SQLite, but the vector approach is binary-coarse plus full-precision rescore for
unfiltered queries and filter-then-exact-rerank for anchored ones, because the real indexed count is
around 63,000 vectors and growing, not the twenty thousand first assumed, and a single-threaded float
scan breaks past 65,000. See [ADR 0006](decisions/0006-storage-engine-sqlite.md) for the measurements,
and [ADR 0001](decisions/0001-storage-location-and-archive-split.md) and
[ADR 0003](decisions/0003-normalize-event-grain.md) for the archive-integrity and grain amendments from
the same pass. [ADR 0013](decisions/0013-embed-delivery-hook-gpu.md) later reversed the Phase-4 embed
delivery, and [ADR 0014](decisions/0014-incremental-ingest-and-cutover.md) settled the incremental
ingest and cutover.

## Open, not yet decided

Nothing structural is open. Two carried follow-ups sit in [.planning/CAPTURE.md](../.planning/CAPTURE.md):
capping or chunking profile text to the embedder's 4096-token context, since the longest artifact and
event units are truncated today; and the live-hook human-verify for Phase 7.

## Build order

Bottom-up, each step independently testable against the archive. Done through Phase 6; Phase 7 in flight.

1. **(done)** Repo scaffold: the Cargo project and the one static binary carrying the daemon, CLI, and
   MCP server as subcommands ([ADR 0012](decisions/0012-implementation-language-rust.md)).
2. **(done, Phase 1)** Capture: the daemon, the systemd socket and service units, the session hooks, the
   watermark, the verbatim archive writer.
3. **(done, Phase 2)** Normalize: the JSON parser, the four record types, the three-tier grain, the
   secrets scrub.
4. **(done, Phase 3)** Store: the SQLite schema, the loader, FTS5 wiring, sqlite-vec setup.
5. **(done, Phase 4)** Fuzzy: profile construction, Ollama embedding, vectors into sqlite-vec.
6. **(done, Phase 5)** Embed delivery: hook-triggered, always-GPU, detached, batched drain; timer and
   CPU path removed ([ADR 0013](decisions/0013-embed-delivery-hook-gpu.md)).
7. **(done, Phase 6)** Query and surfaces: the two-path core, RRF fusion, archive resolution, the MCP
   server, and the CLI over it.
8. **(in flight, Phase 7)** Backfill and cutover: the retention window is already raised (see below),
   then `hindsight ingest` archives, indexes, and embeds the corpus; wire the hooks to `hindsight
   ingest`, mask the prior memory tool ([ADR 0014](decisions/0014-incremental-ingest-and-cutover.md)).

## Already done ahead of build

- **Retention window raised.** Claude Code's transcript cleanup, `cleanupPeriodDays` in the user
  settings, is set to 36500 days, up from the default 30, the [ADR 0010](decisions/0010-backfill-coldstart-sweep.md)
  precondition pulled forward so nothing ages out before the archive became ground truth. With the
  backfill run and the archive durable, the window can be dialed back down at leisure; that is
  operational, not a repo deliverable.

## Note for a fresh session

The design conversation happened in a different working directory, so the private working notes from it
do not auto-load here. This repo is the authoritative record. Read [CLAUDE.md](../CLAUDE.md) first for the
standing rules, then this file, then the ADRs.
