# 0013 - Embed delivery: hook-triggered, always-GPU, detached

Status: accepted

Supersedes the Phase-4 delivery specifics in [ADR 0004](0004-embedder-and-gpu-scheduling.md): the
systemd timer trigger, the `nvidia-smi` GPU-busy defer, and the CPU fallback are all reversed here. The
embedder choice (qwen3-embedding:8b via Ollama, 4096 dimensions, the dimension pin), the transport
(`ureq` to `/api/embed`), and the resumable `embed_ledger` from that ADR still stand.

## Context

Phase 4 built embedding as a deferrable batch a systemd timer fired on a schedule, running on the GPU
when a poll of the card said it was free and falling back to the CPU when a game was holding it. Two
things about that aged badly once the rest of the system was real.

The timer is the wrong clock. Nothing new lands to embed except when a session ends, and a session
ending is exactly a hook firing. A fixed-interval timer either lags a just-finished session until its
next tick or burns wakeups draining an already-drained store. The event that creates work should be the
event that triggers the drain.

The CPU fallback is a quality trap disguised as resilience. An 8B model on the CPU is slow enough that a
"fallback" run is a different, worse product, and the `nvidia-smi` polling, the defer budget, and the
CPU request path were a large amount of machinery whose only job was to sometimes produce a slower
answer nobody was waiting on. Embedding lag is already harmless because exact and keyword recall are
live the instant a session lands. If the GPU is busy, the right move is to wait for the next hook, not
to limp through on the processor.

## Decision

Embedding is triggered by the session-lifecycle hooks, runs unconditionally on the GPU, and runs as a
detached process.

**Hook-triggered, detached.** A session hook fires `hindsight embed --detach`. The binary self-detaches
with `setsid` into a new session and process group, nulls its three standard streams, and the parent
returns at once so the hook is not blocked and Claude Code's reaping of the hook process group does not
take the drain down with it. This is distinct from `hindsight poke`, which only writes one byte to the
capture-daemon socket. The drain runs as this detached process, never inside the capture daemon, so the
daemon's 15-minute idle self-terminate contract is untouched.

**Always GPU, no fallback.** Every embed request pins `options.num_gpu` high enough to force full GPU
offload. There is no CPU path and no placement decision to make. Deleting the request option entirely
would hand placement to Ollama's own heuristic, which partial-offloads layers to the CPU under video-
memory pressure, which is the exact slow path this reverses, so the pin stays as one unconditional
value. An embed either runs fully GPU-resident or Ollama errors, and an error is caught per unit.

**Single-flight.** A drain takes a non-blocking advisory `flock` on a lock file under `state_dir()`
before it assembles or embeds. A second invocation that finds the lock held logs that a drain is already
running and exits cleanly, never double-embedding. The lock is scoped to the open file descriptor, so
the kernel releases it on any process exit, crash included, with no PID-file staleness to reconcile.

**Continue-on-error.** A single Ollama failure is caught, recorded on the unit's `embed_ledger` row as
`status = 'failed'` with the error string and an incremented attempt count, and the drain proceeds to
the next unit rather than aborting the whole run. A unit that keeps failing is retired after a small
attempts cap so it stops burning a request on every drain.

**Observable.** Each drain opens an `embed_run` record carrying its start time, a heartbeat refreshed
around every unit, its pid, and running counts, and marks it done at the end. `hindsight embed --status`
reads that record plus the per-unit ledger and reports the drain state: running with progress while a
heartbeat is fresh, stalled when a run's heartbeat has gone stale or its process died, done (or done
with N failed) once a run is terminal, and not-yet-embedded on an empty store.

## Accepted divergence

A running drain does not chase units that land after it started. The set of units to embed is assembled
once at drain start, and a session that finishes mid-drain is picked up by the next hook-fired
invocation, not by the drain in flight. Single-flight still prevents any double-embed, and the lag is
bounded by the next session event, so this is the accepted behavior rather than a live-updating queue.

## Backfill is a full drain with hooks off

There is no separate backfill mode. `hindsight load` wipes the vector table and the ledger, so the next
`hindsight embed` drain is already a full-corpus embed. The one-time cold start is: raise the retention
window, load the corpus, and run `hindsight embed` directly with the hooks not yet wired, which fills
the store; then wire the hooks so ongoing sessions embed incrementally. Removing the old timer and
service files from the tree does not stop an already-installed unit, so the flip also disables the
legacy timer (`systemctl --user disable --now hindsight-embed.timer`, whatever scope it was installed
under) before hooks take over, otherwise a still-enabled timer keeps firing drains alongside the hook
path.

## Consequences

The `nvidia-smi` polling, the defer loop, the CPU request path, the four GPU-scheduling config knobs,
and the timer and service units are all gone, which is a large net deletion. Delivery is simpler and its
clock now matches the workload: work is created by a session ending and drained by that same event.
Recall still degrades gracefully because exact and keyword search are current the instant a session
lands and semantic search catches up on the next drain. The one thing given up is CPU as a floor, which
was only ever a slower answer to a question nobody was blocking on.
