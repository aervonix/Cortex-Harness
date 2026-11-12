# 0013: Names: the command `cortex`, its alias `cx`, and how the alias ships

- **Status:** Accepted. The owner chose `cortex` with the short alias `cx` on 2026-11-12, and asked for the way the alias ships to be decided here. The check of both names for clashes is part of step 2 and its result is reported there.
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

## Why

- **The prefix stays.** No reason to change it was found. The bare crate name `cortex` and several `cortex-*` names are taken on crates.io, which would matter only if these crates were published, and they are not.
- **A link costs nothing at start-up,** on either OS, and nothing on disk.
- **Longer directory names** avoid colliding with other software that uses a "cortex" directory, which a short command name cannot avoid on its own.

## Revisit when

- Step 2's clash check finds a conflict the owner does not want to live with.
- Crates are to be published: check names then, and expect to move to a longer prefix.
