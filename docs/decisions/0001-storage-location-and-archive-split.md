# 0001 - Storage location and the archive / index split

Status: accepted

## Context

Hindsight has to store two very different things: a durable record of every captured transcript,
and a searchable index built on top of it. Before deciding *how* to store either, I had to decide
whether they are one store or two, and what each one owes in terms of durability.

The transcripts themselves are the only irreplaceable input. Claude Code deletes them after a
retention window (default thirty days), and once they are gone they are gone. Anything I derive
from them, by contrast, is a pure function of them and can be regenerated.

## Decision

Two stores, with data flowing one direction between them.

The **verbatim archive** holds a compressed, byte-for-byte copy of each captured transcript. It is
write-once and never mutated after capture. It is the ground truth and the only thing that must
survive.

The **derived index** holds everything computed from the archive: the normalized records, the
full-text index, the vectors, the entity inventory. It is disposable. It is rebuilt from the
archive whenever the schema changes, the parser improves, or it corrupts, and it is never backed
up, because backing up a regenerable artifact is wasted effort.

Both live under a single configurable base directory on a backed-up volume, never scattered at the
root of that volume. One config key sets the base, and the archive, the index, and the daemon's
runtime state all sit beneath it.

## Layout under the base

The archive is a directory per session, not one big blob. Each captured session lives at
`<base>/archive/<project>/<session-id>/`, and every capture drops a numbered, timestamped,
zstd-compressed generation into it next to a `meta.json` that records the source path and, per
generation, its timestamp, uncompressed size, and sha256. A session that grows gets a new
generation, a compaction rewrite gets a new generation with the old one kept, and nothing already
written is ever touched again. The write-once rule lives on the generation, the session directory
just accumulates them.

Mapping a transcript file to that `<project>/<session-id>` pair is less obvious than it looks,
because the tree is not flat. Claude Code writes the top-level session at
`projects/<project>/<session-id>.jsonl`, but a session that spawns subagents or runs a workflow
writes those transcripts a level deeper, at `projects/<project>/<session-id>/subagents/agent-<id>.jsonl`
and below. On my own machine 815 of 1430 transcript files, 57 percent of them, sit at that deeper
level. The naive read, take the parent directory as the project and the filename as the session,
files every one of those subagent logs under a project literally named `subagents` and cuts it off
from the session it belongs to. So the rule is anchored to the tree root instead: the project is the
first path segment under `projects/`, the session is the second, and anything past that is carried
through as a sub-path, so the file lands at `<base>/archive/<project>/<session-id>/<sub-path>/`,
grouped under the real session where the normalize phase expects to find it. The sweep and the
`PreCompact` hook both go through the same mapping, because two different answers to "where does this
file belong" is a bug waiting for the first subagent compaction.

## Alternatives considered

**One combined store.** Simpler on paper, but it forces the durable and the disposable to share a
backup and a lifecycle. You end up either backing up gigabytes of regenerable index nightly or
carving exceptions into the backup, and a corruption in the index puts the irreplaceable archive at
risk. The split removes the coupling entirely.

**Index only, re-read the transcripts lazily.** This was a non-starter once the retention window was
clear. A lazy reader over a tree that deletes itself is not a memory, it is a memory with a
countdown. The archive has to own its own durable copy.

## Consequences

The thing I must protect is small and grows slowly: compressed transcript text. Compression on
line-delimited JSON with heavy key repetition is substantial, so the durable footprint is modest
and cheap to back up anywhere.

The thing that is large and complicated protects itself by being rebuildable, which means I can be
reckless with it. Reindex experiments, schema changes, and format-drift fixes all become "rebuild
from archive" rather than "migrate carefully or lose data."

The rebuild path is not a rainy-day feature, it is exercised on day one, because the initial
backfill is itself a rebuild from the archived (or about-to-be-archived) transcripts. See
[ADR 0010](0010-backfill-coldstart-sweep.md).

The write-once rule on the archive is load-bearing for later decisions. Keeping secrets verbatim in
the archive while scrubbing them from the index ([ADR 0008](0008-secrets-scrub-index-only.md))
depends on the archive being the untouched ground truth, and preserving pre-compaction content
([ADR 0011](0011-hooks-and-daemon-knobs.md)) depends on the archive keeping generations rather than
overwriting.

Because the project and session names come off a file path and a hook payload, the mapping treats
them as untrusted. The segments are anchored to the tree root, and a `.`, a `..`, or a segment
carrying a path separator is rejected rather than walked, so a malformed or hostile name cannot steer
a write outside the base directory. That is the runtime half of ARC-02, the config half being the
base directory itself refusing to sit at a volume root.
