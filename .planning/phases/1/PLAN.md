---
phase: 1
plan: 1
requirements: [CAP-01, CAP-02, CAP-03, CAP-04, ARC-01, ARC-02]
files:
  - Cargo.toml
  - .gitignore
  - src/main.rs
  - src/config.rs
  - src/archive.rs
  - src/watermark.rs
  - src/sweep.rs
  - src/daemon.rs
  - src/poke.rs
  - src/precompact.rs
  - systemd/hindsight.socket
  - systemd/hindsight.service
  - README.md
---

# Phase 1: Capture - Plan

## Goal

A systemd socket-activated daemon that archives every session transcript verbatim before Claude
Code's cleanup sweep can take it, plus the repo scaffold and the one-binary skeleton
(`daemon` / `precompact` / `poke` subcommands).

## Must be true when done

- Poking `$XDG_RUNTIME_DIR/hindsight.sock` (via `hindsight poke` or one raw byte) starts the daemon
  under systemd socket activation, and with no further pokes it self-terminates after the idle
  timeout, both the start and the self-exit visible in `journalctl --user`.
- A sweep copies every new-or-changed `.jsonl` under the transcript tree into
  `<base>/archive/<project>/<session-id>/[<sub-path>/]` as a zstd generation (a nested subagent/workflow
  transcript keeps its sub-path and stays under its real project/session, not a bogus `subagents`
  project), including a transcript whose session fired no end hook (a file present with no poke).
- Re-running a sweep over an unchanged tree writes zero new generations, and a sweep killed partway
  then re-run resumes and produces no duplicate generations for already-archived sessions.
- Every archived generation decompresses (`zstd -d`) byte-identical to its source transcript, its
  `meta.json` records a matching sha256, and every archive path sits under the configured `base_dir`
  subdirectory, never the data-volume root.
- `hindsight precompact` fed a PreCompact stdin JSON payload writes a `precompact` generation holding
  the transcript's pre-invocation bytes, and exits non-zero (vetoing compaction) when the archive
  write is forced to fail.
- The registered PreCompact hook fires on a live Claude Code compaction and the pre-compaction
  transcript lands in the archive.

## Context

Locked decisions bind this plan: D-01 zstd compression; D-02 per-session archive layout with a
numbered+timestamped generation plus `meta.json` (source path, per-generation timestamp/size/sha256);
D-03/ARC-01 write-once generations; D-04 single direct-write path shared by sweep and PreCompact (no
staging/promote); D-05 PreCompact fails loud and blocks (exit 2) on write failure; D-06 TOML config
at `~/.config/hindsight/config.toml` holding `base_dir` and daemon knobs, read by all subcommands;
D-07 watermark is the daemon's own persistent state under `base_dir`, independent of any SQLite index;
D-08 stat-based (mtime+size) change detection; D-09 sweep root resolves `$CLAUDE_CONFIG_DIR` else
`~/.claude`, walking `projects/**.jsonl`; D-10 systemd user unit pair; D-11 poke is one byte to a
systemd-owned Unix socket at `$XDG_RUNTIME_DIR/hindsight.sock`, `Accept=no`, one warm daemon; D-12
PreCompact reads its input as JSON on stdin; D-13 Phase 1 registers PreCompact enough to fire in a
real compaction test (the general SessionStart/SessionEnd cutover is Phase 6); D-14 one binary with
`daemon`/`precompact`/`poke` only. Out of scope for this phase: normalize, records, grain,
secret-scrub, SQLite/FTS5/sqlite-vec, embeddings, query/MCP/CLI-search, the empty-watermark backfill
run and the SessionStart/SessionEnd cutover. The archive here is a pure verbatim copy - no scrubbing.

Toolchain confirmed present: cargo 1.97, `systemctl`, `socat`, `zstd`, and a live `~/.claude/projects`
tree. Crates: `clap` 4, `listenfd` 1, `zstd` 0.13, plus `serde`/`toml`/`serde_json`/`sha2`/`anyhow`/
`tracing`. This is a docs-only repo; all source below is created from scratch.

