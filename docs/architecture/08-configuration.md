# Configuration

`cortex-config` loads settings from several layers, merges them, records where each value came from, and validates the result. It also owns credential storage.

The [vision](../vision.md) adds four requirements that this page covers: profiles, reloading without a restart, reading what other tools already wrote, and a first run that needs no manual set-up.

**Every key, with its type, default and rules, is in the generated [configuration reference](../configuration/reference.md).** This page is the design; that page is the list.

## Format

TOML. One schema, versioned with a top-level `config_version = 1`. The reasons for TOML over JSON are in [decision 0007](../decisions/0007-configuration-format.md).

A length of time is written as text with a unit: `"20s"`, `"10m"`, `"1h30m"`, `"500ms"` (units `ms`, `s`, `m`, `h`, `d`). A bare number is refused, so nobody has to remember whether a key counts seconds or milliseconds.

## Layers

Lowest priority first. Later layers override earlier ones.

| # | Layer | Location | Notes |
|---|---|---|---|
| 1 | Built-in defaults | compiled in | `cortex config defaults` prints them. |
| 2 | Managed | `/etc/cortex-harness/config.toml`; `%ProgramData%\cortex-harness\config.toml` | Optional. For shared machines. Can lock keys (below). |
| 3 | User | `$XDG_CONFIG_HOME/cortex-harness/config.toml` (usually `~/.config/cortex-harness/`); `%APPDATA%\cortex-harness\config.toml` | The usual place. `CORTEX_CONFIG` names another file. |
| 4 | Project | `<workspace>/.cortex/config.toml` | Committed with the repo. Applies only as far as trust allows (below). |
| 5 | Project, local | `<workspace>/.cortex/config.local.toml` | Not committed. The repo skeleton adds it to the ignore file template. Held to the same trust rules as layer 4. |
| 6 | Profile | a `[profiles.<name>]` table from layers 2 to 5, selected by `--profile <name>` or `profile = "<name>"` | A named set of overrides. See "Profiles". |
| 7 | Environment | `CORTEX_*` variables | A short, fixed list, below. Not a general override mechanism: a variable that is not on the list does nothing. |
| 8 | Command line | dedicated flags, and `-c key=value` for any key | Highest priority. A mistake here is always an error. |

**The environment layer.**

| Variable | Sets |
|---|---|
| `CORTEX_LOG` | `log.level` |
| `CORTEX_DATA_DIR` | `session.data_dir`. An absolute path. |
| `CORTEX_CONFIG` | Not a key: the path of the user's configuration file (layer 3). |
| A provider's key variable (`ANTHROPIC_API_KEY` and so on) | Not a key: a credential ("Credentials", below). |

Colour is the fifth item of the list. Its key is in the `[terminal]` section, which the terminal step declares with the variable that sets it.

**On the command line** `-c model.main=some-model` needs no quotes: the value is read as TOML where the key takes that (`-c retry.attempts=3`, `-c sandbox.protect=["a"]`) and as plain text otherwise.

The workspace root is the nearest ancestor of the working directory that contains `.cortex/` or `.git`, or the working directory itself.

**Nothing is read behind the caller's back.** The loader is given the environment and the workspace as values. It does not read the process's environment or its working directory, so a test loads configuration from a folder of its own and cannot see the real home directory.

## Merge rules

- **Tables merge key by key**, recursively.
- **Scalars and arrays replace.** There is no syntax for appending to or removing from an inherited array. A layer that sets an array sets all of it.
- **Named collections are tables keyed by name**, so layers can add or override one entry without restating the rest: `[providers.<name>]`, `[models.<name>]`, `[mcp.servers.<name>]`, `[extensions.<name>]`, `[hooks.<name>]`, `[keys.<mode>]`. An entry is removed by setting `enabled = false`.
- **Three lists are joined, not replaced:** `[[permissions.rules]]`, `sandbox.write` and `sandbox.protect`. Each layer adds its entries to those of the layers below. Their meaning does not depend on order ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)), and a layer must not be able to drop a restriction by restating the list without it: if `sandbox.protect` were replaced, a project's `protect = []` would unprotect what the user protected.
- **Locks.** The managed layer can list keys under `[managed] locked = [...]`. A locked key ignores every higher layer, the command line included, and `cortex config show` says so. Naming a table locks every key in it. A lock on a key this version does not know is kept and warned about, not dropped. `[managed]` in any other file is ignored, with a warning.

