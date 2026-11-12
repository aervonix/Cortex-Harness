# 0009: Autonomy default, permission rules and how far to sandbox

- **Status:** Accepted by the owner on 2026-11-12. The level names follow [0026](0026-one-autonomy-model.md).
- **Date:** 2026-11-12

## Context

The harness runs commands chosen by a model. The vision sets the default: reads run automatically, edits inside the project apply automatically with a snapshot to undo them, and shell commands ask first unless they are on a learned or approved safe list.

The reference harnesses mark the two ends. One has no permission system by design and tells users to bring their own container. The other has allow, ask and deny rules and no isolation, and says in its own security notes that the rules are a usability aid. Other agent tools sandbox with OS mechanisms, and their experience is in the research notes.

## Options considered

**Where the boundary is.**

| Option | Assessment |
|---|---|
| No permissions, no sandbox | Honest and simple. Unacceptable as a default for a broad audience. |
| Rules and approvals only | Familiar. Documented bypasses exist in every tool that relies on parsing commands. Not a boundary. |
| OS sandbox for child processes, with rules and approvals above it | A real boundary where the OS provides one. More work, and uneven across platforms. |
| Containers or virtual machines | Strong. Requires software the user may not have, and separates the agent from the developer's real environment. |

**Linux mechanism.**

| Option | Needs | Assessment |
|---|---|---|
| Landlock and seccomp, applied by a launcher | Kernel 5.13 or later with Landlock on. No root, no helper, no user namespaces. | Works for an unprivileged single binary. Cannot protect a path inside a writable directory, and cannot filter the network by host. |
| Mount and network namespaces (the approach of an external sandboxing helper) | Unprivileged user namespaces. | Stronger: read-only subpaths, per-host network through a proxy. Blocked by default on Ubuntu 24.04 and later unless an administrator installs a profile. |

**Windows mechanism.**

| Option | Needs | Assessment |
|---|---|---|
| Job Objects | Nothing. | Process-tree control and limits. No confinement. |
| AppContainer with access-control grants on the workspace | No administrator rights, per the platform documentation. | Real file and network confinement. Three unknowns to resolve first (console behaviour, toolchain read access, clean-up after a crash). |
| Dedicated sandbox users with firewall rules | A one-time administrator set-up. | What other tools use for per-host network control. Out of reach for an unprivileged binary. |

**Rule evaluation.** Last-match-wins (as in one reference harness) against fixed precedence. With last-match-wins, the order of rules across merged layers decides the outcome, and a broad allow written later can undo a deny.

## Decision

- **Default level `supervised`** (called `standard` in earlier drafts), exactly as the vision states. The safe list is the built-in read-only table, the user's approvals, and learned commands the user accepted.
- **Rules:** deny beats ask beats allow, whatever the order. Projects may tighten freely and loosen only with trust.
- **Command analysis fails closed.** Anything it cannot model asks.
- **Sandbox whenever a command runs,** where the OS has one, at every level except `unrestricted`. Asking and confining are separate protections.
- **Linux, first release:** Landlock and seccomp through a re-executed launcher. Protected paths inside the workspace are checked after each command.
- **Windows, first release:** Job Objects and approvals. AppContainer is a separate step after a spike.
- **An opt-in `autonomous` level** runs commands without asking inside an enforcing sandbox, for users who want fewer prompts. `guided`, between the two, arrives with task leases.
- **Extension scripts** are confined by the script runtime on every OS ([0005](0005-extension-mechanism.md)).

## Why

- The vision's default asks often for commands. The sandbox is not what removes those prompts; the safe list is. The sandbox is what makes a wrong approval, or a safe-listed command that turns out not to be safe, survivable.
- Landlock and seccomp are the only Linux mechanisms that work for a single unprivileged binary on every current distribution.
- Shipping a Windows sandbox before its three unknowns are resolved would risk a default that breaks common toolchains.

## What is given up

- On Linux, a sandboxed command can still damage the workspace itself and can read files outside it.
- On Windows in the first release, an approved command has the user's full rights.

## Revisit when

- The Windows spike resolves: build the AppContainer backend and enable `autonomous` there.
- Unprivileged user namespaces become reliably available: add the namespace backend for prevention instead of detection.
- Users find the default too chatty: the first lever is a better learned safe list, not a weaker default.