## Tasks

### Task 1: Scaffold the binary and the config module

- **Files:** Cargo.toml, .gitignore, src/main.rs, src/config.rs
- **Action:** Create a Rust binary crate `hindsight`. In Cargo.toml add dependencies `clap` (v4,
  `derive` feature), `serde` (`derive`), `toml`, `serde_json`, `zstd` (v0.13), `sha2`, `listenfd`
  (v1), `anyhow`, `thiserror`, and `tracing` + `tracing-subscriber`. In src/main.rs define a `clap`
  derive CLI with exactly three subcommands - `daemon`, `precompact`, `poke` - dispatching to
  functions in the respective modules; initialize `tracing-subscriber` at startup so daemon logs
  reach the journal. Declare modules `config`, `archive`, `watermark`, `sweep`, `daemon`, `poke`,
  `precompact`; for this task the archive/watermark/sweep/daemon/poke/precompact entry points may be
  minimal stubs that return `Ok(())` or `todo!()`-free placeholders that compile. In src/config.rs
  define a `Config` struct holding `base_dir: PathBuf` and daemon knobs `idle_timeout_secs: u64`
  (default 900) and a `load()` that reads TOML from `$XDG_CONFIG_HOME/hindsight/config.toml` else
  `~/.config/hindsight/config.toml`; `base_dir` is required with a clear error if the file or key is
  missing (never guess a default under the volume root). Add a `Config::archive_dir()` returning
  `base_dir/archive` and `Config::state_dir()` returning `base_dir/state`. Enforce ARC-02: `load()`
  errors if `base_dir` resolves to a filesystem root or has no parent. Add a
  `Config::archive_key(sweep_root, source_path) -> (project, session_id, sub_path)` helper that maps a
  transcript path to its archive coordinates from the path *relative to `sweep_root/projects`*: the first
  segment is `<project>`, the second is `<session-id>` (strip a trailing `.jsonl`), and any remaining
  segments form `<sub-path>` (empty for a top-level transcript). Reject a segment that is `.`, `..`,
  empty, or contains a path separator with a clear error, and error if the key would resolve outside
  `archive_dir()` (ARC-02 runtime guard, not a debug-only assert). Both the sweep and PreCompact call this
  one helper so the path->coordinates mapping is identical on both write paths. Append a `/target` ignore to
  .gitignore. Do not assert the built binary path anywhere - the target dir is redirected in this
  environment; invoke via `cargo run --`.
- **Verify:** `cargo build` succeeds; `cargo run -- --help` lists `daemon`, `precompact`, and `poke`;
  `cargo test config::` passes unit tests asserting `load()` errors when `base_dir` is a root path,
  returns `base_dir/archive` from `archive_dir()`, and that `archive_key` maps
  `projects/<p>/<s>.jsonl` to `(<p>, <s>, "")`, maps `projects/<p>/<s>/subagents/agent-<id>.jsonl` to
  `(<p>, <s>, subagents/agent-<id>)`, and rejects a path containing a `..` segment.

### Task 2: Socket-activation daemon skeleton, poke subcommand, and systemd units

