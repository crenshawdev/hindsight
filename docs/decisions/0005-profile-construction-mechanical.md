# 0005 - Mechanical profile construction

Status: accepted

## Context

The hardest recall query is "I don't remember the name, find it." You describe a tool or a script by
what it did and you want the identifier back. That is asymmetric, a short vague description on one
side and a specific name on the other, and a bare name embeds terribly against a description. If I
just embed entity names, the fuzzy path fails on exactly the query it exists to serve.

## Decision

Embed **synthetic profiles**, built mechanically, not the bare names and not the raw code.

For an **entity**, the profile is a small synthetic document: the name and its aliases, the context
where it was introduced or explained, a handful of deduplicated usage sentences, the entities it was
used alongside, and the projects it appeared in. A description now lands near it because the profile
describes the entity the way I would.

For a **lost artifact**, the profile is the natural-language wrapper, the request I made and the
explanation around it, plus the path, the language, and a mechanically extracted signature like
function names and notable flags. The code body is deliberately left out of the embedded text, because
code embeds poorly against a plain-English "find the script that renamed my screenshots." The body is
reached two other ways, a keyword search over the raw code, and a link back to the archive once the
wrapper is found.

Profiles are re-embedded on a **threshold**, when an entity crosses a mention count or gains a new
project, not on every mention, which would mean constant re-embedding for no gain.

## Alternatives considered

**A language-model gloss per entity.** A model writing a clean one-line description of each entity
would embed beautifully against a description query. Rejected as the default, it puts a language model
back in the ingest path I spent the rest of the design keeping out, and a good gloss is not a small
local-model job. It is kept as a purely additive enrichment layer to bolt on later if a real
"find the name" test shows the mechanical profiles underperform. The instruction-aware embedder
already does much of the asymmetry bridging on its own, which is part of why mechanical is enough to
start.

**Embedding the code itself for artifacts.** Rejected, code does not match a natural-language
description of what the code does. The wrapper matches, the code is reached by keyword and by link.

## Consequences

The fuzzy path is served without any language model in ingest, and the profiles are rebuildable from
the normalized records like everything else. The threshold rule keeps incremental embedding cheap, a
new session mentioning an existing entity usually adds nothing to embed.

The three embedded units, entity profiles, artifact wrappers, and prose chunks, are what the ranked
search path fuses over ([ADR 0007](0007-query-interface.md)).
