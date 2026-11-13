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

| Level | Read | Read beyond the scope | Edit | Write beyond the scope | Read-only command | Safe command that may write | Other command | Network from a tool |
|---|---|---|---|---|---|---|---|---|
| `read-only` | holder | supervisor | refused | refused | holder | supervisor | supervisor | supervisor |
| `supervised` | holder | supervisor | holder | supervisor | holder | holder | supervisor | supervisor |
| `guided` | holder | supervisor | holder | supervisor | holder | holder | supervisor | supervisor |
| `autonomous` | holder | supervisor | holder | supervisor | holder | holder | holder | supervisor |
| `unrestricted` | holder | holder | holder | holder | holder | holder | holder | holder |

- **The scope of a standing lease is its writable roots:** the workspace and the session's private temporary directory, plus `sandbox.write`. A read or an edit under one of them is inside the scope. The sandbox's `read` set is not asked: it says what a child process may read, and a file tool's read outside the workspace asks ([05-tools.md](05-tools.md), "Paths").
- **Three things are beyond the scope wherever they are.** A path outside the writable roots. A write to a protected path (`.git/hooks`, `.git/config`, `.cortex/`, shell start-up files, `sandbox.protect`): whoever can write one can run code later. And a file that looks like a secret, read or written ("Files that look like secrets", below).
- **A level that refuses is final.** At `read-only` a write does not happen, whatever a rule allows. Rules settle what a level leaves open; they do not open what it closes.
- **A rule still holds at the levels that ask least.** A `deny` is a denial at `unrestricted`, and an `ask` is a question.
- **`guided` reads like `supervised` in this table** because the kinds of command a guided lease names are allow rules of that lease, and rules are settled before the level is asked. It cannot be chosen until task leases exist (step 37).
- **Where the sandbox does not enforce,** `autonomous` is refused when the session starts, with the reason and what to use instead (the error code `sandbox_required`), and every other level shows one line, "sandbox: off", that says what follows: commands on a safe list run without asking, every other command asks first, and one that is approved runs with the user's full rights. Until step 26 builds the sandbox, that is every session.
- **A level this build does not know decides nothing:** every matter is refused, and a session cannot be started at it.

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
    fn narrow(&self, requested: &Lease) -> Result<Lease, NarrowError>;
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
- **An allow rule is kept only when the parent has the same rule,** by its effect and its pattern as text. A child that asks for `cargo test *` under a parent that allows `cargo *` does not get the rule: deciding that one pattern lies inside another is where a mistake would widen a lease, so it is not attempted. A proposal names the parent's rules it wants.
- **Roots are compared exactly.** A writable root reaches only folders spelled as it is, so that on Windows it never reaches a folder that differs only in case.
- **An expiry that cannot be ordered becomes a stop condition.** A lease that ends "at the end of the session" and one that ends "at noon" cannot be put in order ahead of time. The child's expiry is the event, and the moment is added as a `time` stop condition, which the harness enforces the same way. A time nobody can read is taken to have passed.
- **Budgets are narrowed as written.** To cap a child by what the parent has left, the caller narrows a copy of the parent's lease whose budget is the remainder (`with_budget`).
- **Rule patterns are relative to each session's own workspace.** A parent's `deny write secrets/**`, carried to a child in another worktree, protects that worktree's `secrets/`. The Coordinator step checks that this is what a team wants.

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
| `**/.env` | Anywhere, in the workspace or outside it. |
| `src/**`, `docs/*.md` | With a separator: under the workspace root. |
| `*.lock`, `Cargo.toml` | With none: that name at any depth in the workspace. |

A pattern that ends in `/**`, or in a separator, matches the directory itself as well as what is in it, so that a rule on a tree covers a listing of its top.

**A rule that holds back is matched as widely as it can be, and a rule that allows as narrowly.**

