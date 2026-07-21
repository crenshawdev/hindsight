# Status

**Phase: design complete, implementation not started.** This repo currently holds documentation only.
There is no code yet. The architecture is settled end to end and recorded in the
[decision records](decisions), narrated in [DESIGN.md](DESIGN.md), and drawn in
[diagrams.md](diagrams.md).

## Decided

All twelve ADRs are accepted. In short: socket-activated capture daemon; verbatim archive plus a
rebuildable SQLite index under a configurable data-volume subdirectory; mechanical normalize into
Session / Event / Artifact / Mention with a three-tier grain; secrets scrubbed from the index only;
qwen3-embedding:8b via Ollama on an opportunistic GPU schedule; SQLite with FTS5 and sqlite-vec as the
one store; a two-path query interface (exact listing plus RRF-fused ranked search) over an MCP server
and a CLI; backfill as an empty-watermark sweep; a synchronous PreCompact snapshot and a 15-minute idle
daemon; and the whole system built in Rust as one static binary with daemon, CLI, and MCP subcommands.

## Open, not yet decided

These are the calls that were deliberately left for build time.

- **The concrete base directory name** under the data volume. The rule is "a configurable subdirectory,
  never the volume root," but the actual path is unset.
- **The secret-scrub ruleset.** The decision is to scrub the index; the specific patterns and
  entropy thresholds are not written.
- **The MCP tool surface.** Which named tools the server exposes and their argument shapes.

## Build order

Roughly bottom-up, each step independently testable against the archive.

1. Repo scaffold: the Cargo project and the one static binary that carries the daemon, CLI, and MCP
   server as subcommands ([ADR 0012](decisions/0012-implementation-language-rust.md)).
2. Capture: the daemon, the systemd socket and service units, the one-line session hooks, the
   watermark, the verbatim archive writer (generational, compressed).
3. Normalize: the JSON parser (both historical transcript formats), the four record types, the
   three-tier grain, the secrets scrub.
4. Store: the SQLite schema, FTS5 wiring, sqlite-vec setup.
5. Fuzzy: profile construction, Ollama embedding with the GPU-deferral scheduling, vectors into
   sqlite-vec.
6. Query: the two-path core, RRF fusion, archive resolution, then the MCP server and the CLI over it.
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
