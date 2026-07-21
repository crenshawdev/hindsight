---
status: testing
phase: 1
started: 2026-07-21
updated: 2026-07-21
---

## Items

### 1. Socket-activation daemon lifecycle
expected: Poking the socket (hindsight poke, or one byte via socat) starts the daemon under systemd socket activation; with no further pokes it self-terminates within ~15 min, both the start and self-exit visible in journalctl --user. (human-verify: needs a live systemctl --user cycle)
status: pass
first_pass: pass
reported: journalctl: acquired listening socket from systemd, Spawned, sweep new_generations=1461 then 0, Idle timeout self-terminating at 5s

### 2. Sweep archives every new-or-changed transcript
expected: A sweep copies every new-or-changed .jsonl in the transcript tree into the archive as a zstd generation under <base>/archive/<project>/<session-id>/, including a transcript left by a session that fired no SessionEnd hook (a file present with no poke).
status: pass
first_pass: pass
source: verifier
evidence: Daemon sweep over temp tree archived 2/2: archive/projA/sess1/0000.zst and archive/projA/sess1/subagents/agent-xyz/0000.zst; no bogus subagents project. sweep.rs:31-101 walks projects/**/*.jsonl via archive_key, poke-independent (CAP-01).

### 3. Sweep idempotence and resumability
expected: Immediately re-running the sweep over an unchanged tree archives zero new generations; killing a sweep partway then re-running resumes and produces no duplicate generations for already-archived sessions.
status: pass
first_pass: pass
source: verifier
evidence: Immediate second sweep logged new_generations=0; sweep::tests::resume_after_crash_before_watermark_save_writes_no_duplicate passed; sha-dedup archive.rs:133-137; watermark saved per-file sweep.rs:97.

### 4. Byte-identical decompression and ARC-02 containment
expected: Every archived generation decompresses (zstd -d) byte-identical to its source transcript, and all archive paths sit under the configured base_dir subdirectory, never the data-volume root.
status: pass
first_pass: pass
source: verifier
evidence: zstd -d of both generations cmp-equal to source; meta.json sha256==source sha256; all paths under base/archive. ARC-02 root-reject config.rs:65-76 + runtime guards config.rs:142, archive.rs:249-256; load_errors_when_base_dir_is_root passes.

### 5. PreCompact writes generation and fails loud on write failure
expected: hindsight precompact fed a PreCompact stdin JSON payload writes a precompact generation holding the transcript's pre-invocation bytes; when the archive write is forced to fail the command exits non-zero (blocking compaction) rather than exiting 0.
status: pass
first_pass: pass
source: verifier
evidence: CLI success wrote archive/projB/sessX/0000.zst (kind=precompact, byte-identical), exit 0; read-only base -> exit 2; malformed stdin -> exit 2. main.rs:46-52 maps error to ExitCode::from(2); precompact.rs:32-51.

### 6. Live compaction fires the registered PreCompact hook
expected: In a live Claude Code session, triggering an actual compaction fires the registered PreCompact hook and the pre-compaction transcript appears in the archive. (human-verify: needs a live Claude Code compaction)
status: pass
first_pass: fail
reported: Live /compact in session 095cee31 (-data-projects-hephaestus) wrote a new precompact generation; zstd -d = 42 lines valid transcript JSON, 75012 bytes, kind=precompact
severity: major
cause: Hook fired and fail-loud veto worked, but the source transcript did not exist on disk at hook-fire time (fresh session, /compact run before the transcript was flushed). precompact treats ENOENT-on-source like any write failure and exits 2, vetoing the compaction even though there are zero pre-compaction bytes to protect. Code path is correct: the same payload run manually once the file existed wrote a byte-identical precompact generation, exit 0. Root cause is D-05 fail-loud not distinguishing 'source absent/unflushed -> nothing to lose, should allow' from 'source exists but archive write failed -> veto'.
fix: e149267, retest

## Summary

total: 6
passed: 6
failed: 0
pending: 0
skipped: 0
blocked: 0
reworked: 1
