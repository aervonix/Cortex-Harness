# Configuration

`cortex-config` loads settings from several layers, merges them, records where each value came from, and validates the result. It also owns credential storage.

The [vision](../vision.md) adds four requirements that this page covers: profiles, reloading without a restart, reading what other tools already wrote, and a first run that needs no manual set-up.

## Format

TOML. One schema, versioned with a top-level `config_version = 1`. The reasons for TOML over JSON are in [decision 0007](../decisions/0007-configuration-format.md).

## Layers

Lowest priority first. Later layers override earlier ones.

| # | Layer | Location | Notes |
|---|---|---|---|
| 1 | Built-in defaults | compiled in | `cortex config defaults` prints them. |
| 2 | Managed | `/etc/cortex-harness/config.toml`; `%ProgramData%\cortex-harness\config.toml` | Optional. For shared machines. Can lock keys (below). |
| 3 | User | `$XDG_CONFIG_HOME/cortex-harness/config.toml` (usually `~/.config/cortex-harness/`); `%APPDATA%\cortex-harness\config.toml` | The usual place. |
| 4 | Project | `<workspace>/.cortex/config.toml` | Committed with the repo. Applies only as far as trust allows (below). |
| 5 | Project, local | `<workspace>/.cortex/config.local.toml` | Not committed. The repo skeleton adds it to the ignore file template. |
| 6 | Profile | a `[profiles.<name>]` table from layers 2 to 5, selected by `--profile <name>` or `profile = "<name>"` | A named set of overrides. See "Profiles". |
| 7 | Environment | `CORTEX_*` variables | A short, documented list: log level, configuration path, data directory, colour, and the provider key variables. Not a general override mechanism. |
| 8 | Command line | dedicated flags, and `-c key=value` for any key | Highest priority. |

The workspace root is the nearest ancestor of the working directory that contains `.cortex/` or `.git`, or the working directory itself.

## Merge rules

- **Tables merge key by key**, recursively.
- **Scalars and arrays replace.** There is no syntax for appending to or removing from an inherited array. A layer that sets an array sets all of it.
- **Named collections are tables keyed by name**, so layers can add or override one entry without restating the rest: `[providers.<name>]`, `[models.<name>]`, `[mcp.servers.<name>]`, `[extensions.<name>]`, `[hooks.<name>]`, `[keys.<mode>]`. An entry is removed by setting `enabled = false`.
- **Permission rules are the exception.** `[[permissions.rules]]` arrays from all layers are concatenated, because their evaluation does not depend on order ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)).
- **Locks.** The managed layer can list keys under `[managed] locked = [...]`. A locked key ignores every higher layer, and `cortex config show` says so.

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

- Any key may appear in a profile, with the same trust rules as the layer the profile is defined in.
- `cortex --profile review` selects one for a run. `/profile <name>` switches in a session; settings that affect the prompt or the tool list take effect at the next context epoch, and the UI says so.
- "Modes" are the permission and behaviour presets in [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md). A profile can select a mode along with anything else.

## Reloading without a restart

Configuration, extensions, themes and key bindings reload while the harness runs.

**How changes are noticed.** There is no file watcher and no polling timer, which keeps the idle cost at zero. The harness checks the modification times of the files it loaded:

- when the terminal regains focus (terminals report this);
- on a key press, at most once a second;
- at the start of every turn;
- when the user runs `/reload`.

The check is a handful of `stat` calls, well under a millisecond. It compares each file's modification time and size with what they were when the file was loaded (and on Unix which file it is, so that a replaced file shows even when both are the same). Two details keep it from missing a change:

- **The stamp is taken before the file is read.** A change that lands while a file is being read and parsed is then seen by the next check.
- **A file written in the last two seconds is not trusted yet.** File systems record times in steps as coarse as two seconds, so a second write can leave the time as it was. Such a file is reported as changed until it has settled. The cost is a few reloads of a file that did not change again, so a reload that produces the same configuration is not announced.

**What happens to a change.**

| Changed | Takes effect |
|---|---|
| Theme, key bindings, UI settings | Immediately, on the next frame. |
| Permission rules, limits, retry and verification settings | Immediately, for the next tool call or request. |
| Model, reasoning level, anything that shapes the system prompt or the declared tool list | At the next context epoch. A notice says what is pending. `/reload --now` starts a new epoch at once. |
| An extension's script or manifest | The extension is reloaded. If its hash no longer matches what was approved, it is disabled and the user is asked to review the change ([12-extensions.md](12-extensions.md)). |
| An MCP server entry | That server is restarted. A changed command needs approval again. |
| An invalid file | The old configuration stays in force and the error is shown. A reload never leaves the harness without a valid configuration. |

