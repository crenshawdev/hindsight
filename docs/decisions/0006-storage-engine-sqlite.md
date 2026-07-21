# 0006 - SQLite as the single store

Status: accepted

## Context

The derived index needs a home for four things at once: the relational records, a full-text keyword
index, the vectors, and the entity inventory. The deciding fact is that the data is tiny, roughly
twenty thousand vectors and a few hundred thousand records. This is a fits-in-memory problem, not a
scaling one. The layer is also derived and disposable, so durability does not matter here, only
simplicity and query power.

There is one more requirement from the query model. Recall narrows by structural facts first, this
project, this week, this file, and then ranks what is left. The filter, the keyword search, and the
vector search need to compose, ideally in a single query.

## Decision

Hold all of it in **SQLite**, one embedded file.

The relational records, joins, and filters are what SQLite is for. Keyword search with proper
ranking comes built in through its **FTS5** module, which has BM25 ranking, so the separate
full-text engine I expected to choose simply disappears. The **vectors sit alongside** in the same
file through an extension, and at this size an exact scan is fast enough that an approximate index
is probably unnecessary. Structural pre-filtering is a `WHERE` clause that hands the vector scan a
restricted set of ids, so "narrow then rank" is one query plan, not a cross-store dance.

## Alternatives considered

**A server-based vector database** (the usual suspects). Rejected as absurd overkill for twenty
thousand vectors. It also violates the embedded, no-server posture every other piece follows.

**DuckDB.** Tempting for the occasional analytical query, and it has full-text and vector extensions.
Rejected because it is a column-store built for batch-append analytics, and the write pattern here is
frequent small inserts as sessions land, which is closer to what a row-store like SQLite handles
well. At this data size DuckDB's scan advantage buys nothing.

**A best-of-breed constellation**, a dedicated keyword engine plus a dedicated embedded vector store
plus SQLite for the records. Each is stronger at its one job, but I would maintain three stores in
sync and fuse results across process boundaries, for twenty thousand vectors. All cost, no benefit at
this scale.

## Consequences

One embedded file, no server, rebuildable in a single pass. The query model composes cleanly because
everything lives together, which is the point.

Because the whole store is disposable, outgrowing SQLite is not a risk. If it ever happens I
re-derive into whatever comes next from the same archive, at no risk to the ground truth. The exact
keyword ranking this gives for free is what the ranked search path leans on
([ADR 0007](0007-query-interface.md)).
