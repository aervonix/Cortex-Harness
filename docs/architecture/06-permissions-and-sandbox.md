# Permissions, approvals and sandboxing

Three layers, each with a different job. The distinction matters because only one of them is a security boundary.

| Layer | Job | A security boundary? |
|---|---|---|
| **Policy rules** | Decide allow, ask or deny for each planned tool effect. | No. Rules reason about what a command says, not what it does. |
| **Approvals** | Put a person in the loop when the policy says ask. | No. People approve things they have not read. |
| **OS sandbox** | Limit what a child process can touch, whatever it runs. | Yes, where it is enforcing. |

The sandbox enforces. Approvals decide when to step outside it. Rules exist to reduce how often a person is asked. The threat model is in [../security.md](../security.md).

The default behaviour follows the [vision](../vision.md)'s autonomy policy: reads run, edits inside the project apply with a snapshot to undo them, and shell commands ask first unless they are on a safe list. The owner accepted this posture, and how far the sandbox goes on each OS, on 2026-11-12 ([decision 0009](../decisions/0009-sandboxing.md)).

## What is checked

Every tool call is planned before it runs ([05-tools.md](05-tools.md)), and the plan lists its effects.

| Effect | Target | Examples |
|---|---|---|
| `read` | a path | `read`, `search`, `list` |
| `write` | a path | `write`, `edit` |
| `exec` | a command line and working directory | `shell` |
| `network` | a host, or any | `fetch`, MCP over HTTP |
| `ext` | an extension or MCP tool by name | extension and MCP tools, plus any of the effects above that an extension's script asks the host to perform |

Built-in tools, extension tools and MCP tools all go through the same check. There is no path around it.

## Autonomy: one model

How much the agent may do without asking is described by one model. It covers a single session, a time-limited piece of delegated work, and a Coordinator team ([17-coordinator.md](17-coordinator.md)). There is no separate "permission mode", "lease" and "team level": every session holds a **lease**, and every lease has a **level**. The owner asked for this on 2026-11-12 ([decision 0026](../decisions/0026-one-autonomy-model.md)).

### Levels

A level says who decides when no rule has already settled the question: the **holder** of the lease, which is the agent, or its **supervisor**. The supervisor of an ordinary session is the user. The supervisor of an Employee is its Manager, as far as the Manager's own lease reaches, and the user beyond that.

| Level | Reads | Edits inside the lease's scope | Commands on a safe list | Other commands | Anything beyond the lease's scope |
|---|---|---|---|---|---|
| `read-only` | holder | refused | holder, for read-only commands | supervisor | supervisor for reads; writes refused |
| `supervised` (default) | holder | holder, with a snapshot | holder | supervisor | supervisor |
| `guided` | holder | holder, with a snapshot | holder | holder, for the kinds of command the lease names, inside an enforcing sandbox; otherwise supervisor | supervisor |
| `autonomous` | holder | holder, with a snapshot | holder | holder, inside an enforcing sandbox | supervisor |
| `unrestricted` | holder | holder | holder | holder, with no sandbox | holder |

- **`supervised` is the vision's default**, on every OS: reads and in-project edits do not ask, and shell commands ask unless they are on a safe list. The command line and configuration call a session's level its *mode* (`--permission-mode supervised`).
- **The scope** of a lease is its rules and its sandbox policy: which paths may be edited, which commands may run, whether tools may use the network. For a session's ordinary lease the scope is the workspace. "Beyond the scope" covers leaving the sandbox, writing outside the workspace, and network access from tools.
- **The safe list** is three lists together: the built-in table of read-only commands (below), commands the user approved for the session or the project, and commands learned for the project and accepted by the user ([14-adaptation.md](14-adaptation.md)), such as its build and test commands.
- **The sandbox applies at every level except `unrestricted`.** A command that runs, whether it was asked about or not, runs inside the sandbox where one is enforcing. Asking and confining are separate protections.
- **`guided` and `autonomous` need an enforcing sandbox.** They are refused, with an explanation, where the sandbox is not enforcing: Windows until its sandbox step, or a Linux kernel without Landlock. There, `supervised` is the highest level short of `unrestricted`, for single sessions and for teams alike.
- **`unrestricted` must be chosen explicitly**, by flag or user configuration. A project cannot select it, nor `guided` or `autonomous`. It cannot be given to a team or passed down to a child session. The status line shows any level above `supervised` prominently.
- **The decisions a team adds** (the plan, starting Employees, merging, spending past a threshold) follow the same ladder. Their rows are in [17-coordinator.md](17-coordinator.md).
- A front end that cannot ask (headless without pre-approval) treats "supervisor" as "refused" and says so in the result.

### Leases

Everything a session is allowed to do is one value.

