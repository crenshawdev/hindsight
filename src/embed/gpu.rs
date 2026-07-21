//! GPU-opportunistic scheduling (D-04, D-05): detect a busy GPU by polling
//! `nvidia-smi` and choose where Ollama runs each embed. The policy never fails a
//! run - a busy card defers then falls back to CPU, and an absent card runs on CPU
//! immediately.

use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use crate::config::EmbedConfig;
use crate::embed::ollama::Placement;

/// The card's state as read from `nvidia-smi`. A tri-state (not a `bool`) so
/// "present and idle" is distinct from "no GPU here": a missing `nvidia-smi` must
/// mean CPU-only, never be mistaken for an available card (D-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuState {
    Free,
    Busy,
    Unavailable,
}

/// Env hook that forces a busy reading, making the defer/CPU-fallback path
/// machine-checkable without real GPU contention (criterion 5).
const FORCE_BUSY_ENV: &str = "HINDSIGHT_EMBED_FORCE_BUSY";

/// Read the current GPU state. `HINDSIGHT_EMBED_FORCE_BUSY` truthy forces `Busy`;
/// otherwise `nvidia-smi` utilization/free-VRAM is compared against the configured
/// thresholds. A missing or erroring `nvidia-smi` is `Unavailable`, never a
/// failure.
pub fn gpu_state(cfg: &EmbedConfig) -> GpuState {
    if force_busy() {
        return GpuState::Busy;
    }

    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        // No `nvidia-smi`, or it errored: treat as no usable GPU (CPU-only).
        _ => return GpuState::Unavailable,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = match stdout.lines().next() {
        Some(line) => line,
        None => return GpuState::Unavailable,
    };

    // Expected shape: "<util>, <free_mib>" (e.g. "5, 23000").
    let mut fields = first.split(',').map(str::trim);
    let util = fields.next().and_then(|s| s.parse::<u32>().ok());
    let free_mib = fields.next().and_then(|s| s.parse::<u64>().ok());
    let (util, free_mib) = match (util, free_mib) {
        (Some(u), Some(f)) => (u, f),
        // Unparseable output: do not guess the card is free, defer to CPU.
        _ => return GpuState::Unavailable,
    };

    if util > cfg.gpu_util_busy_pct || free_mib < cfg.gpu_min_free_mib {
        GpuState::Busy
    } else {
        GpuState::Free
    }
}

/// Decide where to run the embeds (D-05): a free card runs on the GPU, an absent
/// card runs on CPU immediately, and a busy card defers - polling every
/// `gpu_defer_poll_secs` until the card frees or goes away, or the accumulated
/// defer reaches `gpu_max_defer_secs`, at which point it falls back to CPU. A busy
/// GPU never aborts the run.
pub fn choose_placement(cfg: &EmbedConfig) -> Placement {
    let poll = cfg.gpu_defer_poll_secs.max(1);
    let mut deferred_secs: u64 = 0;
    loop {
        match gpu_state(cfg) {
            GpuState::Free => return Placement::Gpu,
            GpuState::Unavailable => return Placement::Cpu,
            GpuState::Busy => {
                // Check the defer budget BEFORE sleeping so a zero budget falls
                // back to CPU immediately (the test path).
                if deferred_secs >= cfg.gpu_max_defer_secs {
                    tracing::info!(
                        deferred_secs,
                        "GPU busy past defer budget, falling back to CPU"
                    );
                    return Placement::Cpu;
                }
                tracing::info!(deferred_secs, poll, "GPU busy, deferring");
                sleep(Duration::from_secs(poll));
                deferred_secs = deferred_secs.saturating_add(poll);
            }
        }
    }
}

fn force_busy() -> bool {
    match std::env::var(FORCE_BUSY_ENV) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false" || v == "no")
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EmbedConfig;

    /// Both assertions share the process-global `HINDSIGHT_EMBED_FORCE_BUSY`, so
    /// they run in one test to avoid a parallel env-var race: a forced-busy
    /// reading is `Busy`, and with a zero defer budget `choose_placement` falls
    /// straight back to CPU without ever sleeping (criterion 5).
    #[test]
    fn forced_busy_reads_busy_and_falls_back_to_cpu() {
        std::env::set_var(FORCE_BUSY_ENV, "1");
        let state = gpu_state(&EmbedConfig::default());
        let cfg = EmbedConfig {
            gpu_max_defer_secs: 0,
            ..EmbedConfig::default()
        };
        let placement = choose_placement(&cfg);
        std::env::remove_var(FORCE_BUSY_ENV);

        assert_eq!(state, GpuState::Busy);
        assert_eq!(placement, Placement::Cpu);
    }
}