## Profiles

A profile is a named table of settings that is applied on top of the file layers when selected.

```toml
[profiles.review]
model.main = "claude-opus-5-5"
model.effort = "high"
permissions.mode = "read-only"

[profiles.quick]
model.main = "claude-haiku-5-5"
verify.mode = "off"
```

- Any key may appear in a profile except the four that describe a file (`config_version`, `profile`, `profiles`, `managed`), with the same trust rules as the layer the profile is defined in.
- A profile defined in several layers merges like any other table, higher layer last.
- `cortex --profile review` selects one for a run. `/profile <name>` switches in a session; settings that affect the prompt or the tool list take effect at the next context epoch, and the UI says so. A profile that does not exist is an error that lists the ones that do.
- **A project may define a profile and may not select one.** `profile = "<name>"` is the user's key. A profile the user wrote is applied with the user's rights, so a repository that could select it could switch on whatever the user's loosest profile allows.
- "Modes" are the permission and behaviour presets in [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md). A profile can select a mode along with anything else.

## Reloading without a restart

Configuration, extensions, themes and key bindings reload while the harness runs.

**How changes are noticed.** There is no file watcher and no polling timer, which keeps the idle cost at zero. The harness checks the modification times of the files it loaded:

- when the terminal regains focus (terminals report this);
- on a key press, at most once a second;
- at the start of every turn;
- when the user runs `/reload`.

The check is a handful of `stat` calls, well under a millisecond. It compares each file's modification time and size with what they were when the file was loaded (and on Unix which file it is, so that a replaced file shows even when both are the same). A file that did not exist is watched too: creating it is a change. Two details keep it from missing a change:

- **The stamp is taken before the file is read.** A change that lands while a file is being read and parsed is then seen by the next check.
- **A file written in the last two seconds is not trusted yet.** File systems record times in steps as coarse as two seconds, so a second write can leave the time as it was. Such a file is reported as changed until it has settled. The cost is a few reloads of a file that did not change again, so a reload that produces the same configuration is not announced.

**What happens to a change.** Each key says when a change to it takes effect, in the [reference](../configuration/reference.md), and a reload returns the changed keys with that.

| Changed | Takes effect |
|---|---|
| Theme, key bindings, UI settings | Immediately, on the next frame. |
| Permission rules, limits, retry and verification settings, the speed tier | Immediately, for the next tool call or request. |
| Model, reasoning level, anything that shapes the system prompt or the declared tool list | At the next context epoch. A notice says what is pending. `/reload --now` starts a new epoch at once. |
| The data directory, the credential store | When the harness is next started. |
| An extension's script or manifest | The extension is reloaded. If its hash no longer matches what was approved, it is disabled and the user is asked to review the change ([12-extensions.md](12-extensions.md)). |
| An MCP server entry | That server is restarted. A changed command needs approval again. |
| An invalid file | The old configuration stays in force and the error is shown. A reload never leaves the harness without a valid configuration. |

**An invalid file never takes the configuration away.** There is no partly loaded configuration: a load either gives a whole one or lists what is wrong, in every layer, in one go. The same broken file is reported once, not on every check.

## Reading other tools' set-up

People switching from another agent should not have to rewrite anything. The harness reads, and never modifies, these:

| What | Where it is looked for | How it is used |
|---|---|---|
| Instruction files | `AGENTS.md` and `CLAUDE.md`, as in [07-sessions-and-context.md](07-sessions-and-context.md) | As project instructions. |
| MCP server definitions | The project's `.mcp.json`, and the per-user files of common MCP clients | Offered at onboarding and by `cortex mcp import`. Project-level definitions appear as proposed servers that need the same trust and approval as any other. See [13-mcp.md](13-mcp.md). |
| Agent Skills folders | `.cortex/skills/`, `.agents/skills/` and `.claude/skills/` in the project and the user's home | Listed in the prompt by name and description, and read on demand. See [12-extensions.md](12-extensions.md). |

Everything else (settings, permissions, hooks, key bindings, themes) uses Cortex-Harness's own formats. Each compatibility reader can be turned off under `[compat]`, which is the user's alone: a project cannot turn a reader back on.

**Discovery reports and does not read.** `cortex-config` answers "what is there?" for the onboarding flow and the import commands: it asks the file system whether each candidate exists and whether it is a file or a folder, and opens nothing. Another tool's file can hold that tool's tokens. Reading one is an import, which the user asks for.

