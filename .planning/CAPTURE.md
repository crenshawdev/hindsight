# Capture

## Todos

- [ ] (phase 1) Human-verify criterion 1: run the full systemd cycle - `systemctl --user daemon-reload && start hindsight.socket`, poke, confirm daemon start + idle self-exit in `journalctl --user`
- [ ] (phase 1) Human-verify criterion 5: trigger a live Claude Code compaction with the README hook registered, confirm a `precompact` generation appears and `zstd -d`s to the pre-compaction bytes
- [ ] (phase 1) Durability: fsync the temp generation (and parent dir) and check close errors before the hard_link claim, or document the backup-layer tradeoff as final
- [ ] (phase 1) Resilience: decide fail-loud vs quarantine-and-continue when a pre-existing `NNNN.zst` is corrupt/undecodable in the dedup scan
- [ ] (phase 1) ARC-02 hardening: canonicalize the resolved path in `resolve_session_dir` instead of the lexical `starts_with` (symlink-inside-archive_dir redirect)
- [ ] (phase 1) `update_meta` concurrency: guard the read-modify-write of `meta.json` against concurrent writers (bounded - sidecar is rebuildable)
- [ ] (phase 1) PreCompact trust model: tighten or fail-loud the cwd/session_id fallback that chooses the archive coordinate
- [ ] (phase 1) Standalone daemon bind: bind-first and unlink only a proven-stale socket, instead of the unconditional unlink (split-brain footgun on the non-systemd path)
- [ ] (phase 1) Watermark save: use a unique temp name instead of the fixed `watermark.json.tmp`
- [ ] (phase 1) Config: require `base_dir` to be absolute (a relative value resolves against cwd, which differs between manual and service runs)
- [ ] (phase 1) Doc-sync (standing rule 1): amend ADR 0001 / DESIGN + diagrams for the index-only `NNNN.zst` filename, the D-04 single-direct-write path, and the nested-transcript `<project>/<session-id>/<sub-path>/` layout
- [ ] (phase 2) `collect_generations` descends only one hardcoded `subagents/<agent>/` level while `archive_key` files at an arbitrary sub-path (`segments[2..]`); deeper-nested subagents or workflow transcript dirs (ADR 0001 "and below") drop silently. Decide whether to recurse the session dir for every `NNNN.zst`. Intersects Phase 6 sweep wiring and D-05's `subagents/`-only framing
- [ ] (phase 2) `read_generations` aborts the whole session on one invalid JSON line; a mid-append truncated final line makes an otherwise-complete session emit zero records - consider skip-and-continue per line
- [ ] (phase 2) `extract.rs` heredoc close matches `trim() == delim`, so an indented delimiter-lookalike inside a plain (non `<<-`) heredoc truncates the artifact early
- [ ] (phase 2) `extract.rs` fenced-block detection breaks on an inner ``` line or unclosed opening fence, truncating or dropping subsequent fenced artifacts in the same message
- [ ] (phase 3) Artifact FTS5 inner-join drops an artifact whose `source_event_uuid` matches no `event` row (D-04 says "every Artifact.content"); a LEFT JOIN would preserve it with a null session_id
- [ ] (phase 3) Malformed line missing its `uuid` (empty `source_event_uuid`, empty-uuid events exempt from dedup) could fan an artifact out across every empty-uuid event, producing misattributed FTS rows; reachable only on malformed input plus a multi-session load
- [ ] (phase 3) Doc-sync (standing rule 1): no dedicated store-schema ADR yet - the Phase-3 schema lives only in CONTEXT decisions D-05..D-11; consider promoting it to an ADR before Phase 5/6 build on it
- [x] (phase 4) Delivery mechanism superseded: Task 5 CPU fallback + Task 6 systemd timer were built to ADR 0004, but the locked embed redesign (hook-triggered, always-GPU, never CPU, no timer) supersedes both and is scoped to Phase 5. Relocate/replace the timer + CPU-fallback code in Phase 5; confirm before treating it as final -> DONE in Phase 5 (gpu.rs deleted, timer/service removed, ADR 0013); runtime confirmation tracked as the phase-5 human-verify items below
- [ ] (phase 4) Cross-project entity carries one `project` (most-frequent) on its vector, so Phase 6's structural pre-filter cannot narrow a shared entity to its other projects. Per-project retrieval would need one vector per (entity, project) or a multi-valued project index (changes the D-09 shape) - flagged for John, left as-is pending decision
- [ ] (phase 5) [medium, from diff review] `attempts` accumulates across `embedder_version` changes while the give-up skip-check is version-scoped (`src/embed/mod.rs`): a unit that failed 5x under model A can be retired after a single attempt under model B when re-embedding without a `hindsight load` between. Reset `attempts` when the ledger row's `embedder_version` differs from current
- [ ] (phase 5) Human-verify (needs Ollama + qwen3-embedding:8b + GPU): `time hindsight embed --detach` from a real session hook exits ~1s while the detached child (reparented to PID 1, new session) survives and `vec_embedding` grows after the parent returned - confirm from a hook, not an interactive shell
- [ ] (phase 5) Human-verify: a full drain lands every assembled unit's vector (`vec_embedding` count == `--dump-profiles` line count, every vector 4096-dim) with the model GPU-resident (`ollama ps` shows the GPU processor)
- [ ] (phase 5) Human-verify: two concurrent `hindsight embed` runs produce no duplicate `(unit_kind, source_id)` rows, the second logs "already running" and exits 0, and a unit added after the lock releases is embedded by the next run
- [ ] (phase 5) Doc-sync: `docs/STATUS.md:28` still summarizes ADR 0004 as "opportunistic GPU schedule", now stale against ADR 0013's hook-triggered always-GPU delivery - refresh STATUS
- [ ] (phase 5) `src/archive.rs:57` dead-code warning (`Outcome` variant field `path` never read); pre-existing, surfaced by the build - clean up or wire the field
- [ ] (phase 6) Human-verify (needs live Claude Code MCP client): `claude mcp add hindsight -- <path>/hindsight mcp`, restart, confirm a recall tool call returns results for a seeded query. In-process handler test already passes
- [ ] (phase 6) [low, diff review] `src/query/vector.rs` two-stage-recall test does not truly exercise the `coarse_k` guard (near row is both Hamming- and cosine-nearest in a 2-row table); construct a true cosine-neighbor that is NOT Hamming-nearest, crowded out under a tight pool
- [ ] (phase 6) [low, diff review] `hindsight search <terms>` with multiple unquoted words is rejected by clap (positional is a single `Option<String>`); Must-be-true #2 runs only when quoted - consider a trailing var-arg joining tokens
- [ ] (phase 6) Optional CLI `--resolve` affordance not added (MCP `resolve` tool is the primary caller, plan-permitted); wire it if a CLI resolve path is wanted
- [ ] (phase 6) 4 benign `field never read` warnings (`rank`/`project`/`distance` pub hit-struct fields RRF orders on via SQL, `tool_router` macro-accessed); revisit if pub API is trimmed
- [ ] (phase 7) [medium, from re-embed token-cost measurement] Profile units can far exceed the embedder's 4096-token (~16k char) context, so Ollama silently truncates them: `--dump-profiles` measured artifacts up to 117,778 chars (avg 1,766) and events up to 166,813 chars (avg 435). Two costs: wasted transfer of text that is discarded, and a long artifact's vector reflects only its first ~16k chars, not the whole artifact. Fix in `src/embed/profile.rs` (`assemble_artifacts` / `assemble_events`): cap profile text to the context window before embedding, and consider chunking a large artifact into several units instead of truncating so the whole thing is represented. Dominates the drain's slow front phase (artifact/entity units are ~4x the event-unit length)

## Seeds

## Notes
