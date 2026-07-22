# 0004 - Embedder choice and GPU scheduling

Status: accepted

## Context

Fuzzy recall needs embeddings, which need somewhere to run and something to run. Two constraints
shaped it. The data is small, a couple hundred thousand records and roughly twenty thousand vectors,
so this is a fits-in-memory problem, not a scaling one. And privacy matters, the transcripts already
went to Anthropic by existing, but handing a third-party embedding API a coherent index of everything
I have built would be a brand new disclosure to a brand new party.

The hardware question took the longest to settle.

## Decision

Embed locally with **qwen3-embedding:8b** (Q4_K_M quantization, 4096 dimensions) served through
**Ollama**. Local closes the privacy gap, and at this data size running the best model I can fit costs
nothing extra. It is a top-ranked open embedding model on retrieval benchmarks, it is instruction-aware
(which directly helps the asymmetric "describe it, get the name" query), and it is strongest on the
technical and reasoning-heavy material this corpus is made of.

Run it on my **desktop GPU as an opportunistic accelerator, not a resident service**. The card is
plenty for this, but I game on it. The resolution is that embedding is deferrable and nothing waits
on it, the archive and the parse happen the instant a session lands and give exact and keyword recall
on their own, while the embeddings are a downstream step that can lag for hours and lose nothing but
freshness on the fuzzy path.

Three settings follow from that. The model loads only while it is actually embedding, through a short
keep-alive, so it is not parked in video memory between bursts. It defers when a game is holding the
card, queuing the work in the watermark and draining when the card frees. And it falls back to the
CPU otherwise, which at this size finishes the whole backfill in an afternoon.

## Alternatives considered

**A separate box on the network.** The first plan. It came off the table over a hardware question I
did not want to chase, and dropping it cost almost nothing, because the embedding workload is small
and deferrable enough that it never needed dedicated hardware.

**The integrated GPU.** Investigated and rejected on the numbers. It has roughly a fifth of the
throughput of the CPU sitting next to it, and no memory-bandwidth advantage, because it shares the
same system RAM. It is slower than just using the processor. It is a display engine, not a compute
part.

**A warm resident model on the desktop GPU.** This is the specific thing gaming kills, a model
keeping itself warm in video memory while a game wants that memory. Rejected in favor of the
opportunistic scheme above. Gaming killed warm-resident, not the GPU.

**A smaller model that is always instant on the CPU.** Rejected, because instant buys nothing when
nothing blocks on the result. Embedding lag is harmless, so there is no reason to trade model quality
for CPU speed, quality is permanent and latency is invisible.

## Consequences

Standing up the embedder is a config decision, not an architecture one, and the network box going
away barely mattered. Recall degrades gracefully, exact and keyword search are always current, and
semantic search catches up as the queue drains.

Output dimensions are set explicitly (4096) rather than left to a default, a known footgun where an
unrecognized model silently falls back to the wrong dimension and breaks the first write. At Q4_K_M the
model is a few gigabytes of video memory, which fits comfortably on the desktop card and leaves room to
game, and because it only loads while embedding it is not competing for that memory the rest of the time.

What actually gets embedded, the synthetic profiles rather than raw text, is
[ADR 0005](0005-profile-construction-mechanical.md). The vectors are stored in the same SQLite file
as everything else ([ADR 0006](0006-storage-engine-sqlite.md)).

## Amendment (2026-07-21, Phase 4 build)

**Partly superseded by [ADR 0013](0013-embed-delivery-hook-gpu.md) (Phase 5).** The delivery specifics
in this amendment, the systemd timer trigger, the `nvidia-smi` GPU-busy defer, and the CPU fallback,
are all reversed there: embedding is now hook-triggered, detached, and always-GPU with no CPU path. The
transport (`ureq` to `/api/embed`, the dimension pin) and the resumable `embed_ledger` described below
still stand.

The deferrable-embedder decision is built now, and the parts left open above resolved to these
specifics.

Transport is a light blocking `ureq` POST to Ollama's `/api/embed` at the local port, not the CLI and
not a client crate. The request carries the model, the raw profile text as `input`, a short
`keep_alive` so the model stays warm across a drain and unloads after, and an explicit 4096-dimension
pin. CPU fallback is a per-request `options.num_gpu = 0`; the GPU path just omits it and takes Ollama's
default. The dimension pin is enforced twice, once as the request-side intent and once as a hard
post-response length check so a wrong-width vector fails loud rather than landing in the store.

The deferrable queue is not the transcript watermark after all. It is a durable `embed_ledger` table
that stamps each embedded unit with its `(unit_kind, source_id)` and an embedder version. A unit's
vector and its ledger stamp commit in one transaction, so an interrupted or CPU-fallback run resumes
exactly, re-embedding only what did not land. The ledger is wiped in lockstep with the vector table on
every `hindsight load`, so an empty ledger safely means not-embedded and a fresh load re-embeds the
corpus. This is the batch re-embed only; the per-session incremental trigger rides the same core later.

GPU-busy detection polls `nvidia-smi` for utilization and free video memory against configured
thresholds. A free card runs on the GPU, a busy card defers and re-polls on an interval until the card
frees or a defer budget is spent and it falls back to CPU, and an absent `nvidia-smi` runs on CPU
immediately. A busy card never fails the run. Embedding is a `hindsight embed` subcommand driven by a
systemd timer, deliberately kept out of the capture daemon so a multi-hour deferring run cannot fight
the daemon's idle self-terminate.
