//! The Ollama embeddings client (D-01): a light blocking `ureq` POST to the local
//! Ollama HTTP API's `/api/embed`, requesting `qwen3-embedding:8b` with an explicit
//! 4096-dim expectation and a short `keep_alive`. Not the `ollama` CLI, not the
//! `ollama-rs` crate, no async runtime - a drain-and-exit batch command wants a
//! synchronous call.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::EmbedConfig;

/// The vector width the store's `float[4096]` column expects (ADR 0004). The
/// request-side `dimensions` pin states the intent; the post-response length
/// check below is the hard enforcement backing ADR 0004's dimension footgun.
pub const EMBED_DIMS: usize = 4096;

/// Where Ollama runs the embed: on the GPU (its default) or forced onto the CPU
/// via `options.num_gpu = 0` (the D-05 fallback path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Gpu,
    Cpu,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
    keep_alive: &'a str,
    /// Request-side dimension pin (D-01). `qwen3-embedding:8b` returns native
    /// 4096, so this states intent; Ollama accepts and ignores it for this model.
    dimensions: usize,
    /// Present only for `Placement::Cpu` (`num_gpu = 0`); omitted for GPU so
    /// Ollama uses its default placement.
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<EmbedOptions>,
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
pub fn embed_document(cfg: &EmbedConfig, text: &str, place: Placement) -> Result<Vec<f32>> {
    let options = match place {
        Placement::Cpu => Some(EmbedOptions { num_gpu: 0 }),
        Placement::Gpu => None,
    };
    let req = EmbedRequest {
        model: &cfg.model,
        input: text,
        keep_alive: &cfg.keep_alive,
        dimensions: EMBED_DIMS,
        options,
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
