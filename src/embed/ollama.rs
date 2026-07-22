//! The Ollama embeddings client (D-01): a light blocking `ureq` POST to the local
//! Ollama HTTP API's `/api/embed`, requesting `qwen3-embedding:8b` with an explicit
//! 4096-dim expectation and a short `keep_alive`. Not the `ollama` CLI, not the
//! `ollama-rs` crate, no async runtime - a drain-and-exit batch command wants a
//! synchronous call.
//!
//! Every embed runs on the GPU (D-05, ADR 0013): every request pins
//! `options.num_gpu` high enough to force full GPU offload, so an embed either runs
//! fully GPU-resident or Ollama errors (caught by the drain's continue-on-error).
//! There is no CPU path and no placement decision to make.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::EmbedConfig;

/// The vector width the store's `float[4096]` column expects (ADR 0004). The
/// request-side `dimensions` pin states the intent; the post-response length
/// check below is the hard enforcement backing ADR 0004's dimension footgun.
pub const EMBED_DIMS: usize = 4096;

/// `num_gpu` layer count sent on every request: high enough to force Ollama to
/// offload all of the model's layers to the GPU (D-05, ADR 0013). Deleting the
/// option entirely would hand placement to Ollama's auto heuristic, which
/// partial-offloads to CPU under VRAM pressure - the exact CPU path forbidden here.
/// A value well above the model's real layer count means "all layers on GPU".
const FORCE_GPU_LAYERS: u32 = 999;

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
    keep_alive: &'a str,
    /// Request-side dimension pin (D-01). `qwen3-embedding:8b` returns native
    /// 4096, so this states intent; Ollama accepts and ignores it for this model.
    dimensions: usize,
    /// Always sent (D-05, ADR 0013): pins full GPU offload so no request silently
    /// partial-offloads to CPU under VRAM pressure.
    options: EmbedOptions,
}

#[derive(Serialize)]
struct EmbedOptions {
    num_gpu: u32,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

/// Embed one document's raw profile text and return its 4096-dim vector.
///
/// The text is sent verbatim as `input` with NO instruction prefix: the qwen3
/// query-side instruction template is a Phase 5 query concern, documents embed
/// raw (ADR 0005's "describe it, get the name" asymmetry lives on the query side).
pub fn embed_document(cfg: &EmbedConfig, text: &str) -> Result<Vec<f32>> {
    let req = EmbedRequest {
        model: &cfg.model,
        input: text,
        keep_alive: &cfg.keep_alive,
        dimensions: EMBED_DIMS,
        options: EmbedOptions {
            num_gpu: FORCE_GPU_LAYERS,
        },
    };

    let url = format!("{}/api/embed", cfg.ollama_url.trim_end_matches('/'));
    let resp: EmbedResponse = ureq::post(&url)
        .send_json(&req)
        .with_context(|| format!("POST {url}"))?
        .into_json()
        .context("parsing Ollama /api/embed response")?;

    let vector = resp
        .embeddings
        .into_iter()
        .next()
        .context("Ollama /api/embed returned no embeddings")?;

    // Hard enforcement of the dimension contract (ADR 0004): a width mismatch
    // must fail loud, never silently write a wrong-shaped vector to the store.
    if vector.len() != EMBED_DIMS {
        bail!(
            "Ollama returned a {}-dim vector, expected {EMBED_DIMS} (model {})",
            vector.len(),
            cfg.model
        );
    }
    Ok(vector)
}
