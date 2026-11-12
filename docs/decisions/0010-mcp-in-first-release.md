# 0010: MCP client scope in the first release

- **Status:** Accepted in principle (the vision settles that MCP client support is in). The scope below is provisional.
- **Date:** 2026-11-12

## Context

MCP is how most third-party tools reach coding agents. The vision requires that people switching from another agent keep their MCP servers, and that existing MCP configurations are read.

The protocol changed fundamentally in its July 2026 revision: no handshake, no sessions, server requests replaced by retried results, several features deprecated. Most servers in use still speak the older, handshake-based revisions, so a client must handle both. The official Rust SDK does. Details are in the research notes and must be re-read when the step starts.

## Options considered

**Scope.**

| Option | Assessment |
|---|---|
| Tools only, standard I/O and HTTP with static credentials | Covers the great majority of developer use. Small enough for one step. |
| Add OAuth for remote servers | Needs a browser hand-off, a loopback redirect, a hosted client identity document, token storage per issuer, and a paste-the-URL fallback. A step of its own. |
| Add prompts, resources, server-to-user questions | Useful, optional, and each is more surface. |

**Implementation.**

| Option | Clean release build (4 cores) | Size added | Crates | Assessment |
|---|---|---|---|---|
| Official SDK, standard I/O only | 27.6 s | 1.8 MB | 60 | Handles both protocol eras. |
| Official SDK, standard I/O and HTTP with authentication support | 45.3 s | 3.6 MB | 128 | The same, for remote servers. Shares the HTTP stack the harness already has, so the marginal cost in the full binary is lower than the figure here. |
| Hand-written client | none beyond what exists | small | 0 | Small for standard I/O with one protocol era. Tracking two eras and a moving specification by hand is the real cost. |

**How tools reach the model.** Declaring every tool (one reference harness) costs context and makes the tool list change mid-session. Hiding tools behind a scripting sandbox plus a search tool (the other) keeps the prompt stable at the price of a large subsystem. A fixed search-and-call pair keeps the prompt stable with no extra subsystem.

## Decision

- Tools only. Standard I/O, and HTTP with static credentials. Both protocol eras.
- The official SDK, client features only, confined to `cortex-mcp` behind our own transport trait and a cargo feature.
- Tools are reached through `tool_search` and `tool_call`, shared with extension tools, with opt-in direct exposure per server.
- Existing configurations are read: the project's `.mcp.json` live, and other clients' per-user files by import.
- Every tool definition is hashed at approval. A change needs approval again.

## Why

- The SDK's cost is real (the largest single addition after the HTTP stack) and is paid for something that would otherwise be ours to track through a specification that is still moving.
- Confinement to one crate behind a trait keeps the option of replacing it.
- A stable tool list is required by the context design ([0016](0016-context-management.md)), and the search-and-call pair gives it on every provider.

## Revisit when

- The SDK's churn costs more than tracking the specification would: write the standard I/O client by hand and keep the SDK for HTTP.
- Binary size or CI time is over budget: the `mcp` feature can be measured in isolation and trimmed.
- Users need a remote server that requires OAuth: that step moves up.