- **Files:** src/daemon.rs, src/poke.rs, src/main.rs, systemd/hindsight.socket, systemd/hindsight.service
- **Action:** Implement the end-to-end daemon lifecycle as a runnable tracer bullet. In src/daemon.rs
  acquire the listening Unix socket from systemd via `listenfd::ListenFd::from_env().take_unix_listener(0)`;
  if no fd was passed (standalone run), fall back to binding `$XDG_RUNTIME_DIR/hindsight.sock` directly,
  unlinking a stale socket file first (only on this non-systemd fallback path, so a leftover socket does
  not cause `EADDRINUSE`), so the daemon is testable without systemd. Run the lifecycle from docs/diagrams.md "Daemon
  lifecycle": on start log a Spawned line, run a sweep (call `sweep::run(&config)` - which is still the
  stub from Task 1, logging a swept-count), then enter an Idle loop that accepts connections on the
  socket, draining one byte per poke; each poke (or a dirty flag set during a sweep) triggers one
  re-sweep with no concurrent sweeps; after `idle_timeout_secs` with no poke, log a self-exit line and
  return so the process exits. Use a non-blocking/`poll`-with-timeout accept (or set a read timeout) so
  the idle timer fires without a poke. In src/poke.rs implement `poke`: connect to
  `$XDG_RUNTIME_DIR/hindsight.sock` and write one byte, printing a clear error if the socket is absent.
  Create systemd/hindsight.socket with `[Socket] ListenStream=%t/hindsight.sock`, `Accept=no`, and
  `[Install] WantedBy=sockets.target`; create systemd/hindsight.service with `[Service] Type=simple`
  and `ExecStart=` invoking the installed `hindsight daemon` (document in README.md that ExecStart must
  point at the built binary path and units install under `~/.config/systemd/user/`). Add a short
  "Install (Phase 1)" section to README.md covering `systemctl --user daemon-reload`, enabling the
  socket, and setting a low `idle_timeout_secs` for testing. `%t` in a user unit expands to
  `$XDG_RUNTIME_DIR`, matching the poke and fallback-bind path.
- **Verify:** Set `idle_timeout_secs = 5` in a test config, install both units under
  `~/.config/systemd/user/` with ExecStart pointing at the `cargo run`-built binary, run
  `systemctl --user daemon-reload && systemctl --user start hindsight.socket`, then `hindsight poke`
  (which writes exactly one byte per D-11; if using `socat`, send one byte, e.g.
  `printf 'x' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/hindsight.sock`, not an empty connect); `journalctl --user -u
  hindsight.service --since "1 min ago"` shows the daemon Spawned line and, ~5s after the last poke,
  the self-exit line.

### Task 3: Verbatim zstd archive writer with generations and meta.json

- **Files:** src/archive.rs
- **Action:** Implement the single direct-write archive primitive that both the sweep and PreCompact
  call (D-04, no staging). Expose `write_generation(config, project, session_id, sub_path, source_path, kind)`
  where `kind` is `Sweep` or `Precompact` and `sub_path` (possibly empty) locates a nested transcript
  under the session. It reads the source file's current bytes into memory,
  computes their sha256, ensures `base_dir/archive/<project>/<session-id>/<sub-path>/` exists. Derive both
  the dedup check and the next generation index from the actual generation files on disk in that directory
  (the filesystem is the source of truth; `meta.json` is a rebuildable sidecar): if any existing generation
  already carries a matching sha256 it returns without writing (write once, no duplicate); otherwise the
  next index is one past the highest index present as a file. This survives a crash between the generation
  write and the `meta.json` update, and a concurrent sweep/PreCompact for the same session, without reusing
  an index or writing a duplicate. It writes a new generation whose filename encodes that zero-padded
  index, a UTC timestamp, and the kind (e.g. `0002-20260720T221500Z-precompact.zst`),
  zstd-compressing the verbatim bytes. Write to a temp file in the same directory then claim the final name
  via an exclusive (`O_EXCL`) create/link so two writers cannot both take the same index; on an index
  collision, recompute the next index and retry, so an interrupted write leaves no partial generation.
  Maintain a `meta.json` in the
  session directory recording `source_path` and, per generation, its filename, timestamp, uncompressed
  size, and sha256; update it via temp-file-plus-rename as well. Never mutate an existing generation
  (ARC-01). Return an error (not a debug-only assert) if the resolved output path is not under
  `config.archive_dir()`, so ARC-02 holds in release builds.
- **Verify:** `cargo test archive::` passes tests that: write a known-bytes source, then `zstd -d` (or
  the zstd crate) the produced generation and assert it is byte-identical to the source; assert the
  `meta.json` sha256 for that generation equals the sha256 of the source bytes; assert every produced
  path is under `base_dir/archive`; assert a second `write_generation` call over unchanged source
  bytes produces no new generation file; and assert that when a generation file exists on disk but is
  absent from `meta.json` (a simulated crash between the two renames), a re-run over unchanged bytes
  neither reuses that index nor writes a duplicate.

