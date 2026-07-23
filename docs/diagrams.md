# Hindsight diagrams

These are the living UML views of the design. They are kept in sync with the [decision
records](decisions) as the design changes. If a diagram and an ADR disagree, the ADR is right
and the diagram is a bug.

Rendered with Mermaid, which GitHub displays inline.

## Component view

Where the pieces live and how data moves between them. Solid arrows are data flow. The session
hooks run `hindsight ingest`, which sweeps, indexes, and fires the embed drain; PreCompact
snapshots one transcript straight to the archive; the poke-activated capture daemon is the
archive-only path for a manual `hindsight poke`.

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

    ingest[hindsight ingest\nsweep + index + fire drain\nsession-hook driven]
    daemon[Capture daemon\npoke-activated, archive-only]
    embed[Embed job\nhook-triggered detached drain\ndrain-and-exit]
    archive[(Verbatim archive\ncompressed, generational\nDURABLE, never mutated)]
    index[(SQLite index\nSession / Event / Artifact / Mention\nFTS5 BM25 + sqlite-vec\nREBUILDABLE)]
    ollama[Ollama\nqwen3-embedding:8b\nGPU-resident while embedding]

    mcp[MCP server\nrecall inside a session]
    cli[CLI\noperate + ground-truth search]

    hooks -- SessionStart/End: run --> ingest
    hooks -- PreCompact: snapshot --> archive
    ingest -- sweep vs watermark --> tree
    ingest -- verbatim copy --> archive
    ingest -- normalize + scrub + load --> index
    ingest -- fire detached drain --> embed

    embed -- read records --> index
    embed -- embed request --> ollama
    ollama -- vectors --> index

    cli -- poke --> sock
    sock -- activates --> daemon
    daemon -- sweep vs watermark --> tree
    daemon -- verbatim copy --> archive

    session -- recall --> mcp
    mcp -- query --> index
    mcp -- resolve artifact bytes --> archive
    cli -- query --> index
    cli -- rebuild from --> archive
    cli -- status --> daemon
```

## Capture and ingest sequence

One `hindsight ingest` pass, from a session hook to data at rest. Backfill is this same
sequence over an empty watermark and an empty ingest ledger, so every session looks new.

```mermaid
sequenceDiagram
    participant H as Session hook
    participant I as hindsight ingest
    participant T as Transcript tree
    participant A as Verbatim archive
    participant X as SQLite index
    participant E as Embed drain

    H->>I: SessionStart/End runs `hindsight ingest`
    I->>T: sweep: stat-walk, diff against watermark
    T-->>I: changed / new session files
    Note over I,A: unchanged -> skip. grew -> re-copy generation.\nrewritten (compaction) -> new generation, old one kept.
    I->>A: verbatim copy (never mutated), advance watermark
    Note over I: fingerprint each archived session vs ingest_ledger;\nunchanged sessions skipped
    I->>X: normalize, scrub secrets, session-scoped replace of\nchanged sessions' records + FTS
    Note over I,X: exact + lexical recall live here already
    I->>E: fire `hindsight embed --detach` iff a session changed
    Note over E: detached, always-GPU drain reads records and writes\nvectors, embedding only new units; semantic recall catches up
    I->>I: update ingest_ledger fingerprints
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
