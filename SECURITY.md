# Security policy

Cortex-Harness runs model-chosen commands and edits files on your machine, so security reports are taken seriously. The threats the design considers, and how each is handled, are in [docs/security.md](docs/security.md).

## Reporting a vulnerability

Please report it privately, not in a public issue:

1. Open the repository's **Security** tab on GitHub.
2. Choose **Report a vulnerability**. This opens a private advisory that only the maintainers can see.

Include what you found, how to reproduce it, the version or commit, and your OS. If you have a fix in mind, say so.

A maintainer will reply in the advisory. Fixes for confirmed problems are released as soon as they are ready, and the advisory is then published, with credit to the reporter unless you ask otherwise.

## Supported versions

The project is in early development and has no releases yet. Until the first release, only the `main` branch is supported. After it, the latest release is.

## What counts

Reports of these are especially welcome:

- A way for a model, a tool result, a repository's contents, an extension or an MCP server to act outside the permissions the user gave.
- An escape from the sandbox, or a command that runs without the approval the policy requires.
- A leak of credentials or session contents: to disk with the wrong permissions, to logs, or over the network.
- Anything that sends data off the machine that the user did not ask to send.

Behaviour that the user explicitly allowed (for example, running with the `unrestricted` level) is not a vulnerability.
