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

/// The qwen3-embedding query-side task description (D-04). qwen3-embedding is an
/// instruction-tuned model whose query side is wrapped in an
/// `Instruct: {task}\nQuery: {query}` template while the document side embeds raw
/// (ADR 0005's "describe it, get the name" asymmetry lives on the query side). The
/// task frames the retrieval target as this tool's corpus of past-session records.
const QUERY_TASK: &str =
    "Given a search query, retrieve relevant records from past Claude Code coding \
     sessions (files, commands, code, and discussion) that answer it";

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

/// Embed a query and return its 4096-dim vector (D-04). Distinct from
/// `embed_document`: the query text is wrapped in the qwen3 query-side instruction
/// template via `query_input` before the POST; everything else - model,
/// `num_gpu` full-GPU pin, `dimensions` pin, and the post-response `EMBED_DIMS`
/// length enforcement - is identical to the document path. Documents never take
/// this prefix; only the query side is asymmetric.
pub fn embed_query(cfg: &EmbedConfig, query: &str) -> Result<Vec<f32>> {
    let input = query_input(query);
    let req = EmbedRequest {
        model: &cfg.model,
        input: &input,
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

    if vector.len() != EMBED_DIMS {
        bail!(
            "Ollama returned a {}-dim vector, expected {EMBED_DIMS} (model {})",
            vector.len(),
            cfg.model
        );
    }
    Ok(vector)
}

/// Wrap a raw query in the qwen3 query-side instruction template (D-04). Pure and
/// prefix-only: the document path (`embed_document`) never calls this, so a
/// document is embedded raw while a query carries the `Instruct:`/`Query:` frame.
fn query_input(query: &str) -> String {
    format!("Instruct: {QUERY_TASK}\nQuery: {query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-04: the query-side input carries the instruction prefix and the raw query
    /// text, and it is NOT the bare query (which is what `embed_document` sends).
    #[test]
    fn query_input_wraps_with_instruction_prefix() {
        let q = "find the deploy script";
        let wrapped = query_input(q);

        assert!(wrapped.contains("Instruct: "), "carries the instruct prefix");
        assert!(wrapped.contains("Query: "), "carries the query marker");
        assert!(wrapped.contains(q), "carries the raw query text verbatim");
        assert!(wrapped.ends_with(q), "the raw query is the tail after the frame");
        assert_ne!(wrapped, q, "the query side is not the bare document-side input");
        assert!(
            wrapped.starts_with("Instruct: "),
            "the frame precedes the query, so a document (raw) and a query differ"
        );
    }
}