| Kind | In the workspace | In the home directory |
|---|---|---|
| Instruction files | `AGENTS.md`, `CLAUDE.md` | |
| MCP server definitions | `.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json` | `.claude.json`, `.cursor/mcp.json`, `.codex/config.toml`, and `Claude/claude_desktop_config.json` in the configuration folder |
| Agent Skills folders | `.agents/skills/`, `.claude/skills/` | `.agents/skills/`, `.claude/skills/` |

The other tools' locations were written down on 2026-11-12 and go stale. The steps that read the files confirm them against each tool's current documentation.

## First run

Running `cortex` with no configuration starts a short guided set-up in the TUI. It can be skipped, and every step has a default.

1. **Provider.** Detect provider keys in the environment and offer them. Otherwise ask for a provider and a key, and store the key in the credential store.
2. **Import.** List what was found from other tools (instruction files, MCP servers, skills) and ask what to bring in.
3. **Project.** Ask whether to trust the project, showing what trust enables. Offer to detect the build, test and check commands, and show them for confirmation before they enter the profile.
4. **Preferences.** Permission mode, with `supervised` preselected and explained in two lines.

Headless mode never runs this. It reports what is missing and how to provide it.

## Provenance

Every effective value remembers its layer, its file and its line.

```
$ cortex config show --origin
model.effort = "high"                       # profile review (user: ~/.config/cortex-harness/config.toml:32)
model.fast = "claude-haiku-5-5"             # user: ~/.config/cortex-harness/config.toml:6
model.main = "claude-sonnet-5-5"            # project: ~/work/app/.cortex/config.toml:2
model.speed_wait = "5s"                     # command-line: -c model.speed_wait
permissions.rules = [
    { action = "deny", effect = "write", match = "/etc/**" },  # managed: /etc/cortex-harness/config.toml:9
    { action = "allow", effect = "exec", match = "cargo test *" },  # user: ~/.config/cortex-harness/config.toml:23
]
retry.backoff_cap = "30s"                   # default
sandbox.enabled = true                      # managed: /etc/cortex-harness/config.toml:6, locked
```

- `cortex config show` prints the effective configuration. `--origin` adds the source of each value. `--layer <name>` prints one layer as it was read, before locks and the trust rules: `default`, `managed`, `user`, `project`, `project-local`, `profile`, `environment` or `command-line`.
- Each entry of a joined list has an origin of its own. That is how the policy engine knows which layer wrote a rule: managed rules cannot be weakened by user rules, nor user rules by project rules.
- `cortex config check` validates all layers and exits non-zero on any problem.

## Validation

- Unknown keys are warnings with a nearest-match suggestion. `--strict-config` (and `cortex config check`) makes them errors.
- **In `[permissions]`, `[sandbox]` and `[limits]` an unknown key or value is always an error, and the whole file is refused** ([decision 0028](../decisions/0028-enforced-values-fail-closed.md)). These are the values the harness enforces, and a configuration file is read by more than one build. A build that skipped a restriction it did not understand would run with a wider configuration than the file asks for. So nothing is skipped: the load fails, and a harness that is running keeps the configuration it had. A permission rule is read by the protocol's own strict reader, and a mode must be a level this build lists.
- Type and range errors name the file, line and key.
- A value that depends on the catalog (a model name, a reasoning level, a speed tier) is checked for shape here. `cortex-config` has no catalog; the crate that has one checks the value against it and the user's overlays.
- A JSON Schema for the configuration file is generated from the Rust types and published with each release, so editors can complete and check the file. It is checked in at [configuration/schema.json](../configuration/schema.json), beside the [reference](../configuration/reference.md) generated from the same declarations, and a test fails when either is stale.
- A file with a newer `config_version` than the build understands is refused with a clear message, before any of its keys is judged. An older one is migrated in memory, with a note. A new enforced section is added only with a new version, because an older build only warns about a top-level table it does not know.
- A file that exists and cannot be read, is not UTF-8, is not TOML or is larger than 1 MiB is an error. It is never a layer to skip: skipping a managed file would drop its locks.

**One declaration per key.** A key is declared once, as a field of a settings struct in `cortex-config`: its type, its default, what a project's file may do with it, and when a change takes effect. Validation, the trust rules, the reload notices, the JSON Schema and the reference are all read from that declaration, so none of them can describe a different configuration from the one the code reads.

