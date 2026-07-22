# Status

**Phase: capture, normalize, store, and fuzzy built, query next.** Phases 1 through 4 are implemented;
1 through 3 are merged to main and 4 is on its feature branch. The architecture is still settled end to
end and recorded in the [decision records](decisions), narrated in [DESIGN.md](DESIGN.md), and drawn in
[diagrams.md](diagrams.md); what changed is that the first four phases are code now, not just a plan.

Phase 1 (Capture) shipped the socket-activated daemon, the poke path, the systemd units, the watermark
and full-tree sweep, the verbatim zstd archive writer, and the synchronous PreCompact hook. Phase 2
(Normalize) shipped the `hindsight normalize` subcommand: it reads the archived `.zst` generations and
emits tagged NDJSON Session / Event / Artifact / Mention records with the three-tier grain and a
fixed-pattern secret scrub over indexed text only. Phase 3 (Store) shipped the `hindsight load`
subcommand and the SQLite index: the relational schema, the loader that drains normalized NDJSON into
it, and the FTS5 BM25 term index over indexed events and artifacts. Phase 4 (Fuzzy) shipped the
`hindsight embed` subcommand: mechanical profile assembly from the loaded records (entity profiles,
artifact wrappers, and prose chunks, carrying no secrets and no full-code bodies), a `ureq` Ollama
client that embeds each profile as a 4096-dim vector, the two-stage sqlite-vec table backed by a
resumable `embed_ledger` drain, `nvidia-smi` GPU-busy detection with defer-then-CPU fallback, and a
systemd timer that runs the job off the capture daemon. Phases 1 through 3 passed their UAT with every
acceptance criterion verified against the real binary; Phase 4's verification is its passing test suite
and the plan's runnable checks, with the conversational UAT still to come.

## Decided

All twelve ADRs are accepted. In short: socket-activated capture daemon; verbatim archive plus a
rebuildable SQLite index under a configurable data-volume subdirectory; mechanical normalize into
Session / Event / Artifact / Mention with a three-tier grain; secrets scrubbed from the index only;
qwen3-embedding:8b via Ollama on an opportunistic GPU schedule; SQLite with FTS5 and sqlite-vec as the
one store; a two-path query interface (exact listing plus RRF-fused ranked search) over an MCP server
and a CLI; backfill as an empty-watermark sweep; a synchronous PreCompact snapshot and a 15-minute idle
daemon; and the whole system built in Rust as one static binary with daemon, CLI, and MCP subcommands.

Three of those ADRs were amended on 2026-07-21, after a stress test on the real corpus and an
adversarial review of the design. The store stays SQLite, but the vector approach is now
binary-coarse plus full-precision rescore for unfiltered queries and filter-then-exact-rerank for
anchored ones, not a plain exact scan, because the real indexed count is about 55,000 vectors and
growing, not the twenty thousand first assumed, and a single-threaded float scan breaks past 65,000.
See [ADR 0006](decisions/0006-storage-engine-sqlite.md) for the measurements, and
[ADR 0001](decisions/0001-storage-location-and-archive-split.md) and
[ADR 0003](decisions/0003-normalize-event-grain.md) for the archive-integrity and grain amendments
from the same pass.

## Open, not yet decided

- **The MCP tool surface.** Which named tools the server exposes and their argument shapes. Lands in
  Phase 5.

Two items that used to sit here are settled now. The base directory is not defaulted at all: Phase 1's
config makes `base_dir` required and rejects a filesystem root (ARC-02, src/config.rs), so there is no
guessed path left to pin. The secret-scrub pattern set is written and shipped in Phase 2
(src/normalize/scrub.rs, a fixed set of token, private-key, connection-string, auth-header, and
config-value patterns over indexed text only); entropy-based detection stays deferred per
[ADR 0008](decisions/0008-secrets-scrub-index-only.md), which is a later hardening pass, not an open
question.

## Build order

Roughly bottom-up, each step independently testable against the archive. Done through step 5.

1. **(done)** Repo scaffold: the Cargo project and the one static binary that carries the daemon, CLI,
   and MCP server as subcommands ([ADR 0012](decisions/0012-implementation-language-rust.md)).
2. **(done, Phase 1)** Capture: the daemon, the systemd socket and service units, the one-line session
   hooks, the watermark, the verbatim archive writer (generational, compressed).
3. **(done, Phase 2)** Normalize: the JSON parser (both historical transcript formats), the four record
   types, the three-tier grain, the secrets scrub.
4. **(done, Phase 3)** Store: the SQLite schema, the loader, FTS5 wiring, sqlite-vec setup.
5. **(done, Phase 4)** Fuzzy: profile construction, Ollama embedding with the GPU-deferral scheduling,
   vectors into sqlite-vec.
6. **(next, Phase 5)** Query: the two-path core, RRF fusion, archive resolution, then the MCP server and
   the CLI over it.
7. Backfill: raise the retention window first (already done, see below), then run the empty-watermark
   sweep newest-first.
8. Cutover: wire the hooks, disable the prior background memory tool.

## Already done ahead of build

- **Retention window raised.** Claude Code's transcript cleanup, `cleanupPeriodDays` in the user
  settings, is set to 36500 days, up from the default 30. This is the [ADR 0010](decisions/0010-backfill-coldstart-sweep.md)
  precondition pulled forward, because the oldest transcripts were within days of the 30-day cleanup and
  the daemon that would archive them does not exist yet. It stops the loss now and takes effect at the
  next Claude Code start. It is a stopgap, not the durability layer, the verbatim archive becomes ground
  truth once capture is built and the window can be dialed back then.

## Note for a fresh session

The design conversation happened in a different working directory, so the private working notes from it
do not auto-load here. This repo is the authoritative record. Read [CLAUDE.md](../CLAUDE.md) first for
the standing rules, then this file, then the ADRs.
