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

## Seeds

## Notes