### Task 4: Watermark and full-tree sweep, wired into the daemon

- **Files:** src/watermark.rs, src/sweep.rs, src/daemon.rs
- **Action:** In src/watermark.rs implement the daemon's own persistent state (D-07) at
  `config.state_dir()/watermark.json`, mapping each transcript file path to its last-seen `(mtime,
  size)` and last-archived sha256; provide `load`, `get`, `record`, and `save` with save done via
  temp-file-plus-rename. In src/sweep.rs implement `run(config)`: resolve the sweep root as
  `$CLAUDE_CONFIG_DIR` else `~/.claude`, walk `projects/**/*.jsonl`, and for each file compute
  `(project, session-id, sub-path)` via `config::archive_key` (D-09) rather than `parent-dir + stem`: a
  top-level `projects/<project>/<session>.jsonl` maps to an empty sub-path, while a nested
  `projects/<project>/<session>/subagents/agent-<id>.jsonl` (~57% of the live tree) keeps
  `subagents/agent-<id>` as its sub-path and stays grouped under its real project/session instead of a
  bogus `subagents` project. Skip a file
  whose current `(mtime, size)` equals its watermark entry (D-08 unchanged -> skip); otherwise call
  `archive::write_generation(config, project, session_id, sub_path, path, Sweep)`, then `record` the new `(mtime, size)` and sha256 into the
  watermark and `save` after each file so an interrupted sweep resumes without redoing completed files
  and the writer's sha-dedup prevents any duplicate generation on resume. The sweep archives every
  changed file unconditionally - it does not depend on any poke or end-hook, so a transcript left by a
  crashed session is captured (CAP-01). Replace the Task 2 stub call in src/daemon.rs so the daemon's
  sweep step and re-sweep-on-poke invoke `sweep::run`. Return a count of newly archived generations for
  the daemon to log.
- **Verify:** `cargo test sweep::` passes integration tests over a temp tree with a temp `base_dir`:
  (a) a first sweep archives one generation per `.jsonl` including a file that received no poke, and a
  nested `projects/<p>/<s>/subagents/agent-<id>.jsonl` archives under
  `archive/<p>/<s>/subagents/agent-<id>/` (not under a `subagents` project); (b) an
  immediate second sweep over the unchanged tree archives zero new generations; (c) truncating the
  watermark save after the first of two files (simulating a kill) then re-running produces exactly one
  generation per session with no duplicates. Additionally, point a config `base_dir` at a temp dir and
  `HINDSIGHT`-run a sweep against a copy of a real `~/.claude/projects` subtree, then confirm each
  archived generation `zstd -d`s byte-identical to its source.

### Task 5: PreCompact subcommand with fail-loud veto

- **Files:** src/precompact.rs, src/main.rs
- **Action:** Implement `precompact`: read the PreCompact payload as JSON on stdin (D-12) with fields
  `session_id`, `transcript_path`, `cwd`, `trigger`; derive `(project, session-id, sub-path)` from
  `transcript_path` via the same `config::archive_key` helper the sweep uses (so a subagent-triggered
  compaction files correctly), falling back to the payload `session_id` when the path is ambiguous, then call
  `archive::write_generation(config, project, session_id, sub_path, transcript_path, Precompact)` to snapshot its current
  pre-invocation bytes as a `precompact` generation. On any failure to read stdin, parse the payload,
  or write the generation, print a diagnostic to stderr and exit with code 2 (D-05 fail-loud-and-block,
  vetoing the compaction) rather than exiting 0; on success exit 0. Do not update the daemon watermark
  from this path - PreCompact writes directly and the next sweep reconciles via the writer's sha-dedup.
  Ensure `main.rs` maps the `precompact` subcommand's returned exit code (2 on failure) through the
  process exit status rather than letting `anyhow` produce a default code-1 error.
