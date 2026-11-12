# Permissions, approvals and sandboxing

Three layers, each with a different job. The distinction matters because only one of them is a security boundary.

| Layer | Job | A security boundary? |
|---|---|---|
| **Policy rules** | Decide allow, ask or deny for each planned tool effect. | No. Rules reason about what a command says, not what it does. |
| **Approvals** | Put a person in the loop when the policy says ask. | No. People approve things they have not read. |
| **OS sandbox** | Limit what a child process can touch, whatever it runs. | Yes, where it is enforcing. |

The sandbox enforces. Approvals decide when to step outside it. Rules exist to reduce how often a person is asked. The threat model is in [../security.md](../security.md).

The default behaviour follows the [vision](../vision.md)'s autonomy policy: reads run, edits inside the project apply with a snapshot to undo them, and shell commands ask first unless they are on a safe list. How far to go beyond that is the owner's call, and those points are marked provisional in [../build-plan.md](../build-plan.md).

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

## Modes

A mode is a named set of defaults. Rules refine it.

| Mode | Reads | Edits inside the workspace | Shell | Network from tools | Outside the workspace |
|---|---|---|---|---|---|
| `read-only` | allow | deny | safe-listed read-only commands run; anything else asks | ask | read: ask; write: deny |
| `standard` (default) | allow | allow, with a snapshot | ask first, unless the command is on a safe list | ask | ask |
| `autonomous` | allow | allow, with a snapshot | runs without asking inside an enforcing sandbox; asks to leave it | off inside the sandbox; ask otherwise | ask |
| `unrestricted` | allow | allow | allow, no sandbox | allow | allow |

- `standard` is the vision's default, on every OS.
- **The safe list** is three lists together: the built-in table of read-only commands (below), commands the user approved for the session or the project, and commands learned for the project and accepted by the user ([14-adaptation.md](14-adaptation.md)), such as its build and test commands.
- **The sandbox applies in every mode except `unrestricted`.** A command that runs, whether it was asked about or safe-listed, runs inside the sandbox where one is enforcing. Asking and confining are separate protections.
- `autonomous` is for people who want fewer prompts and have an enforcing sandbox. It is refused, with an explanation, where the sandbox is not enforcing (Windows in the first release, or a Linux kernel without Landlock).
- `unrestricted` must be chosen explicitly, by flag or user configuration. A project cannot select it or `autonomous`. The status line shows both prominently.
- A front end that cannot ask (headless without pre-approval) treats "ask" as "deny" and says so in the result.

## Permissions are data

Everything a session is allowed to do is one value, a **grant**: its mode, its rules, its sandbox policy, the models it may use, its budget, and its hard limits.

```rust
pub struct Grant {
    pub mode: Mode,
    pub rules: Vec<Rule>,
    pub sandbox: SandboxPolicy,
    pub models: Vec<ModelId>,
    pub budget: Budget,
    pub limits: HardLimits,
}

impl Grant {
    /// The widest grant inside both. Never wider than `self`.
    pub fn narrow(&self, requested: &Grant) -> Grant;
    /// True if everything `self` permits, `parent` permits too.
    pub fn is_within(&self, parent: &Grant) -> bool;
}
```

- A session's grant is built from configuration, flags and the user's approvals. The permission engine and the sandbox launcher read only the grant.
- **A parent can hand a child a subset, and only a subset.** A session that starts another session (a run, or an Employee under the Coordinator) proposes a grant, and the harness gives the child `parent.narrow(proposed)`. Nothing a model writes can make the child's grant wider than the parent's.
- Narrowing is defined field by field: the stricter mode; the parent's `deny` and `ask` rules always carried over; `allow` rules kept only where the parent allows the same thing; writable roots intersected; the network allowed only if both allow it; budgets capped by what the parent has left.
- A property test checks that `narrow` always returns a grant within both inputs.

This is in the first release because settings profiles, headless flags and the non-interactive policy of runs all compile to grants, and because the Coordinator ([17-coordinator.md](17-coordinator.md)) depends on it.

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
- **Evaluation.** All matching rules are collected. `deny` beats `ask` beats `allow`. If no rule matches, the mode's default applies. Order in the file does not matter, so a broad allow can never hide a deny.
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

**When a session is supervised** by another session, its approval requests go to the supervisor first. The supervisor may answer only what its own grant covers, and passes the rest up to the user. A user attached to the session can always answer directly. See [17-coordinator.md](17-coordinator.md).

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

**Default policy for `standard` mode:** read anywhere, write only inside the workspace and a per-session temporary directory, no network.

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
- **Detection.** At start-up the harness asks the kernel which Landlock version it has. If Landlock is missing or disabled, the sandbox is reported as "not enforcing" in the status line, commands that run are not confined, and `autonomous` mode is refused.
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

There is no filesystem or network confinement of child processes on Windows in the first release. The status line says "sandbox: off", and `autonomous` mode is not available. Extension scripts are still confined, because their confinement is the script runtime and not the OS.

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
