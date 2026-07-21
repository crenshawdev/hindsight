//! Poke subcommand: one byte to the daemon socket (D-11).
//!
//! Connecting to the systemd-owned socket is what triggers socket activation
//! (or wakes an already-warm daemon). The payload is a single byte; the daemon
//! only cares that a connection arrived.

use std::io::Write;
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result};

use crate::daemon::socket_path;

pub fn run() -> Result<()> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "cannot connect to the daemon socket {} \
             (start it with `systemctl --user start hindsight.socket`)",
            path.display()
        )
    })?;
    stream.write_all(b"x").context("writing the poke byte")?;
    Ok(())
}
