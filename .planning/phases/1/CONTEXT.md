# Phase 1: Capture - Context

Gathered: 2026-07-20
Feeds: /cad-plan 1

## Scope boundary

In: The Rust repo scaffold and one-binary skeleton (subcommands `daemon` / `precompact` /
`poke`); a systemd socket-activated capture daemon that self-terminates after 15 minutes idle;
the systemd user socket + service units; the verbatim, zstd-compressed, generational, write-once
archive writer; the watermark and full-tree sweep (idempotent and resumable); and the synchronous
PreCompact snapshot hook. Serves CAP-01, CAP-02, CAP-03, CAP-04, ARC-01, ARC-02.
Out: Normalize / records / grain / secret-scrub (Phase 2 - the Phase 1 archive is a pure verbatim
copy, no scrubbing in capture); the SQLite index, FTS5, sqlite-vec (Phase 3); embeddings (Phase 4);
the query core, MCP server, and CLI search (Phase 5); the empty-watermark backfill run and the full
SessionStart/SessionEnd hook cutover plus prior-memory-tool retirement (Phase 6).
Deferred: None.
Plan shape: Multiple plans, same phase - /cad-plan breaks the six criteria into multiple plans
(e.g. scaffold + daemon lifecycle, then archive + watermark + sweep, then PreCompact).

## Decisions

- D-01 (Archive): Archive compression is zstd. Evidence: docs/decisions/0001-storage-location-and-archive-split.md.
- D-02 (Archive): On-disk layout is per-session directories - `<base>/archive/<project>/<session-id>/`
  holds one numbered+timestamped zstd generation per capture plus a `meta.json` (source path, per-generation
  timestamp/size/sha256). Evidence: docs/decisions/0001-storage-location-and-archive-split.md, docs/diagrams.md.
- D-03 (Archive): Generations are write-once - each is a verbatim copy, never mutated; a grown transcript
  is re-copied as a new generation and a compaction rewrite creates a new generation with the old one kept.
  Evidence: docs/decisions/0001-storage-location-and-archive-split.md, docs/decisions/0011-hooks-and-daemon-knobs.md, docs/diagrams.md.
- D-04 (Capture): Single direct-write path - the sweep and the PreCompact hook both call the same
  archive-writer and write final zstd generations directly; no staging area and no promote step. This
  reads ADR 0011's "archive staging" as the in-memory read-before-return, not a separate on-disk area
  (doc-sync follow-up: record this in an ADR or DESIGN note).
  Evidence: docs/decisions/0011-hooks-and-daemon-knobs.md.
- D-05 (Capture): PreCompact error posture is fail-loud-and-block - if the snapshot write fails, the hook
  exits non-zero (exit 2 / `decision:block`) to veto the compaction so no pre-compaction bytes are lost.
  The hook contract supports a PreCompact veto. Evidence: Claude Code hooks docs (PreCompact exit-code semantics).
- D-06 (Config): Configuration is a TOML file at an XDG path (`~/.config/hindsight/config.toml`) holding
  `base_dir` and daemon knobs; the daemon, CLI, and hooks all read the same file. Evidence:
  docs/decisions/0001-storage-location-and-archive-split.md ("one config key sets the base").
- D-07 (Capture): The watermark is the daemon's own persistent state under the base directory, independent
  of the SQLite index (which does not exist until Phase 3). Evidence:
  docs/decisions/0001-storage-location-and-archive-split.md, .planning/ROADMAP.md (Store is Phase 3).
- D-08 (Capture): Change detection is stat-based per file (mtime + size) - unchanged skips, grown re-copies,
  rewritten opens a new generation. Evidence: docs/diagrams.md, docs/DESIGN.md, docs/decisions/0002-capture-daemon-socket-activation.md.
- D-09 (Capture): The sweep root resolves `$CLAUDE_CONFIG_DIR` else `~/.claude`, walking `/projects/**.jsonl`.
  Evidence: Claude Code claude-directory docs (`CLAUDE_CONFIG_DIR` relocates the whole tree); docs/diagrams.md, README.md.
