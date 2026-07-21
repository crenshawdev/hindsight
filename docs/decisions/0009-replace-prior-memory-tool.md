# 0009 - Replace the prior memory tool, leave its observations

Status: accepted

## Context

I already had a background memory tool installed for Claude Code. Before building Hindsight I looked
at what it actually captured, so I would either reuse it or understand exactly why not.

It works by having a language model observe the session live and write prose summaries, one round
trip per tool call, and it stores only that generated prose. Out of over a thousand stored
observations, exactly one contained a line of code. The bodies of what I did, the commands, the file
contents, the scripts, are sent to the observer and then discarded, and the only link from a summary
back to the original transcript breaks when that transcript ages out. It solves a genuinely different
problem, recall of what was learned, and it cannot answer "find the script you wrote me," even in
principle, because it never keeps a script.

It was also expensive and noisy, its observer generated a large fraction of my entire transcript pile
as a byproduct and spent tokens per tool call to do it.

## Decision

**Replace it. Leave its existing observations in place.**

Hindsight takes over the memory role going forward. The old observations stay where they are, they are
not migrated into the new store and not deleted. Backfill sources only from the raw transcripts and
the archive, never from the old tool's derived database, so Hindsight inherits none of its lossiness.

## Alternatives considered

**Absorb the old observations into the new index.** Rejected, they are lossy prose summaries with no
verbatim content and no reliable link back to source. Importing them would seed the new store with
exactly the shape of data Hindsight exists to improve on.

**Keep both running side by side.** Mechanically fine, the old tool's observer noise is filtered out
by normalize regardless, but it keeps burning tokens per tool call and generating byproduct
transcripts for a role Hindsight now fills. Redundant, so the observer gets turned off once Hindsight
is capturing.

## Consequences

The new store is built from ground truth rather than someone else's summaries. The old tool's history
remains readable in place if I ever want it, at no cost, and it stops competing for the same job. Its
observer noise is dropped at the archive-only tier during backfill and then never generated again once
it is disabled ([ADR 0003](0003-normalize-event-grain.md)).
