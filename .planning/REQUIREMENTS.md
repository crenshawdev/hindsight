# Requirements: Hindsight

**Defined:** 2026-07-20
**Core Value:** Past Claude Code work stays findable and retrievable, verbatim, long after the cleanup
sweep would have deleted the transcript.

## v1 Requirements

Committed scope. Each maps to exactly one roadmap phase.

### Capture (CAP)

- [ ] **CAP-01**: The daemon archives every transcript in the tree that is new or changed since the
  last watermark, regardless of how the session ended.
- [ ] **CAP-02**: The daemon starts on demand via systemd socket activation when a session hook pokes
  the socket, and self-terminates after 15 minutes idle.
- [ ] **CAP-03**: A PreCompact hook snapshots a transcript before Claude Code compacts it in place.
- [ ] **CAP-04**: The watermark makes sweeps idempotent and resumable, so an interrupted or repeated
  sweep skips already-archived sessions.

### Archive (ARC)

- [ ] **ARC-01**: Each captured session is written once to a verbatim, compressed archive that is never
  mutated afterward.
- [ ] **ARC-02**: The archive and index live under a configurable subdirectory of the data volume,
  never the volume root.

### Normalize (NRM)

- [ ] **NRM-01**: Normalize parses a raw transcript into Session, Event, Artifact, and Mention records.
- [ ] **NRM-02**: Each event is assigned one of three grains (indexed / skeleton / archive-only)
  controlling how much of it enters the index.
- [ ] **NRM-03**: The parser reads both historical Claude Code transcript formats in a single run.
- [ ] **NRM-04**: Secrets are scrubbed from the index while the archive keeps them verbatim.

### Store (STO)

- [ ] **STO-01**: Records persist into a single SQLite database with the relational schema.
- [ ] **STO-02**: An FTS5 index provides BM25 keyword search over indexed content.
- [ ] **STO-03**: sqlite-vec stores and queries embedding vectors in the same database.

### Fuzzy (EMB)

- [ ] **EMB-01**: Synthetic profiles are constructed mechanically from records for embedding, rather
  than raw names or code.
- [ ] **EMB-02**: Profiles are embedded via Ollama qwen3-embedding, deferring to the GPU when it is
  free and falling back to CPU.

### Query (QRY)

- [ ] **QRY-01**: A user can run an exact, recall-complete listing query and get all matches.
- [ ] **QRY-02**: A user can run a ranked fuzzy search that fuses keyword and vector results via RRF
  with structural pre-filters.
- [ ] **QRY-03**: A query result resolves back to the verbatim archived content it came from.

### Interface (IFC)

- [ ] **IFC-01**: An MCP server exposes recall to Claude Code as named tools.
- [ ] **IFC-02**: A CLI operates the system and runs ground-truth search.

### Go-live (MIG)

- [ ] **MIG-01**: A first run over existing history backfills the archive and index as an
  empty-watermark sweep, newest-first and resumable.
- [ ] **MIG-02**: The prior background memory tool is disabled and the session hooks are wired, cutting
  over to Hindsight.

## v2 Requirements

Deferred. Tracked, not in the current roadmap.

(None yet - the design is scoped as a single coherent v1.)

## Out of Scope

| Feature | Reason |
|---------|--------|
| LLM knowledge-graph extraction (cognee-style) | A transcript is a structured log, so extraction is a parse not an inference, and the per-chunk model cost is not worth it (ADR 0003) |
| Migrating the prior memory tool's database | It is replaced, its old observations left in place (ADR 0009) |
| A client/server datastore | SQLite is one file with no server (ADR 0006) |
| Backing up the index | Rebuildable from the archive by construction (ADR 0001) |
| Disabling Claude Code cleanup outright | Not possible, so the retention window is raised instead |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| CAP-01 | Phase 1 | Complete |
| CAP-02 | Phase 1 | Complete |
| CAP-03 | Phase 1 | Complete |
| CAP-04 | Phase 1 | Complete |
| ARC-01 | Phase 1 | Complete |
| ARC-02 | Phase 1 | Complete |
| NRM-01 | Phase 2 | Complete |
| NRM-02 | Phase 2 | Complete |
| NRM-03 | Phase 2 | Complete |
| NRM-04 | Phase 2 | Complete |
| STO-01 | Phase 3 | Complete |
| STO-02 | Phase 3 | Complete |
| STO-03 | Phase 3 | Complete |
| EMB-01 | Phase 4 | Complete |
| EMB-02 | Phase 4 | Complete |
| QRY-01 | Phase 6 | Pending |
| QRY-02 | Phase 6 | Pending |
| QRY-03 | Phase 6 | Pending |
| IFC-01 | Phase 6 | Pending |
| IFC-02 | Phase 6 | Pending |
| MIG-01 | Phase 7 | Pending |
| MIG-02 | Phase 7 | Pending |

**Coverage:** 22 v1 requirements, 22 mapped, 0 unmapped

---
*Last updated: 2026-07-20 after project initialization*