- **Verify:** `cargo test precompact::` passes a test that pipes a JSON payload pointing at a temp
  transcript and asserts a `precompact`-kind generation appears under `base_dir/archive` holding the
  pre-invocation bytes; and a CLI check where `printf '{"session_id":"s","transcript_path":"<temp
  .jsonl>","cwd":".","trigger":"manual"}' | cargo run -- precompact` exits 0 and writes the generation,
  while pointing `base_dir` at a read-only directory makes the same invocation exit 2.

### Task 6: Register the PreCompact hook and verify a live compaction

- **Files:** README.md
- **Action:** Add a "PreCompact hook (Phase 1)" section to README.md documenting the Claude Code
  settings entry that registers `hindsight precompact` as the `PreCompact` hook command (the hook
  passes its payload on stdin, matching Task 5), including where the user places it in their Claude
  Code `settings.json` `hooks.PreCompact` array and that the command must be the installed binary path.
  This is the Phase 1 PreCompact registration only; the general SessionStart/SessionEnd poke wiring and
  the prior-memory-tool cutover are Phase 6 (D-13) and must not be added here.
- **Verify:** human-verify (needs a live Claude Code compaction): with the hook registered per the
  README, trigger an actual compaction in a live Claude Code session and confirm a new `precompact`
  generation for that session appears under `<base>/archive/<project>/<session-id>/` and `zstd -d`s to
  the pre-compaction transcript bytes.

## Notes

- Plan-shape deviation: the CONTEXT directive asked to split Phase 1 into multiple plans
  (scaffold+lifecycle / archive+watermark+sweep / precompact). The file-independence test overrides it
  here - every proposed slice writes-then-edits shared files (`src/main.rs` dispatch, `src/daemon.rs`
  loop, `src/archive.rs` writer used by both sweep and precompact), so the slices are not independent
  and cannot be parallel or non-overlapping plans. This is delivered as one ordered PLAN.md,
  skeleton-first: Task 2 proves the riskiest seam (systemd fd inheritance via `listenfd`) end to end on
  commit 2, and Tasks 3-6 add depth to a spine that already activates and idles.
- Review adjudication (plan trigger, adjudicated gate; gemini dropped - provider unavailable): applied
  five grounded findings from the claude-subagent and openai reviewers. The path->(project, session,
  sub-path) mapping now handles the ~57% of live transcripts that are nested (was a blocker: naive
  parent-dir+stem misfiled them under a `subagents` project). The archive writer derives its dedup and
  next index from on-disk generation files with an exclusive create, closing the crash-between-renames
  and concurrent sweep/PreCompact duplicate windows. ARC-02 is a release runtime guard plus path-segment
  sanitization, not a debug assert. The non-systemd fallback unlinks a stale socket, and the Task 2 poke
  check sends a real one byte. Killed one finding: openai's "CAP-01 not delivered without a periodic
  sweep" - the SessionStart/SessionEnd poke cutover is deliberately Phase 6 (D-13), and Phase 1's CAP-01
  is met by the poke-triggered full-tree sweep vs watermark, so the manual-poke-until-Phase-6 gap is
  roadmap phasing, not a plan defect.
- Doc-sync follow-up: the nested-transcript archive layout (`<project>/<session-id>/<sub-path>/`) extends
  D-02's per-session directory shape; record it in ADR 0001 or a DESIGN note alongside the D-04
  direct-write follow-up.
- Flagged assumptions carried forward: `base_dir` is a full path the user sets in config (no volume-root
  guessing); the archive layout under it is `archive/` and daemon state is `state/` (planner's call per
  the ARC-02 boundary). Change detection is mtime+size (D-08); a same-size same-mtime in-place rewrite
  would be skipped - the content-hash fallback is noted but not built unless it proves real. The
  `listenfd` `take_unix_listener(0)` fd-inheritance path is the pinned socket-activation mechanism.
