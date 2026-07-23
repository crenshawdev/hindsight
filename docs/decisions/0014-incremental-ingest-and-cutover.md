# 0014 - Incremental ingest and cutover

Status: accepted

Builds on [ADR 0010](0010-backfill-coldstart-sweep.md) (backfill as a cold-start sweep) and
[ADR 0013](0013-embed-delivery-hook-gpu.md) (hook-triggered, always-GPU embedding). It resolves the one
thing both of those left implicit: what actually carries a new session from the archive into the
searchable index, on day one and on every session after.

## Context

The capture daemon archives. That is all it does. A poke runs a full-tree sweep that copies new or
changed transcripts into the verbatim archive and advances the watermark (`src/sweep.rs`, `src/daemon.rs`
`run_sweep`), and `sweep::run` returns a count of generations written. It never normalizes, never loads,
never touches the SQLite index. ADR 0010 framed backfill as "the daemon's first sweep with an empty
watermark, the normal pipeline runs over all of it," and that framing was true for the archive half and
quietly wrong for the index half: there was no code path wiring archive into normalize into load. The
first fill was done by hand, `hindsight normalize <session> | hindsight load` over the whole tree, and
`hindsight load` is a full wipe-and-rebuild that clears every table and empties the embed ledger, so it
is a cold-rebuild tool, not something a session-end hook can call to fold in one new session without
discarding the other seven hundred.

Phase 7 CONTEXT proposed a `hindsight backfill` command to script that first fill. It was never built,
because the same gap that backfill needed to close is the gap the live path needs to close, and one
command should do both. What was missing was not a first-fill script but an incremental ingest.

## Decision

Add `hindsight ingest`, one command that carries the archive into the index and behaves the same on the
first run over all of history as on the ten-thousandth run over a single new session.

**One pass, three steps.** Ingest runs a synchronous sweep first, so the archive is current as of this
moment, then reconciles the index against the archive one session at a time, then fires
`hindsight embed --detach` once at the end if and only if a session actually changed. The sweep is the
same `sweep::run` the daemon poke uses; ingest calls it directly rather than poking, so it knows the
archive is settled before it reads it, instead of racing a background daemon sweep.

**Fingerprint, then session-scoped replace.** Ingest enumerates the archived sessions at
`archive/<project>/<session-id>/` and computes a cheap stat-fingerprint of each session's generations,
the sha256 of the sorted `relpath|size|mtime` of every `*.zst` under the session directory, no
decompression. It compares that against a new `ingest_ledger` table keyed by `session_id`. A session
whose fingerprint matches the ledger is already current and is skipped. A new or changed session is
re-normalized and its rows are replaced in the index in one transaction: delete just this session's
rows and insert the fresh ones. Artifacts carry no `session_id` and are scoped only through their
`source_event_uuid` to `event.uuid` link, so the artifact delete runs before the event delete, while the
events it joins against still exist. Every other session and every already-landed vector is left
untouched.

**Fire embed only on change.** When at least one session was re-indexed, ingest fires
`hindsight embed --detach`, which self-detaches and takes its own single-flight lock per ADR 0013. When
nothing changed, no drain is spawned. The embed ledger already skips units it has embedded, so the drain
picks up only genuinely-new units rather than the corpus.

**Single-flight.** Ingest takes a non-blocking advisory `flock` on `ingest.lock` under `state_dir()`,
the same pattern the embed drain uses. Two overlapping Claude Code sessions cannot race the sweep
watermark or the per-session index writes: the second ingest finds the lock held and exits cleanly, its
work already covered by the one holding it.

**The ledger is ingest's own state.** `ingest_ledger` (`session_id`, `project`, `fingerprint`,
`ingested_at`) sits parallel to `embed_ledger` and is deliberately outside the loader's
`FRESH_BUILD_TABLES`, so a full `hindsight load` does not wipe it. A reload rebuilds the same rows the
fingerprints already describe, so the ledger stays valid and the next ingest correctly skips unchanged
sessions.

## Cutover

The session hooks run `hindsight ingest`, not a bare poke. Both `SessionStart` and `SessionEnd` call the
same command in the user's Claude Code `settings.json`: start sweeps up any session a missing end hook or
a crash left un-indexed, end folds in the session that just closed and fires the embed drain for its new
turns. `PreCompact` stays as it was ([ADR 0011](0011-hooks-and-daemon-knobs.md)). The old
`SessionStart`/`SessionEnd` pokes that earlier ADRs describe are superseded here: a poke only archives,
and ingest is a superset of a poke plus the index and embed steps a poke never did.

The prior background memory tool is turned off ([ADR 0009](0009-replace-prior-memory-tool.md)). On this
install it is a masked systemd user service, so nothing can start it by name; its stored observations
stay in place, unmigrated and undeleted. The legacy embed timer ADR 0013 warned about is gone from the
tree with no unit left installed.

There is no `backfill` command. The cold start is: raise the retention window (ADR 0010), then run
`hindsight ingest` with the hooks not yet wired, which archives and indexes the whole corpus and drains
the embeddings behind it, then wire the hooks and mask the old tool. On an empty ledger the first ingest
reconciles every session; on the real corpus that was 715 sessions in 44 seconds, and the next ingest,
seconds later, re-indexed only the two sessions that were still being written, in 0.17 seconds.

## Consequences

Two correctness changes fall out of session-scoped replace, because deleting and re-inserting a session's
rows reassigns SQLite's autoincrement ids.

Event embed-units were keyed by `event.id`, so a re-ingest reassigned every id and orphaned every event
unit's ledger stamp, which re-embedded a session's full event set on every hook fire. Event units are
now keyed by `{uuid}:{ordinal}`, the transcript-line uuid plus a deterministic per-uuid `ROW_NUMBER`,
stable across re-ingest and unique for the multi-block turns that share a uuid. The key is load-bearing
in the query layer too, so the identical expression is computed in three places: `src/embed/profile.rs`
(the vectors' key), `src/query/vector.rs` (the time-window candidate set), and `src/query/ranked.rs`
(fusion resolution, which splits off the uuid prefix). `PROFILE_SCHEMA_VERSION` moved to `2` so the drain
clears the stale id-keyed vectors and re-embeds once under the stable keys. Verified on the real corpus:
after one clean re-embed, re-ingesting a grown session skipped all 63,482 prior units and embedded only
its 298 new turns, not the session's full event set.

A forked or resumed session replays its parent's events verbatim, so a multi-session load can carry the
same `artifact_id` (`{event-uuid}-{n}`) with identical content. The loader inserts artifacts with
`INSERT OR IGNORE`, so the first write wins and a benign duplicate is dropped rather than aborting the
load.

Two accepted limits. A session that is re-ingested with changed content keeps its stable-keyed vectors
until the next full re-embed, since the embed ledger skips a key it has seen; that drift is reconciled by
the occasional whole-corpus re-embed, which a `PROFILE_SCHEMA_VERSION` bump forces and which is cheap
enough to re-run. And profile units can exceed the embedder's 4096-token context, so Ollama truncates the
longest ones (a 117k-char artifact, a 166k-char event were measured), which wastes transfer and leaves a
long artifact's vector reflecting only its first slice; capping or chunking profile text to the context
window is a tracked follow-up in `.planning/CAPTURE.md`, not done here.