## Reading other tools' set-up

People switching from another agent should not have to rewrite anything. The harness reads, and never modifies, these:

| What | Where it is looked for | How it is used |
|---|---|---|
| Instruction files | `AGENTS.md` and `CLAUDE.md`, as in [07-sessions-and-context.md](07-sessions-and-context.md) | As project instructions. |
| MCP server definitions | The project's `.mcp.json`, and the per-user files of common MCP clients | Offered at onboarding and by `cortex mcp import`. Project-level definitions appear as proposed servers that need the same trust and approval as any other. See [13-mcp.md](13-mcp.md). |
| Agent Skills folders | `.cortex/skills/`, `.agents/skills/` and `.claude/skills/` in the project and the user's home | Listed in the prompt by name and description, and read on demand. See [12-extensions.md](12-extensions.md). |

Everything else (settings, permissions, hooks, key bindings, themes) uses Cortex-Harness's own formats. Each compatibility reader can be turned off under `[compat]`.

## First run

Running `cortex` with no configuration starts a short guided set-up in the TUI. It can be skipped, and every step has a default.

1. **Provider.** Detect provider keys in the environment and offer them. Otherwise ask for a provider and a key, and store the key in the credential store.
2. **Import.** List what was found from other tools (instruction files, MCP servers, skills) and ask what to bring in.
3. **Project.** Ask whether to trust the project, showing what trust enables. Offer to detect the build, test and check commands, and show them for confirmation before they enter the profile.
4. **Preferences.** Permission mode, with `supervised` preselected and explained in two lines.

Headless mode never runs this. It reports what is missing and how to provide it.

## Provenance

Every effective value remembers its layer and file.

```
$ cortex config show --origin model.default
model.default = "claude-opus-5-5"        # user: ~/.config/cortex-harness/config.toml:3
```

- `cortex config show` prints the effective configuration. `--origin` adds the source of each value. `--layer <name>` prints one layer.
- `cortex config check` validates all layers and exits non-zero on any problem.

## Validation

- Unknown keys are warnings with a nearest-match suggestion. `--strict-config` (and `cortex config check`) makes them errors.
- Type and range errors name the file, line and key.
- A value that depends on the catalog (a model name, a reasoning level) is checked against the catalog and the user's overlays.
- A JSON Schema for the configuration file is generated from the Rust types and published with each release, so editors can complete and check the file.
- A file with a newer `config_version` than the build understands is refused with a clear message. An older one is migrated in memory, with a note.

## Trust and what a project may set

A repository must not be able to take over the harness by containing a configuration file.

| Class of key | Examples | Project layer |
|---|---|---|
| Harmless preferences | default model, reasoning level, compaction settings, instruction file names, UI hints | Applied always. |
| Can run code | hooks, extensions, MCP servers, custom commands with shell steps | Applied only after the user trusts the project, and pinned by hash ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). |
| Can widen access | permission `allow` rules, sandbox `write` paths, network on | Applied only after trust. Shown to the user at that time. |
| User-only | credentials, provider base URLs and headers, permission modes above `supervised`, sandbox off, usage sources, the data directory, proxy and CA settings, anything under `[managed]` | Ignored in project layers, with a warning. |

The list of user-only keys is part of the schema, and a test checks that every key is classified.

## Sections

The schema, in outline. The configuration build step writes the full reference.