- **Case, on Windows.** A `deny` or an `ask` matches a path without regard to case, and also exactly. An `allow` matches exactly, against the resolved path, which is in the file system's own spelling. So no spelling gets round a deny. And an allow for `App` never grants `app`, which is another folder in a directory marked case-sensitive. Since every path is resolved before it is matched, an allow works however the *path* was typed; it is the *rule* that must spell the folder as it is stored, and one that does not simply does not match. A drive letter, and a share's server and name, have no case on disk and are always folded.
- **Commands.** An `allow` matches one whole simple command of a line the analysis models, from its first word: `git status` allows `git status` and not `git status --short`, and a rule for `git` does not allow `./git` or `/tmp/git`. In PowerShell, which finds commands without regard to case, the program's name is compared that way and `git.exe` is `git`. A `deny` or an `ask` matches wherever its words appear in the raw line, one after another: `rm *` matches `sudo rm -rf x`, `echo x; rm y`, `bash -c 'rm y'` and `/bin/rm y`. It matches too much on purpose, and it is the only match there is for a line that could not be analysed.
- **A call that cannot say which host it reaches** meets every rule that holds back, and is allowed only by an allow for every host (`*`).

**Who wrote a rule.** Each rule reaches the engine with its source: the managed configuration, the user's, the command line, the project's, the project's learned commands, a stored approval, a session approval, or a lease. An `allow` from the project or from its learned commands is set aside until the user has trusted the project, and the set-aside rules are kept for a front end to show. The configuration loader already holds such rules back ([08-configuration.md](08-configuration.md)); the engine applies the same rule again where rules are enforced, so that nothing upstream has to be right for it to hold. Layers need no rank beyond that: a managed `deny` stands whatever a user rule allows, because `deny` beats `allow` whoever wrote them.

**A rule that cannot be compiled refuses the whole set,** and the session does not start: a rule that is skipped could be the one that denies.

**The built-in protection** is not a list of rules but a part of every rule set that nobody wrote and nobody can remove. It denies, so no allow and no level lifts it.

| Closed | To |
|---|---|
| The configuration directory (credentials, configuration) | Reading and writing. |
| The state directory (trust, stored approvals, what was learned) | Reading and writing. |
| The data directory | Writing. |
| The session directories of other projects | Reading. This project's own are outside the workspace, so reading one asks. |

Closing the state directory to writing is what makes trust and approvals mean something: a model that could write `trust.toml` could trust its own project.

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
| `< file`, `> file`, `>> file` with one digit before or none; `2>&1`, `>&2`, `>&-` | `<<`, `<<<`, `<>`, `>\|`, `&>`, `>&file`, `<(…)`, `>(…)`, several digits before a redirection (`20>f` is a descriptor to bash and an argument to dash) |
| | `NAME=value` where a command is expected |
| | A keyword (`if`, `for`, `time`, …) or a builtin that runs text as code or changes what later commands mean (`eval`, `exec`, `source`, `.`, `trap`, `command`, `alias`, `export`, `set`, `unset`, `shopt`, `pushd`, `exit`, …) as the command |

**The PowerShell subset** is one command with literal arguments: words of ASCII letters, digits and `_ - . / \ : = +`; a whole word in single or double quotes, with no `$` and no back-tick in double quotes; `~` for the home directory; `*` and `?`, flagged. Everything else is asked about: `;`, `\|`, `&`, a line break, `$`, a back-tick, any bracket, `@`, `,`, `%`, `#`, `!`, `<`, `>`, a quote that is not a whole word, a parameter that ends with `:`, any character outside ASCII that is not inside quotes, a typographic quote anywhere (PowerShell reads `‘ ’` as `'` and `“ ”` as `"`, and several dashes as `-`), a keyword, and `powershell`, `pwsh`, `cmd`, `Invoke-Expression`, `iex` and `.` as the command, which is where an encoded command would be.

**`cmd.exe` is not analysed, and neither is a shell this build does not know** (`fish`, `nu`): the shell tool starts any unknown program as if it read `-c <command>`, which is no reason to believe it reads the line like a POSIX shell. The engine is told the shell by its program's name (`ShellSyntax::of_program`).

**What "not modelled" means.** No allow rule applies to the line and no safe list does. The level then decides it as any other command: asked about at `read-only`, `supervised` and `guided`, and run at `autonomous`, where the sandbox confines it. A `deny` or `ask` rule is still matched against the raw line. This is rule 2 above, stated for every level: the analysis never turns an unmodelled line into "allow", and it does not ask where the level would not.

