---
phase: 1
status: complete
completed: 2026-07-21
---

# Phase 1: Capture - Summary

A systemd socket-activated `hindsight` daemon that archives every session transcript as a write-once,
zstd-compressed, sha-verified generation, plus the repo scaffold and the one-binary skeleton
(`daemon` / `precompact` / `poke`).

## What shipped

- One-binary `hindsight` CLI with `daemon` / `precompact` / `poke` subcommands - `src/main.rs` (clap
  derive, tracing-subscriber, precompact exit-2 mapping).
- TOML config (`base_dir` + daemon knobs), required with an ARC-02 root-reject guard and the shared
  `archive_key` path->coordinates helper - `src/config.rs`.
- Verbatim zstd archive writer: write-once generations, sha256 dedup and next-index derived from
  on-disk files, exclusive index claim, `meta.json` sidecar - `src/archive.rs`.
- Persistent watermark ((mtime, size, sha), temp+rename save) and the full-tree sweep that walks
  `projects/**/*.jsonl`, maps nested subagent transcripts under their real session, skips unchanged
  files, and resumes after an interrupted run - `src/watermark.rs`, `src/sweep.rs`.
- Socket-activation daemon lifecycle: listenfd fd-take (systemd) else standalone bind, resweep-on-poke
  with no concurrent sweeps, idle self-exit - `src/daemon.rs`; one-byte poke - `src/poke.rs`.
- PreCompact subcommand: parse stdin JSON, snapshot pre-invocation bytes as a `precompact` generation,
  fail-loud exit 2 (veto compaction) on any read/parse/write failure - `src/precompact.rs`.
- systemd user units and README Install + PreCompact-hook sections - `systemd/hindsight.{socket,service}`,
  `README.md`.

## Commits

| Plan | Task | Commit | Description |
|---|---|---|---|
| 1 | 1 | d5ae935 | Scaffold `hindsight` binary + config module (8 config tests: ARC-02 root reject, `archive_key` top-level/nested/`..`) |
| 1 | 2 | 72cf2c5 | Socket-activation daemon, poke, systemd units (standalone lifecycle + systemd fd-inheritance verified) |
| 1 | 3 | 1ffc855 | Verbatim zstd archive writer + `meta.json` (risk_surface gate PASS after 2 blocker fixes; 9 archive tests) |
| 1 | 4 | bf195aa | Watermark + full-tree sweep (4 sweep tests; real-subtree `zstd -d` byte-identical incl. nested subagent) |
| 1 | 5 | 9653522 | PreCompact subcommand, fail-loud veto (risk_surface gate PASS; CLI exit 0 + byte-identical, exit 2 on forced failure) |
| 1 | 6 | 51d4b2c | Register PreCompact hook (README; verify is human-verify - live compaction) |

Full build clean, `cargo test` 23/23 green, working tree clean.

## Deviations

- [deviation] Generation filename changed from the plan's cosmetic `NNNN-ts-kind.zst` to index-only
  `NNNN.zst` (timestamp/kind moved into `meta.json`) - commit 1ffc855. Required by risk_surface blocker
  1: the atomic index claim must collide regardless of timestamp/kind so two concurrent writers
  (sweep vs PreCompact, the D-04 shared path) cannot both take the same index.
- [deviation] Temp generation created with exclusive `create_new` + safe stale-orphan reclaim instead
  of `std::fs::write` - commit 1ffc855. Required by risk_surface blocker 2: a truncating re-open of a
  PID-reused stale temp hardlinked to a committed generation would mutate write-once data.
- [deviation] Delivered as one ordered PLAN, not the CONTEXT's multi-plan split - adjudicated in the
  plan itself (shared-file dependency across the slices); no action.
- [deviation] `main.rs` already mapped the `precompact` error to exit code 2 (added in Task 2), so
  Task 5 needed no `main.rs` change despite the plan listing it.
- [deviation, verification] `systemd-socket-activate` did not propagate an exported `XDG_CONFIG_HOME`
  to the child; `-E XDG_CONFIG_HOME=...` was needed to exercise the listenfd fd-take. Test-harness
  detail, not a daemon defect (production reads config from the real XDG path).
- [deviation, verification] A first real-subtree sweep reported 0 archived because it ran a stale
  `debug/hindsight` binary (`cargo test` does not rebuild the bin target); after `cargo build` it
  archived both transcripts byte-identically. Process artifact, no code bug, recorded so the false
  "0" is visible.

## Open items

Human-verify (could NOT run headlessly - not counted as passed; run before marking the phase verified):

