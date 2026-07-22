//! The MCP recall server (IFC-01, D-09), built on the official `rmcp` Rust SDK.
//! Exposes three recall tools over stdio JSON-RPC - `exact_listing` (QRY-01),
//! `ranked_search` (QRY-02, the fuzzy RRF vector path), and `resolve` (QRY-03) -
//! for in-session recall from Claude Code.
//!
//! The tokio async runtime is confined to `run`, built inside the `hindsight mcp`
//! subcommand (never `#[tokio::main]` on `main`), so the rest of the deliberately
//! synchronous binary (ureq/rusqlite blocking) is untouched (D-09). Each tool runs
//! its blocking rusqlite work inside `tokio::task::spawn_blocking` so the async
//! runtime is not stalled. All logging stays on stderr (init_tracing) so stdout
//! carries only the JSON-RPC stream.
//!
//! FIXED CONTRACT: the three tool names/arities (`exact_listing`, `ranked_search`,
//! `resolve`) and the stdio JSON-RPC transport are fixed. Only crate-internal
//! identifier spellings track the resolved `rmcp` 2.2.0 API.

use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::config::Config;
use crate::embed::ollama;
use crate::query::{exact, ranked, resolve};
use crate::store::open_db;

/// The recall server: a resolved `Config` (shared into each blocking task) and the
/// rmcp tool router.
#[derive(Clone)]
pub struct HindsightServer {
    config: Arc<Config>,
    tool_router: ToolRouter<HindsightServer>,
}

/// Arguments for `exact_listing` (QRY-01): the recall-complete listing for an
/// entity, with optional structural pre-filters.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExactListingArgs {
    /// The entity to list (a file path or command name).
    pub entity: String,
    /// Restrict to this entity_type (`file`/`command`).
    pub entity_type: Option<String>,
    /// Restrict to this project.
    pub project: Option<String>,
    /// RFC3339 lower time bound (inclusive).
    pub since: Option<String>,
    /// RFC3339 upper time bound (inclusive).
    pub until: Option<String>,
}

/// Arguments for `ranked_search` (QRY-02): the fuzzy RRF-fused ranked search.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RankedSearchArgs {
    /// The free-text query.
    pub query: String,
    /// Structural pre-filter: only this project.
    pub project: Option<String>,
    /// RFC3339 lower time bound (inclusive).
    pub since: Option<String>,
    /// RFC3339 upper time bound (inclusive).
    pub until: Option<String>,
}

/// Arguments for `resolve` (QRY-03): pinpoint a hit's verbatim archived bytes.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveArgs {
    /// The session the hit belongs to.
    pub session_id: String,
    /// `event` or `artifact`.
    pub source_type: String,
    /// An `event.uuid` (for `event`) or an `artifact_id` (for `artifact`).
    pub source_id: String,
}

#[tool_router]
impl HindsightServer {
    /// Build the server over a resolved config.
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            tool_router: Self::tool_router(),
        }
    }

    /// Recall-complete exact listing: every session that references an entity.
    #[tool(
        description = "List every session that references an entity (file path or command name), \
                       recall-complete, with optional entity_type/project/time pre-filters."
    )]
    async fn exact_listing(
        &self,
        Parameters(args): Parameters<ExactListingArgs>,
    ) -> Result<CallToolResult, McpError> {
        let config = Arc::clone(&self.config);
        let sessions = spawn_db(move || {
            let conn = open_db(&config.db_path())?;
            exact::exact_listing(
                &conn,
                &args.entity,
                args.entity_type.as_deref(),
                args.project.as_deref(),
                args.since.as_deref(),
                args.until.as_deref(),
            )
        })
        .await?;

        let body = serde_json::json!({ "count": sessions.len(), "sessions": sessions });
        Ok(CallToolResult::success(vec![ContentBlock::json(body)?]))
    }

    /// Fuzzy ranked search: RRF fusion of the keyword and vector arms, degrading to
    /// keyword-only when the embedder is unavailable.
    #[tool(
        description = "Fuzzy ranked recall over past sessions (RRF fusion of keyword and vector \
                       search) with optional project/time pre-filters. Degrades to keyword-only \
                       if the embedder is unavailable."
    )]
    async fn ranked_search(
        &self,
        Parameters(args): Parameters<RankedSearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let config = Arc::clone(&self.config);
        let result = spawn_db(move || {
            let conn = open_db(&config.db_path())?;
            let embed_cfg = config.embed.clone();
            ranked::ranked_search(
                &conn,
                &args.query,
                args.project.as_deref(),
                args.since.as_deref(),
                args.until.as_deref(),
                |q| ollama::embed_query(&embed_cfg, q),
            )
        })
        .await?;

        let sessions: Vec<_> = result
            .sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "session_id": s.session_id,
                    "score": s.score,
                    "target": {
                        "source_type": s.target.source_type,
                        "source_id": s.target.source_id,
                    },
                })
            })
            .collect();
        let body = serde_json::json!({
            "sessions": sessions,
            "degraded": result.degraded,
            "degraded_reason": result.degraded_reason,
        });
        Ok(CallToolResult::success(vec![ContentBlock::json(body)?]))
    }

    /// Resolve a hit to the verbatim archived bytes of its pinpointed record.
    #[tool(
        description = "Resolve a hit (event uuid or artifact_id in a session) to the verbatim \
                       bytes of its archived transcript line."
    )]
    async fn resolve(
        &self,
        Parameters(args): Parameters<ResolveArgs>,
    ) -> Result<CallToolResult, McpError> {
        let config = Arc::clone(&self.config);
        let bytes = spawn_db(move || {
            let conn = open_db(&config.db_path())?;
            resolve::resolve(
                &conn,
                &config,
                &args.session_id,
                &args.source_type,
                &args.source_id,
            )
        })
        .await?;

        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for HindsightServer {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (InitializeResult) is #[non_exhaustive]: mutate a default
        // rather than a struct literal.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Hindsight recall: cross-session memory for Claude Code. Use exact_listing for \
             recall-complete lookups by file/command, ranked_search for fuzzy recall, and \
             resolve to pull a hit's verbatim archived bytes."
                .to_string(),
        );
        info
    }
}