**How a modelled line is decided.**

1. A `deny` or `ask` rule on the raw line or on any part is settled first.
2. Each part must be allowed: by the table of read-only commands, or by an allow rule. A line is on a safe list only if every part is.
3. **What a command names is checked as a read.** `cat` only reads, and `cat ~/.ssh/id_rsa` reads a private key. So every file a table command names, and the directory it runs in, is put to the same judgement a `read` call gets, and the command is safe only if each would be allowed unasked. Where a rule denies a named path, the command is asked about, not refused: an argument that reads like a protected file may be a search pattern (`grep .env README.md`), and nothing is lost by asking. For every other modelled command, an argument that names a path a rule denies or asks about, or one of the harness's own files, makes the command one to ask about, whatever allows the command.
4. **`cd dir && …` is followed:** the parts after it are judged from the new directory. A `cd` whose effect cannot be known (in a pipeline, before `;` or `||`, with no argument or two) takes the line off every safe list.
5. **A redirection is a write of its file,** or a read, judged as one: outside the scope it asks, to a protected path or a secret it asks, and where a rule denies it the line is refused, because a redirection is a write for certain. `/dev/null` is nothing.
6. **A line whose every part only reads is marked read-only,** which is how the loop comes to run it beside other calls ([03-agent-loop.md](03-agent-loop.md)).

The paths a command names are followed by the same resolver the file tools use, so a link in the workspace leads where the file system says. The analysis itself looks only at text.

**The table of commands that only read.** A command is on it with its options, never as a whole.

| Shell | On the table |
|---|---|
| POSIX | `ls`, `cat`, `head`, `tail`, `wc`, `grep`, `egrep`, `fgrep`, `rg`, `fd`, `fdfind`, `find`, `sort`, `uniq`, `stat`, `file`, `du`, `df`, `tree`, `readlink`, `realpath`, `basename`, `dirname`, `pwd`, `whoami`, `id`, `uname`, `hostname`, `which`, `true`, `false`, `echo`, `printf`, `test`, `date`, `sed` (only as `sed -n '<lines>p'`), and `git` with `status`, `diff`, `show`, `log`, `branch`, `tag`, `rev-parse`, `ls-files`, `blame`, `describe`, `grep`, `remote`, `stash list`, `version` |
| PowerShell | `Get-ChildItem`, `Get-Content`, `Select-String`, `Test-Path`, `Resolve-Path`, `Get-Item`, `Get-Location`, `Get-Date`, `Write-Output`, `Write-Host`, `Get-FileHash` and their aliases (`gci`, `ls`, `dir`, `gc`, `cat`, `type`, `sls`, `pwd`, `echo`, …), `whoami`, `hostname`, `rg`, `fd`, and `git` as above |

- **Each entry lists the options that are safe, by their exact names.** An option that is not listed takes the command off the table: `sort -o`, `find -delete` and `-exec` and `-fprint`, `git diff --output`, `git -c …`, `rg --pre`, `fd -x`, `tail -f`, `date -s`. So does an abbreviation of a long option, which GNU programs accept and the table does not, and a parameter of a PowerShell command that is not written out in full.
- **A subcommand that can write is listed only in the form that cannot:** `git branch` and `git tag` with no name, `git stash list`, `git remote` with no subcommand.
- **Programs that are languages are not on it:** `awk`, `perl`, `python`, `xargs`, `env`, `tee`, and `less` and `more`, whose keys run commands. `sed` is on it only to print lines, because its scripts can write files and run commands.
- **No option that follows links is listed** (`grep -R`, `rg -L`, `find -L`, `ls -L`), and `diff` is not on the table, because it reads through the links inside the directories it compares. What a command names is checked name by name; what it finds by walking a tree must stay in the tree.
- **A glob takes a command off the table.** The names it expands to can begin with a `-`, which the program then reads as an option.
- **The program is named plainly:** `ls`, not `./ls` or `/tmp/ls`.
- **`git` is on the table only in a trusted project.** A repository's own configuration can name programs that `git status` and `git diff` run ([../security.md](../security.md), section 2), and an untrusted repository can arrive with that configuration.

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

