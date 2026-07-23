# Hindsight

Claude Code remembers everything for thirty days, then deletes it.

Every session you run gets written to a transcript on disk. Every prompt, every answer, every command, every file the model wrote for you, all of it sits in `~/.claude/projects` as line-delimited JSON. Then a cleanup sweep runs on startup and anything older than `cleanupPeriodDays` is gone. The default is thirty days. There is no setting to turn it off, the minimum is one day, and I found this out the interesting way, by wiping a drive on purpose and watching what came back.

Hindsight is the thing that keeps it. A local, cross-session, cross-project memory for Claude Code that captures those transcripts before the sweep takes them, and makes the whole history searchable, the fuzzy way and the exact way both. "Find me every session that touched that config file." "What was I working on the week the applet broke." "You wrote me a script that renamed screenshots by date and I lost it, find it."

That last one is the one that started this. The script isn't gone, it's sitting verbatim in a transcript that ages out in a few days. The problem was never storage, it was recall.

## What it does

It watches for new session data, copies the raw transcript to a durable archive the moment it lands, then builds a searchable index on top. The archive is the ground truth and it is never touched again. Everything else, the parsed records, the full-text index, the vectors, is derived from that archive and can be rebuilt from scratch any time I change my mind about how to index it. That split is the whole design, and it is the thing that lets me be reckless with the index and careful with exactly one file per session.

The pipeline:

```
session hook  ->  hindsight ingest  ->  verbatim archive   (durable, never mutated)
                                             |
                                             v
                                         normalize  ->  SQLite index   (rebuildable)
                                             |             FTS5 (BM25) + sqlite-vec
                                             v
                                         embeddings  ->  qwen3-embedding:8b, local, via Ollama, on the GPU
                                             |
                                             v
                                         query  ->  MCP server (recall inside a session)
                                                    CLI (operate it, and ground-truth search)
```

The session hooks run `hindsight ingest`, which sweeps new transcripts into the archive, folds any changed session into the index, and kicks off the embed drain for its new turns. A separate socket-activated capture daemon covers a manual `hindsight poke`, and a PreCompact hook snapshots a transcript straight to the archive right before Claude Code rewrites it.

Recall runs two ways, because "find me all occurrences" and "find the thing I can't name" are not the same question. The first wants a complete, exact list and vector search structurally cannot give you that, there is no top-k that means "all of them." The second wants fuzzy ranking. So exact lookups hit a structural inventory, fuzzy lookups fuse keyword and semantic search, and the structural facts, which project, which time window, which file, narrow the candidate set before anything gets ranked. Fuzzy on one axis, exact on another.

## What runs where

Everything is local. The embedder is a model running on a desktop GPU I also game on, and it runs only on the GPU, so when the card is busy the drain waits for the next session hook rather than limping through on the CPU, and nothing blocks on it because exact and keyword recall are live the moment a session lands. The index is one SQLite file. There is no server to stand up, no vector database to operate, nothing phones home during ingest. The one honest exception is the query path: when the model calls the memory mid-session, whatever comes back rides into that session's context, which does go over the wire. That is why secrets get scrubbed out of the index but left verbatim in the archive.

## Install

The system is built end to end: capture, normalize into the index, embed, query over an MCP server and a CLI, and the incremental `hindsight ingest` the session hooks run. The command surface is `hindsight normalize | load` to build the index, `hindsight embed` to drain vectors on the GPU, `hindsight ingest` for the live capture-index-embed pass, `hindsight search` for no-model ground-truth lookups, and `hindsight mcp` for the recall server. What follows sets up the capture layer, the PreCompact hook, and the ingest hooks; the [design docs](docs/DESIGN.md) and the [decision records](docs/decisions) carry the rest.

First build the binary and put it somewhere stable:

```
cargo build --release
install -Dm755 "$(cargo metadata --format-version 1 | python -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/release/hindsight" ~/.local/bin/hindsight
```

