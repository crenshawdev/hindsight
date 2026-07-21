# 0012 - Implementation language and runtime

Status: accepted

## Context

The design settled every part of the system but left one build decision open, the language the whole
thing is written in. The daemon, the normalizer, the query core, the MCP server, and the CLI all share
one runtime, so this is a single choice that constrains all of them. The constraints were fixed at
design time: the runtime has to drive SQLite with FTS5 and sqlite-vec cleanly, talk to Ollama over
HTTP, and ship as a small always-available executable that systemd can socket-activate, all from one
file with no server to babysit.

## Decision

The language is **Rust**, and the whole system ships as **one static binary with subcommands**, the
daemon, the CLI, and the MCP server are three entry points into the same executable rather than three
programs.

Rust fits the constraints without fighting them. Socket activation wants a tiny thing that is always on
disk and costs nothing until poked, and a single statically linked binary is exactly that, there is no
interpreter to keep warm and no runtime to install. SQLite comes in through rusqlite with the bundled
amalgamation, FTS5 is a compile flag on that same build, and sqlite-vec loads as an extension against
it, so the whole store is one linked dependency and not a system-package hunt. Ollama is a plain HTTP
endpoint, so it is a request and nothing more. The normalizer parses transcript JSON that this tool did
not write and cannot fully trust across format drift, and a language that makes the parse total and the
error paths explicit is worth more here than in most code. It is also the language this project is
already written in, so the maintenance cost is real experience rather than a bet.

## Alternatives considered

**Go.** Also a single static binary, and simpler to write. Rejected because the SQLite story is worse
for this exact stack, the cgo-free drivers give up FTS5 and the sqlite-vec extension path, the cgo ones
give back the static-binary simplicity that was the reason to reach for Go in the first place, and it
is not the language this project already runs on.

**Python.** The fastest to write, with mature sqlite and HTTP libraries. Rejected on the deploy shape,
a socket-activated always-available service in an interpreted language means keeping a warm process or
paying import cost on every poke, and packaging a small self-contained thing is the awkward part of
Python rather than the easy one. The normalizer over drifting untrusted JSON is also where dynamic
typing hurts most.

**Node or TypeScript.** The same interpreter-and-packaging objections as Python, with a better type
story than Python and a worse one than Rust. Rejected for the same deploy-shape reason.

**C or Zig.** Closest to the metal and the smallest binaries. Rejected because parsing untrusted
transcript JSON is precisely the workload where the safety Rust adds over C earns its keep, and Zig is
neither that safety nor the language this project already runs on.

## Consequences

One Cargo project, one binary, three subcommands off the same build. rusqlite with the bundled SQLite
and the FTS5 feature carries the store, and the sqlite-vec extension is the one integration to prove
out early, because it is the only piece of the stack that is not already a settled Rust dependency.
Socket activation binds through the systemd integration crates rather than reinvented plumbing. Rust is
slower to write than the interpreted alternatives, and that cost is paid back in a single-file deploy,
a total parser, and a runtime the maintainer already knows. This decision is build-time only, it
changes nothing in the architecture the other eleven ADRs settled, it only names what they are built
in.