```rust
pub struct Lease {
    pub id: LeaseId,
    pub grant: Grant,                   // what is allowed
    pub stop_when: Vec<StopCondition>,  // when the holder must stop and report
    pub expires: Expiry,                // Never | At(time) | EndOfTask | EndOfSession
    pub granted_by: Grantor,            // User | Parent(LeaseId)
}

pub struct Grant {
    pub level: Level,
    pub rules: Vec<Rule>,               // allow / ask / deny
    pub sandbox: SandboxPolicy,         // writable roots, protected paths, network
    pub models: Vec<ModelId>,
    pub budget: Budget,                 // money, tokens, requests, wall-clock time
    pub limits: HardLimits,             // what no level relaxes
}

impl Lease {
    /// The widest lease inside both. Never wider than `self`.
    pub fn narrow(&self, requested: &Lease) -> Lease;
    /// True if everything `self` permits, `parent` permits too.
    pub fn is_within(&self, parent: &Lease) -> bool;
}
```

There are three kinds of lease, and they are the same type.

| Kind | Who grants it | Lasts | Used for |
|---|---|---|---|
| **Standing lease** | Built from configuration, flags and the user's stored approvals. | The session. | Every session. Its level is the configured mode. Its budget is the configured session and daily limits. |
| **Task lease** | The user, for one piece of work, confirmed in exact terms. | Until the work is done, the time or budget runs out, a stop condition fires, or the user revokes it. | "For the next 30 minutes or 2 dollars, edit under `src/` and `tests/`, run the build and the tests, and stop if the tests fail three times running." |
| **Team lease** | The user, to a Manager session. Each Employee holds a **sub-lease** narrowed from it. | As a task lease. | The Coordinator. |

- **Granting a task lease.** The user says what they want in a sentence, or picks a preset. The model may draft the terms, but a draft is only text. The harness shows the exact terms it will enforce (scope, level, budget, time, stop conditions) and the user confirms them. Only a client in the `user` role can create, extend or revoke a lease ([11-rpc-protocol.md](11-rpc-protocol.md)). Nothing a model writes can.
- **Inside a task lease** the holder does not ask for what the lease covers. **At its edge** it does not start asking question after question: it stops at the next safe boundary, falls back to the standing lease, and reports what it did, what remains of the task, and what it would need to continue. The user can extend the lease or carry on step by step.
- **Stop conditions** are checked by the harness: verification failed a set number of rounds in a row, an action outside the scope was needed, a spending threshold was crossed, the time ran out. They are data, not instructions to the model.
- **A parent can hand a child a subset, and only a subset.** A session that starts another session (a run, or an Employee) proposes a lease, and the harness gives the child `parent.narrow(proposed)`. Narrowing is defined field by field: the lower level; the parent's `deny` and `ask` rules always carried over; `allow` rules kept only where the parent allows the same thing; writable roots intersected; the network allowed only if both allow it; budgets capped by what the parent has left; an expiry no later than the parent's; the stop conditions of both.
- **What reads the lease.** The permission engine and the sandbox launcher read only the current lease's grant. The limits ledger ([03-agent-loop.md](03-agent-loop.md)) charges the lease's budget before each request.
- **A task lease cannot exceed what the user could configure.** Managed configuration caps it, and a project cannot grant one.
- The status line shows the level and, for a task lease, what remains of its time and budget. `/lease` shows the full terms.
- Property tests check that `narrow` always returns a lease within both inputs, and that no sequence of narrowing widens anything.
- **A lease that is not understood in full is refused whole.** The wire form of a lease, a grant, a rule, a sandbox policy, a budget, the hard limits and a stop condition rejects any field and any value it does not know, with the error code `lease_not_understood`. Elsewhere the protocol ignores what it does not know; here that would let a build that predates a restriction drop it, and hold a wider lease than was granted ([decision 0028](../decisions/0028-enforced-values-fail-closed.md)).

**In the first release:** the types, the standing lease, the levels `read-only`, `supervised`, `autonomous` and `unrestricted`, and `narrow` with its tests. Settings profiles, headless flags and the non-interactive policy of runs all compile to leases. **After it:** task leases and the `guided` level (one build step), then team leases with the Coordinator, which is built on that step. See the [build plan](../build-plan.md).

## Rules

Rules live in configuration ([08-configuration.md](08-configuration.md)).

```toml
[[permissions.rules]]
effect = "exec"
match  = "cargo test *"
action = "allow"

[[permissions.rules]]
effect = "write"
match  = "**/.env*"
action = "deny"

[[permissions.rules]]
effect = "read"
match  = "~/notes/**"
action = "allow"
```

