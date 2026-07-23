# 0011 - Hooks and daemon knobs

Status: accepted

## Context

Two loose ends from the capture design ([ADR 0002](0002-capture-daemon-socket-activation.md)) needed
settling: the one gap a sweep cannot cover, and the daemon's runtime behavior between pokes.

The gap is compaction. When context fills, Claude Code rewrites a transcript in place, replacing older
turns with a summary. A sweep that arrives after the rewrite copies a file that has already lost
content.

## Decision

**A `PreCompact` hook, done as a synchronous single-file snapshot.** When it fires, the hook copies
that one transcript's current bytes into the archive staging before returning, and the daemon
normalizes it later. Synchronous, because a plain poke could lose the race, the rewrite could land
before the daemon reads the file. Copying one file is trivially fast, and compaction fires rarely for
me because I clear a session around twenty percent context and almost never compact, so the occasional
brief block costs nothing and guarantees the pre-compaction bytes survive. PreCompact stays exactly this
across later phases; the session-start and session-end hooks did not. This ADR describes them as bare
pokes, and the cutover in [ADR 0014](0014-incremental-ingest-and-cutover.md) moved them to run
`hindsight ingest`, which sweeps, indexes the new session, and fires the embed drain, a superset of a
poke. The poke-only sweep-trigger argument below still holds: the trigger is a session event, not a
filesystem watch, whether the hook writes one byte or runs a full ingest.

The snapshot fails loud and vetoes the compaction (exit 2) only when it holds bytes it could not
persist, so the veto exists to protect a real capture. A source transcript that is not on disk when the
hook fires, a fresh session compacted before Claude Code has flushed its transcript, puts no bytes at
risk, so that case allows the compaction (exit 0) rather than blocking the user over nothing. Read the
bytes and fail to archive them, that vetoes; find no bytes to read, that does not.

**Poke-only, no filesystem watch.** Each poke triggers a full-tree sweep against the watermark, not
just the session that poked. That is the safety property, a session that crashed before any end hook
fired still left its bytes on disk, and the next session's start poke sweeps the tree and archives that
orphaned file. There is no data loss, only latency, so a filesystem watcher would buy marginal latency
for real added complexity.

**Idle timeout of fifteen minutes, daemon self-terminates.** The daemon owns the timeout rather than
systemd, and systemd respawns it on the next poke. My clear-and-restart rhythm keeps poking it inside
the window across a work block, so it stays warm while I work and shuts down once I walk away.

## Alternatives considered

**A `PreCompact` poke instead of a synchronous copy.** Rejected, it can lose the race with the
in-place rewrite. The whole point is to capture bytes that are about to be overwritten, so best-effort
is not good enough here even though it is fine for ordinary capture.

**A filesystem watcher for mid-session appends.** Rejected. It catches long-session writes a little
sooner, but the next poke's full-tree sweep already recovers anything left on disk, so the only thing
lost is latency, and the watcher adds real complexity for it.

**Letting systemd stop the idle daemon** rather than a self-timeout. A minor toss-up, resolved in favor
of the daemon owning its own lifecycle, which keeps the behavior in one place.

## Consequences

The compaction gap is closed for the rare times it matters, and capture stays correct without a
watcher, because a full-tree sweep on every poke means no session falls through even if its end hook
never fired. The fifteen-minute idle window matches how I actually work, warm during a block, gone
afterward.
