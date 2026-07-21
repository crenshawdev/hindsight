# 0008 - Scrub secrets from the index, keep the archive verbatim

Status: accepted

## Context

Transcripts contain secrets, environment reads, tokens, connection strings, the contents of config
files. The question is whether Hindsight needs to scrub them, given that everything runs locally and
nothing leaves the box during ingest. The privacy-from-a-vendor argument, the usual reason to scrub,
is gone, because local embedding and a local index mean no third party sees the data on the way in.

Two risks survive that, though, and one common assumption turns out to be false.

## Decision

**Scrub the index. Leave the archive verbatim.**

The scrub is a redaction pass at normalize time that keeps live-credential shapes, tokens, connection
strings, config-file values, private keys, auth headers, out of the records, the full-text index, and
the embeddings. The archive keeps the original bytes untouched, because it is the ground truth and
the format-drift firewall, and the scrub is reversible from it, if the redactor improves I re-derive a
cleaner index from the same archive.

## Alternatives considered

**Scrub nothing.** Tempting on the "nothing leaves the box" argument, but that argument is false on
the query path. When the model pulls a memory into a live session, whatever comes back rides into that
session's context and goes over the wire. An unscrubbed secret in an indexed chunk gets re-transmitted
the moment recall surfaces it. Separately, a durable searchable index turns a password buried in a
transcript that ages out into a password I can casually grep up years later. Both are real, so
scrubbing the index earns its place.

**Scrub the archive too.** Rejected. It would cost the verbatim ground truth for no privacy gain,
because the raw transcripts already sit on the same backed-up disk the archive lives on, so redacting
the archive protects nothing that is not already exposed there, and it is irreversible, a redacted
secret I later actually needed is gone.

## Consequences

The two surviving risks, re-transmission through the query path and casual self-surfacing years later,
are both handled by keeping credential shapes out of what I query and what gets fed back to the model.
The archive stays intact as ground truth, and the scrubber is free to improve over time because it
only ever touches the rebuildable side. The scrub lives inside the normalize step
([ADR 0003](0003-normalize-event-grain.md)), and the write-once archive it depends on is
[ADR 0001](0001-storage-location-and-archive-split.md).