- **Matching.** Path effects match with globs against the canonical path. Exec effects match against the analysed command (below). Extension and MCP tools match by `server/tool` name.
- **Evaluation.** All matching rules are collected. `deny` beats `ask` beats `allow`. If no rule matches, the level's default applies. Order in the file does not matter, so a broad allow can never hide a deny.
- **Layers.** Managed rules cannot be weakened by user rules, and user rules cannot be weakened by project rules. A project's rules can add `deny` and `ask`. A project's `allow` rules take effect only if the user has trusted the project and are shown at that time.
- **Built-in deny rules** protect the harness's own files and credentials: the configuration directory, credential storage, and session logs of other projects.

## Shell command analysis

Rules on commands are a convenience. They are useful only if they fail closed.

1. The command line is parsed into a sequence of simple commands. Pipelines and the `&&`, `||` and `;` operators are split, and each part is checked on its own.
2. If the command contains anything the analyser does not model, the rule result is "ask": command substitution, process substitution, backticks, `eval`, here-documents, redirection to a path outside the workspace, variable assignment prefixes, subshells, function definitions, or a parse error.
3. An `allow` rule applies only if every part is allowed.
4. A small built-in table marks commands and argument forms that are read-only (for example listing, printing, and status commands with fully enumerated safe flags). Only these are scheduled as read-only. Commands that can write through an option, such as `git`, `find`, `sed` and `tar`, are never in the table as a whole.
5. The analyser never runs anything. It never calls a shell, an interpreter or a program found in the workspace.

On Windows the same rules apply to PowerShell through a conservative tokenizer: one command, literal arguments, no script blocks, no invocation operators, no encoded commands. Anything else is "ask". `cmd.exe` syntax is not analysed at all.

Which parser to use for the POSIX shell grammar is decided in the permissions build step, between a pure-Rust parser and a hand-written tokenizer. A C grammar is excluded by the dependency policy.

Published bypasses of command allow-lists in other agent tools are summarised in the research notes. They are the reason for rule 2 and for treating this layer as convenience only.

## Approvals

When the answer is "ask", the core sends an approval request to the front end and the tool call waits.

**The request shows** the tool, the one-line summary from the plan, and the details: the full command and working directory, or the diff, or the path. Nothing is hidden or shortened in the detail view, and control characters are made visible.

**The choices.**

| Choice | Effect |
|---|---|
| Allow once | This call only. |
| Allow for this session | Adds a session rule, held in memory, for the matching effect. For commands, the rule covers the command's program and subcommand, not the whole line. |
| Always allow in this project | Writes a rule to the user's per-project state (not to the repo). |
| Deny | The call gets an error result. |
| Deny with a note | As deny, and the note goes to the model as the reason. |

An approval never outlives its scope. Session approvals are not saved. Stored approvals are listed and removed with `cortex permissions`.

**In the RPC protocol** an approval is a request from the server to the client ([11-rpc-protocol.md](11-rpc-protocol.md)). If no client answers within the configured time, the answer is deny.

**When a session is supervised** by another session, its approval requests go to the supervisor first. The supervisor may answer only what its own lease covers, and passes the rest up to the user. A user attached to the session can always answer directly. See [17-coordinator.md](17-coordinator.md).

## Project trust

Opening a project for the first time asks one question before anything from the project that can run code or grant permissions is used.

