# Security notes

What can go wrong when a model drives tools on a developer's machine, and what Cortex-Harness does about each threat. The mechanisms are specified in [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md); this page is the threat-by-threat view.

## Position

- The model is not trusted. Its output is treated like input from the network: it may be wrong, and it may be steered by text an attacker placed where the model would read it.
- The user is trusted. So is the user-level configuration.
- A repository is not trusted until the user says so, and then only for the specific definitions they saw.
- **Only the OS sandbox is a boundary.** Permission rules and approval prompts reduce risk and catch mistakes. They do not stop a determined attack, and the docs and the UI never suggest they do.
- Limits are stated where they exist. A weaker platform (Windows in the first release) shows its state in the status line and falls back to asking.

## 1. Model-driven shell commands

**Threat.** The model runs a command that destroys data, changes the system, installs something, or sends data out. It may do so by mistake or because injected text told it to.

| Mitigation | Covers | Limits |
|---|---|---|
| Sandbox for every shell command where enforcing (Linux): writes only inside the workspace and a private temporary directory, no network. | Damage outside the workspace. Data leaving the machine. | Reads are unrestricted by default, so secrets can be read (but not sent while the network is off). Not available on Windows in the first release. |
| Protected paths inside the workspace (`.git/hooks`, `.git/config`, `.cortex/`, shell start-up files) are checked after each sandboxed command. A change stops the turn and offers a restore. | Planting code that runs later outside the sandbox. | Detection, not prevention, with the default Linux backend. |
| Approval before any command leaves the sandbox, and before every unlisted command where there is no sandbox. The prompt shows the full command and directory with control characters made visible. | Unintended escalation. | People approve without reading. |
| Command analysis that fails closed: anything it cannot model is "ask", never "allow". Read-only status only for commands with fully enumerated safe flags. | Allow-rules being used to smuggle a different command. | Convenience layer. Not a boundary. |
| Process-tree control: process groups and Job Objects, staged kill, clean-up on exit and crash. | Runaway and orphaned processes. | A process that leaves the group on Linux can outlive it. |
| Timeouts, output bounds, and the loop guards (request limit, repetition guard, cost limit). | Runaway loops and resource exhaustion. | |
| Credentials are removed from the environment given to commands. | A command reading the provider key from its environment. | The user's other secrets in the environment are passed through unless listed. |
| File tools checkpoint before writing. | Recovering from a bad edit. | Shell-made changes are not checkpointed. |

**Residual risk.** Inside the workspace, a sandboxed command can delete or corrupt anything. Version control is the recovery path. On Windows in the first release, an approved command has the user's full rights.

## 2. Prompt injection through files and web content

**Threat.** Text the model reads (a source file, a README, a tool result, a web page, an MCP tool description, an instruction file in a cloned repository) contains instructions. The model follows them: it runs commands, edits files, or sends data out.

No harness can make a model immune to this. The design limits what a successful injection can do.

| Mitigation | Effect |
|---|---|
| The sandbox and approvals above. | An injected instruction to run something harmful meets the same wall as any other command. With the network off, it cannot send data anywhere. |
| **No silent path from untrusted text to the network.** Network access from tools is "ask" by default. Fetching a URL (when that tool exists) is checked per host. | The usual injection goal, sending secrets or code to an attacker, needs a visible approval. |
| Tool results are data. They are delimited in the request as tool output through the provider's tool-result structure, never placed in the system prompt or presented as a user message. | The model can tell where text came from. This helps and is not a guarantee. |
| Third-party text is labelled: MCP tool descriptions and results, extension-provided content and fetched pages carry their source. | The same. |
| Instruction files cannot grant anything. A project's permission rules, hooks, extensions and MCP servers need trust. | A cloned repository cannot configure its way to running code. An instruction file can still mislead the model, within the sandbox. |
| Nothing fetches automatically. Opening a session does not retrieve URLs, install packages or start project-defined processes without trust. | No injection before the user has done anything. |
| Approval prompts show exactly what will run. Hidden and look-alike characters are made visible. | An injection cannot hide part of a command from the person approving it. |
| For the future fetch tool: block loopback, link-local, private and metadata addresses after name resolution, cap response size, strip markup to text, and never send credentials. | Server-side request forgery and oversized or active content. |