- **What a choice would add is worked out before the question is put,** and travels with it, so a front end shows it in full: "always allow `cargo build *` in this project". For a command that is the program and its subcommand, with any further words: `cargo build *`, `npm run *`, `make *`. For a path, that one path. For a host, that host. For an extension's tool, that tool.
- **A program that runs its arguments is approved line by line.** For `sudo`, `env`, `xargs`, `timeout`, `nohup`, a shell, an interpreter, `ssh`, `docker` and the like, the rule is the exact line, word for word: approving `sudo apt list` must not become "allow `sudo *`". Where the line cannot be written as a pattern (`bash -c 'rm -rf x'` has a word with a space), no rule is offered.
- **Where no rule could say the approval safely, only "allow once" and "deny" are offered:** a line the analysis does not model, a question an `ask` rule raised (an allow would not lift it), a call that reaches "any host". Rules are offered only when they would settle every reason the call was asked about.
- **An approval is only ever an allow, and only ever lifts a default.** A `deny` or an `ask` rule still beats it, and so does the built-in protection.
- **Stored approvals are the user's own:** they are kept outside the repository and apply whether or not the project is trusted. The file is `approvals/<project key>.toml` in the state directory, one `[[approval]]` table per rule with its id, the time it was given and the rule. It is written owner-only and atomically on every write, and read strictly: a key this build does not know, a newer `version`, or an entry that does anything but allow, refuses the whole file, which then grants nothing and is not overwritten.
- **An answer the server does not know is a denial,** as [decision 0028](../decisions/0028-enforced-values-fail-closed.md) says. A choice that was not on offer allows the one call and adds nothing.
- Two sessions of one project that store an approval in the same instant can lose one of the two: the file is replaced whole, with no lock. The loss is of a convenience, and the next question restores it.

**In the RPC protocol** an approval is a request from the server to the client ([11-rpc-protocol.md](11-rpc-protocol.md)). If no client answers within the configured time, the answer is deny.

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
- **A project's allow rules are a definition too,** hashed together and without regard to their order. Rules that deny or ask need no trust and are left out of the hash, so a project can tighten its rules without being asked about again.
- **A definition stands in one of four ways.**

| Standing | Meaning | Used |
|---|---|---|
| Approved | Its hash is the one the user approved. | Yes. |
| Changed | It was approved, and has changed since. | No, until the user approves the change. |
| New | The project is trusted, and this was not among what the user saw. | No, until approved. |
| Untrusted | The project is not trusted. | No. |

- **The file is `trust.toml` in the state directory:** one `[[project]]` table per trusted project with its key, its root as it should be shown, the time, and a `[[project.pinned]]` table per definition. It is written owner-only and atomically, and read strictly: a key this build does not know or a newer `version` refuses the whole file, and then **nothing is trusted**.
- **Nothing a tool does can write it.** The state directory is closed to every tool by the built-in protection, at every level.

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

The shell tool has the hook for it since step 11 (`cortex_tools::process::Launcher`): the tool hands the shell's command line to its launcher, and starts what the launcher returns, with the bytes the launcher gives as the standard input of what is started. That input is the file descriptor the policy travels on: the launcher reads it to its end and gives the command a closed input. The process is started as a tree either way, so stopping it and cleaning up after it do not depend on the sandbox.

### Linux: in the first release

| Mechanism | Used for | Needs |
|---|---|---|
| `no_new_privs` | Required by the two below. | Nothing. |
| Landlock | Filesystem allow-list: read-everything, write only to the listed roots. | Kernel 5.13 or later with Landlock enabled. Rule coverage improves with newer kernels. |
| seccomp filter | Network off (deny creating internet sockets), plus denying the calls that would let a process undo the sandbox or inspect others (new user namespaces, tracing, and similar). | Nothing. |
| Session, process group, death signal and the guard process | Clean-up. Built in step 11, in `cortex_sandbox::tree` ([decision 0030](../decisions/0030-process-tree-control-in-cortex-sandbox.md)), and used with or without the sandbox. | Nothing. |

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
| Job Object, kill on close | The whole process tree dies with the command or the harness. Built in step 11 (`cortex_sandbox::tree`). Limits on process count and memory are available, and are not set yet. |
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