Create the config at `~/.config/hindsight/config.toml`. `base_dir` is required and must be a subdirectory under your backed-up data volume, never the volume root:

```toml
base_dir = "/data/hindsight"
# Daemon self-terminates after this many idle seconds with no poke.
# 900 (15 min) is the default; set it low (e.g. 5) while testing the lifecycle.
idle_timeout_secs = 900
```

Install the user units. Copy `systemd/hindsight.socket` and `systemd/hindsight.service` into `~/.config/systemd/user/`, and edit `ExecStart=` in the service so it points at your installed binary (the default assumes `~/.local/bin/hindsight`):

```
mkdir -p ~/.config/systemd/user
cp systemd/hindsight.socket systemd/hindsight.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now hindsight.socket
```

The socket unit listens on `$XDG_RUNTIME_DIR/hindsight.sock`. A poke (a single byte to that socket) starts the daemon under socket activation; with no further pokes it self-terminates after `idle_timeout_secs`, and the next poke respawns it. Trigger a poke and watch the lifecycle:

```
hindsight poke
journalctl --user -u hindsight.service --since "1 min ago"
```

You should see a `Spawned` line and, `idle_timeout_secs` after the last poke, a self-terminating line. To test the loop quickly, set `idle_timeout_secs = 5` in the config before poking.

### PreCompact hook

The sweep catches a transcript after it lands, but compaction rewrites a transcript in place, so the pre-compaction bytes are gone before the next sweep runs. The `precompact` subcommand closes that gap: Claude Code runs it synchronously right before it compacts, it reads the hook payload on stdin (`session_id`, `transcript_path`, `cwd`, `trigger`), and it writes a `precompact` generation holding the current bytes. If that write fails it exits non-zero to veto the compaction rather than let the bytes go, so a failed capture blocks the rewrite instead of losing it.

Register it in your Claude Code `settings.json` (user-level `~/.claude/settings.json`, or a project `.claude/settings.json`) under `hooks.PreCompact`. The command must be the absolute path of your installed binary, the same one `ExecStart` points at:

```json
{
  "hooks": {
    "PreCompact": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "/home/you/.local/bin/hindsight precompact" }
        ]
      }
    ]
  }
}
```

An empty `matcher` runs the hook for every compaction, manual (`/compact`) and automatic (full context window) both. The command takes no arguments beyond `precompact`; everything it needs arrives on stdin.

### Session hooks (ingest)

The live capture-index-embed pass runs on the session lifecycle. Register `hindsight ingest` under both `hooks.SessionStart` and `hooks.SessionEnd` in the same `settings.json`, again as the absolute path to your installed binary:

```json
{
  "hooks": {
    "SessionStart": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "/home/you/.local/bin/hindsight ingest", "timeout": 60 } ] }
    ],
    "SessionEnd": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "/home/you/.local/bin/hindsight ingest", "timeout": 60 } ] }
    ]
  }
}
```

Start sweeps up any session a missing end hook or a crash left un-indexed; end folds in the session that just closed and fires the embed drain for its new turns. Ingest is idempotent and single-flight, so running it at both ends and across overlapping sessions is safe. This is the cutover: with the hooks wired, Hindsight is capturing and indexing on its own, and the prior background memory tool gets turned off so it stops observing sessions.

## Why it exists

I wanted my own memory, and I wanted to see the pipeline end to end instead of trusting a black box. Cognee was the reference, a good one, and reading it is what clarified what I actually needed versus what a general-purpose knowledge-graph engine gives you. Most of what I wanted turned out to be cheaper than that, because a Claude Code transcript is not a wall of prose, it is a structured event log that already knows which files I touched and which commands I ran. A lot of the graph is just sitting there in the JSON, exact and free, no language model required to pull it out.

The design is documented decision by decision. Start with [DESIGN.md](docs/DESIGN.md) for the narrative, or read the [decision records](docs/decisions) for one call at a time, the alternatives on the table, and why each one went the way it did.