- **Needs trust:** project configuration that sets permission `allow` rules, hooks, extensions, MCP servers (including ones found in another tool's configuration file in the repository), and learned or shared commands used for the safe list.
- **Does not need trust:** project instruction files (`AGENTS.md`, `CLAUDE.md`), the shared project profile, custom command files and skills. They are text for the model. They cannot grant anything, and the injection risk they carry is covered in [../security.md](../security.md).
- Trust is stored per canonical project path in the user's state directory, with a hash of the trusted definitions. If a hook, extension or MCP server definition changes, it is disabled until the user approves the change. This closes the "approved once, changed later" hole.
- Headless and RPC modes cannot ask. They skip untrusted project resources and report that they did, unless a flag grants trust for the run.

## The sandbox

`cortex-sandbox` defines a policy and applies it when a child process starts.

```rust
pub struct SandboxPolicy {
    pub read: PathSet,        // default: everything readable
    pub write: PathSet,       // default: the workspace and a private temp directory
    pub protect: PathSet,     // inside `write`, but must not be modified
    pub network: Network,     // Off | Full
}
```

**Default policy at the `supervised` level:** read anywhere, write only inside the workspace and a per-session temporary directory, no network.

**Protected paths inside the workspace:** `.git/hooks`, `.git/config`, the project's `.cortex` directory, and shell start-up files. A process that can write these can run code later, outside the sandbox.

### How it is applied

The harness re-executes itself as a small launcher: `cortex` with a hidden subcommand, the policy passed on a file descriptor, then the command. The launcher is single-threaded and does nothing but apply the policy and `exec` the target. Doing this in a fresh process avoids the hazards of running setup code between `fork` and `exec` in a multi-threaded program. The cost is one extra process start of our own binary per command, which the performance budget covers.

### Linux: in the first release

| Mechanism | Used for | Needs |
|---|---|---|
| `no_new_privs` | Required by the two below. | Nothing. |
| Landlock | Filesystem allow-list: read-everything, write only to the listed roots. | Kernel 5.13 or later with Landlock enabled. Rule coverage improves with newer kernels. |
| seccomp filter | Network off (deny creating internet sockets), plus denying the calls that would let a process undo the sandbox or inspect others (new user namespaces, tracing, and similar). | Nothing. |
| Process group and death signal | Clean-up. | Nothing. |

- No root, no helper program, no user namespaces, no container runtime. This matters because Ubuntu 24.04 and later restrict unprivileged user namespaces by default, which breaks the namespace-based approach other tools use unless an administrator installs a profile.
- **Detection.** At start-up the harness asks the kernel which Landlock version it has. If Landlock is missing or disabled, the sandbox is reported as "not enforcing" in the status line, commands that run are not confined, and the `guided` and `autonomous` levels are refused.
- **Known limits, stated plainly.**
  - Landlock rules only add permissions. With the workspace writable, protected paths inside it cannot be blocked by Landlock alone. The harness therefore checks the protected paths after every sandboxed command and, if one changed, stops the turn, shows the change and offers to restore it from a checkpoint taken before the command. This detects; it does not prevent.
  - Reads are not restricted by default, so a sandboxed command can read files such as SSH keys. It cannot send them anywhere while the network is off. A stricter read policy is a configuration option.
  - Network control is all or nothing. There is no per-host allow-list.

### Linux: later, opt-in

A namespace-based backend (mount and network namespaces, with protected paths mounted read-only and a proxy for per-host network rules) where the system allows unprivileged user namespaces. It gives prevention where the default gives detection. It is optional because it cannot be relied on to work.

### Windows: in the first release

| Mechanism | Used for |
|---|---|
| Job Object, kill on close | The whole process tree dies with the command or the harness. Limits on process count and memory are available. |
| Approvals | Every shell command that no rule allows is asked. |

There is no filesystem or network confinement of child processes on Windows in the first release. The status line says "sandbox: off", and the `guided` and `autonomous` levels are not available. Extension scripts are still confined, because their confinement is the script runtime and not the OS.

### Windows: later, after its own spike

An AppContainer-based backend: a per-session container identity, access-control entries that grant it the workspace, no network capability, inside a Job Object. It needs no administrator rights. It is a separate build step, after a spike, because three things are unproven:

1. Whether a console program behaves correctly under an AppContainer when attached to a pseudo-console.
2. How much of a developer's toolchain needs read grants outside system directories.
3. Reliable clean-up of the access-control entries after a crash.

Per-host network filtering on Windows needs a one-time administrator set-up in every tool that has it. It is out of scope.

### macOS: not a target now

The policy type is OS-neutral, so a Seatbelt backend can be added behind the same interface.

### What runs in the sandbox

| Child | Policy |
|---|---|
| Shell commands | The session policy. |
| Verification commands | The session policy. |
| Hook commands | Their own policy: read the workspace, no writes, no network, unless the hook's entry declares more and the user approved it. |
| Programs started by an extension script through the host | The narrower of the session policy and what the extension's manifest declares. |
| MCP stdio servers | Read the workspace, no writes, no network, unless the server's entry declares more and the user approved it. Many need the network. |

**What runs inside the harness process, and how it is confined.**

| Code | Confinement |
|---|---|
| Built-in file tools | The permission check on canonical paths. |
| Extension scripts | The script runtime ([12-extensions.md](12-extensions.md)). A script has no access to files, processes or the network of its own. Everything it does goes through host functions, and each of those is checked against the extension's manifest and the permission engine. This confinement does not depend on the OS, so it is the same on Windows as on Linux. |

### Leaving the sandbox

When a sandboxed command fails because of the sandbox, the result names the blocked operation. The model can ask to run the command again with more access. That request is an approval like any other, with the choices above, and the status line shows when a command ran outside the sandbox.

## Summary of what is realistic

| | Linux | Windows |
|---|---|---|
| First release | Enforcing filesystem and network sandbox for shell commands, verification, hook commands and MCP servers, with the stated limits. Extension scripts confined by the script runtime. Approvals to leave the sandbox. | Process-tree control. Approvals for every command not on a safe list. Extension scripts confined by the script runtime. No OS confinement of child processes. |
| Later | Namespace backend for prevention of protected-path writes and per-host network rules. | AppContainer backend after a spike. |
| Not planned | Anything that needs root or a daemon. | Anything that needs administrator rights, including per-host network filtering. |
