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

**As built in step 12** (`cortex-policy`, `level`). The table is one function, `decider(level, matter)`, with a column for each kind of thing to decide, and a test holds it against the rows above.

| Level | Read | Read beyond the scope | Edit | Write beyond the scope | Read-only command | Safe command that may write | Other command | Network from a tool | Leave the sandbox |
|---|---|---|---|---|---|---|---|---|---|
| `read-only` | holder | supervisor | refused | refused | holder | supervisor | supervisor | supervisor | supervisor |
| `supervised` | holder | supervisor | holder | supervisor | holder | holder | supervisor | supervisor | supervisor |
| `guided` | holder | supervisor | holder | supervisor | holder | holder | supervisor | supervisor | supervisor |
| `autonomous` | holder | supervisor | holder | supervisor | holder | holder | holder | supervisor | supervisor |
| `unrestricted` | holder | holder | holder | holder | holder | holder | holder | holder | holder |

- **The scope of a standing lease is its writable roots:** the workspace and the session's private temporary directory, plus `sandbox.write`. A read or an edit under one of them is inside the scope. The sandbox's `read` set is not asked: it says what a child process may read, and a file tool's read outside the workspace asks ([05-tools.md](05-tools.md), "Paths").
- **Three things are beyond the scope wherever they are.** A path outside the writable roots. A write to a protected path ("Protected paths", below): whoever can write one can run code later. And a file that looks like a secret, read or written ("Files that look like secrets", below).
- **A level that refuses is final.** At `read-only` a write does not happen, whatever a rule allows. Rules settle what a level leaves open; they do not open what it closes.
- **A rule still holds at the levels that ask least.** A `deny` is a denial at `unrestricted`, and an `ask` is a question.
- **`guided` reads like `supervised` in this table** because the kinds of command a guided lease names are allow rules of that lease, and rules are settled before the level is asked. It cannot be chosen until task leases exist (step 37).
- **Where the sandbox does not enforce,** `autonomous` is refused when the session starts, with the reason and what to use instead (the error code `sandbox_required`), and every other level shows one line, "sandbox: off", that says what follows: commands on a safe list run without asking, every other command asks first, and one that is approved runs with the user's full rights. Since step 26 that is a session on Windows, on a Linux kernel without Landlock or with Landlock older than version 3, in a build without the `sandbox` feature, or with `sandbox.enabled = false` ([decision 0032](../decisions/0032-what-the-linux-sandbox-needs.md)).
- **A level this build does not know decides nothing:** every matter is refused, and a session cannot be started at it.
- **Leaving the sandbox,** the last column, was added in step 26: a command the model asked to run outside the sandbox ("Leaving the sandbox", below). It is asked about at every level but `unrestricted`, where nothing is confined to leave. Where the sandbox does not enforce there is nothing to leave either, and it decides nothing.

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

