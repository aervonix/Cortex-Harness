# 0008: One typed protocol, carried as JSON-RPC over newline-delimited JSON

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

Four front ends drive the core: the TUI, headless mode, an RPC server, and extensions that observe it. Other harnesses show two failure modes. One is a UI that calls into the core directly, so an outside client can never do everything the UI does. The other is a UI that talks to the core through an emulated network protocol inside one process, paying serialisation costs for nothing. Rust agent harnesses that began with separate code paths for the terminal and for other clients later rebuilt around one protocol (see the research notes).

## Options considered

**The contract.**

| Option | Assessment |
|---|---|
| Front ends call core functions directly; RPC is a separate layer written later | Fastest to start. Guarantees drift: the RPC layer always lags the UI. |
| One set of operation and event types, used in-process as typed values and out of process as serialised messages | A little more design up front. The built-in UI proves the protocol complete every day. |
| An HTTP API with a generated client, used even in-process | One surface, but the in-process UI pays for serialisation, and server-to-client requests (approvals) fit HTTP badly. |

**The wire format.**

| Option | Assessment |
|---|---|
| JSON-RPC 2.0, one JSON object per line | Requests, responses and notifications in both directions, which approvals need. Every language can read lines and parse JSON. The same framing as MCP's standard I/O transport, so one reader and writer serves the RPC server, the extension host and the MCP client. |
| JSON-RPC with length-prefixed framing | Tolerates embedded newlines, which JSON does not have anyway. Needs a header parser in every client. |
| HTTP with server-sent events | Familiar to web clients. Needs a port or a socket even for a child process, and two channels to get bidirectional requests. |
| gRPC or another binary protocol | Efficient and typed. A heavy dependency and a code-generation step for every client language. |

## Decision

- `cortex-protocol` defines `Op`, `Event` and the item types once, in Rust.
- In-process front ends exchange them over typed channels.
- `cortex-rpc` carries the same types as JSON-RPC 2.0 over newline-delimited JSON, on standard I/O first and local sockets second.
- An integer protocol version for breaking changes, capabilities for everything else.
- Amended on 2026-11-12: receivers tolerate what they do not know, **except in the values the harness enforces**, which fail closed ([0028](0028-enforced-values-fail-closed.md)).
- JSON Schema and TypeScript declarations are generated from the Rust types and checked in.

## Why

- Completeness by construction: the TUI can only do what the protocol can express.
- No cost in-process: no serialisation between the UI and the core.
- A TypeScript client needs only the generated declarations and a line reader. This is the requirement attached to the TUI language decision.
- Message volume is small. At 2,000 tokens per second the stream is a few hundred kilobytes per second of JSON, which the prototype's RPC variant handled with no measurable effect on latency ([0001](0001-tui-language.md)).

## Revisit when

A client needs binary payloads at high volume, or a browser client needs a transport that standard I/O and local sockets cannot give (the WebSocket transport planned after the first release covers the second case).