- **Success criterion 1 - full systemd cycle.** `systemctl --user daemon-reload && start
  hindsight.socket`, then a real poke, and confirm the daemon start + ~idle self-exit lines in
  `journalctl --user`. Confirmed headlessly instead: units present and well-formed; daemon acquires a
  systemd-passed fd (via `systemd-socket-activate`, logging "acquired listening socket from systemd");
  standalone-bind lifecycle (Spawned, resweep-on-poke, idle self-exit, exit 0).
- **Success criterion 5 - live compaction.** Trigger an actual Claude Code compaction with the README
  hook registered and confirm a new `precompact` generation for that session appears and `zstd -d`s to
  the pre-compaction bytes. Confirmed headlessly instead: `hindsight precompact` fed a stdin payload
  writes a byte-identical `precompact` generation and exits 0; exits 2 (vetoing) when the write is
  forced to fail; the documented `settings.json` shape matches the code's stdin contract.

Grounded review follow-ups (not fixed this phase):

- **Durability (fsync).** The temp generation is not fsync'd (nor is the parent dir) before the
  hard_link claim, and close errors are not checked, so a power loss / late ENOSPC/EIO could leave a
  committed generation non-durable or partial. Grounded to a follow-up: archive durability rests on
  the backup layer, and fsync on the synchronous PreCompact path is a latency tradeoff to decide.
- **Resilience.** `write_generation`'s dedup loop calls `generation_sha` on every existing generation,
  so one pre-existing corrupt/undecodable `NNNN.zst` hard-fails all future writes for that session.
  Fail-loud is defensible for a ground-truth store; quarantine-and-continue is a follow-up decision.
- **ARC-02 hardening.** The escape check in `resolve_session_dir` is lexical (`starts_with`), not
  canonicalized; a pre-existing symlink inside `archive_dir` could redirect a write. Low severity - the
  daemon creates no symlinks in its own tree.
- **`update_meta` concurrency.** Unsynchronized read-modify-write; two concurrent writers can clobber
  each other's `meta.json` entries. Bounded - `meta.json` is a rebuildable sidecar and dedup/index
  derive from the filesystem.
- **PreCompact trust model.** The cwd-fallback trusts the payload `cwd`/`session_id` to choose the
  archive coordinate; a crafted local payload could file a spurious generation under a chosen
  coordinate within `archive_dir` (no escape, no overwrite). Low severity under the local same-user
  trust model; a follow-up could validate more tightly or fail loud instead of the cwd-fallback.
- **Standalone bind unlink (advisory diff review).** `daemon.rs` unconditionally unlinks the socket
  path before the standalone bind, so a second standalone daemon detaches the first (split-brain).
  Dev-only path - production uses systemd socket activation and D-11 mandates one warm daemon; a
  follow-up could bind-first and unlink only a proven-stale socket.
- **Watermark temp name (advisory diff review).** `watermark.json.tmp` is a fixed temp name; two
  daemons sharing `base_dir` would race on it. Same single-daemon invariant as above.
- **`base_dir` absoluteness (advisory diff review).** Config does not require `base_dir` to be
  absolute, so a relative value would resolve against cwd (differs between manual and service runs).
  Cheap hardening (`bail` unless absolute); worth doing before the daemon ships as a service.
- **Doc-sync (standing rule 1).** The two filename/write-path deviations above and the nested-transcript
  archive layout (`<project>/<session-id>/<sub-path>/`) + the D-04 single-direct-write decision need an
  ADR 0001 amendment / DESIGN note + diagram update. Deferred to the docs pass (this phase's scope was
  code).

## Goal check

The six commits deliver the phase goal: a socket-activated daemon that archives every transcript
verbatim before cleanup, plus the scaffold and the three-subcommand skeleton. The riskiest seam -
systemd fd inheritance via `listenfd::take_unix_listener(0)` - is proven end to end (`daemon.rs:~40`,
confirmed via `systemd-socket-activate` logging "acquired listening socket from systemd"), and the
write-once archive path is proven byte-identical: `cargo test archive::` and a real
`~/.claude/projects` subtree sweep both `zstd -d` back to source, and nested subagent transcripts
(~57% of the live tree) file under their real session, not a bogus `subagents` project (`sweep.rs`,
verified). Fail-loud PreCompact is confirmed at the CLI (exit 0 + generation on success, exit 2 vetoing
on forced write failure). The archive writer survived a three-round adversarial risk_surface gate that
caught and fixed two real concurrency blockers (same-index collision; stale-temp truncation of
write-once data). Two of the five success criteria are NOT machine-closed and are the honest gap: the
full `systemctl --user` socket-activation cycle (criterion 1) and a live Claude Code compaction firing
the hook (criterion 5) are human-verify only - the code paths under them are tested headlessly, but the
live integration must be run by hand before the phase is marked verified. Nothing else in the goal is
missing.