**Reserved sections.** A section that belongs to a part of the harness that is not built yet is reserved: a file may hold it, what is in it is kept and not checked, and the step that builds the part declares its keys. Until then a reserved section fails closed for projects: it is the user's alone, or (hooks, extensions, MCP servers, verification commands) waits for trust. It is never treated as a harmless preference by default.

## Trust and what a project may set

A repository must not be able to take over the harness by containing a configuration file.

| Class of key | Examples | Project layer |
|---|---|---|
| Harmless preferences | default model, reasoning level, the `standard` speed tier, retry settings, a `deny` or `ask` permission rule, a protected path | Applied always. |
| Can run code | hooks, extensions, MCP servers, verification commands, custom commands with shell steps | Applied only after the user trusts the project, and pinned by hash ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). |
| Can widen access | permission `allow` rules, sandbox `write` paths, network on, a permission mode above the one in effect (up to `supervised`) | Applied only after trust. Shown to the user at that time. |
| Can raise cost | a speed tier other than `standard`, for the session or for a role | Applied only after trust. Shown to the user at that time. |
| User-only | credentials, everything about a provider (base URL, headers, protocol, key variable), permission modes above `supervised`, sandbox off, a higher limit, the selected profile, usage sources, the data directory, proxy and CA settings, the log level, the compatibility readers, anything under `[managed]` | Ignored in project layers, with a warning. Trust does not change this. |