**Residual risk.** A model that has been steered can still make bad edits inside the workspace and can waste money. A user who approves a network request can still be tricked. Review of diffs and the cost limits are the backstops.

## 3. Malicious or compromised extensions and MCP servers

**Threat.** An extension script, a hook command or an MCP server is malicious from the start, is replaced by a malicious version, or is defined by a repository the user cloned. A special case: the harness itself writes an extension at the user's request, and injected text steers what it writes.

| Mitigation | Effect |
|---|---|
| **Extension scripts run in an embedded script runtime with no access of their own.** A script cannot open a file, start a process or reach the network. It can only call host functions, and each call is checked against the extension's manifest and then against the session's permission rules. | A malicious or badly written script is confined on every OS, including Windows, where no OS sandbox is available in the first release. It cannot read the harness's credentials: no host function returns them. |
| Each script runtime has a memory limit, a time limit per call and an instruction interrupt. | A runaway script is stopped. It cannot stall the agent. |
| A manifest lists what an extension may ask the host to do: which programs it may run, which paths it may read or write, which hosts it may contact. | The user approves a short, readable list, and the runtime enforces it. |
| Programs an extension asks the host to run go through the sandbox launcher, under the narrower of the session policy and the manifest. | The same OS confinement as shell commands, where the OS has it. |
| **Self-written extensions are reviewed before they load.** The harness shows the manifest and the full source, checked for syntax, and loads nothing until the user approves. The approval is pinned to a hash. | Injected text can at most produce a proposal that the user sees in full. What it could do if approved is bounded by the manifest shown beside it. |
| MCP servers and hook commands are separate processes, started in the sandbox with a narrow default: read the workspace, no writes, no network. More must be declared and approved. | A malicious server is confined where the sandbox is enforcing. A crash cannot hurt the harness. |
| Project-defined hooks, extensions and MCP servers need project trust, including MCP servers found in another tool's configuration file in the repository. | Cloning a repository runs nothing. |
| Trust is pinned to a hash of the definition (script and manifest; command and arguments; for MCP, each tool definition). A change disables it until approved again. | The "approved once, changed later" attack. |
| Their tools go through the same permission check as built-in tools, and ask by default. | A tool cannot act without the policy's say. |
| Their output passes the same truncation and is labelled as third-party. | Flooding and some injection. |
| The provider credentials are never passed to them. Environment variables given to MCP servers and hook commands are an explicit list. | Credential theft. |
| Hook decisions are typed and bounded: allow, deny, ask, or a rewritten argument that is shown to the user and re-checked by policy. Hooks have deadlines. | A hook cannot silently widen a command or stall the agent. |
| No installation at start-up. The harness never downloads or installs extension code on its own. | Supply-chain surprises at launch. |

**Residual risk.**
- The script engine is a C library. A memory-safety bug in it could let a script escape the runtime. Scripts are code the user approved, the engine is kept current, and it is the one C dependency in the binary; this risk is the price of an engine that authors and models already know. See [decision 0005](decisions/0005-extension-mechanism.md).
- On Windows in the first release, MCP servers and hook commands are not confined by the OS.
- An extension the user approved with network access to a host can send that host whatever its manifest lets it read. This is stated at approval time.

The reference harnesses load extensions into the agent process with the user's full rights. See [reference-notes.md](reference-notes.md).

## 4. Secrets in logs and session files

**Threat.** Credentials or other secrets end up on disk in logs, session files, artifacts, crash reports or exports, and leak from there.