| Section | Holds |
|---|---|
| top level | `config_version` |
| `profile`, `[profiles.<name>]` | The selected profile, and the profiles. |
| `[model]` | Models by role (`main`, `fast`, `judge`), reasoning level and visibility, maximum output, and the speed tier: one for the session, which the `main` role runs at, and where wanted one per role (the utility roles are at `standard` unless set), with whether to fall back to standard speed when a tier is refused ([04-providers.md](04-providers.md), "Speed tiers"). The key names are settled in the configuration step. See [14-adaptation.md](14-adaptation.md). |
| `[providers.<name>]` | Protocol, base URL, credential source, extra headers, timeouts, capability flags. Built-in providers need no entry. |
| `[models.<name>]` | Catalog overrides and additions: limits, prices, reasoning control, capability flags. |
| `[tools]` | Enabled tools, output limits, per-tool settings. |
| `[shell]` | Program and arguments, default and maximum timeout, environment variables to remove. |
| `[permissions]` | Mode and rules. |
| `[sandbox]` | Extra writable and protected paths, network setting. |
| `[context]` | Compaction trigger and settings, instruction file names and size cap. |
| `[session]` | Durability, data directory, retention. |
| `[retry]` | Attempts, backoff, maximum wait, stream timeouts. |
| `[limits]` | Requests, cost, tokens and time per turn, per session and per day. |
| `[verify]` | Verification mode, commands, time limits, rounds. See [15-efficiency.md](15-efficiency.md). |
| `[adapt]` | Variant overrides per model, whether to explore, the map and profile token budgets. |
| `[stats]` | Recording on or off, retention. |
| `[index]` | Repository map caps. |
| `[compat]` | Which of the other tools' files to read. |
| `[runs]` | Run engine: concurrency, workspace location, set-up command, budgets (after the first release). |
| `[usage]` | Week start, calendar or rolling ranges, and, after the first release, which other tools' usage records may be read ([18-usage.md](18-usage.md)). |
| `[tui]` | Presenter (inline or full screen), theme, density, markdown and diff options. |
| `[keys.<mode>]` | Key bindings ([09-tui.md](09-tui.md)). |
| `[terminal]` | Capability overrides, each defaulting to automatic detection. All terminal quirks live here. |
| `[rpc]` | Socket path, token, client timeouts. |
| `[hooks.<name>]`, `[extensions.<name>]`, `[skills]` | [12-extensions.md](12-extensions.md) |
| `[mcp.servers.<name>]`, `[mcp]` | [13-mcp.md](13-mcp.md) |
| `[log]` | Level, targets, rotation. |

**Values from elsewhere.** A string value can be written as `{ env = "NAME" }` to read an environment variable when it is used. There is no general interpolation syntax and no "run this command to get a value" form in the first release: a configuration file that runs commands is a larger attack surface than the feature is worth.

## Credentials

- **Never in `config.toml`.** The schema has no key that takes a secret as a literal. A provider entry names where its key comes from: an environment variable or the credential store.
- **Lookup order:** command-line flag, then the provider's environment variable, then the credential store.
- **The store.**

| OS | Default | Alternative |
|---|---|---|
| Windows | Windows Credential Manager | A file. |
| Linux | A file, `credentials.toml` in the configuration directory, with owner-only permissions | The Secret Service keychain, with the `keychain` build feature and `credentials.store = "keychain"`. |

On Linux the keychain is not the default because it depends on a desktop session service that is absent on servers, in containers and over SSH, and because the client library for it is a large dependency. See [decision 0018](../decisions/0018-credential-storage.md).

- **Commands.** `cortex auth login <provider>` reads a key without echo and stores it. `cortex auth status` shows which source is active for each provider, never the value. `cortex auth logout <provider>` removes it.
- When both an environment variable and a stored key exist, the environment variable is used and `auth status` says so.
- Access to the credentials file is guarded by a lock file, so two processes cannot interleave writes.

## Other state

Separate from configuration, in the state directory (`$XDG_STATE_HOME/cortex-harness`, `%LOCALAPPDATA%\cortex-harness\state`):

| File | Holds |
|---|---|
| `trust.toml` | Trusted projects and the hashes of what was trusted. |
| `projects/<project key>/knowledge.toml` | What the harness has learned about a project ([14-adaptation.md](14-adaptation.md)). |
| `projects/<project key>/index/` | The repository-map cache ([15-efficiency.md](15-efficiency.md)). |
| `stats/` | Local statistics ([14-adaptation.md](14-adaptation.md)). |
| `approvals/<project key>.toml` | "Always allow in this project" rules. |
| `catalog-overlay.toml` | Results of `cortex models refresh`. |
| `history` | Prompt history for the editor, per project. |
| `logs/` | Log files and crash reports. |

Nothing in the state directory is needed to run. Deleting it loses convenience, not data.

## Windows notes

- Paths in configuration accept `/` or `\`. A leading `~` expands to the user's profile directory.
- Files are read as UTF-8, with or without a byte-order mark, and with either line ending.
- Owner-only permissions are applied through access-control lists.
