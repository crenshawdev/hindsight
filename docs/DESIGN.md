# Designing Hindsight

This is the build log. It walks the design end to end, the calls I made, the ones I threw out,
and why. Every decision here has its own record in [decisions](decisions) if you want the short
structured version. The [diagrams](diagrams.md) are the picture. This is the story.

## The problem is recall, not storage

Claude Code writes every session to disk as line-delimited JSON, and a cleanup sweep deletes
anything older than thirty days on startup. You can raise the number, you cannot turn it off,
and the floor is one day. I confirmed the behavior the hard way, I wiped a drive on purpose and
watched a month of work be the only thing that came back.

The thing that bugged me was never the storage. A script I asked for and lost is not gone, it
is sitting verbatim in a transcript, complete, exactly as it was written, in a file that ages
out in a few days. What I was missing was a way to reach back and find it. The transcripts are
a fine record. They are just a record with a timer on it and no index.

Then I actually looked at the record. A few weeks of my own transcripts came to hundreds of
megabytes, and when I broke it down, the conversational signal, the prompts I typed and the
answers I got back, was about two and a half percent of the bytes. The rest was tool output,
duplicated payloads, injected context, machine chatter. One background tool I had installed was
generating a third of the entire pile on its own. The useful part was tiny and buried, which is
the whole reason a plain full-text grep over the raw tree never felt like memory.

## One split does most of the work

The design turns on a single idea. There are two stores, and they have nothing in common except
the data flows one direction between them.

The archive is a verbatim copy of the raw transcript, compressed, written once, never touched
again. It is the ground truth. The moment a session lands, its bytes get copied here before
anything else happens.

Everything else, the parsed records, the full-text index, the vectors, the entity inventory, is
derived from that archive. It is disposable by construction. If it corrupts, if I change how I
parse things, if a Claude Code update shifts the transcript format out from under me, I delete
the whole index and rebuild it from the archive. It never gets backed up, because backing up a
thing you can regenerate is a waste, and that means I can be as reckless with the index as I am
careful with the one file per session that matters.

That asymmetry is what makes the rest cheap. The thing I have to protect is a small pile of
compressed text that grows slowly. The thing that is large and complicated protects itself by
being rebuildable. See [ADR 0001](decisions/0001-storage-location-and-archive-split.md).

## Catching it before the sweep

Recall can be built whenever. The archive cannot be built after the fact, every day without a
capture running, thirty-day-old sessions fall off the far end for good. So capture came first,
and it had to be reliable without me thinking about it.

The obvious move is a hook. Claude Code fires a `SessionEnd` hook when a session ends, and it
hands you the transcript path. The trouble is the guarantees are undocumented, nobody promises
it fires on a crash, a `SIGKILL`, a closed terminal, a dropped SSH connection. Building capture
on a trigger that might not fire is how you find out months later that half your sessions are
missing.

So the primary mechanism is not the hook, it is a sweep. A daemon walks the transcript tree,
compares every file against a watermark of what it has already archived, and copies whatever is
new or changed. Because the sweep is based on file age the same way the cleanup is, any sweep
inside the thirty-day window catches everything, no matter how the session died. The hooks just
make it prompt instead of periodic. The one thing the walk cannot be naive about is where a file
sits, because most of them are not top-level sessions at all, 815 of the 1430 transcripts on my
machine are subagent and workflow logs nested a level down under the session that spawned them, so
the archive path is anchored to the tree root and carries the nested part through as a sub-path
rather than filing every subagent under a phantom project. See [ADR 0001](decisions/0001-storage-location-and-archive-split.md).

I did not want a timer polling on a clock whether or not anything happened, and I did not want a
service sitting resident all day doing nothing. systemd socket activation gives you the third
thing. systemd owns a socket that costs nothing when idle, the session hook writes one byte to
it, and that first byte starts the daemon. The daemon sweeps, idles for fifteen minutes watching
for more pokes, and if none come it exits. The next session pokes the socket and systemd starts
it again. On-demand start, self-shutdown, and systemd handles the liveness and the restart and
the logging I would otherwise have hand-rolled. Because I tend to clear a session and start fresh
at twenty percent context, the daemon stays warm across a work block on its own and shuts down
once I actually walk away. See [ADR 0002](decisions/0002-capture-daemon-socket-activation.md).

The one hole a sweep cannot close is compaction. When context fills, Claude Code rewrites the
transcript in place, replacing older turns with a summary, and a sweep that arrives afterward
copies a file that has already lost content. The fix is the one other hook worth wiring, a
`PreCompact` hook that copies that single transcript before the rewrite happens. It fires rarely
for me, so a brief synchronous copy costs nothing and guarantees the pre-compaction bytes are
kept. See [ADR 0011](decisions/0011-hooks-and-daemon-knobs.md).

## The transcript is a log, not an essay

Here is where reading cognee paid off, by showing me what I did not need. Cognee builds a
knowledge graph out of prose by having a language model read it and extract entities and
relationships, and that extraction is where nearly all the cost lives, two model calls per chunk
of text. That is the right tool when your input is a wall of unstructured writing.

