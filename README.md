# Hindsight

Claude Code remembers everything for thirty days, then deletes it.

Every session you run gets written to a transcript on disk. Every prompt, every answer, every command, every file the model wrote for you, all of it sits in `~/.claude/projects` as line-delimited JSON. Then a cleanup sweep runs on startup and anything older than `cleanupPeriodDays` is gone. The default is thirty days. There is no setting to turn it off, the minimum is one day, and I found this out the interesting way, by wiping a drive on purpose and watching what came back.

Hindsight is the thing that keeps it. A local, cross-session, cross-project memory for Claude Code that captures those transcripts before the sweep takes them, and makes the whole history searchable, the fuzzy way and the exact way both. "Find me every session that touched that config file." "What was I working on the week the applet broke." "You wrote me a script that renamed screenshots by date and I lost it, find it."

That last one is the one that started this. The script isn't gone, it's sitting verbatim in a transcript that ages out in a few days. The problem was never storage, it was recall.

## What it does

It watches for new session data, copies the raw transcript to a durable archive the moment it lands, then builds a searchable index on top. The archive is the ground truth and it is never touched again. Everything else, the parsed records, the full-text index, the vectors, is derived from that archive and can be rebuilt from scratch any time I change my mind about how to index it. That split is the whole design, and it is the thing that lets me be reckless with the index and careful with exactly one file per session.

The pipeline:

```
session hook  ->  capture daemon  ->  verbatim archive   (durable, never mutated)
                                          |
                                          v
                                      normalize  ->  SQLite index   (rebuildable)
                                          |             FTS5 (BM25) + sqlite-vec
                                          v
                                      embeddings  ->  qwen3-embedding:8b, local, via Ollama
                                          |
                                          v
                                      query  ->  MCP server (recall inside a session)
                                                 CLI (operate it, and ground-truth search)
```

Recall runs two ways, because "find me all occurrences" and "find the thing I can't name" are not the same question. The first wants a complete, exact list and vector search structurally cannot give you that, there is no top-k that means "all of them." The second wants fuzzy ranking. So exact lookups hit a structural inventory, fuzzy lookups fuse keyword and semantic search, and the structural facts, which project, which time window, which file, narrow the candidate set before anything gets ranked. Fuzzy on one axis, exact on another.

## What runs where

Everything is local. The embedder is a model running on a desktop GPU I also game on, so it defers when the game wants the card and falls back to the CPU otherwise, nothing blocks on it. The index is one SQLite file. There is no server to stand up, no vector database to operate, nothing phones home during ingest. The one honest exception is the query path: when the model calls the memory mid-session, whatever comes back rides into that session's context, which does go over the wire. That is why secrets get scrubbed out of the index but left verbatim in the archive.

## Why it exists

I wanted my own memory, and I wanted to see the pipeline end to end instead of trusting a black box. Cognee was the reference, a good one, and reading it is what clarified what I actually needed versus what a general-purpose knowledge-graph engine gives you. Most of what I wanted turned out to be cheaper than that, because a Claude Code transcript is not a wall of prose, it is a structured event log that already knows which files I touched and which commands I ran. A lot of the graph is just sitting there in the JSON, exact and free, no language model required to pull it out.

The design is documented decision by decision. Start with [DESIGN.md](docs/DESIGN.md) for the narrative, or read the [decision records](docs/decisions) for one call at a time, the alternatives on the table, and why each one went the way it did.