| Mitigation | Effect |
|---|---|
| Credentials are a `Secret` type. It prints as `[redacted]`, and it cannot be serialised. | The harness's own keys cannot reach a log or session by accident. A test greps all output of the test suite for the test key. |
| Request logging prints an allow-list of headers. Bodies are logged only at debug level, truncated and filtered. | Keys in headers are never logged. |
| Credentials are stored in the OS keychain or an owner-only file, never in the configuration file, and are not readable by child processes through the environment. | |
| Session logs, artifacts, checkpoints and log files are created owner-only. | Other local users cannot read them. |
| A redaction filter runs when session entries and artifacts are written: the harness's own credentials by value, and strings in well-known key formats. | Catches common accidents, such as a command that prints its environment. Best-effort, and described as such. |
| No sharing or upload feature. Export applies a stricter filter and lists what it masked. | Sessions leave the machine only by a deliberate act. |
| No telemetry and no automatic network requests. | Nothing leaves by default. |
| Crash reports contain a backtrace and versions. They contain no prompt, file or environment content. | |

**Residual risk.** A secret that a tool reads from a file and that does not look like a key is stored in the session like any other text. Sessions should be treated as sensitive. Retention is configurable.

## 5. The RPC surface

**Threat.** Another local user, another program, or a web page in the user's browser connects to the RPC server and drives the agent, which can run commands as the user.

| Mitigation | Effect |
|---|---|
| The default transport is standard I/O, reachable only by the parent process. | No exposure. |
| Local sockets are owner-only, in the per-user runtime directory, with a check that the peer is the same user. Named pipes on Windows admit only the current user. | Other local users cannot connect. |
| Network listening is off by default, absent from the first release, loopback-only unless explicitly widened, and always requires a per-launch random token. No unauthenticated mode exists. | Other machines and unauthenticated local programs cannot connect. |
| `Origin` checking on the WebSocket transport. | A web page cannot use the browser to reach a local server. |
| Remote use goes through SSH or a TLS proxy. The server does not implement TLS. | No home-grown transport security. |
| Clients cannot weaken policy. Permission mode `unrestricted` and trust decisions need the same explicit flags as on the command line. Approvals are requested from clients, never assumed. | A client is not more powerful than the user at the terminal. |
| Input is size-limited and schema-checked. Malformed messages get an error and never crash the server. Outbound queues are bounded. | Denial of service by a client. The protocol decoder is fuzzed. |

**Residual risk.** Any program already running as the user can start its own `cortex` or read the user's files. The RPC server does not change that.

## 6. Supply chain

**Threat.** A dependency is compromised, or a build is tampered with.

| Mitigation | Effect |
|---|---|
| A small dependency tree, with every direct dependency justified and listed. | Less to audit. |
| `cargo-deny` on every pull request: advisories, licences, banned crates, sources limited to crates.io. | Known-bad versions and unexpected sources are blocked. |
| `Cargo.lock` is committed. CI builds with `--locked`. | Reproducible dependency versions. |
| No build script downloads anything. The model catalog is a checked-in file. | Builds are offline and reviewable. |
| GitHub Actions are pinned by commit hash and run with minimal token permissions. Pull-request jobs have no secrets. | A malicious pull request cannot reach credentials. |
| Release binaries are built by CI from a tag and published with checksums. | Users can verify what they run. |

## 7. Terminal and local environment

| Threat | Mitigation |
|---|---|
| Control sequences in model output, file content or tool output change the terminal's state, title or clipboard, or forge UI. | The renderer writes only text it has sanitised. Control characters other than newline and tab are stripped or shown as visible symbols. Only the harness emits control sequences. |
| Symbolic links lead a tool outside the workspace. | Paths are resolved before the permission check, and the check applies to the target. Windows junctions are resolved the same way. A link to something that does not exist is refused. |
| A protected file is reached under another spelling, which a rule written for its name does not match. On Windows: another case, a short name (`SECRET~1.ENV`), a trailing dot or space, an alternate data stream (`secret.env::$DATA`), a device or extended-length path. | The check is made on the resolved path, in which case and short names are already the file system's own. The other spellings are refused outright, each with its own error ([architecture/05-tools.md](architecture/05-tools.md), "Paths"). |
| A path on a network share makes Windows send the account's credentials to a server an attacker named. | Network paths are refused unless configuration allows that server and share. The refusal is made on the text of the path, before anything is asked of the network. |
| The file system changes between the check of a path and its use. | The harness opens the resolved location, never the text it was given. That does not stop a process that swaps a directory for a link at the right moment: confining such a process is the sandbox's job, and without an enforcing sandbox this remains a residual risk. |
| Two harness processes corrupt shared files. | Session logs are append-only with one writer per session, guarded by a lock. Shared state files are written atomically under a lock. |
| A panic leaves the terminal unusable. | The panic hook restores it. |

