# MCP

The Model Context Protocol (MCP) is how most third-party tools reach coding agents today. This page says what Cortex-Harness supports, when, and how MCP sits beside the extension system.

The protocol facts here were read from the MCP specification and SDK documentation in November 2026 and are summarised in the research notes. The specification changed fundamentally in July 2026. The MCP build step starts by re-reading it.

## Decision

**MCP client support is in the first usable release, limited to tools.** This is settled by the [vision](../vision.md): people switching from another agent bring their MCP servers with them, and the harness reads the configurations they already have. What is left to decide is scope, which is in [decision 0010](../decisions/0010-mcp-in-first-release.md).

| | First usable release | Later |
|---|---|---|
| Role | Client only. | The same. Cortex-Harness is not an MCP server. |
| Features | Tools: list and call. Tool-list change handling. Cancellation. | Prompts as slash commands. Resources. Questions from servers to the user. |
| Transports | Standard I/O (local servers). Streamable HTTP with static credentials (a bearer token or headers from the environment). | OAuth for remote servers. |
| Protocol eras | Both: the current stateless revision and the older handshake-based revisions that most existing servers still speak. | Tracking new revisions. |
| Deprecated features (sampling, roots, server logging, the old HTTP+SSE transport) | Not implemented. | Not planned. |

## Where MCP sits

MCP and extensions overlap on one thing, tools, and differ on everything else.

| | MCP servers | Script extensions ([12-extensions.md](12-extensions.md)) |
|---|---|---|
| Purpose | Use tools that already exist for other agents, and tools that need a real ecosystem behind them. | Change how this harness behaves, and add small tools that glue together what is already on the machine. |
| Can provide | Tools (later prompts and resources). | Tools, slash commands, hooks on the agent's lifecycle. |
| Runs as | A separate process, in any language. | JavaScript inside the harness's script runtime. |
| Protocol owner | The MCP project. | Us. |
| Written for | Any MCP client. | Cortex-Harness. The agent can write one on request. |

Rule of thumb for authors: if it needs a client library, a database driver or a long-running process, or should work in other agents too, write an MCP server. If it needs to gate, observe or change the agent, or is a small tool built from commands and files, write an extension.

Inside the harness they share machinery: the dynamic-tool registry behind `tool_search` and `tool_call`, the trust store with hash-pinning, the permission check and the output limits. MCP servers and hook commands also share the process launcher and the sandbox.

## How MCP tools reach the model

The common approach, declaring every MCP tool to the model, has two costs. Tool definitions fill the context (tens of thousands of tokens for a handful of servers), and any change to the tool list changes the request prefix, which breaks prompt caches and, on some providers, invalidates reasoning ([07-sessions-and-context.md](07-sessions-and-context.md)).

Cortex-Harness keeps the declared tool list fixed and reaches MCP tools through the same fixed pair of tools that serves extension tools, `tool_search` and `tool_call` ([05-tools.md](05-tools.md)).

- The system prompt lists each connected server with its one-line description and tool count. It does not list the tools.
- The model searches when it needs something, reads the schema in the result, and calls. Search results are ordinary tool results, so they are append-only content.
- `tool_call` validates the arguments against the server's schema before sending. A mismatch is an error result that includes the schema.
- Servers that connect, disconnect or change their tools mid-session do not touch the request prefix. A notice tells the model when a server's availability changes.

**Direct exposure, by choice.** A server entry can set `expose = "direct"`, optionally with a list of tool names. Those tools are declared to the model like built-in tools, named `<server>__<tool>`, at the start of the next context epoch. This suits a small server whose tools are used constantly. The harness warns when directly exposed definitions exceed a share of the context window (default 5%).

**Provider-side tool search.** Some providers can defer tool definitions and search them on the provider's side. An adapter may use that instead later. The fixed pair works on every provider and is the default.

**Promotion by use.** The statistics show which tools a user calls often. At the start of a context epoch the harness may expose the most-used few directly, inside the same context share limit, and says so in the status view. This is off until the statistics exist, and can be turned off.

The search is a simple ranked match over names and descriptions, done in the harness. It needs no model call.

## Configuration