- D-10 (Deploy): Capture ships as a systemd user unit pair (socket + service, `systemctl --user`), running
  as the single developer user so it reaches that user's `~/.claude` and `$HOME`. Evidence:
  docs/decisions/0002-capture-daemon-socket-activation.md, .planning/PROJECT.md (single-user Linux+systemd).
- D-11 (Capture): The poke is a single byte to a Unix socket that systemd owns, `Accept=no`, one warm daemon
  accumulating pokes; sent via a `hindsight poke` subcommand. The socket path is a fixed convention
  (`$XDG_RUNTIME_DIR/hindsight.sock`) shared by the socket unit and the poke. Evidence:
  docs/decisions/0002-capture-daemon-socket-activation.md, docs/decisions/0011-hooks-and-daemon-knobs.md.
- D-12 (Capture): PreCompact is a synchronous direct copy that reads its input as JSON on stdin
  (`session_id`, `transcript_path`, `cwd`, `trigger`) - not a CLI path argument - and writes a `precompact`
  generation directly. Evidence: Claude Code hooks docs (PreCompact stdin schema).
- D-13 (Scope): Phase 1 implements and registers the PreCompact hook enough to fire in a real compaction
  test; the general SessionStart/SessionEnd poke wiring and the cutover are Phase 6 (MIG-02). Evidence:
  .planning/ROADMAP.md, .planning/REQUIREMENTS.md, docs/STATUS.md.
- D-14 (Scaffold): One Rust binary with subcommands; the Phase 1 surface is `daemon` / `precompact` / `poke`.
  The rusqlite/index subcommands are deferred to later phases. Evidence: .planning/PROJECT.md (one static
  binary with daemon/CLI/MCP subcommands).

## Acceptance criteria

- [ ] Poking the socket (`hindsight poke`, or one byte via `socat`) starts the daemon under socket
      activation, and with no further pokes it self-terminates within ~15 minutes, with both the start and
      the self-exit visible in `journalctl --user`.
- [ ] A sweep copies every new-or-changed `.jsonl` in the transcript tree into the archive as a zstd
      generation under `<base>/archive/<project>/<session-id>/`, including a transcript left by a session
      that fired no SessionEnd hook (a file present in the tree with no poke).
- [ ] Immediately re-running the sweep over an unchanged tree archives zero new generations, and killing a
      sweep partway then re-running it resumes and produces no duplicate generations for already-archived
      sessions.
- [ ] Every archived generation decompresses (`zstd -d`) byte-identical to its source transcript, and all
      archive paths sit under the configured `base_dir` subdirectory, never the data-volume root.
- [ ] Invoking `hindsight precompact` with a PreCompact stdin JSON payload writes a `precompact` generation
      holding the transcript's pre-invocation bytes, and when the archive write is forced to fail the command
      exits non-zero (blocking compaction) rather than exiting 0.
- [ ] In a live Claude Code session, triggering an actual compaction fires the registered PreCompact hook and
      the pre-compaction transcript appears in the archive. (human-verify: needs a live Claude Code compaction)

## Flagged assumptions

- The concrete base-directory name under the data volume is the planner's call, within the hard rule: a
  configurable subdirectory, never the volume root (ARC-02).
- Change detection by mtime + size (D-08) would skip a same-size, same-mtime in-place rewrite; if wrong,
  that edge is a missed capture. A per-file content hash is the fallback if it proves real.
- On `/clear`, the timing of the SessionEnd hook versus the old transcript's final byte flush is undocumented;
  if the SessionEnd-poke sweep races the flush the capture is one sweep late, not lost (the next sweep catches
  it via the watermark).
- The Rust-side systemd socket-activation fd-inheritance mechanism (`sd_listen_fds` / `LISTEN_FDS`, e.g. the
  `listenfd` crate) is an implementation detail for the planner to pin.
- The `~/.claude/projects` project-directory path encoding and the prompt-history decode map referenced in
  docs/decisions/0010-backfill-coldstart-sweep.md need confirming when mapping an archive path back to a
  project; not blocking for capture itself.
