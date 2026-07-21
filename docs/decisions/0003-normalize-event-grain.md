# 0003 - Normalize schema and the three-tier event grain

Status: accepted

## Context

Between the verbatim archive and the searchable index sits a normalize step whose job is to turn
raw transcript JSON into records I control. That layer earns its place for two reasons. Claude Code
warns that the transcript format is internal and changes between versions, and my own corpus already
shows drift inside a few weeks, older sessions inline their subagent turns while newer ones split
them into separate files. Normalize is the firewall, a format change costs a parser fix, not a lost
archive, because the archive is untouched and the normalize step is re-runnable over it.

The bigger question is how much of each event to keep in the index. A transcript is dominated by
noise, tool-result bodies that are mostly file dumps and command output, the model's private
reasoning, duplicated payloads, injected listings. Indexing all of it would bloat the store and
pollute the vector space, while dropping it risks losing something I later want.

## Decision

Normalize emits four record types. A **Session** for each logical session, an **Event** for each
meaningful turn, an **Artifact** for each script or file the model produced, and a **Mention** for
each entity that appeared. The structure of each is in the [data model diagram](../diagrams.md#data-model).

Most of the extraction is mechanical, a parse rather than an inference. Which files a session
touched comes straight from the arguments of the read, edit, and write calls. Which commands ran
comes from the shell calls. Project, branch, timestamp, subagent, skill, all of it is already a
field in the JSON. This is exact, and more accurate than a language model reading prose would be.

How much of each event to keep is settled by **three tiers**, and the tiers lean on the archive.

**Indexed.** Prompts, model answers, tool-call invocations with their extracted arguments, and
artifacts. Full text goes into the keyword index and the embeddings.

**Skeleton.** The model's private reasoning and the bodies of tool results. The record exists, its
place in the thread and its flags are kept, but the body stays out of the index. Two cheap things are
kept from tool results regardless, whether it errored and which tool it answered, because "sessions
where the build failed" is a valuable free query. Result bodies are gated by tool name, file reads
and command output are skeletoned while web fetches and searches are kept, since those are content I
would actually want to recall.

**Archive-only.** Duplicated payloads, injected listings, and the machine-generated observer noise.
No record at all.

## Alternatives considered

**Index everything for full fidelity.** Rejected as unnecessary given the archive already holds full
fidelity. This would trade a bloated, noisier index for completeness I can always recover one layer
down.

**A hard keep-or-drop binary.** Rejected in favor of the skeleton middle tier. The binary forces a
choice between storing the model's reasoning, which pollutes recall, and losing that those turns
ever existed. The skeleton keeps the thread structure and the provenance cheaply and gates only the
heavy body.

## Consequences

The guiding principle is that every gate is reversible, because the full bytes are in the archive.
That lets me optimize the index for signal instead of completeness and let completeness live one
layer down. In practice this takes a few weeks of transcripts down to the couple of megabytes that
carry the meaning.

Gating tool-result bodies costs the artifact store nothing, because artifacts come from tool-call
*inputs* (the content of a write, the strings of an edit, a heredoc in a shell command) and from the
model's answer text, never from tool results. See [ADR 0005](0005-profile-construction-mechanical.md).

The secrets scrub runs here, on the way into the index, while the archive stays verbatim
([ADR 0008](0008-secrets-scrub-index-only.md)).

## Amendment (2026-07-21): the tiers, measured

Running the real corpus through this grain put numbers on it. Across 1,446 transcript files the
indexed tier takes 24,766 prose chunks and 29,920 tool-call invocations, while the skeleton tier
holds 29,917 tool-result bodies and 18,651 private-reasoning blocks back out of the embeddings. The
skeleton tier is doing exactly what it was built for, it keeps roughly 49,000 vectors of file-dump
and reasoning noise out of the store and nearly halves the indexed vector count. Worth noting for
later tuning, the tool-call invocations are the larger half of what does get embedded, so if they
ever prove more noise than signal, moving them to skeleton is a cheap lever that halves the vector
total again, and the storage stress test in [ADR 0006](0006-storage-engine-sqlite.md) holds at either
count.
