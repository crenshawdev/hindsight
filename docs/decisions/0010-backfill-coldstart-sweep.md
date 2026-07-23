# 0010 - Backfill as a cold-start sweep

Status: accepted (mechanics refined by [ADR 0014](0014-incremental-ingest-and-cutover.md))

The principle here holds: backfill is not special machinery, it is the normal path run over an empty
watermark, and it inherits idempotency and resumability for free. Two specifics below were refined once
the code was real. The daemon's sweep only archives; the archive-to-index-to-embed orchestration this ADR
attributes to "the normal pipeline" is `hindsight ingest`, added in [ADR 0014](0014-incremental-ingest-and-cutover.md),
which is also the steady-state path, so backfill and live capture really are one command. And the
embedding phase runs always on the GPU with no CPU fallback per [ADR 0013](0013-embed-delivery-hook-gpu.md),
not the game-defer-then-CPU behavior described below.

## Context

On day one there is already a history to ingest, the transcripts that exist at install time. The
question is whether backfill is special machinery or something the normal pipeline already does.

## Decision

Backfill **is the daemon's first sweep with an empty watermark**. Nothing looks archived yet, so
every session looks new and the normal pipeline runs over all of it. Idempotency and resumability come
for free, because the watermark already knows how to skip what is done, an interrupted backfill
resumes where it stopped, and a re-run after a wipe replays from the archive.

It sources from the live transcript tree, using the prompt-history file as a decode map for project
paths, and never from the old memory tool's database ([ADR 0009](0009-replace-prior-memory-tool.md)).

Order matters in one place: **raise the retention window before the first run.** The corpus is close
enough to the cleanup age that a startup cleanup firing mid-backfill could delete sessions not yet
archived. Bump retention first, then backfill, and nothing races.

Backfill runs in the same two natural phases the pipeline always has. The mechanical phase, archive,
parse, structural and keyword index, needs no GPU and finishes quickly, so exact and keyword recall
over all of history are live almost immediately. The embedding phase drains behind it on the GPU, so
semantic recall fills in progressively. Both run newest-first, because recent work is the most likely to
be recalled and should be ready first.

## Alternatives considered

**A separate one-shot import tool.** Rejected as duplicated machinery. Backfill and steady-state
capture are the same operation, a sweep against a watermark, so making them one path means one thing to
build and one thing to trust.

**Wait for a fully-embedded index before exposing recall.** Rejected, the mechanical phase already
gives exact and keyword recall, and blocking all recall on the slow embedding phase would leave the
tool useless for an afternoon for no reason.

## Consequences

No special code, and the properties I want, idempotent, resumable, replayable after a wipe, are
inherited rather than added. The retention bump is the one operational precondition, and it is cheap.

Backfill is also the hardest stress test of the normalizer, because it spans the format drift between
older and newer transcripts in one run, so parser bugs surface on day one. That is fine by design,
the archive is verbatim, so any parser fix is a re-derive away and never a data loss
([ADR 0003](0003-normalize-event-grain.md)).