A Claude Code transcript is not that. It is a structured event log that happens to contain some
prose. Which files I touched is sitting right there in the arguments of every read and edit and
write. Which commands I ran is in the shell calls. Which project, which branch, which timestamp,
which subagent, which skill, all of it is a field in the JSON, exact and already labeled. Pulling
that out is a parse, not an inference, and it is more accurate than a model reading prose would
ever be, because "every session that touched this config" derived from the actual tool calls is
ground truth, while the same question answered by a model reading around the topic will miss the
sessions where I edited the file without discussing it and invent ones where I only talked about
it.

So normalize is mostly mechanical. It reads the raw JSON and emits four record types, a Session,
an Event for each meaningful turn, an Artifact for each script or file the model produced, and a
Mention for each entity that showed up, a file path, a command, a package, a symbol. The
[data model diagram](diagrams.md#data-model) has the shape.

The one real judgment call was how much of each event to keep in the searchable index. The answer
is three tiers, and it leans on the archive again. The good stuff, prompts, answers, tool calls,
artifacts, gets fully indexed. The heavy noise, the model's private reasoning and the bodies of
tool results that are mostly file dumps and command spew, gets a skeleton record, I keep that it
existed and where it sits in the thread, but its body stays out of the index. And the pure junk,
the duplicated payloads and the injected listings, does not even get a skeleton. Every one of
those gates is reversible, because the full bytes are in the archive, so I optimized the index for
signal instead of completeness and let completeness live one layer down. That took a few weeks of
transcripts down to the couple of megabytes that actually mean something. See
[ADR 0003](decisions/0003-normalize-event-grain.md).

## The embedder, which took the scenic route

Fuzzy recall needs embeddings, and this is the decision that wandered the most before it landed.

The killer query is "I don't remember the name, find it." You describe a tool by what it did and
you want the name back. That is asymmetric, a short vague description on one side, a specific name
on the other, and a bare name embeds terribly against a description. The fix is to embed a
synthetic profile of each entity, the name plus the sentences it appeared in plus what it got used
alongside, so a description lands near it. Same trick for the lost scripts, I embed the request I
made and the explanation around it, not the code itself, because code embeds poorly against a
plain-English "find the script that renamed my screenshots." The natural-language wrapper is what
matches, the code gets reached through a keyword index and a link back to the archive. See
[ADR 0005](decisions/0005-profile-construction-mechanical.md).

First plan was to stand the embedder up on a separate box on the network. That fell over on a
hardware question I did not want to chase, so it came off the table. Then I looked at the
integrated GPU, which turned out to be a display engine with about a fifth of the throughput of
the CPU sitting next to it, and no memory-bandwidth advantage either since it shares the same
system RAM, so it was slower than just using the processor. Dead end.

Which left the desktop GPU, a decent card, plenty for this. Except I game on it. A model parked in
video memory keeping itself warm while a game wants that memory is a fight I did not want to have.

The thing that resolved it is that embedding is deferrable and nothing waits on it. The archive
and the parse happen the instant a session lands, and those give you exact and keyword recall on
their own. The embeddings are a step downstream that can lag for hours and lose nothing but a
little freshness on the fuzzy path. So the GPU became an opportunistic accelerator, not a resident
service. The model loads only while it is actually embedding, it defers when a game is holding the
card, and it falls back to the CPU otherwise, which at this data size finishes the whole backfill
in an afternoon anyway. Gaming killed the idea of a warm resident model, not the GPU. See
[ADR 0004](decisions/0004-embedder-and-gpu-scheduling.md).

The model is qwen3-embedding:8b, quantized, run locally through Ollama. The data is tiny, a couple
hundred thousand records and on the order of fifty thousand vectors and growing, so this is a fits-in-memory problem,
not a scaling one, and running the best local model I can fit costs nothing extra at this size. Local
also closes the one privacy gap that mattered, because the transcripts already went to Anthropic by
existing at all, but sending them to a third-party embedding API would be handing a brand new party a
coherent index of everything I have built, and I would rather not.

## Holding it in one boring file

For the store, the data staying small is the deciding fact. Fifty-odd thousand vectors growing
slowly is not a database-server problem, it is a single-file problem, and standing up a vector
service for it would be absurd. It is also derived and disposable, so durability does not matter
here, only simplicity and query power.

SQLite holds all of it. The relational records, joins, and filters are exactly what SQLite is for.
Full-text search with proper keyword ranking comes built in through its FTS5 module, which quietly
deleted a whole component I thought I would need to pick. The vectors sit alongside in the same file
through an extension. I first assumed a plain exact scan would be fast enough to skip an approximate
index, and a stress test on the real vectors corrected that, a single-threaded float scan over
4096-dimensional vectors holds an interactive bar only up to about sixty-five thousand of them, and
the real count is already past that on the way up. The fix stayed inside the file. Unfiltered fuzzy
queries run a binary-coarse pass and then rescore the candidates at full precision, which holds the
bar out to half a million vectors in a sixteen-megabyte index at essentially the same recall as
exact. One embedded file, no server, rebuildable in a single pass.

The clincher was the query model. Recall narrows by structural facts first, this project, this
week, this file, and then ranks what is left. When the filter and the keyword search and the vector
search all live in one store, that is one query. For the anchored queries that carry a filter, the
filter runs in SQL and the vector step exact-reranks the survivors, which is cheap because the
survivor set is small, and the unanchored fuzzy query is the one that needs the coarse-then-rescore
pass over everything. Split across three specialized stores this becomes a filter here, pass a set
of ids there, fuse across a process boundary, all for a corpus this size, and a bench of usearch,
LanceDB, and DuckDB on the real vectors came back the same way, none of them beat the one boring file
once anchored queries and footprint and their weaker relational halves were counted. And because the
whole thing is rebuildable, if it ever genuinely outgrows SQLite I re-derive into whatever is next
from the same archive, at no risk. See [ADR 0006](decisions/0006-storage-engine-sqlite.md).

## Two ways to ask

Recall runs two paths, because two different questions are hiding under the word "search." See
[ADR 0007](decisions/0007-query-interface.md).

"Find me all occurrences" wants a complete, exact, countable list, and vector search cannot give
you that, there is no top-k that means "this is all of them." That one hits the structural
inventory directly and returns the whole set.

"Find the thing I can't quite name" wants fuzzy ranking, and that one fuses the keyword search and
the semantic search with reciprocal-rank fusion, which combines two rankings without having to
reconcile their incompatible scores. The structural facts are not a third ranker competing with
those two, they are the filter that shrinks the candidate set before ranking, because the anchors I
actually reach for, "while I was working on that applet," "a few weeks ago," "I was using it with
my notes app," are exact constraints on a fuzzy search. Fuzzy on one axis, exact on another.

Every hit resolves back to the archive, so "find the script" returns the actual script, the real
bytes, not a summary of a script. That last part is the specific failure I was fixing, a memory
that can only tell you a script once existed is not memory, it is a card catalog for a library that
burned down.

The memory surfaces as an MCP server, so the model can call it mid-session the moment recall is
worth most, right when I am about to solve something I might have already solved. There is a CLI
too, but not really for searching, I live inside sessions so the MCP path covers that. The CLI is
for operating the thing, running the backfill, checking that the daemon is keeping up, rebuilding
the index, and for a plain no-model search when I want to see exactly what is in there without a
ranking algorithm's opinion in the way.

## The parts that stayed small

A few decisions did not need much argument once the frame was set.

Secrets get scrubbed on the way into the index but left verbatim in the archive. Locally, nothing
leaves the box on ingest, so the old worry about leaking to a vendor is moot. Two real risks
survive, though. The query path does leave the box, when the model pulls a memory into a live
session it rides back over the wire, and a durable searchable index turns a password buried in a
transcript that ages out into a password I can casually grep up years later. Scrubbing the index
handles both. Leaving the archive verbatim keeps the ground truth intact, and it costs nothing on
privacy, because the raw transcripts already sit on the same backed-up disk. See
[ADR 0008](decisions/0008-secrets-scrub-index-only.md).

The background memory tool I already had installed gets replaced, not absorbed. It stores a model's
prose summaries of sessions, and out of over a thousand of them exactly one held a line of code, so
it cannot answer "find the script" even in principle. Its old summaries stay where they are, I just
stop feeding it and build from the raw transcripts instead, inheriting none of its lossiness. See
[ADR 0009](decisions/0009-replace-prior-memory-tool.md).

And the first-day backfill is not special code, it is the daemon's normal sweep with an empty
watermark, so every session looks new and the same pipeline runs over all of history. Idempotent
and resumable for free, since the watermark already knows how to skip what is done. The one ordering
rule is to raise the retention window before the first run, so nothing ages out in the gap between
starting and finishing. See [ADR 0010](decisions/0010-backfill-coldstart-sweep.md).

## Where it stands

The shape is settled end to end. Capture the raw transcript before the sweep takes it, keep it
verbatim, derive everything else and treat all of it as disposable, pull the exact structure out of
the log for free and spend the language model only where prose actually needs reading, hold it in
one file, and ask it two ways because there are two questions.

None of it is exotic. The reason it works is that a coding assistant's transcript is a far better
raw material than a pile of documents, it already knows what I did, and most of the memory I wanted
was sitting in that structure waiting to be read out, exact and cheap, if I just kept the file from
being deleted first.

The one build decision the design deliberately left open, the language it is all written in, is now
settled. It is Rust, shipped as a single static binary with the daemon, CLI, and MCP server as
subcommands, because a small always-on executable is what socket activation wants and rusqlite carries
SQLite, FTS5, and sqlite-vec as one linked dependency. See
[ADR 0012](decisions/0012-implementation-language-rust.md).
