//! Socket-activation daemon lifecycle (ADR 0002, docs/diagrams.md "Daemon
//! lifecycle").
//!
//! The daemon acquires its listening Unix socket from systemd (socket
//! activation) or, when run standalone, binds the socket itself. On start it
//! logs a Spawned line, runs one full-tree sweep, then enters an idle loop:
//! each poke drains a byte and triggers a re-sweep, and after `idle_timeout_secs`
//! with no poke it logs a self-exit line and returns so the process exits.

use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use listenfd::ListenFd;
use tracing::{info, warn};

use crate::config::Config;
use crate::sweep;

/// How often the idle loop wakes to check for pokes and the idle deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The fixed socket-path convention shared by the socket unit, the standalone
/// fallback bind, and the poke subcommand (D-11): `$XDG_RUNTIME_DIR/hindsight.sock`.
pub fn socket_path() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|x| !x.is_empty())
        .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set; cannot locate the daemon socket"))?;
    Ok(PathBuf::from(dir).join("hindsight.sock"))
}

pub fn run() -> Result<()> {
    let config = Config::load()?;
    let listener = acquire_listener()?;
    listener
        .set_nonblocking(true)
        .context("setting the daemon socket non-blocking")?;

    info!(
        idle_timeout_secs = config.idle_timeout_secs,
        "Spawned: hindsight capture daemon"
    );

    // Sweep once on start (catches transcripts left by sessions that fired no poke).
    run_sweep(&config);

    let idle_timeout = Duration::from_secs(config.idle_timeout_secs);
    let mut last_activity = Instant::now();

    loop {
        // Drain every queued poke without blocking. Pokes that arrived during a
        // sweep sit in the socket backlog (Accept=no, one warm daemon); draining
        // them and re-sweeping once is the "dirty flag" semantic from the diagram.
        let mut poked = false;
        loop {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    drain_poke(stream);
                    poked = true;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    warn!("accept error on daemon socket: {e}");
                    break;
                }
            }
        }

        if poked {
            last_activity = Instant::now();
            run_sweep(&config);
            continue;
        }

        if last_activity.elapsed() >= idle_timeout {
            info!(
                idle_timeout_secs = config.idle_timeout_secs,
                "Idle timeout reached with no poke; self-terminating"
            );
            return Ok(());
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Acquire the listening socket from systemd, or bind it ourselves standalone.
fn acquire_listener() -> Result<UnixListener> {
    let mut fds = ListenFd::from_env();
    if let Some(listener) = fds
        .take_unix_listener(0)
        .context("taking the systemd socket-activation fd")?
    {
        info!("acquired listening socket from systemd (socket activation)");
        return Ok(listener);
    }

    // Standalone (non-systemd) fallback so the daemon is testable without units.
    let path = socket_path()?;
    if path.exists() {
        // Only on this fallback path: unlink a stale socket so bind does not hit
        // EADDRINUSE. systemd owns and cleans its own socket, so never do this
        // for a fd passed by socket activation.
        std::fs::remove_file(&path)
            .with_context(|| format!("removing stale socket file {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding daemon socket {}", path.display()))?;
    info!(socket = %path.display(), "standalone bind (no systemd fd passed)");
    Ok(listener)
}

/// Read and discard the poke byte(s) on a connection. Best-effort: a poke
/// carries no payload we need beyond the fact it arrived.
fn drain_poke(mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut buf = [0u8; 64];
    let _ = stream.read(&mut buf);
}

fn run_sweep(config: &Config) {
    match sweep::run(config) {
        Ok(n) => info!(new_generations = n, "sweep complete"),
        Err(e) => warn!("sweep failed: {e:#}"),
    }
}