```toml
[mcp.servers.docs]
command = "docs-mcp"                 # local server over standard I/O
args = ["--root", "."]
env = { DOCS_TOKEN = { env = "DOCS_TOKEN" } }
network = true                       # sandbox: allow network for this server
description = "Search the product documentation"

[mcp.servers.tracker]
url = "https://tracker.example.com/mcp"      # remote server
headers = { Authorization = { env = "TRACKER_AUTH" } }
expose = "direct"
tools = ["create_issue", "find_issue"]
timeout_ms = 30000
```

- A server is local (`command`) or remote (`url`). The schema is ours. `cortex mcp add` writes entries.
- **Existing configurations are read.** The harness looks for the project's `.mcp.json` and for the per-user files of common MCP clients. What it finds is listed at onboarding and by `cortex mcp import`, and can be brought in with one confirmation per server. A project's `.mcp.json` is also read live: its servers show up as proposed, and each needs the same trust and approval as a server written in our own format. The files are never modified.
- Per server: `enabled`, `expose`, an allow or deny list of tools, start-up and call timeouts, the sandbox additions it needs.
- Secrets are references to environment variables. They are never literals in the file.
- Servers defined in a project's configuration need project trust and are pinned by hash ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)).

## Lifecycle

- **Lazy, parallel start.** Servers are started in the background when a session starts. The first prompt does not wait for them. A `tool_search` or `tool_call` that needs a server still starting waits for it, up to the start-up timeout (default 10 s).
- **Era detection.** For each server the client probes for the current protocol revision and falls back to the older handshake, then remembers the answer for that server.
- **Tool lists** are cached per server with the lifetime the server gives. Change notifications refresh them.
- **Failure.** A server that fails to start or crashes is reported in the status line and to the model on its next use. Local servers are restarted on next use, with backoff.
- **Shutdown.** Standard input is closed, then the process tree is terminated through the same staged kill as shell commands.
- **Windows.** Many servers are started through script shims (`npx` and similar), which cannot be executed directly. The launcher resolves the program on the path, detects shims, and runs them through the command interpreter with arguments it has escaped itself, refusing arguments that cannot be escaped safely.

## Safety

MCP servers are third-party code and third-party text. Both are treated as untrusted.

| Risk | Handling |
|---|---|
| A server process does damage | Local servers run in the sandbox with a narrow default policy: read the workspace, no writes, no network unless the entry declares it and the user approved it. |
| Tool descriptions carry hidden instructions | Descriptions reach the model only as search results, which are marked as third-party content. They are size-capped (2 KiB each). `cortex mcp inspect` shows them in full to the user. |
| A server changes a tool after approval | Each tool definition is hashed when first approved. A changed definition needs approval again before it can be called. |
| One server's text steers another server's tools | Calls are checked per `server/tool` by the permission engine. A description cannot grant permission. |
| A tool returns a huge result | The standard output policy applies ([05-tools.md](05-tools.md)): truncation, with the full result as an artifact. |
| A tool claims to be read-only | Annotations from servers are hints for the UI only. Permission defaults do not trust them: MCP tools ask by default until a rule allows them. |
| Secrets in configuration | Environment references only. Environment variables passed to a server are an explicit list, not the whole environment. |

More in [../security.md](../security.md).

## Implementation

- `cortex-mcp` is the only crate that knows MCP. It registers its servers' tools with the dynamic-tool registry behind `tool_search` and `tool_call`, and exposes directly exposed tools through the ordinary `Tool` trait.
- It uses the official Rust SDK (`rmcp`) behind a small transport trait of our own, with only the client features enabled. The SDK already handles both protocol eras, which is the expensive part to track by hand. Its measured build and size cost is in [../measurements/dependency-costs.md](../measurements/dependency-costs.md).
- Because the SDK is confined to one crate behind our trait, replacing it with a hand-written client for the standard I/O transport is possible later without touching anything else. The condition for doing that is in the decision record.
- The `mcp` cargo feature builds the harness without it.

## OAuth, later

Remote servers that need OAuth are a separate step after the first release. It involves a browser hand-off with a loopback redirect, a paste-the-URL fallback for remote shells, token storage in the credential store keyed by issuer, and a client identity document that the project must host. It is large, and static credentials cover most developer use meanwhile.
