# 0013: Names: the command `cortex`, its alias `cx`, and how the alias ships

- **Status:** Accepted. The owner chose `cortex` with the short alias `cx` on 2026-11-12, and asked for the way the alias ships to be decided here. Step 2 checked both names for clashes the same day and found one that matters, on `cortex`. The owner saw the result and kept both names. The findings and the decision are under "The clash check: result and decision".
- **Date:** 2026-11-12

## Context

The brand is fixed: Cortex-Harness. Crates use the `cortex-` prefix unless there is a reason to change it, and the owner confirmed they are not published. The command is typed many times a day, so the owner wants a two-letter alias that runs the same program on Linux and Windows.

## Names

| Thing | Name |
|---|---|
| Project, in prose | Cortex-Harness |
| Crates | `cortex-<part>`, all with `publish = false` |
| Command | `cortex` |
| Alias | `cx`: the same program under a second name |
| Configuration, data and state directories | `cortex-harness` under the platform's standard locations |
| Project directory | `.cortex/` |
| Environment variables | `CORTEX_*` |
| Instruction file | `AGENTS.md` |

## How the alias ships

The start-up budget on a desktop is 5 ms for `cortex --version` and 20 ms to the first frame ([performance-budgets.md](../performance-budgets.md)). The alias must not spend any of it.

| Option | Start-up cost | Size on disk | Linux | Windows | Assessment |
|---|---|---|---|---|---|
| **A second small entry binary** that finds `cortex` and runs it | One extra process start. On Linux it can replace itself, about 0.3 ms (the measured cost of starting an empty program). On Windows a process cannot replace itself, so the launcher must start a child, wait, and relay the console, signals and exit code: tens of milliseconds and a second process for the whole session. | Small | Works | Works, badly | The Windows cost lands on the exact budget the project cares most about, and relaying a console correctly is a known source of bugs. Rejected. |
| **Two full binaries** built from the same code | None | Double | Works | Works | Doubles the download and the disk use for a convenience. Rejected. |
| **A script shim** (`cx.cmd`, a shell alias) | An interpreter start on Windows, tens of milliseconds | None | Per-shell, not for programs that start `cx` directly | Breaks Ctrl+C handling and is not a real executable | Rejected. |
| **One file, two names:** `cx` is a hard link to `cortex`, made at install. A copy where a link cannot be made. | **None.** It is the same file, already in the file cache. | None for a link; double only in the copy fallback | Hard link or symbolic link | Hard link on NTFS, in the same directory | **Chosen.** |
| **The binary checks its own name** | None | None | | | Not needed for behaviour, because both names do the same thing. Used only so that help and error text print the name the user typed. |

## Decision

- **One executable, two names.** `cx` is a link to `cortex`. No launcher, no second build target, no shim.
- **The program behaves identically under both names.** It reads the name it was started with only to echo it in usage and help text. An unexpected name is treated as `cortex`.
- **Linux archives** contain `cortex` and a relative symbolic link `cx`.
- **Windows archives** contain `cortex.exe` and an install script that creates `cx.exe` as a hard link beside it, or as a copy if the file system cannot link.
- **`cortex alias install`** and **`cortex alias remove`** create or remove the alias beside the running executable, for people who unpack by hand, and after an update replaces the executable (replacing a file breaks a hard link to its old contents, so the updater refreshes the alias).
- **Package managers** that support a second command name for one executable declare both names there.
- **Never overwrite someone else's `cx`.** If another program named `cx` or `cortex` is already on the path, installation leaves it alone, says so, and tells the user how to choose. The alias is optional: `--no-alias`.
- **Tests:** the real binary is run under both names on both OSes and must give the same `--version`, the same behaviour, and help text in the name used. The start-up measurement is taken under both names.

## The clash check, for step 2

Step 2 checks both names and reports before anything is published. To check:

- Commands on a default path in current Ubuntu, Debian, Fedora, Arch and Windows.
- Packages or commands of either name in the common package managers for those systems, in the npm and Python package indexes (as installed commands), and among cargo-installed binaries.
- Widely used developer tools. From memory, and to be verified by that step: at least one security-scanning vendor ships a command-line tool named `cx`, and several unrelated projects ship a command named `cortex`.

If a clash is serious, the report goes to the owner with options: keep the names and rely on the "never overwrite" rule, ship without the alias by default, or choose another alias. Nothing else in the design depends on the two letters.

## The clash check: result and decision

Step 2 ran the check on 2026-11-12. Sources were the distributions' package-contents searches, the package managers' listings, the npm, PyPI and crates.io registries, and the tools' own documentation. The three findings that matter were confirmed against the vendors' own pages.

**Default installs.** Nothing on a default path in current Ubuntu, Debian or Arch uses either name, and no built-in Windows command does. No Fedora package was found; its file-level search gave no clean answer. winget and Scoop could not be searched directly.

**`cortex`.**

| Tool | What installs the command | How likely on a developer's path |
|---|---|---|
| Snowflake's Cortex Code CLI | A command named `cortex`, on Linux, macOS, WSL and native Windows. Its installer puts it in `~/.local/bin` (on Windows, a folder under the user's local application data) and adds that to the path. As read in Snowflake's documentation on 2026-11-12. | It needs a Snowflake account, so mostly data engineers. It is also a terminal coding agent: the same kind of product. |
| Menlo's cortex.cpp, a local model runtime | `cortex`, in `/usr/bin` from its Linux installer | Low. The project is archived. |
| Cortex, the Prometheus long-term store | A server binary named `cortex` (a Homebrew formula, and release packages) | Low. It runs in containers. |
| cortexlabs' model-serving tool | A command-line tool, not confirmed to be named `cortex` | Low. No longer maintained. |
| Two npm packages, one AUR package | `cortex` | Negligible: under 100 downloads a week. |

**`cx`.**

| Tool | What installs the command | How likely on a developer's path |
|---|---|---|
| The Checkmarx One CLI, a security scanner | `cx` (`cx.exe` on Windows), unpacked by hand from a release archive | Security teams and CI machines. |
| The Coralogix CLI | `cx`, from a Homebrew formula of that name, cargo, or an install script | Coralogix users. New in 2026. |
| The `cx-cli` crate, a code-navigation tool | `cx`, from `cargo install` | Small: a few hundred downloads. |

**Assessment.** `cx`: minor. `cortex`: one serious clash, Snowflake's, because the command is the same, the installer puts it on the path, and the product is of the same kind.

**The owner's decision, 2026-11-12: the names stay `cortex` and `cx`.** The owner's reasons: Snowflake has since renamed its tool (to "Snowflake CoCo", in June 2026, on the owner's information), and it is a lesser-known tool. What step 2 read on 2026-11-12 is recorded beside that: Snowflake's documentation called the product "CoCo CLI" and still gave `cortex` as the command to type. So a machine that has Snowflake's tool can still have a `cortex` on its path, and the rule that covers it is the one this record already has:

- **Never overwrite another program's command.** If a `cortex` or a `cx` that is not ours is already on the path, installation leaves it alone, says so, and tells the user how to choose. This rule is kept, and it is what the decision rests on. The install script and `cortex alias install` are tested for it.

The options the owner did not take: making `cortex-harness` the installed command with both short names as aliases, and shipping without the `cx` alias or under another one.

## Why

- **The prefix stays.** No reason to change it was found. The bare crate name `cortex` and several `cortex-*` names are taken on crates.io, which would matter only if these crates were published, and they are not.
- **A link costs nothing at start-up,** on either OS, and nothing on disk.
- **Longer directory names** avoid colliding with other software that uses a "cortex" directory, which a short command name cannot avoid on its own.

## Revisit when

- Users report that the clash with another `cortex` gets in their way: the option of `cortex-harness` as the installed command is still open.
- Crates are to be published: check names then, and expect to move to a longer prefix.