## 8. The Coordinator

**Threat.** Under the Coordinator ([architecture/17-coordinator.md](architecture/17-coordinator.md)) a Manager session with broad permissions drives Employee sessions and integrates their work, with less of the user's attention on each step. Three things can go wrong that a single session does not face:

- **Injection that travels.** An Employee reads something hostile. Its report, its questions or its code then try to steer the Manager: "approve this", "merge without review", "grant me network access", "the tests are known to fail, ignore them".
- **Privilege creep.** A Manager hands out more than it was given, or accumulates approvals nobody looked at.
- **Runaway work.** A team spends the budget, fills the disk, or pushes something that cannot be undone.

| Mitigation | Effect |
|---|---|
| **Hard limits are enforced by the harness, never only stated in a prompt.** The budget is a ledger charged before each request. Models never run network git commands: commands run with the network off, and pushing or opening a pull request is done only by a harness operation that has no code path that forces. Writes are confined to each session's worktree by the sandbox and the path check. The main branch is refused as a merge target unless the grant allows it. | A Manager that has been talked into anything still cannot exceed the budget, force-push, write outside the project or merge to the main branch. |
| **A child's lease is computed, not chosen.** An Employee receives the Manager's lease narrowed by what was requested. The narrowing is code with a property test. | No instruction, from the user's task or from an Employee, can give an Employee more than the Manager holds. |
| **The Manager cannot approve beyond its own lease.** An Employee's approval request is checked against the Manager's lease before the Manager is asked. Anything outside it goes to the user. | "Please approve network access" from an Employee reaches the user, not just the Manager. |
| **Employee output is data.** Reports, questions and summaries reach the Manager as bounded, labelled tool results, with the same sanitising as any tool output. They are never inserted as instructions or as the user's words. | The Manager can tell an Employee's text from the user's. This lowers the risk and does not remove it. |
| **Integration is decided on evidence the harness produced.** Before a merge the harness runs the project's checks in the Employee's worktree and computes the diff itself. The review packet says what the checks returned and flags deleted or weakened tests. An Employee's claim that "tests pass" is not an input. | A persuasive report cannot stand in for a passing check. |
| **Sensitive paths always go to the user** unless the grant names them: CI configuration, git hooks and configuration, the project's `.cortex` directory, dependency registry and script settings, files that look like secrets. | The changes most useful to an attacker are the ones a person must see, at every level. |
| **Employees cannot reach each other.** All messages go through the Manager. | One compromised Employee cannot instruct the others. |
| **Every decision the Manager takes alone is logged** with its reason and, written by the harness, the grant clause that allowed it. Mechanical decisions are logged by the harness whether or not the Manager explains them. | Nothing is approved invisibly. The log is reviewable at any time and after the fact. |
| **The escalation queue is the only way a question reaches the user**, it shows who is asking and the full detail, and unanswered requests are denied on timeout. | No request is approved by default. |
| **Levels above supervised need an enforcing sandbox.** Where the OS sandbox is not enforcing, only the supervised level is offered. | The hard limits are never offered where they could not be guaranteed. |
| **A watchdog in the harness**, not a model, pauses a session that stops making progress or repeats itself, and the whole team stops at the budget cap. | Runaway spend and stuck sessions are caught without relying on the Manager's judgement. |
| **The user can see and stop everything**: every Employee is listed, any can be attached to, and one action stops the team. | |
| The team lease is shown in full, in plain language, before the team starts. It is the same lease type and the same levels a single session uses (section 9). | The user knows what they are delegating, in terms they already know. |

