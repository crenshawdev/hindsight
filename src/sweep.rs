//! Full-tree watermark sweep (Task 4). Stub for the scaffold.

use anyhow::Result;

use crate::config::Config;

/// Walk the transcript tree and archive new-or-changed transcripts.
/// Returns the count of newly written generations. Stub returns 0.
pub fn run(_config: &Config) -> Result<usize> {
    Ok(0)
}
