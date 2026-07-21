# 0006 - SQLite as the single store

Status: accepted (amended 2026-07-21, see Amendment)

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

## Amendment (2026-07-21): measured, not asserted

The number this decision leaned on, "roughly twenty thousand vectors," was a guess, and it was low.
Before writing any code I embedded the real corpus at the grain
[ADR 0003](0003-normalize-event-grain.md) defines and benchmarked the candidates on those real
vectors, CPU-only, on the workstation this runs on. The engine choice held. Three of the specifics
under it did not, so here is what the measurement put in their place.

**The count.** The indexed floor is about 55,000 vectors, not 20,000: 24,766 prose chunks plus
29,920 tool-call invocations, from 1,446 transcript files with zero parse errors, before any entity
profiles. The three-tier grain is what keeps it that low, the 29,917 tool-result bodies and the
18,651 private-reasoning blocks are skeleton-tier and never become vectors. The count climbs from
there because the corpus never deletes, so the planning number is tens of thousands and growing, not
a fixed twenty.

**Exact float scan is not fast enough, and that was the load-bearing claim.** sqlite-vec's float KNN
is a single-threaded scan, and at 4096 dimensions it holds an interactive bar only while the corpus
is small. Measured p95 on one query: 63 ms at 25k vectors, 214 ms at 90k, 1,178 ms at 500k. It
breaks past about 65,000, which the real corpus reaches. "An approximate index is probably
unnecessary" is false at the size that matters.

**The fix is quantization, not a different engine.** A binary-coarse pass over bit-quantized vectors
followed by a full-precision rescore of the candidates holds the bar the whole way out, 12 ms at 90k
and 74 ms at 500k, in a 16 MB index, at 0.998 recall against exact once the over-fetch is 80 or more.
That is a two-stage approximate retriever, so the honest name for it is coarse-then-rerank, not exact
scan.

**"Narrow then rank is one query plan" was wrong about the mechanism.** vec0 can only narrow a KNN
scan on partition or metadata columns sitting on the vector table itself. It cannot take a restricted
id set from a joined relational filter, which is exactly what the file and tool anchors are, since
those are many-to-many through Mention. The naive path, nearest-neighbors first and filter the
results after, under-fills badly, at a 3% filter it returned 0.4 of 10 rows in-set and came up short
on all 100 test queries. The path that works runs the other direction, filter in SQL then
exact-rerank the survivors, and it is cheap because the survivor set is small, 0.1 ms at a 3% filter
and 10 ms at 30%, exact every time. The query that actually ships, structural filter plus FTS5/BM25
plus vector rerank fused with reciprocal-rank fusion, measured p95 11 ms end to end in one file.

**The engine still wins, and it wins alone.** I benched the alternatives on the same real vectors
instead of arguing them on paper. usearch was the tempting second store, a real HNSW graph answering
unfiltered queries in about 1 ms, but that speed already sits far under the bar, so its one edge goes
unspent while it stays a second store. The filtered search that might have earned it was absent from
the Python binding I benched and unverified in the Rust binding I would actually link
([ADR 0012](0012-implementation-language-rust.md)), and it does not decide the question anyway,
because in-file filter-then-rerank answers anchored queries cheaply. LanceDB, the best paper case
for a purpose-built store, wanted a 33 second index build and 504 MB on disk, its recall needed heavy
refine tuning to catch up, and its relational half is weak enough that I would pair SQLite with it
anyway and land two stores with the worse relational one. DuckDB's filtered vector path returned 0.03
recall, the same under-fill trap, and its column-store fights the frequent-small-insert write
pattern. One SQLite file, records and FTS5 and the vectors together, filter-then-rerank for anchored
queries and coarse-then-rescore for unfiltered ones, beat all of them at this scale.

What this corrects: the 20k count becomes about 55k and growing, "exact scan, no ANN" becomes
"binary-coarse plus float-rescore," and "one query plan hands the scan a restricted id set" becomes
"filter in SQL, rerank the survivors." What it does not touch is the decision this ADR exists for,
everything in one SQLite file, which the measurement backed.