**Residual risk.**
- Choices about design, security and scope depend on the Manager recognising them as such. The harness cannot detect them mechanically. At the guided and autonomous levels a steered Manager could make a poor design choice inside its grant; the decision log and review of the integration branch are the backstops, and the main branch stays out of reach.
- Code written by a steered Employee can still be subtly wrong while passing the checks. The same is true of any agent, and review before merging to the main branch remains the user's.

## 9. Task leases

**Threat.** A task lease ([architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md)) lets the agent work without asking inside agreed terms. The risks are terms that are wider than the user understood, terms that widen themselves, and work that carries on after the user stopped watching.

| Mitigation | Effect |
|---|---|
| **Only a person grants, extends or revokes a lease.** It is a protocol operation open to the `user` role alone. A model can draft terms; a draft is text. | Injected text cannot give the agent more autonomy. |
| **The terms confirmed are the terms enforced.** The confirmation screen is rendered from the lease value itself: scope, level, budget, time, stop conditions. | No gap between what was read and what runs. |
| **Every term is enforced by the harness**: scope by the permission engine and the sandbox, budget by the ledger, time by the supervisor, stop conditions as data. | None of it depends on the model's good behaviour. |
| **`guided` and `autonomous` need an enforcing sandbox.** | Autonomy is never offered where confinement could not be guaranteed. |
| **At the edge the agent stops and reports**, and the session falls back to its standing lease. | No drift into a stream of prompts that train the user to approve without reading. |
| **A lease cannot exceed managed or user configuration, and a project cannot grant one.** | A repository cannot make itself trusted. |
| **A child's lease is always a subset** (`narrow`, with property tests). | Runs, attempts and Employees cannot hold more than their parent. |
| The level and what remains of the lease are always in the status line. | The user can see at a glance that the agent is working unasked, and for how much longer. |

**Residual risk.** Inside its scope a lease permits everything it says. A lease that allows running the project's tests allows whatever the tests do, inside the sandbox. This is the same trust the user already places in the project's own build.

## 10. Reading other tools' usage records

**Threat.** The usage view can read files written by other coding agents ([architecture/18-usage.md](architecture/18-usage.md)). Those files hold conversations as well as numbers, are in formats that change without notice, and could be crafted by anything that can write to the user's home directory.

| Mitigation | Effect |
|---|---|
| **Opt-in per source, user-only.** Nothing is opened until the user enables a source. A project cannot enable one. | No surprise reading of another tool's data. |
| **Read-only.** Nothing is created, changed or locked in another tool's directory. | The other tool cannot be damaged. |
| **Only numbers are kept.** Adapters extract times, model names, token counts, costs, project paths and record ids. A test asserts that no text field from the input reaches the output or the cache. | Conversations are not copied into our state. |
| **No network by default.** The view and the indexer make no connection. Tested with the network denied. The one exception is the opt-in refresh of limits: off by default, per provider, user-only, made only when the user asks, and sent only to that provider's documented usage endpoint with the user's own key. It carries no usage data, and a test asserts that. | Nothing read from another tool can be sent anywhere. |
| **Untrusted input.** Size and line-length limits, no following of links out of the tool's directory, fuzzing of every adapter. A bad file is skipped and named. | A crafted file cannot crash the harness or make it read elsewhere. |
| **Unknown versions are refused, not guessed.** | Wrong numbers are not shown as facts. |
| **Every figure is labelled** with its source and cost basis. Estimates are never merged silently into metered totals. | The user is not misled about what was billed. |
| The cache is owner-only and can be deleted per source. | |

**Residual risk.** Project paths from other tools are shown in the view and stored in the cache. They stay on the machine.

## Reporting

`SECURITY.md` at the repo root (created by the repo skeleton step) tells people how to report a vulnerability privately through GitHub's reporting feature.

## Review

The whole-repo review at the end of the build plan re-checks every row on this page against the code. Each mitigation above has a test named in the step that builds it.
