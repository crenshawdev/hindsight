# Hindsight diagrams

These are the living UML views of the design. They are kept in sync with the [decision
records](decisions) as the design changes. If a diagram and an ADR disagree, the ADR is right
and the diagram is a bug.

Rendered with Mermaid, which GitHub displays inline.

## Component view

Where the pieces live and how data moves between them. Solid arrows are data flow, the hook
poke is control.

```mermaid
flowchart TD
    subgraph cc[Claude Code]
        session[Active session]
        hooks[SessionStart / SessionEnd / PreCompact hooks]
        tree[(Transcript tree\n~/.claude/projects)]
        session --> tree
        session -. fires .-> hooks
    end

    subgraph sd[systemd]
        sock{{socket unit}}
    end

    daemon[Capture daemon]
    embed[Embed job\nhook-triggered detached drain\ndrain-and-exit]
    archive[(Verbatim archive\ncompressed, generational\nDURABLE, never mutated)]
    index[(SQLite index\nSession / Event / Artifact / Mention\nFTS5 BM25 + sqlite-vec\nREBUILDABLE)]
    ollama[Ollama\nqwen3-embedding:8b\nGPU-resident while embedding]

    mcp[MCP server\nrecall inside a session]
    cli[CLI\noperate + ground-truth search]

    hooks -- poke --> sock
    sock -- activates --> daemon
    daemon -- sweep vs watermark --> tree
    daemon -- verbatim copy --> archive
    daemon -- normalize + scrub --> index

    embed -- read records --> index
    embed -- embed request --> ollama
    ollama -- vectors --> index

    session -- recall --> mcp
    mcp -- query --> index
    mcp -- resolve artifact bytes --> archive
    cli -- query --> index
    cli -- rebuild from --> archive
    cli -- status / poke --> daemon
```

## Capture sequence

One sweep, from a hook poke to data at rest. Backfill is this same sequence with an empty
watermark, so every session looks new.

```mermaid
sequenceDiagram
    participant H as Session hook
    participant S as systemd socket
    participant D as Capture daemon
    participant T as Transcript tree
    participant A as Verbatim archive
    participant X as SQLite index

    H->>S: poke (one byte)
    S->>D: activate (or deliver to running daemon)
    D->>T: stat-walk, diff against watermark
    T-->>D: changed / new session files
    Note over D,A: unchanged -> skip. grew -> re-copy generation.\nrewritten (compaction) -> new generation, old one kept.
    D->>A: verbatim copy (never mutated)
    D->>X: normalize, scrub secrets, upsert records + FTS
    Note over D,X: exact + lexical recall live here already
    Note over D,X: embedding is NOT here. A session hook fires a\nhook-triggered detached embed process that reads records and writes\nvectors, so a long drain never fights the daemon's idle exit.
    D->>D: mark watermark, idle 15 min, then exit
```

## Query sequence

Two paths. Exact listing is recall-complete and unranked. Ranked search fuses lexical and
semantic, narrowed first by structural filters.

```mermaid
sequenceDiagram
    participant U as Caller (MCP or CLI)
    participant Q as Query core
    participant X as SQLite index
    participant A as Verbatim archive

    U->>Q: query (+ optional filters: project, time, touches, cooccurs)
    alt exact listing ("all occurrences")
        Q->>X: structural lookup over entity inventory
        X-->>Q: complete, countable list
    else ranked search ("find the thing")
        Q->>X: structural filter -> candidate id set
        Q->>X: BM25 (FTS5) over candidates
        Q->>X: vector (sqlite-vec) over candidates
        X-->>Q: two ranked lists
        Q->>Q: RRF fuse
    end
    Q->>A: resolve hits to verbatim bytes (e.g. the lost script)
    A-->>U: results + real artifact content
```

## Data model

The four normalized record types and how they relate. All are derived from the archive and
rebuilt by normalize.

```mermaid
classDiagram
    class Session {
        session_id
        project
        git_branch
        cc_version
        started_at
        ended_at
        end_reason
        title
        archive_refs
    }
    class Event {
        id
        uuid
        parent_uuid
        session_id
        role
        kind
        timestamp
        text
        tool_name
        attribution
        is_sidechain
        agent_id
        agent_type
    }
    class Artifact {
        artifact_id
        kind
        path
        language
        content
        request_bundle
        source_event_uuid
    }
    class Mention {
        entity
        entity_type
        event_uuid
        session_id
        project
        timestamp
    }

    Session "1" --> "0..*" Event : contains
    Event "1" --> "0..*" Mention : mentions
    Event "1" --> "0..*" Artifact : produces
    Artifact ..> Event : request_bundle points back to
```

## Daemon lifecycle

The state machine behind socket activation and idle self-termination.

```mermaid
stateDiagram-v2
    [*] --> Spawned : socket activation (first poke)
    Spawned --> Sweeping : full-tree sweep vs watermark
    Sweeping --> Idle : sweep complete, watermark saved
    Idle --> Sweeping : poke, or dirty flag set during sweep
    Idle --> Exited : 15 min no activity
    Exited --> [*]
    note right of Sweeping
        pokes during a sweep set a dirty flag;
        the loop re-sweeps once on completion.
        no concurrent sweeps.
    end note
    note right of Idle
        next poke after Exited
        respawns via systemd.
    end note
```
