---
phase: 6
status: complete
completed: 2026-07-22
---

# Phase 6: Query and surfaces - Summary

The two-path query core (recall-complete exact listing + RRF-fused ranked search over FTS5 keyword and
sqlite-vec vector arms) built on the Phase 3-5 store, surfaced through an `rmcp` 2.2.0 MCP recall server
and a no-model CLI `hindsight search`, with every hit resolvable to its verbatim archived bytes.

## What shipped

- Exact recall-complete listing - `src/query/exact.rs` (`exact_listing`), wired to CLI `hindsight search --exact <entity>`.
- FTS5 keyword arm, embedder-free - `src/query/keyword.rs` (`keyword_search`), CLI positional `hindsight search <term>`.
- Query-side embed with the qwen3 instruction prefix - `src/embed/ollama.rs` (`embed_query`, `query_input`); `embed_document` untouched.
- Two-stage vector read (binary-coarse KNN -> full-precision rescore) with project + time pre-filters - `src/query/vector.rs` (`vector_search`, `TimeFilter`).
- RRF fusion at `session_id` granularity with canonical resolvable-target annotation and keyword-only fallback - `src/query/ranked.rs` (`ranked_search`).
- Shared archive-read primitive (path-guarded `zstd` decode) - `src/archive.rs` (`read_generation`).
- Pinpoint re-normalize returning raw generation-line bytes (verbatim, no re-serialize) + hit resolution - `src/normalize/mod.rs` (`pinpoint`), `src/query/resolve.rs` (`resolve`).
- rmcp MCP server over stdio with three tools (`exact_listing`, `ranked_search`, `resolve`) and a subcommand-scoped tokio runtime - `src/mcp/mod.rs`, `hindsight mcp` in `src/main.rs`, deps in `Cargo.toml`.

## Commits

| Plan | Task | Commit | Description |
|---|---|---|---|
| 1 | 1 | e07babf | exact recall-complete listing and CLI search tracer |
| 1 | 2 | 956f46e | FTS5 keyword arm and no-model CLI search dispatch |
| 1 | 3 | 274a87e | query-side embed with the qwen3 instruction prefix |
| 1 | 4 | 4dc5905 | two-stage vector read with project and time pre-filters |
| 1 | 5 | 16d36d9 | RRF fusion at session granularity with keyword-only fallback |
| 1 | 6 | be1d62b | shared archive-read primitive for hit resolution |
| 1 | 7 | 73c4345 | pinpoint re-normalize and hit resolution to verbatim bytes |
| 1 | 8 | 9f13f7a | rmcp MCP recall server and hindsight mcp subcommand |

## Deviations

- [deviation] Task 7 (73c4345): exposed `normalize::run_to` and `store::load::run_from` as `pub(crate)` so the resolve test drives the real normalize|load pipeline in-process. `load.rs` was outside Task 7's declared files; visibility-only change, behavior unchanged.
- [deviation] Task 8 (9f13f7a): the plan assumed an rmcp ~0.x API; the resolved SDK is `rmcp 2.2.0`. Per the plan's explicit allowance (crate-internal spellings track the resolved version, the three tool names/arities and JSON-RPC-over-stdio contract stay fixed), identifiers were adjusted: feature `transport-io` (not "stdio-transport"), `ServerInfo` built by mutating a `#[non_exhaustive]` default, tools return `Result<CallToolResult, ErrorData>`, content via `ContentBlock::json/text`, serve via `ServiceExt::serve(stdio()).await` then `.waiting()`. Fixed contract preserved; handler-invocation test passes unchanged.
- [deviation / risk-surface drop] Task 2: FTS5 MATCH is built from user text; dropped as provably harmless - value bound as a SQL parameter, each token quoted, with `operator_syntax_is_quoted_literal` proving operator syntax cannot inject. Local single-user tool.
- [deviation / risk-surface drop] Task 8: new MCP JSON-RPC wire contract and a `resolve` tool returning verbatim (unscrubbed) archive bytes; dropped as provably harmless - local single-user tool, first release with no downstream consumer, read-only recall, client is the operator's own Claude Code over stdio (same trust domain). Verbatim resolution is the locked D-07/D-08 headline design, not introduced here. Flagged for human override if disagreed.

## Open items

- Human-verify (Task 8, last acceptance criterion): register the built binary with Claude Code (`claude mcp add hindsight -- <path>/hindsight mcp`), restart, confirm a recall tool call returns results for a seeded query. Needs a live MCP client; the three handlers are proven in-process by `mcp::tests::all_three_tool_handlers_return_results`.
- [low, diff review] `src/query/vector.rs` two-stage-recall test does not truly exercise the `coarse_k` guard: the near row is both Hamming-nearest and cosine-nearest in a 2-row table, so a regression tightening `COARSE_MULTIPLIER`/`MIN_COARSE_K` to 1 would still pass. Construct a case where the true cosine-neighbor is NOT Hamming-nearest and is crowded out under a tight pool.
- [low, diff review] `hindsight search <terms>` with multiple unquoted words is rejected by clap (`search` positional is a single `Option<String>`); the Must-be-true #2 path runs only when the phrase is quoted. Consider a trailing var-arg that joins tokens.
- Optional CLI `--resolve` affordance (Task 7) not added; the MCP `resolve` tool is the primary caller, which the plan explicitly permitted.
- 4 benign `field never read` build warnings (`path` pre-existing; `rank`/`project`/`distance` are pub hit-struct fields RRF orders on via SQL; `tool_router` accessed by the rmcp macro). Left unsuppressed to avoid `#[allow]` noise on pub API.

## Goal check

The eight commits deliver the phase goal. Both CLI ground-truth paths are model-free and wired end to end: `main.rs:109` routes `Command::Search` to `query::run_search`, which calls `exact::exact_listing` for `--exact` and `keyword::keyword_search` for a positional query, neither touching an embedder (`query::exact` + `query::keyword` tests pass, and the `--exact` count assertion equals `COUNT(DISTINCT session_id)`). The fuzzy path is RRF-fused across the FTS keyword arm and the two-stage sqlite-vec vector arm at `session_id` granularity with the id-space translation the plan required (synthetic `event.id` -> `event.uuid`, entity -> representative `mention.event_uuid`), the strict-subset invariant enforced by carrying the project/time predicate into the entity remap, and a `degraded: true` keyword-only fallback when the query embed fails - all four `query::ranked` tests pass (fusion, degrade, strict-subset leak, target resolvability). Verbatim resolution returns the untouched raw generation line via `pinpoint` (never a re-serialized `Value`), proven byte-for-byte by `query::resolve` with a companion assertion that a re-serialized line does NOT match. The MCP surface exposes the fixed three tools over stdio with rusqlite work in `spawn_blocking`, and `main` is not `#[tokio::main]` (grep count 0), so the async runtime is confined to `mcp::run` (`mcp::tests` invokes all three handlers). Full suite: 98 tests pass; clean build carries only 4 benign field-never-read warnings. The one thing not machine-verified here is the live Claude Code MCP connection (the Task 8 human-verify), carried as an open item; every other Must-be-true criterion has code + passing-test evidence.
