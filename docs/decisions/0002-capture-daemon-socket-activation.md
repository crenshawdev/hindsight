# 0002 - Capture via a socket-activated daemon

Status: accepted

## Context

Capture has to be reliable without me thinking about it, and it has to run inside the retention
window or the transcripts are gone. Two facts shape the mechanism. First, Claude Code's cleanup is
based on file age, so any capture pass inside the window catches a given session no matter how it
ended. Second, the obvious trigger, a `SessionEnd` hook, has undocumented reliability, nothing
promises it fires on a crash, a kill, a closed terminal, or a dropped connection.

Building capture on a trigger that might silently not fire is how you discover months later that
half your history is missing.

## Decision

The primary mechanism is a **sweep**, not a hook. A daemon walks the transcript tree, diffs every
file against a watermark of what it has already archived, and copies whatever is new or changed.
Because the diff is age-based the same way the cleanup is, a sweep anywhere inside the retention
window is a correctness guarantee, not a best-effort.

The daemon is started by **systemd socket activation**. systemd owns a socket that costs nothing
while idle. A session hook writes one byte to it, and that first byte starts the daemon. The daemon
sweeps, idles briefly watching for more pokes, and exits when idle. The next poke starts it again.

The hooks are demoted to a latency optimization. They make capture prompt instead of periodic, but
correctness rides on the sweep, not on them.

## Alternatives considered

**A systemd timer.** Polls on a clock whether or not anything happened. Rejected, it runs work when
there is none and still adds latency when there is, and it is the mechanism I least wanted, a
background job grinding for no reason.

**An always-on resident service.** Supervised and simple, but it sits in memory all day doing
nothing between work blocks, which is exactly the idle-shutdown behavior I did not want to give up.

**A hand-rolled singleton daemon.** The daemon checks a lock or a socket, spawns itself if absent,
detaches. This is essentially what socket activation does, except I would be writing and debugging
the liveness check, the stale-PID handling, the spawn-and-detach, and the restart-on-crash myself.
systemd already does all of it correctly.

## Consequences

On-demand start with self-shutdown, and systemd handles liveness, restart, boot integration, and
logging for free. The session hook collapses to a single line, write one byte to the socket, with
no spawn logic or liveness check of its own.

Because I tend to clear and restart a session around twenty percent context, the daemon stays warm
across a work block on its own, each new session pokes it before the idle timeout, and it shuts down
once I actually walk away. The idle timeout and the poke-versus-watch behavior are tuned in
[ADR 0011](0011-hooks-and-daemon-knobs.md).

The sweep-first design is also what makes backfill free, since the first run is just a sweep with an
empty watermark ([ADR 0010](0010-backfill-coldstart-sweep.md)). The one case a sweep cannot cover,
in-place compaction rewrites, is handled by the `PreCompact` hook, also in
[ADR 0011](0011-hooks-and-daemon-knobs.md).