- **Some keys are classed by their value.** A permission rule that denies or asks is a preference, and one that allows widens access. `sandbox.network = "off"` is a preference and `"full"` widens access. A speed of `standard` is a preference, and any other tier raises cost: a fast tier costs about twice as much per token, and an untrusted repository must not be able to raise what its user spends (the owner's decision, 2026-11-13). A project can always bring the speed down. A permission mode at or below the one already in effect is a preference; above it, up to `supervised`, it widens access; above `supervised` it is the user's alone.
- **A limit can be tightened by anyone and loosened by the user alone.** A project's limit applies when it is lower than the one in effect, or when there was none. A higher one is ignored, trusted or not: a budget is the user's.
- **Joining a list is not a way in.** The entries a project adds to a joined list are judged one by one: its `allow` rules and its writable paths wait for trust, and the user's entries stay.
- **What is held back is listed.** A load returns every project setting that is waiting for trust, with its class, its value and the line it is on, so a front end can show exactly what trusting the project would turn on.
- **Trust is an input.** `cortex-config` is told whether the project is trusted. The trust store, and the hashes that pin what was trusted, are `cortex-policy`'s.
- **The rules apply to both project files and to a profile a project defines.** The user's own layers, the environment and the command line are not subject to them.

The class of every key is part of the schema. A test snapshots the whole table, so a new key cannot arrive without a reviewer reading its class, and another checks the classes this page names.

## Sections

The schema, in outline. The full list, generated from the declarations, is the [configuration reference](../configuration/reference.md).

| Section | Holds | State |
|---|---|---|
| top level | `config_version`, `profile` | Declared. |
| `[profiles.<name>]` | The profiles. | Declared. |
| `[managed]` | `locked`: the keys no higher layer may change. | Declared. |
| `[model]` | Models by role (`main`, `fast`, `judge`), reasoning level (`effort`) and visibility (`reasoning`), maximum output, and the speed tier: `speed` for the session, which the `main` role runs at; `roles.<role>.speed` for a role of its own (the utility roles are at `standard` unless set); `speed_fallback`, `speed_wait` and `speed_cooldown` for what happens when a tier is refused ([04-providers.md](04-providers.md), "Speed tiers"; [decision 0027](../decisions/0027-speed-tiers.md)). `roles.<role>` also holds a role's own `effort` and `provider`. See [14-adaptation.md](14-adaptation.md). | Declared. |
| `[providers.<name>]` | Protocol, base URL, the key's environment variable (`key_env`), extra headers, the two stream timeouts, capability flags. Built-in providers need no entry. | Declared, except the flags. |
| `[models.<name>]` | Catalog overrides and additions. Each table is one catalog entry, with exactly the keys of a `[[model]]` table in `catalog/models.toml`, whose header documents them: `provider`, `id`, `name`, `protocol`, the limits, `prices` in US dollars per million tokens, `reasoning`, `capabilities`, `variants`, `flags`, and a `speed` list with each tier's own rate card ([04-providers.md](04-providers.md), "Model catalog"). The table's name is the user's own label for it. An entry **replaces** the built-in entry for the same provider and model id whole, and nothing is merged field by field, so an entry says everything about its model. An unknown key is an error, as it is in the built-in file: a misspelt price must not become "no price". The catalog file's own form, a `version` and a list of `[[model]]` tables, cannot be used here, because `[model]` is already a section of this file; the loader gives the user's entries a version of their own, so that a usage record shows that a price came from them. | Reserved. |
| `[http]` | The HTTP client: connect timeout, proxy, extra certificate authorities. | Reserved. |
| `[retry]` | `attempts`, `backoff_base`, `backoff_factor`, `backoff_cap`, `max_wait`, and the stream timeouts `idle_timeout` and `first_event_timeout`, which a provider's entry can override for that provider. | Declared. |
| `[permissions]` | `mode` and `rules`. Enforced. | Declared. |
| `[sandbox]` | `enabled`, the extra writable paths (`write`), the protected paths (`protect`), `network`. Enforced. | Declared. |
| `[limits]` | `requests`, `tokens`, `cost` (US dollars) and `time`, under `turn`, `session` and `day`. Enforced. | Declared. |
| `[credentials]` | `store`: where stored keys are kept. Never a key. | Declared. |
| `[session]` | Data directory (`data_dir`), `durability` (`item`, `turn` or `never`: when the session log is flushed; user only, because `never` could cost the user their sessions), retention. | `data_dir` and `durability` declared. |
| `[compat]` | Which of the other tools' files to read: `instructions`, `mcp`, `skills`. | Declared. |
| `[log]` | Level, targets, rotation. | `level` declared. |
| `[tools]` | Enabled tools, output limits, per-tool settings. | Reserved. |
| `[shell]` | Program and arguments, default and maximum timeout, environment variables to remove. | Reserved. |
| `[context]` | Compaction trigger and settings, instruction file names and size cap. | Reserved. |
| `[verify]` | Verification mode, commands, time limits, rounds. See [15-efficiency.md](15-efficiency.md). | Reserved. |
| `[adapt]` | Variant overrides per model, whether to explore, the map and profile token budgets. | Reserved. |
| `[stats]` | Recording on or off, retention. | Reserved. |
| `[index]` | Repository map caps. | Reserved. |
| `[runs]` | Run engine: concurrency, workspace location, set-up command, budgets (after the first release). | Reserved. |
| `[usage]` | Week start, calendar or rolling ranges, and, after the first release, which other tools' usage records may be read ([18-usage.md](18-usage.md)). | Reserved. |
| `[tui]` | Presenter (inline or full screen), theme, density, markdown and diff options. | Reserved. |
| `[keys.<mode>]` | Key bindings ([09-tui.md](09-tui.md)). | Reserved. |
| `[terminal]` | Capability overrides, each defaulting to automatic detection. All terminal quirks live here. | Reserved. |
| `[rpc]` | Socket path, token, client timeouts. | Reserved. |
| `[hooks.<name>]`, `[extensions.<name>]`, `[skills]` | [12-extensions.md](12-extensions.md) | Reserved. |
| `[mcp.servers.<name>]`, `[mcp]` | [13-mcp.md](13-mcp.md) | Reserved. |

**Values from elsewhere.** A value can be written as `{ env = "NAME" }` to read an environment variable when it is used, where the reference says a key takes "text, or `{ env = "NAME" }`": today a provider's extra headers. A value read this way is a `Secret`. There is no general interpolation syntax and no "run this command to get a value" form in the first release: a configuration file that runs commands is a larger attack surface than the feature is worth.

## Credentials

- **Never in `config.toml`.** The schema has no key that takes a secret as a literal. A provider entry names where its key comes from: an environment variable (`key_env`) or the credential store.
- **Lookup order:** command-line flag, then the provider's environment variable, then the credential store. A flag or a variable that is empty counts as not given. The store is not asked when an earlier source answers, so a broken store does not get in the way of a key from the environment.
- **The store.**

| OS | Default | Alternative |
|---|---|---|
| Windows | Windows Credential Manager | A file. |
| Linux | A file, `credentials.toml` in the configuration directory, with owner-only permissions | The Secret Service keychain, with the `keychain` build feature and `credentials.store = "keychain"`. **Not built yet:** the value gives an error that says so and names the file store. |

On Linux the keychain is not the default because it depends on a desktop session service that is absent on servers, in containers and over SSH, and because the client library for it is a large dependency. See [decision 0018](../decisions/0018-credential-storage.md).

- **`credentials.store`** is `auto` (the OS default), `file` or `keychain`. It is the user's alone.
- **Commands.** `cortex auth login <provider>` reads a key without echo and stores it. `cortex auth status` shows which source is active for each provider, never the value. `cortex auth logout <provider>` removes it.
- When both an environment variable and a stored key exist, the environment variable is used and `auth status` says so.
- **In memory** a credential is a `Secret`: not printable, not serialisable, never logged. No error from a store carries one, and an error about a malformed credentials file names a line number and never the line.

**The file store.**

- `credentials.toml` is written owner-only on every write: mode `0600` on Linux, and on Windows an access list with the user alone ([decision 0029](../decisions/0029-owner-only-files-on-windows.md)). The file is private from the moment its temporary file is created.
- A file someone made by hand with wider permissions is still read, and reported as not private: the status of a provider's credential carries the warning, for `cortex auth status` and the diagnostics to show. The next write makes it private. Its directory, if it was already there, is left as it is. Refusing to read it would stop people working on single-user machines for little gain.
- Access to the credentials file is guarded by a lock file, so two processes cannot interleave writes. The lock is the operating system's, held on an open file across the read, the change and the write: it goes when its holder exits, however it exits, so there is no stale lock to clear. A reader needs no lock, because the file is replaced in one step.
- A file with a newer format version, or one that is not TOML, is an error and is left untouched. Entries this version does not know are kept when the file is rewritten.

**Windows Credential Manager.** One generic credential per provider, named `cortex-harness:<provider>`, visible and removable in the Windows "Credential Manager" control panel. It is stored for this machine only, not copied to the user's other domain machines.

**Where `cortex-config` ends.** The `AuthMethod` trait, which puts a credential on a request ([04-providers.md](04-providers.md)), is in `cortex-llm`, and `cortex-config` may not depend on that crate. `cortex-config` owns the stores and the lookup order, and returns the key with a description of where it came from. `cortex-core` hands both to the provider layer's API-key `AuthMethod`. A sign-in flow added later stores its tokens through the same store.

## Other state

Separate from configuration, in the state directory (`$XDG_STATE_HOME/cortex-harness`, `%LOCALAPPDATA%\cortex-harness\state`):

| File | Holds |
|---|---|
| `trust.toml` | Trusted projects and the hashes of what was trusted. |
| `projects/<project key>/knowledge.toml` | What the harness has learned about a project ([14-adaptation.md](14-adaptation.md)). |
| `projects/<project key>/index/` | The repository-map cache ([15-efficiency.md](15-efficiency.md)). |
| `stats/` | Local statistics ([14-adaptation.md](14-adaptation.md)). |
| `approvals/<project key>.toml` | "Always allow in this project" rules. |
| `catalog-overlay.toml` | Results of `cortex models refresh`, in the catalog file's own form: a `version` and `[[model]]` tables. |
| `history` | Prompt history for the editor, per project. |
| `logs/` | Log files and crash reports. |

Nothing in the state directory is needed to run. Deleting it loses convenience, not data.

- **`cortex-config` says where each file is.** What is in a file is its owner's business: the trust store is `cortex-policy`'s, the knowledge file `cortex-knowledge`'s, and so on.
- **A project key is one directory name.** A key that could be a path (a separator, a dot, a drive letter) has no place in the state directory.
- **The state directory is created for the user alone.** One that already exists is not changed: if it is open to other users, that is reported as a warning and left, because its permissions may be the user's choice. So a private file in it is written private itself, and never relies on the directory.

## Windows notes

- Paths in configuration accept `/` or `\`. A leading `~` expands to the user's profile directory.
- Files are read as UTF-8, with or without a byte-order mark, and with either line ending. A line number in an error is the same either way.
- Owner-only permissions are applied through access-control lists ([decision 0029](../decisions/0029-owner-only-files-on-windows.md)).
- The names of environment variables ignore case, as they do on Windows.
- The managed file's folder is read from `%ProgramData%`. A user who changes that variable can make the harness miss the managed file: locks are an administrator's policy for a shared machine, not a boundary against the person running the program.
