# 0007 - Two-path query interface and surfaces

Status: accepted

## Context

"Search" hides two different questions. "Find me all occurrences" wants a complete, exact,
countable list. "Find the thing I can't quite name" wants fuzzy ranking. A single blended pipeline
serves neither well, blind fusion pollutes the exact list with semantic near-misses, while three
separate search boxes ignore that my real queries mix an exact axis with a fuzzy one.

## Decision

Two paths, with structural facts acting as a filter rather than a ranker.

**Exact listing** answers recall-complete questions directly from the structural inventory, every
session that touched a file, every run of a command, everything in a project inside a time window. It
returns the whole set, unranked or time-ordered, and it is countable. Vector search structurally
cannot do this, there is no top-k that means "this is all of them."

**Ranked search** fuses the keyword search and the semantic search with **reciprocal-rank fusion**,
which combines two rankings by position without having to reconcile their incompatible scores. The
structural facts are the pre-filter that shrinks the candidate set before ranking, because the
anchors I actually reach for, this project, a few weeks ago, used alongside that other tool, are
exact constraints on a fuzzy search. Fuzzy on one axis, exact on another.

Every hit resolves back to the archive, so "find the script" returns the real bytes, not a summary.

The memory has two surfaces over one query core. An **MCP server** is the recall surface, callable by
the model mid-session, which is the moment recall is worth most. A **CLI** is the operator surface,
running the backfill, checking that capture keeps up, rebuilding the index, plus a plain no-model
search for ground truth when I want to see exactly what is in the store without a ranking algorithm's
opinion.

## Alternatives considered

**One always-on hybrid pipeline.** Rejected, forcing "all occurrences" through the same ranker as
"find the thing" pollutes the complete list with near-misses and loses the countable, recall-complete
contract.

**Weighted-sum fusion** instead of reciprocal-rank fusion. Rejected, keyword scores and vector
distances live on incompatible scales, so a weighted sum needs fragile per-query normalization.
Rank-based fusion sidesteps it entirely.

**A language-model query router** that reads intent and dispatches. Rejected, it puts a model in the
query path for latency and cost, when structural filters plus two clear paths already route
deterministically.

**CLI as a second polished recall surface.** Rejected as redundant, I live inside sessions, so the MCP
path covers interactive recall. The CLI earns its place as operator and ground-truth tooling, not as a
prettier search box.

## Consequences

The two questions get honest answers, exact stays exact and fuzzy stays ranked, and structural facts
sharpen both instead of competing with them. Resolving hits to verbatim archive bytes is what fixes
the specific failure this project was built against, a memory that can only tell you a script once
existed. An optional query-time re-rank of the top handful of fuzzy hits, run locally, is left as
additive enrichment for the hardest name-recall cases, never in ingest.
