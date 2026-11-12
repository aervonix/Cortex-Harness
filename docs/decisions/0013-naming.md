# 0013: Names

- **Status:** Provisional. The command name and directory names are the owner's call.
- **Date:** 2026-11-12

## Context

The brand is fixed: Cortex-Harness. Crates are to use the `cortex-` prefix unless there is a reason to change it. The public repo's name is settled in step 2.

## Decision

| Thing | Name |
|---|---|
| Project, in prose | Cortex-Harness |
| Crates | `cortex-<part>`, all with `publish = false` |
| Command | `cortex` |
| Configuration, data and state directories | `cortex-harness` under the platform's standard locations |
| Project directory | `.cortex/` |
| Environment variables | `CORTEX_*` |
| Instruction file | `AGENTS.md` |

## Why

- **The prefix stays.** No reason to change it was found. The bare crate name `cortex` and several `cortex-*` names are taken on crates.io, which would matter only if these crates were published. They are not: the project ships a binary, and library crates would carry a stability promise nobody has asked for.
- **A short command.** It is typed many times a day.
- **Longer directory names** avoid colliding with other software that uses a "cortex" directory, which the short command name cannot avoid on its own.

## Revisit when

- The owner prefers another command name: cheap before step 2, costly after the first release.
- Crates are to be published: check names then, and expect to move to a longer prefix.