/// Run a blocking rusqlite closure on the tokio blocking pool and flatten the
/// join + application errors into an rmcp tool error (D-09: rusqlite is blocking,
/// so it never runs on an async worker).
async fn spawn_db<T, F>(f: F) -> Result<T, McpError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| McpError::internal_error(format!("recall task panicked: {e}"), None))?
        .map_err(|e| McpError::internal_error(format!("{e:#}"), None))
}

/// Serve the MCP server over stdio (D-09). Builds a tokio runtime INSIDE this
/// subcommand (never `#[tokio::main]` on `main`) so the async runtime does not
/// leak into the synchronous binary, then serves until the client disconnects.
pub fn run(config: &Config) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for mcp serve")?;

    runtime.block_on(async {
        let server = HindsightServer::new(config.clone());
        let service = server
            .serve(stdio())
            .await
            .context("starting mcp stdio service")?;
        service.waiting().await.context("mcp stdio service run")?;
        Ok::<(), anyhow::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{self, Kind};
    use std::path::Path;

    /// A config whose embedder points at an unreachable Ollama, so `ranked_search`
    /// deterministically exercises the D-05 keyword-only fallback with no live
    /// Ollama regardless of the test host.
    fn test_config(base: &Path) -> Config {
        Config::from_toml_str(&format!(
            "base_dir = {:?}\nidle_timeout_secs = 5\n[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
            base
        ))
        .unwrap()
    }

    /// Seed the store from a real transcript via the normalize | load pipeline so
    /// all three tools have something to return.
    fn seed(cfg: &Config) {
        let user_line = r#"{"type":"user","uuid":"u-1","timestamp":"2026-07-21T10:00:00Z","sessionId":"sess-1","message":{"role":"user","content":"deploy the payment widget"}}"#;
        let write_line = r#"{"type":"assistant","uuid":"a-1","timestamp":"2026-07-21T10:01:00Z","sessionId":"sess-1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/repo/src/x.rs","content":"fn x() {}"}}]}}"#;
        let transcript = format!("{user_line}\n{write_line}\n");
        let src = cfg.base_dir.join("source.jsonl");
        std::fs::create_dir_all(&cfg.base_dir).unwrap();
        std::fs::write(&src, &transcript).unwrap();

        archive::write_generation(cfg, "proj", "sess-1", "", &src, Kind::Sweep).unwrap();
        let session_dir = cfg.archive_dir().join("proj").join("sess-1");
        let mut ndjson: Vec<u8> = Vec::new();
        crate::normalize::run_to(&session_dir, &mut ndjson).unwrap();
        crate::store::load::run_from(cfg, &ndjson[..]).unwrap();
    }

    fn first_text(r: &CallToolResult) -> String {
        match &r.content[0] {
            ContentBlock::Text(t) => t.text.clone(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    /// Invoke all THREE tool handlers directly over a seeded temp DB (in a tokio
    /// runtime built in the test), so a missing or mis-signatured tool fails the
    /// test, not just the build. `ranked_search` exercises the keyword-only
    /// fallback (unreachable Ollama), needing no live embedder.
    #[test]
    fn all_three_tool_handlers_return_results() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        seed(&cfg);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let server = HindsightServer::new(cfg.clone());

            // exact_listing: the Write's file_path is a file mention in sess-1.
            let listing = server
                .exact_listing(Parameters(ExactListingArgs {
                    entity: "/repo/src/x.rs".to_string(),
                    entity_type: None,
                    project: None,
                    since: None,
                    until: None,
                }))
                .await
                .expect("exact_listing ok");
            let listing_text = first_text(&listing);
            assert!(listing_text.contains("sess-1"), "exact_listing returns the seeded session");

            // ranked_search: keyword arm matches; embed fails -> degraded, still Ok.
            let ranked = server
                .ranked_search(Parameters(RankedSearchArgs {
                    query: "payment".to_string(),
                    project: None,
                    since: None,
                    until: None,
                }))
                .await
                .expect("ranked_search ok");
            let ranked_text = first_text(&ranked);
            assert!(ranked_text.contains("sess-1"), "ranked_search returns fused keyword result");
            assert!(ranked_text.contains("\"degraded\":true"), "degraded to keyword-only with no Ollama");

            // resolve: the artifact hit resolves to verbatim bytes of the Write line.
            let conn = open_db(&cfg.db_path()).unwrap();
            let artifact_id: String = conn
                .query_row("SELECT artifact_id FROM artifact LIMIT 1", [], |r| r.get(0))
                .unwrap();
            let resolved = server
                .resolve(Parameters(ResolveArgs {
                    session_id: "sess-1".to_string(),
                    source_type: "artifact".to_string(),
                    source_id: artifact_id,
                }))
                .await
                .expect("resolve ok");
            let resolved_text = first_text(&resolved);
            assert!(resolved_text.contains("/repo/src/x.rs"), "resolve returns the verbatim Write line");
        });
    }
}