// `LeaseExt`, in `cortex-policy`: the lease type is the protocol's.
impl LeaseExt for Lease {
    /// The widest lease inside both. Never wider than `self`.
    fn narrow(&self, requested: &Lease, sandbox: SandboxState) -> Result<Lease, NarrowError>;
    /// True if everything `self` permits, `parent` permits too.
    fn is_within(&self, parent: &Lease) -> bool;
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

**As built in step 12** (`cortex-policy`, `lease`). The lease type is the protocol's, so `narrow` and `is_within` are an extension trait on it, `LeaseExt`, and the engine adds no other way to read a lease.

- **`narrow` returns a result.** One pair of leases has no lease inside both: a proposal that names models, under a parent that names models, with none in common. An empty list means "the configured models", which would be wider, so that pair is refused (`NarrowError::NoModelInCommon`) and nothing else is.
- **`unrestricted` is never handed on.** The child's level is the lower of the two and at most `autonomous`.
- **`narrow` checks the level it gives,** as a session's start checks its own: it is told whether the sandbox confines the child's commands, and refuses a level the child could not be started at (`NarrowError::Level`): `autonomous` where the sandbox does not enforce, `guided` before step 37. So no caller can hand a child a level a session could not hold (found by the review of step 12).
- **An allow rule is kept only when the parent has the same rule,** by its effect and its pattern as text. A child that asks for `cargo test *` under a parent that allows `cargo *` does not get the rule: deciding that one pattern lies inside another is where a mistake would widen a lease, so it is not attempted. A proposal names the parent's rules it wants.
- **Roots are compared exactly.** A writable root reaches only folders spelled as it is, so that on Windows it never reaches a folder that differs only in case.
- **An expiry that cannot be ordered becomes a stop condition.** A lease that ends "at the end of the session" and one that ends "at noon" cannot be put in order ahead of time. The child's expiry is the event, and the moment is added as a `time` stop condition, which the harness enforces the same way. A time nobody can read is taken to have passed.
- **Budgets are narrowed as written.** To cap a child by what the parent has left, the caller narrows a copy of the parent's lease whose budget is the remainder (`with_budget`).
- **Rule patterns are relative to each session's own workspace.** A parent's `deny write secrets/**`, carried to a child in another worktree, protects that worktree's `secrets/`. The Coordinator step checks that this is what a team wants.
- **The property tests check against an oracle of their own,** which reads roots and times by its own rules and calls none of the code it checks. They are given roots that share a prefix of their text (`/work` and `/work2`), that differ only in case, or are spelled with `.`, `..` and two slashes, and times that are one moment in several spellings (`10:00+01:00` and `09:00Z`) or a fraction of a second apart.

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

### Rules as built

Built in step 12 (`cortex-policy`, `rules` and `glob`).

**Patterns.**

| Effect | The pattern is | Examples |
|---|---|---|
| `read`, `write` | A path glob. | `**/.env*`, `src/**`, `~/notes/**`, `*.lock` |
| `exec` | Words, each a pattern for one word of the command. A last word that is `*` alone stands for any further words. | `cargo test *`, `git status`, `npm run test:*` |
| `network` | A host, compared in lower case. | `docs.rs`, `*.example.com` |
| `ext` | A tool's full name. | `docs/find`, `docs/*` |

In every pattern `*` is any run of characters, `?` any one, and `[abc]`, `[a-z]`, `[!a-z]` a set. In a path glob `*` and `?` stay inside one name, `**` as a whole name is any number of names, and `{a,b}` is either. There is no escape character: a special character is written as a set of one, `[*]`. The matcher is our own (about 300 lines), because only `cortex-tools` may link the search libraries ([01-workspace.md](01-workspace.md)); it is tested against the cases `globset` documents and against a reference matcher.

**Where a path glob is anchored.**

| Written | Read as |
|---|---|
| `/etc/**`, `C:\Tools\**`, `\\server\share\**` | An absolute path. On Windows `\` and `/` both separate. |
| `~/notes/**` | Under the home directory. |
| `**/.env` | In a rule that holds back (`deny`, `ask`): anywhere, in the workspace or outside it. In a rule that allows: anywhere in the workspace. |
| `src/**`, `docs/*.md` | With a separator: under the workspace root. |
| `*.lock`, `Cargo.toml` | With none: that name at any depth in the workspace. |

A pattern that ends in `/**`, or in a separator, matches the directory itself as well as what is in it, so that a rule on a tree covers a listing of its top.

**A rule that holds back is matched as widely as it can be, and a rule that allows as narrowly.**

- **Where `**` starts.** A `deny` or an `ask` for `**/*.pem` holds for every such file on the machine. An `allow` written the same way allows them in the workspace only: an `allow write **` that reached the home directory would hand over the shell start-up files and the SSH keys in it. An allow that is to reach further says where: `~/notes/**`, `/srv/data/**`, `/**`.
- **Case, on Windows.** A `deny` or an `ask` matches a path without regard to case, and also exactly. An `allow` matches exactly, against the resolved path, which is in the file system's own spelling. So no spelling gets round a deny. And an allow for `App` never grants `app`, which is another folder in a directory marked case-sensitive. Since every path is resolved before it is matched, an allow works however the *path* was typed; it is the *rule* that must spell the folder as it is stored, and one that does not simply does not match. A drive letter, and a share's server and name, have no case on disk and are always folded.
- **Commands.** An `allow` matches one whole simple command of a line the analysis models, from its first word: `git status` allows `git status` and not `git status --short`, and a rule for `git` does not allow `./git` or `/tmp/git`. In PowerShell, which finds commands without regard to case, the program's name is compared that way and `git.exe` is `git`. A `deny` or an `ask` matches wherever its words appear in the raw line, one after another: `rm *` matches `sudo rm -rf x`, `echo x; rm y`, `bash -c 'rm y'` and `/bin/rm y`. It matches too much on purpose, and it is the only match there is for a line that could not be analysed. The line is tried a second time with every quote, backslash, back-tick and caret taken out of its words, as the shells take them out, and a line break after one of them taken out with it: `"g"it push`, `g\it push`, `gi''t push` and `g\` then `it push` on the next line are `git push` (found by the review of step 12 and the independent review of its fixes). What no match catches is a name the line makes as it runs (`$(echo git) push`), or a pattern the shell expands to the program (`/usr/bin/gi? push`): **a `deny` on the raw line of a command the analysis does not model can be dodged**, and at `autonomous` and `unrestricted` such a line runs. Where the sandbox enforces, it is what holds it.
- **A call that cannot say which host it reaches** meets every rule that holds back, and is allowed only by an allow for every host (`*`).

**Who wrote a rule.** Each rule reaches the engine with its source: the managed configuration, the user's, the command line, the project's, the project's learned commands, a stored approval, a session approval, or a lease. An `allow` from the project or from its learned commands is set aside until the user has trusted the project, and the set-aside rules are kept for a front end to show. The configuration loader already holds such rules back ([08-configuration.md](08-configuration.md)); the engine applies the same rule again where rules are enforced, so that nothing upstream has to be right for it to hold. Layers need no rank beyond that: a managed `deny` stands whatever a user rule allows, because `deny` beats `allow` whoever wrote them.

**A rule that cannot be compiled refuses the whole set,** and the session does not start: a rule that is skipped could be the one that denies.

**The built-in protection** is not a list of rules but a part of every rule set that nobody wrote and nobody can remove. It denies, so no allow and no level lifts it.

| Closed | To |
|---|---|
| The configuration directory (configuration, and credentials on Linux) | Reading and writing. |
| The user's configuration file, wherever `CORTEX_CONFIG` puts it | Reading and writing. |
| The credentials file and its lock, wherever they are kept, and on Windows their own folder (`%LOCALAPPDATA%\cortex-harness\credentials`), with any temporary file a write left there | Reading and writing. |
| The state directory (trust, stored approvals, what was learned) | Reading and writing. |
| The data directory | Writing. |
| The session directories of other projects | Reading. This project's own are outside the workspace, so reading one asks. |

Closing the state directory to writing is what makes trust and approvals mean something: a model that could write `trust.toml` could trust its own project.

Where each of these is on each system is one table, beside what is closed in it: [08-configuration.md](08-configuration.md), "Where each system keeps the harness's files". The credentials file has a row of its own because on Windows it is not in the configuration directory, and before the review of step 13a nothing closed it to reading there.

The protection holds for a path a command only names, too: `cat` of the credentials file, or `cp` to it, is refused and not asked about. A command that searches a tree with one of these directories somewhere in it is asked about.

### Files that look like secrets

Reading or writing a file whose name says it is likely to hold a secret asks first, at every level short of `unrestricted`, wherever the file is.

| Looks like a secret | Does not |
|---|---|
| `.env`, `.env.local`, `.env.production` | `.env.example`, `.env.sample`, `.env.template`, `.env.dist`, `.env.defaults` |
| `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks` | `keys.rs`, `pem-files.md` |
| `id_rsa`, `id_dsa`, `id_ecdsa`, `id_ecdsa_sk`, `id_ed25519`, `id_ed25519_sk` | `id_rsa.pub`, `src/id_map.rs` |
| `.netrc`, `_netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `.envrc` | |

- **It is a default, not a rule,** and that is deliberate: `ask` beats `allow`, so as a built-in `ask` rule nothing a user wrote could lift it. As the default for such files it is what happens when no rule has settled the question. A user's `allow` lifts it (`allow read **/.env.test`). A project's `allow` lifts it only after trust. A `deny` closes the file for good.
- **The same default decides what a session's base state may keep** ([07-sessions-and-context.md](07-sessions-and-context.md), "The base state"): the store keeps a file only if the engine would let the agent read it without asking. The session store has no list of names of its own.
- The list is by name and short on purpose. An earlier list had `id_*`, and so held back `src/id_map.rs`.
- **It is narrower than the list the session store had before step 12,** and that follows from the owner's decision in step 12 that the store keeps no list of its own ([steps/12/answered.md](../steps/12/answered.md)): `id_rsa_work`, `id_ed25519_github` and `.env-backup` are not on it, so a base state keeps them and a read of one is not asked about. A rule says otherwise where it matters: `deny read **/id_*`.

### Protected paths

Some files in a workspace decide what runs later, outside the sandbox and without anybody being asked. Whoever can write one can run code, or reach a secret, so writing one always goes to the user.

| Protected | Because |
|---|---|
| Everything in a `.git` directory, a file named `.git`, and a file named `HEAD`, at any depth | Git runs the programs its hooks and configuration name: a hook, a file-system monitor, a filter, a text converter, a pager. A nested repository, a submodule's or a worktree's configuration, and a bare repository made of plain files are read the same way. |
| `.cortex/` at the workspace root | The project's own configuration, hooks, extensions and instructions. |
| `.bashrc`, `.bash_profile`, `.bash_login`, `.profile`, `.zshrc`, `.zshenv` and `.zprofile` at the workspace root; `.envrc` at any depth | A shell, or `direnv`, runs them when it starts. `direnv` reads `.envrc` from the directory it is in and upward. |
| `.github/workflows`, `.github/actions`, `.gitlab-ci.yml`, `.circleci`, `.pre-commit-config.yaml`, `.husky` | A CI service runs them with the repository's secrets, and a commit runs the last two. |
| `.cargo/config.toml`, `.cargo/config`, `.npmrc`, `.yarnrc` and `.yarnrc.yml` at any depth, and `.pypirc` | They name a program to run for a build, or the registry a token is sent to. Cargo, npm and yarn read theirs from the directory a build starts in and upward, so one in a subdirectory applies to a build started below it as surely as one at the root does to every build (found by the review of step 12). |
| Whatever `sandbox.protect` lists | The user said so. An entry is an absolute path, a path under `~/`, or a path relative to the workspace. One that is none of these stops the session from starting: an entry that was skipped would not be protected. |
| Files that look like secrets | Above. |

**Who decides, at each level.**

| Level | A write of a protected path |
|---|---|
| `read-only` | Refused, like every write. |
| `supervised`, `guided`, `autonomous` | Asked about. `autonomous` lets a command run unasked and still asks about these. |
| `unrestricted` | Left to the holder: the level asks nothing. A user's `deny` and `ask` rules still hold there, and so does the built-in protection of the harness's own files. |

- **Every route asks.**
  - The `write` and `edit` tools.
  - A redirection: `echo x > .git/config`.
  - A command that names one: `cp evil .git/hooks/pre-commit`, `sed -i … .github/workflows/ci.yml`, `tee`, `rm`, `dd of=…`. This holds whatever allow rule or level lets the command itself run. The engine cannot know that a command only reads a file it names, unless the command is on the table of commands that only read.
  - `git config`, however it is written: it changes the configuration without naming the file.
- **An allow rule that names the path lifts the question,** for that path and by every route: `allow write .github/workflows/**`. A project's own allow does so only after trust.
- **Names are compared without regard to case on Windows,** like every rule that holds back.
- **Reading one is not held back,** unless it also looks like a secret.
- **What this does not cover** is a program that writes one of these without the command line naming it: `python3 fix.py`. At `supervised` such a command is asked about as a command. At `autonomous` it runs, and the sandbox is what holds the protected paths ("The sandbox", below). Two stated limits besides: **the directory above a protected path** is not itself protected, so `mv payload .cargo` or `cp -r staging/workflows .github` can make one (protecting every ancestor would ask about `cp x .`); and **a link that leads nowhere yet** is not followed to a protected path it names (`tee hook`, where a link `hook` to a workflow that does not exist yet was carried in by a repository).
- **After each confined command, the sandbox looks** (step 26, below): what a command wrote without naming it is found then, and stops the turn.
- **The list in the lease, which the sandbox enforces, is narrower in two respects.** Of `.git` it has `hooks`, `config`, `config.worktree` and `info`, because `git commit` has to write the rest. The engine asks about every write a tool or a command line names anywhere in a `.git` directory. And it names the build configuration at the workspace root only: one at any other depth is not a fixed path, so the engine knows it by name, and the sandbox step looks for it by name, as it does a nested repository's configuration.

## Shell command analysis

Rules on commands are a convenience. They are useful only if they fail closed.

1. The command line is parsed into a sequence of simple commands. Pipelines and the `&&`, `||` and `;` operators are split, and each part is checked on its own.
2. If the command contains anything the analyser does not model, the rule result is "ask": command substitution, process substitution, backticks, `eval`, here-documents, redirection to a path outside the workspace, variable assignment prefixes, subshells, function definitions, or a parse error.
3. An `allow` rule applies only if every part is allowed.
4. A small built-in table marks commands and argument forms that are read-only (for example listing, printing, and status commands with fully enumerated safe flags). Only these are scheduled as read-only. Commands that can write through an option, such as `git`, `find`, `sed` and `tar`, are never in the table as a whole.
5. The analyser never runs anything. It never calls a shell, an interpreter or a program found in the workspace.

On Windows the same rules apply to PowerShell through a conservative tokenizer: one command, literal arguments, no script blocks, no invocation operators, no encoded commands. Anything else is "ask". `cmd.exe` syntax is not analysed at all.

### Command analysis as built

Built in step 12 (`cortex-policy`, `command`). **The analysis is two hand-written tokenizers that accept a small listed subset and refuse everything else**, not a parser of a shell's grammar ([decision 0031](../decisions/0031-command-analysis-by-allow-list.md), with the measurements). A parser returns a tree for everything a shell accepts and leaves the refusing to its user; a tokenizer that lists what it accepts refuses what nobody thought of.

**The POSIX subset** means the same in bash, dash, zsh and ksh.

| Modelled | Not modelled, and so asked about |
|---|---|
| Simple commands joined by `\|`, `&&`, `\|\|`, `;` and line breaks | `&` (a background job), `\|&`, `;;` |
| Words of letters, digits and `_ - . / : = + , @ %` | `$`, a back-tick, `( )`, `{ }` anywhere in a word, a backslash outside quotes, `#`, `!`, `^`, any other character |
| Single quotes; double quotes with no `$` and no back-tick | A quote left open |
| `~` and `~/…` for the home directory; a `~` inside a word (`HEAD~1`) | `~user`, `~+`, `~-`, a `~` after `=` or `:`, a word that begins with `=` (zsh's `=cmd`) |
| Globs `*`, `?`, `[…]`: kept, and flagged | A glob where a command is expected: `[`, `[[` |
| `< file`, `> file`, `>> file` with one digit before or none, the file a pattern too (kept, and flagged); `2>&1`, `>&2`, `>&-` | `<<`, `<<<`, `<>`, `>\|`, `&>`, `>&file`, `<(…)`, `>(…)`, several digits before a redirection (`20>f` is a descriptor to bash and an argument to dash) |
| | `NAME=value` where a command is expected |
| | A keyword (`if`, `for`, `time`, …) or a builtin that runs text as code or changes what later commands mean (`eval`, `exec`, `source`, `.`, `trap`, `command`, `alias`, `export`, `set`, `unset`, `shopt`, `pushd`, `exit`, …) as the command |

**zsh with `EXTENDED_GLOB`** reads a `~` inside a word as a glob operator, so `HEAD~1`, which the subset takes as plain text, is a pattern there. The option is off unless the user's `.zshenv` turns it on, and this is a stated limit.

**The PowerShell subset** is one command with literal arguments: words of ASCII letters, digits and `_ - . / \ : = +`; a whole word in single or double quotes, with no `$` and no back-tick in double quotes, and not empty, with no `"` in it, no `\` at its end and none of `& | < > ^ %`; `~` for the home directory; `*` and `?`, flagged. Everything else is asked about: `;`, `\|`, `&`, a line break, `$`, a back-tick, any bracket, `@`, `,`, `%`, `#`, `!`, `<`, `>`, a quote that is not a whole word, a parameter with a `.` or a `:` in it (PowerShell reads `-Path.5` as `-Path` and `.5`, and `-Path:x` as `-Path:` and `x`, which the comparison with its parser found on its first run in CI), any character outside ASCII that is not inside quotes, a typographic quote anywhere (PowerShell reads `‘ ’` as `'` and `“ ”` as `"`, and several dashes as `-`), a keyword, and `powershell`, `pwsh`, `cmd`, `Invoke-Expression`, `iex` and `.` as the command, which is where an encoded command would be.

**What a program receives is not always what PowerShell's parser read.** Windows PowerShell 5.1, which the shell tool runs where pwsh is missing, and pwsh 7.3 and later for a `.cmd` or `.bat` program (`npm.cmd`, `gradlew.bat`), build the program's command line the old way: a text with a space in it is put in double quotes, nothing inside it is escaped, and a text without one is passed bare. So a `"` in a quoted text ends that quoting early, and the rest becomes words of their own (`git diff --src-prefix 'a/ " --output=f'` reaches git with `--output=f`); an empty text is dropped and the words after it move up; a `\` at the end of a quoted text escapes the closing quote, and the next words join it; and `cmd.exe`, which runs a `.cmd` or `.bat` program, reads `& | < > ^ %` in a text passed bare (`npm test 'x&calc'` runs `calc`). Each of these is not modelled, in every PowerShell line, since the analysis cannot know which PowerShell runs it or what the program is (found by the review of step 12). A test on the Windows runner compares what a program receives through `powershell.exe`, through pwsh, and through pwsh passing arguments the old way, with the words the analysis gives.

**`cmd.exe` is not analysed, and neither is a shell this build does not know** (`fish`, `nu`): the shell tool starts any unknown program as if it read `-c <command>`, which is no reason to believe it reads the line like a POSIX shell. The engine is told the shell by its program's name (`ShellSyntax::of_program`).

**What "not modelled" means.** No allow rule applies to the line and no safe list does. The level then decides it as any other command: asked about at `read-only`, `supervised` and `guided`, and run at `autonomous`, where the sandbox confines it. A `deny` or `ask` rule is still matched against the raw line. This is rule 2 above, stated for every level: the analysis never turns an unmodelled line into "allow", and it does not ask where the level would not, **except for what its words could name.** Each word of the raw line, cut both ways a rule that holds back is matched, is checked as a path a command names: one of the harness's own files is refused, and a protected path, a file that looks like a secret or a path a rule holds back is asked about, at every level that asks about those (`cp evil .git/hooks/pre-commit #x` at `autonomous`). A line that merely mentions `.git/…` or `.env` is then asked about; that is accepted. Each word is checked once, and tried from the directory the line runs in, from the workspace root, and from every directory a `cd` among the words could lead to, taken from where the line started and from where the `cd` before it led (`cd .github && cp evil.yml workflows/ci.yml #`, `(cd a && x) && (cd b && y)`); a `cd -` or a `cd` to a `*` or `?` pattern among them could lead anywhere, and is asked about, and so is a line with more such places than are followed (32). The same holds for a word with a space or a shell's operator (`< > | ; &`, a back-tick, `$(`) in it in a modelled line, which may be a command line for another program (`sh -c 'cp evil .git/hooks/pre-commit'`, `bash -c 'cat</tmp/x>.git/hooks/pre-commit'`), tried from every place the part may run in. One of the harness's own files named in such a word is refused even where the word is only text, a commit message among them. The owner's decision on the second independent review of the fixes to step 12, widened by the final review.

**How a modelled line is decided.**

1. A `deny` or `ask` rule on the raw line or on any part is settled first.
2. Each part must be allowed: by the table of read-only commands, or by an allow rule. A line is on a safe list only if every part is.
3. **What a command names is checked as a read.** `cat` only reads, and `cat ~/.ssh/id_rsa` reads a private key. So every file a table command names, and the directory it runs in, is put to the same judgement a `read` call gets, and the command is safe only if each would be allowed unasked. Where a rule denies a named path, the command is asked about, not refused: an argument that reads like a protected file may be a search pattern (`grep .env README.md`), and nothing is lost by asking. The harness's own files are the exception: a command that names one is refused. For every other modelled command, an argument that names a path a rule denies or asks about, a protected path, or a file that looks like a secret makes the command one to ask about, whatever allows the command ("Protected paths", above). An option's value counts as a path it names: after a `=` (`--target-directory=.git/hooks`), and for an option of one dash every text from its third character on (`cp -t.git/hooks`, `cp -vt.git/hooks`, `sort -o.git/config`), since the analysis does not know every program's letters; and after the `:` of an option written with a `/`, as Windows programs take theirs (`/out:file`). Found by the review of step 12.

   **A pattern names every path it matches.** The shell replaces a word with `*`, `?` or `[…]` with the names of the files it matches: `cp evil .gi?/hooks/pre-commit` writes `.git/hooks/pre-commit`. This holds for an argument, an option's value, a redirection's file (which also takes the part off every safe list), a `cd` to a pattern (what comes after it is tried under the pattern), and in PowerShell a quoted pattern, which its own commands expand. Found during the fixes to step 12, and settled by the owner after their second independent review:
   - **The directory a pattern starts under** is judged as a path the command names: a pattern under `.git` is a pattern in a protected directory.
   - **The pattern is expanded against the directory as it is now**, as the shell will: the base is found through the resolver, listed, and each match is judged as the name written out, through any link: a rule that holds it back (`deny write **/*.lock` meets `rm *.lock`), a protected path, a file that looks like a secret (asked about as a read, for a command that only reads), and for a redirection, the scope. A name that begins with `.` is matched as the shells match it, only by a pattern that begins with a `.` written out. So `rm *` and `rm -rf dist/*` run at `autonomous` where nothing protected, secret or held back is there, and `cp evil.yml c?/workflows/x.yml` is asked about where `ci` is a link to `.github`. **The bound:** at most 1,000 names in one directory, and 64 directories for one pattern (`*/*/x` lists the base and each directory its first name matched); past it the pattern is asked about. A thousand names is a directory of sources or build output, and keeps the decision to a few milliseconds: on the desktop a pattern over 1,000 files took 6.7 ms to decide, about 7 µs a name, nearly all of it matching the rules; a hundred files is under a millisecond. **The work of one command's decision is bounded too:** 10,000 units in all, a name listed or a path or pattern looked at (one unit for each 32 characters of it), so that a long word of option letters with a pattern in it cannot list the directory once a letter; past it the command is asked about.
   - **Its text is judged as well** where an earlier part of the line, or one beside it in a pipeline, may write (it could make names the listing would not see: `touch x && rm *`), and **instead** where the directory cannot be listed. What the directory holds is still judged after a part that writes, so `mkdir t && rm *.lock` meets `deny write **/*.lock` (found by the final review of the fixes to step 12). Then a pattern that could match `.git`, `HEAD`, a file that looks like a secret, build configuration read from where a build starts, or one of the protected paths, is asked about at every level that asks about protected paths, and so is one that starts above one of the harness's own directories. Read this way, `rm *` is asked about, since it could match `HEAD` or `id_rsa`, and `rm *.o` runs. A `[` is read as "anything to the end of the name", so that a class, a range or a quoted member is never misread. An allow rule does not lift a question the text asks.
   - **Patterns the listing does not answer alone.** A name that begins with `.` and could be `..` (`.?`, `.*`) matches more than the listing shows: dash, and bash before 5.2, match `.` and `..` with it, so `.?/.github` from `src` is the workspace's `.github`. Such a pattern is listed, so a rule meets what it matches at every level, and its text is judged as well, which asks about it at every level that asks about protected paths. A name `**` is listed as `*`, as bash reads it, so a link it matches is followed; zsh reads it as any number of directories, so it is also asked about where a rule or one of the harness's own directories holds back any path below where it starts, and its text is judged. (The last review of the fixes to step 12 found that skipping the listing for these lost a rule and a link.) A `]` with no `[` before it is no pattern, and the name is judged as written (found by the final review: `> .github/workflows/x].yml` was checked by nothing).
   - **After a part that makes a link** (`ln`, `link`, `mklink`, `cp -s`, `cp -l`), earlier in the line or beside it in a pipeline, every part that names anything, by an argument or a redirection, is asked about, a name written out included: the link could give any path another name, and no listing taken now can see it (`ln -s .github gh && cp evil.yml gh/workflows/x.yml`).
   - **Stated limits.** The directory can change between the listing and the run, and a link made earlier in the line by any other program (`tar x`, `git checkout`) is not seen; the sandbox holds what such a command writes: since step 26, where it enforces, a write outside the write roots is refused, and a protected path written in the workspace is found after the command, stops the turn and is kept for undo. An option the user sets (`dotglob`, `GLOB_DOTS`, `globstar`) that changes what a pattern matches is not known to the analysis. A link copied or moved as a link (`cp -a`, `mv`), or made through another program's name (`env ln`, `busybox ln`), is not seen as a link made.
4. **A search of a whole tree reads files nobody named.** The `git` commands that print what tracked files hold (`diff`, `show`, `grep`, and `log` with `-p` and the like) are asked about when a `deny` or `ask` rule on reading can match something in the tree they read, or when one of the harness's own directories is in it. The check is made on the rules' patterns, so it errs towards asking, and it holds at every level, as a rule does. The `search` tool makes the same search with every file put to the gate.
5. **`cd ./dir && …` is followed** where its effect is certain for every part after it: the parts after it are then judged from the new directory.
   - The `cd` has one plain argument and no redirection, past `--` and `-L`, which change nothing (`-P` takes `..` through links, and such a `cd` is not followed), and **the argument says where it starts:** `/`, `~`, `./` or `../`, or it is `.` or `..`. Any other name (`cd src`) is looked for in each directory of the shell's `CDPATH` first, which the environment can set and so can zsh's `cdpath` in a `.zshenv` that `zsh -c` reads: such a `cd` could lead anywhere, and is not followed. Write `cd ./src`.
   - **`..` is taken logically, as a POSIX shell's `cd` takes it:** it removes the name before it from the directory the shell believes it is in, whatever that name links to. Through a link `a` to `b/c`, `cd ./a/../..` goes up two from where the shell is, where the file system would go up one from `b/c` (found by the review of step 12, with bash, dash and sh). A path a later command names is found from the real directory, as the kernel finds it. **Where the logical directory is not there,** bash takes the target through links instead: if that leads elsewhere, the `cd` is not followed, and both are places (found by the final review of the fixes to step 12: `cd ./l/../workflows`, with a link `l` to `.github/x`). After a `cd` that is not followed, each later `cd` adds both readings.
   - It is not in a pipeline, where it runs in a shell of its own, and it does not follow `||`, where it runs only if what came before failed.
   - Everything after it is joined by `&&`, or is in a pipeline with something that is. Then nothing after the `cd` runs unless the `cd` ran and succeeded.
   - Any other `cd` that is not the last part takes the line off every safe list, and the level decides it. After `a || cd x && …` the rest runs whether the `cd` ran or not. After `cd x && a; b`, `b` runs in one directory if the `cd` succeeded and in another if it failed.
   - **What comes after a `cd` that is not followed is still checked for what it could reach from anywhere:** a path that starts at a root or at `~`, as it is; `git config`; and every other path a part names or redirects to, tried from the directory before the `cd`, from where each `cd` since would lead with no `CDPATH` (read past its options, `-P` through links too, and whatever it redirects), and from the workspace root, for a protected path, a file that looks like a secret, or a path a rule holds back. A `cd` to a pattern is not followed either, and a path after it is tried under the pattern (`cd .gi? && cp evil hooks/pre-commit`). A `cd -` before any other `cd` of the line goes to the directory the environment's `OLDPWD` names, which could be anywhere: what comes after it is asked about where the level asks about writing outside the workspace (step 13a takes `OLDPWD` out of the shell's environment). The independent review of the fixes to step 12 found `cd -- .git/hooks && cp evil pre-commit` running unasked. So `cd sub && git config core.fsmonitor ./run` and `cd .git && cp x hooks/pre-commit` are asked about at `autonomous`, where the rest of such a line runs.
   - A `cd` with a redirection is a command that writes the file, and is judged as one.
   - **A command that may write, run after a followed `cd`, is judged where it runs:** run outside the workspace, or in a protected directory, it is asked about, whatever rule allows it (`cd /tmp && make`, `cd ./.git/hooks && make`).
6. **A redirection is a write of its file,** or a read, judged as one (a quoted `~` is a name, in a folder named `~`, there and in what a command that only reads names; `~\x` is a name outside Windows; in PowerShell a word that begins with `~` is judged both as the home directory and as such a name, quoted or not, since its own commands read a quoted one as home, PowerShell 7 passes a bare one to a native program as home, and Windows PowerShell passes a bare one on as it is): outside the scope it asks, to a protected path or a secret it asks, and where a rule denies it the line is refused, because a redirection is a write for certain. `/dev/null` is nothing, read or written.
7. **A line whose every part only reads is marked read-only,** which is how the loop comes to run it beside other calls ([03-agent-loop.md](03-agent-loop.md)).

The paths a command names are followed by the same resolver the file tools use, so a link in the workspace leads where the file system says. The analysis itself looks only at text.

**The table of commands that only read.** A command is on it with its options, never as a whole.

| Shell | On the table |
|---|---|
| POSIX | `ls`, `cat`, `head`, `tail`, `wc`, `grep`, `egrep`, `fgrep`, `fd`, `fdfind`, `find`, `sort`, `uniq`, `stat`, `file`, `du`, `df`, `tree`, `readlink`, `realpath`, `basename`, `dirname`, `pwd`, `whoami`, `id`, `uname`, `hostname`, `which`, `true`, `false`, `echo`, `date`, `sed` (only as `sed -n '<lines>p'`), and `git` with `status`, `diff`, `show`, `log`, `branch`, `tag`, `rev-parse`, `ls-files`, `blame`, `describe`, `grep`, `remote`, `stash list`, `version` |
| PowerShell | `Get-ChildItem`, `Get-Content`, `Select-String`, `Test-Path`, `Resolve-Path`, `Get-Item`, `Get-Location`, `Get-Date`, `Write-Output`, `Write-Host`, `Get-FileHash` and their aliases (`gci`, `ls`, `dir`, `gc`, `cat`, `type`, `sls`, `pwd`, `echo`, …), `whoami`, `hostname`, `fd`, and `git` as above |

- **Each entry lists the options that are safe, by their exact names.** An option that is not listed takes the command off the table: `sort -o`, `find -delete` and `-exec` and `-fprint`, `git diff --output`, `git -c …`, `fd -x`, `tail -f`, `date -s`. So does an abbreviation of a long option, which GNU programs accept and the table does not, and a parameter of a PowerShell command that is not written out in full.
- **An option that takes its value only attached is listed so,** and one is listed as taking the next word only where the real program takes the next word. Otherwise the table would skip that word as the value, and the program would read it as an option of its own: `git log --pretty --output=f` wrote `f` (found by the review of step 12). Each option with a value was checked by putting after it a word the program would act on as an option: against git 2.53, GNU coreutils 9.4, uutils coreutils 0.10, GNU grep 3.12, fd 10.3 and tree 2.3. Git's `-U`, `--unified`, `--relative`, `--pretty`, `--format`, `--min-parents` and `--max-parents` take theirs only attached; every other option listed with a value takes the next word, or the program refuses it. **And a `git` line with `--out` in any word is off the table,** whatever comes before it.
- **A subcommand that can write is listed only in the form that cannot:** `git branch` and `git tag` with no name, `git stash list`, `git remote` with no subcommand.
- **Programs that are languages are not on it:** `awk`, `perl`, `python`, `xargs`, `env`, `tee`, and `less` and `more`, whose keys run commands. `sed` is on it only to print lines, because its scripts can write files and run commands.
- **`printf` and `test` are not on it,** though they look harmless. In bash both evaluate an array subscript in their arguments (`printf -v 'a[$(cmd)]'`, `test -v 'a[$(cmd)]'`), which runs a command, quoted or not, and `printf -v` sets a variable such as `CDPATH`.
- **No option that follows links is listed** (`grep -R`, `find -L`, `ls -L`), and `diff` is not on the table, because it reads through the links inside the directories it compares. What a command names is checked name by name; what it finds by walking a tree must stay in the tree.
- **No search that reads the content of a whole tree is on it.**
  - `grep -r` reads every file under a directory, a `.env` included, and names none of them. It is off the table, and `grep` of named files is on it.
  - `rg` leaves out hidden files and what ignore files name, but the workspace can change both: a `.ignore` with `!.env` in it brings `.env` back, and so does `rg -t sh`, whose type names `.env` and lifts the rule on hidden files (found by the review of step 12). The owner took `rg` off the table altogether. Through the shell it is an ordinary command, decided by rules and the level; the `search` tool makes the same search with every file put to the read rules.
  - `git grep --untracked`, `git grep --no-index` and `git diff --no-index` read what git does not track, ignored and hidden files included, and are off the table. So is `git diff` with two operands or more unless the first two both read as revisions (`HEAD~1 HEAD`, `abc1234 main~2`): outside a repository, or with one of them outside it, `git diff a b` is `git diff --no-index a b`, and git 2.53 printed the hidden `.env` under `b` (found by the independent review of the fixes to step 12). `git diff main feature` is asked about for that reason.
  - **The limit that remains:** a file that git tracks, a file that looks like a secret included, when `git diff`, `git show`, `git log -p` or `git grep` prints it. A rule on reading closes this, by rule 4: with `deny read **/*.pem` the command is asked about.
  - Commands that print names and not content (`ls -R`, `find`, `tree`, `fd`, `du`) show the names of files a rule protects.
- **A glob takes a command off the table.** The names it expands to can begin with a `-`, which the program then reads as an option. In PowerShell a pattern inside quotes does too, and so does a path that is not a file's (`Env:PATH`, `HKLM:\Software`): PowerShell's commands expand `*`, `?` and `[…]` in a path themselves, so `Get-Content '.en?'` reads `.env`.
- **The program is named plainly:** `ls`, not `./ls` or `/tmp/ls`.
- **`git` is on the table only in a trusted project** (`Parts::repository_trusted`, below). A repository's own configuration can name programs that `git status` and `git diff` run ([../security.md](../security.md), section 2), and an untrusted repository can arrive with that configuration.
  - In a trusted project the `git` forms on the table are as safe as `.git/config` is. That is why nothing writes git's configuration unasked, in the workspace's repository or in one nested in it, and why `git config` is asked about ("Protected paths", above).
  - What is left is a program that runs unasked at `autonomous` and writes the configuration without the command line saying so. Only the sandbox holds that. From step 13 every shell command runs with git's file-system monitor turned off in its environment, which closes the shortest of those routes.

**The tokenizers are compared with the shells themselves.** A test generates lines, keeps the ones the analysis models, and checks that bash and dash pass exactly the words the analysis gave, and that PowerShell's own parser reads the line as one command with those words. The comparison found, on its first run, that dash and bash disagree about `20<file`.

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

**As built in step 12** (`cortex-policy`, `approvals` and `engine`).

- **What a choice would add is worked out before the question is put,** and travels with it, so a front end shows it in full: "always allow `cargo build *` in this project". For a command that is the program and its subcommand, with any further words: `cargo build *`, `npm run *`, `make *`. For a path, that one path. For a host, that host, written so that it matches nothing else: a host `*` is the rule `[*]`, not every host (found by the review of step 12). For an extension's tool, that tool.
- **For a program that works through subcommands, only a subcommand on a short list is approved with any further words:** one whose further words choose what is built, shown or changed in the project, and cannot name a program to run, a package to fetch and run, or a place to send the project (`cargo build`, `cargo test`, `npm run`, `git commit`, `git diff`, …). Any other subcommand, and the program with none, is approved as the exact line. Among those not on the list (found by the review of step 12): `npm exec`, `pnpm dlx` and `yarn dlx`, which fetch a package and run it; `git rebase` (`--exec`), `git bisect` (`run`), `git submodule` (`foreach`), `cargo watch` (`-x`) and `rustup run`, which run a command they are given; `git push`, `git fetch`, `git pull` and `git clone`, which reach the network and take a program to run there (`--receive-pack`, `--upload-pack`); `git grep` (`-O` starts a pager), `git init` (`--template` copies in hooks), and `go run`.
- **A program that runs its arguments is approved line by line.** For `sudo`, `env`, `xargs`, `timeout`, `nohup`, a shell, an interpreter, `java`, an editor with a command language (`vim -c`), `ssh`, `docker`, programs with a language of their own in their arguments (`find`, `sed`, `tar`, `rsync`, `npx`), **programs that reach the network** (`curl`, `wget`, `scp`, `nc`, cloud tools: a download can be written anywhere, `curl -o ~/.bashrc`), programs with an option that runs another (`rg --pre`, `fd -x`, `man -P`, a pager's `!`, `zip -TT`), and the like, the rule is the exact line, word for word. A version after a program's name (`python3.12`) is the same program: approving `sudo apt list` must not become "allow `sudo *`", nor approving one `find` "allow `find *`". The same holds for a program that works through subcommands when an option stands where the subcommand would be: `git -C sub commit -m x` is approved as that line, not as `git *`. Where the line cannot be written as a pattern (`bash -c 'rm -rf x'` has a word with a space), no rule is offered.
- **Where no rule could say the approval safely, only "allow once" and "deny" are offered:** a line the analysis does not model, a question an `ask` rule raised (an allow would not lift it), a call that reaches "any host". Rules are offered only when they would settle every reason the call was asked about.
- **"Always allow in this project" is offered only where it can be kept:** when the engine was given a store (`Question::storable`). Offered without one, it would allow the call once and save nothing, while the person believed it saved (found by the review of step 12).
- **An approval is only ever an allow, and only ever lifts a default.** A `deny` or an `ask` rule still beats it, and so does the built-in protection.
- **Stored approvals are the user's own:** they are kept outside the repository and apply whether or not the project is trusted. The file is `approvals/<project key>.toml` in the state directory, one `[[approval]]` table per rule with its id, the time it was given and the rule. It is written owner-only and atomically on every write, and read strictly: a key this build does not know, a newer `version`, or an entry that does anything but allow, refuses the whole file, which then grants nothing and is not overwritten.
- **An answer the server does not know is a denial,** as [decision 0028](../decisions/0028-enforced-values-fail-closed.md) says. A choice that was not on offer allows the one call and adds nothing.
- **A change to the file holds a lock from the read to the write:** a file beside it with `.lock` added to its name, locked by the operating system as the credentials file is. Two sessions of one project that store an approval at the same moment both keep theirs. On a file system that says it cannot lock there is no lock, as for the session store; any other failure to lock is an error, and nothing is written.
- **A stored approval is not pinned to what it runs.** "Always allow `make *`" still allows `make` after the `Makefile` changes, and `npm run *` whatever `package.json` then says. That is by design and a stated limit: the approval is of a command, and what the project's files make it do is the project's. Where that matters, allow once.

**In the RPC protocol** an approval is a request from the server to the client ([11-rpc-protocol.md](11-rpc-protocol.md)). If no client answers within the configured time, the answer is deny.

**As built in step 13a** (`cortex-core`, `policy` and `clients`).

- **Who is asked.** Every client that has the session open, has the `user` role and declared that it answers approvals. The request carries the choices, the reasons the call asks (`reasons`) and the rules that "for this session" or "in this project" would add (`adds`), so a front end can show exactly what an answer allows from then on.
- **A user client that opens the session while a question waits is asked it too,** under the same id, and may answer it. A question whose askers have all gone waits for that, for its time limit, or for the turn to be interrupted; it is not answered for them.
- **Whose answer counts.** A `user` client's that was asked. An answer from any other client is refused with `role_not_allowed` and changes nothing: the question stays open. A `supervisor` is asked nothing and may answer nothing until task leases give it a grant to answer within (step 37).
- **The first answer wins.** The request is closed with it; a later answer is told the request is not open, and every client of the session gets `request.resolved`.
- **The time limit** is the core's option, and is none by default: a person at a terminal is not hurried. The RPC server sets one (step 27). Past it the call is denied and the clients are told (`timed_out`).
- **A question that is withdrawn** (the turn was interrupted, or the response the call came from was thrown away) is closed, and the clients are told (`cancelled`).
- **Recording** an answer for the project writes a file, so it is done on the blocking pool. If it cannot be written, the call is still allowed, once, and the user is told.
- **With nobody to ask** (no client that may answer: headless mode), the call is refused with the engine's own explanation, and the refusal is kept: it is the headless result's `permission_denials`. With the core's `fail_on_denial` option the same refusal ends the turn, through the loop's `Stop`, with the outcome `denied` ([10-headless.md](10-headless.md)). A rule's own `deny` is neither: it is an ordinary refusal, and is not listed.
- **Whether an answer may stop the turn** (`Policy::answers_may_stop`) is the core's `fail_on_denial` option, or commands being confined. A person's choices have no "stop", so with neither a later call that was allowed runs beside an open question. With the option, a question nobody can be asked ends the turn, and whether somebody can be asked is not known until it is put. While commands are confined, a command beside the question may change protected paths and stop the turn, and the answer is then a stop (found by the second review of step 26). So with either, the loop holds every later call with side effects behind an open question, whoever is there to answer.
- **A changed level** (`session/set`) starts a new engine from the same rules and what was approved so far, and records the old standing lease as ended and the new one as granted. What was allowed for the session stays the session's own, under the ids it had (`Engine::keep_session_approvals_of`, added to `cortex-policy` for this).
- **A tool that declares an effect this version does not know is refused.** The engine cannot be shown the whole of what the call does, so it is not asked to allow it. A level that cannot be held is refused and nothing changes. Raising the level needs a `user` client.

**When a session is supervised** by another session, its approval requests go to the supervisor first. The supervisor may answer only what its own lease covers, and passes the rest up to the user. A user attached to the session can always answer directly. See [17-coordinator.md](17-coordinator.md).

## Project trust

Opening a project for the first time asks one question before anything from the project that can run code or grant permissions is used.

- **Needs trust:** project configuration that sets permission `allow` rules, hooks, extensions, MCP servers (including ones found in another tool's configuration file in the repository), and learned or shared commands used for the safe list.
- **Does not need trust:** project instruction files (`AGENTS.md`, `CLAUDE.md`), the shared project profile, custom command files and skills. They are text for the model. They cannot grant anything, and the injection risk they carry is covered in [../security.md](../security.md).
- Trust is stored per canonical project path in the user's state directory, with a hash of the trusted definitions. If a hook, extension or MCP server definition changes, it is disabled until the user approves the change. This closes the "approved once, changed later" hole.
- Headless and RPC modes cannot ask. They skip untrusted project resources and report that they did, unless a flag grants trust for the run.

**As built in step 12** (`cortex-policy`, `trust`).

- **A project is known by its key** ([07-sessions-and-context.md](07-sessions-and-context.md), "On-disk layout"), made from its resolved root in the case stored on disk. A folder that differs from a trusted one only in case is another project.
- **A definition is a kind, a name and a hash** of everything that decides what it does: a hook's command and each argument, an extension's manifest and script, an MCP server's command, arguments and tool definitions. The parts are hashed with their lengths, so moving one character from an argument to the next is another definition.
- **A project's rules are a definition too,** all of them, hashed together and without regard to their order: the rules that deny and ask as well as those that allow. The rules that deny and ask take effect without trust; they are pinned because what the user trusted was the allow rules *with* what held them back. A project that drops `deny exec make deploy` beside `allow exec make *` has loosened, and its allow rules wait until the user approves its rules again (found by the review of step 12). One that adds a rule that holds back has changed too: the new rule holds at once, and the allow rules wait. `ProjectTrust::rules_approved` is given every rule of the project as its files hold them now.
- **The engine is told two things about trust, side by side.**

| `Parts` field | Filled from | Decides |
|---|---|---|
| `trusted` | `ProjectTrust::rules_approved` with every rule of the project | Whether the project's allow rules apply. Any change to them sets it to `false` until the user approves again. |
| `repository_trusted` | `ProjectTrust::is_trusted` | Whether git's commands that read the repository's own configuration (`git status`, `git diff`) may be on the table of commands that only read. |

  So a trusted project that only adds a rule that holds back keeps git's forms that only read, while its allow rules wait for the user. The split was made in the fixes to step 12, for step 13a.
- **A definition stands in one of four ways.**

| Standing | Meaning | Used |
|---|---|---|
| Approved | Its hash is the one the user approved. | Yes. |
| Changed | It was approved, and has changed since. | No, until the user approves the change. |
| New | The project is trusted, and this was not among what the user saw. | No, until approved. |
| Untrusted | The project is not trusted. | No. |

- **The file is `trust.toml` in the state directory:** one `[[project]]` table per trusted project with its key, its root as it should be shown, the time, and a `[[project.pinned]]` table per definition. It is written owner-only and atomically, under a lock held from the read to the write (`trust.toml.lock`), so that a `trust` and a `revoke` at the same moment cannot undo each other, and read strictly: a key this build does not know or a newer `version` refuses the whole file, and then **nothing is trusted**.
- **Nothing a tool does can write it.** The state directory is closed to every tool by the built-in protection, at every level.

**As wired in step 13a** (`cortex-core`, `assemble::trust`). The core works trust out once for a session, and gives it to four places, the engine twice. It is two answers, because trust lets a project do two different kinds of thing.

| The answer | Who is given it | What it decides there |
|---|---|---|
| **The project is trusted, and its rules are the ones the user saw.** | Configuration (`cortex-config`'s `ProjectTrust`) | Whether the project's file may set what is marked as needing trust: a dearer model, a looser setting. |
| | The permission engine (`Parts::trusted`) | Whether the project's `allow` rules count. |
| **The project is trusted.** | The permission engine (`Parts::repository_trusted`) | Whether git's commands that read the repository's own configuration are commands that only read. |
| | The shell tool (`repository_trusted`) | Whether git is asked what a command changed. |
| | The capture of the base state | Whether git is asked for the repository's state. |

- **The first is about what loosens or costs,** so any change to the project's rules sets it aside until the user approves the change: the project's settings that need trust are held back, its `allow` rules are not used, and the user is told in a notice.
- **The second is about letting git read the repository's own configuration,** which can name programs for git to run. A change to the project's permission rules does not change what git would run, so it does not take this away: a project that adds a rule does not lose its base state, the exact record of what its commands changed, or `git status` as a command that needs no question.
- **`--trust-project` gives both, for the run.** A trust store that cannot be read gives neither, and the user is told.

**Trusting a project from the command line (step 13b).** `cortex permissions trust` trusts the project of the working directory, or approves the change of its rules when it is trusted already. It pins `Definition::of_rules` over **every** rule the project's files hold now, allow, deny and ask, read as a trusted load reads them: the very set a start compares with the pin (`assemble::project_rules`, one function for both), so the next start reads the project as trusted. An approval keeps the other definitions pinned for the project; a first trust pins only the rules. It prints the rules it pinned and the settings that trust now turns on. `permissions untrust` takes the trust away, `permissions list` says which of the three states the project is in (not trusted; trusted; trusted with its rules changed since), with its stored approvals, and `permissions revoke` removes those by id. A system test trusts a project this way and sees its model chosen on the next start, then set aside again when a rule is added.

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

**Protected paths inside the workspace:** git's hooks and configuration (`.git/hooks`, `.git/config`, `.git/config.worktree`, `.git/info`), the project's `.cortex` directory, shell start-up files, CI configuration, and registry and build settings, as listed in "Protected paths" above, plus `sandbox.protect`. A process that can write these can run code later, outside the sandbox. A nested repository's configuration (`sub/.git/config`, `.git/modules/*/config`) is not a fixed path, so the sandbox step looks for it by name.

### How it is applied

The harness re-executes itself as a small launcher: `cortex` with a hidden subcommand, the policy passed on a file descriptor, then the command. The launcher is single-threaded and does nothing but apply the policy and `exec` the target. Doing this in a fresh process avoids the hazards of running setup code between `fork` and `exec` in a multi-threaded program. The cost is one extra process start of our own binary per command, which the performance budget covers.

The shell tool has the hook for it since step 11 (`cortex_tools::process::Launcher`): the tool hands the shell's command line to its launcher, and starts what the launcher returns, with the bytes the launcher gives as the standard input of what is started. That input is the file descriptor the policy travels on: the launcher reads it to its end and gives the command a closed input. The process is started as a tree either way, so stopping it and cleaning up after it do not depend on the sandbox.

### Linux: in the first release

| Mechanism | Used for | Needs |
|---|---|---|
| `no_new_privs` | Required by the two below. | Nothing. |
| Landlock | Filesystem allow-list: read-everything, write only to the listed roots. | Kernel 5.13 or later with Landlock enabled. Rule coverage improves with newer kernels. |
| seccomp filter | Network off (deny creating sockets), plus denying the calls that would let a process undo the sandbox (new user namespaces, `io_uring`, and similar) and keeping every process in the command's group. Tracing other processes is kept inside the sandbox by Landlock. | Nothing. |
| Session, process group, death signal and the guard process | Clean-up. Built in step 11, in `cortex_sandbox::tree` ([decision 0030](../decisions/0030-process-tree-control-in-cortex-sandbox.md)), and used with or without the sandbox. | Nothing. |

- No root, no helper program, no user namespaces, no container runtime. This matters because Ubuntu 24.04 and later restrict unprivileged user namespaces by default, which breaks the namespace-based approach other tools use unless an administrator installs a profile.
- **Detection.** At start-up the harness asks the kernel which Landlock version it has. If Landlock is missing or disabled, the sandbox is reported as "not enforcing" in the status line, commands that run are not confined, and the `guided` and `autonomous` levels are refused.
- **Known limits, stated plainly.**
  - Landlock rules only add permissions. With the workspace writable, protected paths inside it cannot be blocked by Landlock alone. The harness therefore checks the protected paths after every sandboxed command and, if one changed, stops the turn, shows the change and offers to restore it from a checkpoint taken before the command. This detects; it does not prevent.
  - Reads are not restricted by default, so a sandboxed command can read files such as SSH keys. It cannot send them anywhere while the network is off. A stricter read policy is a configuration option.
  - Network control is all or nothing. There is no per-host allow-list.

### As built in step 26

Step 26 built the Linux sandbox in `cortex-sandbox` and wired it into the core and the binary. What the kernel must offer, and what a command may reach, are [decision 0032](../decisions/0032-what-the-linux-sandbox-needs.md).

**The parts.**

| Part | Where | What it does |
|---|---|---|
| The policy | `cortex_sandbox::policy::Policy` | What the launcher applies: what may be read (everything, or the trees under some roots), the write roots, the network, the session's temporary directory, and the Landlock version the harness counted on. It is made from the lease's grant: the lease's `SandboxPolicy` stays the policy of record, and the protocol type is not changed. Its wire form is a list of NUL-terminated fields read strictly: a field or a value it does not know, a field given twice, a missing one, or anything after its end refuses it whole ([decision 0028](../decisions/0028-enforced-values-fail-closed.md)). |
| Detection | `cortex_sandbox::support::detect` | One system call at start-up for the kernel's Landlock version. `Support::assess` says whether the sandbox enforces and names what it leaves open. |
| The launcher's two sides | `cortex_sandbox::launch` | `Launcher` (the harness's side): what to start in place of a command, and the policy's bytes. `run` (the launcher's side): the body of the hidden subcommand `cortex __sandbox -- <command>`. It reads the policy to its end, applies Landlock and then the seccomp filters to itself, marks every descriptor above the standard three to close, gives the command a closed input and `TMPDIR`, and becomes the command. |
| The backends | `cortex_sandbox::confine`, behind the feature `backends` | Landlock and seccomp ([decision 0032](../decisions/0032-what-the-linux-sandbox-needs.md)). Off by default, so that `cortex-tools`, which needs only process trees, links neither; `cortex-core`'s `sandbox` feature, on by default, turns it on. Without it the launcher refuses to run anything and detection says the build has no sandbox. |
| What the model is told | `cortex_sandbox::explain` | One line added to the result of a command that failed: the path of a write the sandbox refused (an absolute path outside the write roots, named in a line that reports a refusal), that the network is off (from what programs print when they cannot make a socket or look up a name), or the launcher's own reason when the command did not run. It names only what it can: a refusal of a path inside the workspace is the file system's own, and is not explained as the sandbox's. |
| The session's sandbox | `cortex-core`, `sandbox` | The state a session reports, the launcher it hands the shell tool, and its private temporary directory. |

**The state a session reports.**

| The system | `sandbox.enabled` | The binary gave a launcher | State | `autonomous` |
|---|---|---|---|---|
| Linux, Landlock 6 or newer, `sandbox` feature | true | yes | enforcing | starts |
| Linux, Landlock 3 to 5 | true | yes | enforcing, with one line at the session's start: a command can still signal a process outside the sandbox | starts |
| Linux, Landlock 1 or 2 | any | any | off: `NoSandbox::LandlockTooOld`, which names the version and what it lacks | refused |
| Linux without Landlock, or with it turned off | any | any | off: `NoLandlock` | refused |
| A write root (the workspace, the temporary directory or `sandbox.write`) holds the harness's own configuration, state or data directory, or lies inside one, or holds the program started as the launcher | true | yes | off: `HarnessFilesWritable`: a confined command could change what it is allowed, or replace the launcher | refused |
| A build without the `sandbox` feature, or a core given no launcher | any | | off: `NotBuilt` | refused |
| Windows | any | any | off: `UnsupportedSystem` | refused |
| any | false | any | off: `TurnedOff` | refused |

- **The state follows the level.** At `unrestricted` nothing is confined, so a session there reports the sandbox off (`SessionInfo::sandbox`, and the session's state after a `session/set`), and an approval's `outside_sandbox` is true. The `initialize` result reports whether a session with the sandbox enabled would be confined on this machine.
- **The binary gives the core its own executable** (`CoreOptions::launcher`) as it gives the guard's: the same path, the subcommand `__sandbox`. `CoreOptions::sandbox` replaces detection for a test, which can then say the kernel has no Landlock, or an old one.

**The network** ([decision 0032](../decisions/0032-what-the-linux-sandbox-needs.md), accepted by the owner on 2026-11-13). With `sandbox.network = "off"`, the default, a confined command makes no socket but netlink's route one and a connected pair: no TCP or UDP, to another machine or to this one's own loopback addresses, no Unix socket by its path or an abstract one. With `"full"` it makes the internet's sockets (TCP and UDP, loopback included) and netlink's route one, and nothing else: still no Unix socket of anyone else's (the user's session bus, an agent or the Docker socket would let it out of the sandbox), no virtual machine's socket to its host, no raw or packet socket. **What it costs:** a test suite that starts a server on loopback or on a Unix socket fails inside the sandbox, and so does a tool that talks to a local daemon (`docker`, a signing agent), and Python's `multiprocessing` with the `forkserver` start method (the default from Python 3.14). The way out is `sandbox.network = "full"` for loopback, or one approved run outside the sandbox (`sandbox: false`).

**Every command below `unrestricted` is started through the launcher** where the sandbox enforces: the shell tool hands the shell's command line to the session's launcher, and starts `cortex __sandbox -- <shell> <arguments>` with the policy on its standard input. The process tree is set up by the runner before the launcher starts, as before.

**It fails closed.** Once the session reports the sandbox enforcing, a failure to apply it fails the call and says why; nothing is run unconfined in its place. Each failure is tested: a policy the launcher does not understand, Landlock refused by the kernel (a parent that refuses its system call), a seccomp filter that does not load (the same), and a launcher that cannot start at all, which the runner reports as a command that was not started. The launcher's own line begins `cortex-sandbox:` and its exit code is 125.

**The temporary directory.** The lease names `cortex-harness-<session>` in the system's temporary directory among its write roots (step 12). Since step 26 the core makes it when the session is assembled, open to the user alone, gives it to every confined command as `TMPDIR`, and removes it, with everything in it, when the session ends. Something already at that path that is not a private directory of the user's own (another user's, or a link) is not used: commands are then given no temporary directory, it is taken out of the launcher's write roots, and the user is told.

**Protected paths after each command.** Landlock cannot keep a path inside the writable workspace from changing, so the core looks at the protected paths before and after every confined command that may write (`cortex_sandbox::protect`; `cortex-core`, `checkpoints`):

- **What is looked at:** the lease's protected paths, each with everything under it, and, found by a walk of the workspace that follows no link, the protected names at any depth: in every repository the walk meets, nested ones and the submodules kept in `.git/modules` included, its `config`, `config.worktree`, `hooks` and `info`, and each worktree's `config.worktree`, `commondir` and `gitdir` (a module or a worktree that is a link is reported as the link, and not looked through); a bare repository made of plain files (a directory with `HEAD`, `objects` and `refs`); a `.git` file; `.cargo/config.toml` and `.cargo/config`; `.npmrc`, `.yarnrc`, `.yarnrc.yml` and `.envrc`.
- **How a change is told:** a path that appears, goes, or changes its kind, mode or content; where the content was not kept, its size, modification time, change time or inode. The change time and the inode are set by the system alone, so a command that puts a file's modification time back is still found. The same bytes written again are no change. A protected path reached through a linked directory in the workspace is not looked through: the link is what is reported, and what was under the directory is reported gone.
- **What happens:** the result names each changed path and how it changed; the turn stops: no later call of it is decided as anything but a stop, an answer to a question that was open at the stop is a stop, and no call of it that changes anything runs, even one decided or approved before the stop (`StopsWithTurn`, around every tool of the session); and what each path held before is recorded in the round's checkpoint, so undoing the turn (`session/rewind` with `restore_files`) puts it back. Content is kept for files up to 256 KiB and 4 MiB in all. A file past that, a link, a special file, a change of mode, a directory, a path outside the workspace, a path that a link leads to when the command ends, and everything in a session kept in memory is found and reported, and the result names it as one to put back by hand. A file whose content and mode both changed keeps its content for undo, and its mode is named as one to put back by hand. **Undo never writes through a link:** a file whose directory on the way, or which itself, is a link when it is put back is left as it is and reported, so a command that swaps a directory for a link cannot have undo write where the link leads (found by the review of step 26; `cortex-session`, `snapshot::through_a_link`).
- **Bounded:** the walk lists at most 20,000 directories and stops after a quarter of a second. Past either the look is incomplete, and **it warns and does not stop**: the result says that the look stopped at its bound and a change further down would not have been seen. Stopping every command in a large workspace would make the sandbox unusable there; asking would ask about every command. **Unless the look before the command was complete:** then the command itself made the workspace too large to look at whole, perhaps to hide a change further down, and the turn stops as for a change (found by the second review of step 26). A change to what *was* looked at is still found, and two looks are compared only where both reached, so a look that stopped sooner than the other reports nothing made or gone beyond it.
- **Nothing a command starts outlives it:** a confined process cannot leave the command's session (`setsid` is answered as done without being done). It may make a process group of its own, which `timeout` and test runners need; the tree kills the session's processes outside its group as well as the group when the command ends. So the whole tree dies with the command, and nothing it started writes after the look that follows it. What this costs, programs that make a terminal among them, is in [decision 0032](../decisions/0032-what-the-linux-sandbox-needs.md).
- **What it costs,** measured on the desktop in a release build: 23.6 ms a look for this repository with its build directory (3,278 directories), so about 47 ms a command, before and after. A workspace of 20,000 directories or more reaches the bound, a quarter of a second a look ([outstanding/the-cost-of-the-look-at-protected-paths.md](../outstanding/the-cost-of-the-look-at-protected-paths.md)).
- **Not looked at:** a command the user let leave the sandbox. Every confined command is looked at, one the engine marked as only reading included, since the program it runs may do otherwise. Nothing is looked at where the sandbox does not enforce.
- **Not on the list,** and so not watched: files that git's configuration names inside the workspace (`core.hooksPath`, `core.fsmonitor`, an `include.path`), and protected names inside a `sandbox.write` root outside the workspace ([outstanding/files-git-runs-that-the-protected-list-does-not-name.md](../outstanding/files-git-runs-that-the-protected-list-does-not-name.md)).
- **A session kept in memory** has no checkpoints: a change is found and the turn stops, and the result says it cannot be put back from there.

**Measured** on the desktop, release build, 200 runs, nothing else running: `bash -c 'exit 0'` alone 0.93 ms; through `cortex __sandbox` 1.71 ms. The launcher adds 0.78 ms, to the runner's 0.18 ms and the tool's 0.24 ms of step 11: about 1.2 ms in all against the 5 ms budget ([performance-budgets.md](../performance-budgets.md)).

### Linux: later, opt-in

A namespace-based backend (mount and network namespaces, with protected paths mounted read-only and a proxy for per-host network rules) where the system allows unprivileged user namespaces. It gives prevention where the default gives detection. It is optional because it cannot be relied on to work.

### Windows: in the first release

| Mechanism | Used for |
|---|---|
| Job Object, kill on close | The whole process tree dies with the command or the harness. Built in step 11 (`cortex_sandbox::tree`). |
| Job Object limits | Since step 26 every tree is held to 1,024 processes at once and, where Windows says how much memory the machine has, three quarters of it committed by the whole tree (`cortex_sandbox::tree::Limits`). A command that starts processes without end, or takes the machine's memory, fails there instead of taking the machine with it. |
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

When a sandboxed command fails because of the sandbox, the result names the blocked operation. The model can ask to run the command again with more access. That request is an approval like any other, and the status line shows when a command ran outside the sandbox.

**As built in step 26.**

- **The model asks with the `shell` tool's argument `sandbox: false`.** The argument is optional and additive: a call without it runs in the sandbox as before. The tool's result for a command the sandbox blocked tells the model of the argument ([05-tools.md](05-tools.md), "`shell`").
- **The plan says so.** Such a call declares the effect `unconfined` beside its `exec` (`cortex_tools::Effect::Unconfined`, `cortex_policy::Access::Unconfined`), so the engine is shown it and nothing about it is left to the tool.
- **It is asked about at every level but `unrestricted`,** where nothing is confined to leave: the column "Leave the sandbox" of the level table. A rule that allows the command does not lift the question; a rule that denies it still refuses it. Where the sandbox does not enforce there is nothing to leave, and the argument decides nothing.
- **Only "allow once" and "deny" are offered,** not the choices of an ordinary approval. A rule says what a command is, not where it runs: an "allow for this session" of `cargo build *` kept from this question would let every later `cargo build` run unasked *inside* the sandbox, which the user did not decide, and nothing could let a later one run *outside* it. So no answer is kept, and each command that leaves the sandbox is asked about. (The coordinator, for the owner, agreed this narrowing on 2026-11-13: [steps/26/answered.md](../steps/26/answered.md).)
- **The approval says the command runs outside the sandbox** (`CommandDetail::outside_sandbox`, per call), and its reason says with what: the user's full rights, writes anywhere the user can, and the network.
- **What the user allowed runs as it is,** with no launcher, and its protected paths are not looked at afterwards: the user allowed the user's own rights.

## Summary of what is realistic

| | Linux | Windows |
|---|---|---|
| First release | Enforcing filesystem and network sandbox for shell commands, verification, hook commands and MCP servers, with the stated limits. Extension scripts confined by the script runtime. Approvals to leave the sandbox. | Process-tree control. Approvals for every command not on a safe list. Extension scripts confined by the script runtime. No OS confinement of child processes. |
| Later | Namespace backend for prevention of protected-path writes and per-host network rules. | AppContainer backend after a spike. |
| Not planned | Anything that needs root or a daemon. | Anything that needs administrator rights, including per-host network filtering. |
