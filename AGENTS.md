# Instructions for agents working in this repository

Cortex-Harness is a terminal agent harness written from scratch in Rust, for Linux and Windows. It is built in ordered steps by sessions that have only this repository and their own prompt.

## Read first

1. [docs/README.md](docs/README.md): the index of the design docs. **The docs are the specification.**
2. [docs/vision.md](docs/vision.md): what the product is for.
3. [docs/build-plan.md](docs/build-plan.md): "The owner's decisions", "Rules for every step", and the section for your step. It names the architecture pages and decision records to read before writing code.
4. [CONTRIBUTING.md](CONTRIBUTING.md): the layout, the commands, the test tiers and the lint rules.

## Rules for every step

These are the rules of the build plan. They hold in every change.

1. **Original work.** Other harnesses may be studied to understand a problem. Nothing is copied from them: no code, structure, names, prompt text, UI or configuration format. No dependency that is a port or extract of their code.
2. **Speed and efficiency are priorities**, in the product and in the tooling. The blocking pull-request checks finish in under 5 minutes per OS on GitHub-hosted runners. Anything slower goes in a non-blocking tier.
3. **No personal details.** The owner's name and email never appear in commits, trailers, docs, code, configuration or fixtures. No absolute home-directory paths: write `~` or a repo-relative path. Commits use the repository's own git identity, never a personal one.
4. **Everything fresh.** Nothing is carried over from earlier attempts.
5. **The brand is Cortex-Harness.** Crates use the `cortex-` prefix. The command is `cortex`, with the alias `cx`.
6. **The docs are the specification.** If the code must differ from a doc, change the doc in the same change. If a decision changes, add or amend a decision record.
7. **No new dependency without a row** in [docs/dependencies.md](docs/dependencies.md) saying why, what else was considered and what it costs.
8. **Linux and Windows together.** A step is not done until its tests pass on both in CI.
9. **No paid API in CI.** Tests use the scripted provider and recorded fixtures.
10. **Stay inside the step.** A step's "Must not change" list is binding. A needed change outside the step is reported, not made.
11. **Conflicts are raised.** Where a feature cannot meet a budget, say so in the step report with a recommendation. Do not cut the feature or loosen the budget quietly.
12. **No real waiting in tests.** Timeouts, retries, backoff and cooldowns are tested with paused or simulated time (the async runtime's test clock, or the fake clock), never with real sleeps.
13. **Budgets are enforced.** Each test has a time limit for its tier: slow at 2 s and stopped at 10 s in tiers 1 and 2, slow at 10 s and stopped at 60 s in tier 3. CI warns when a tier's total test time goes over its budget (20 s, 40 s, and 60 s on Linux or 90 s on Windows) and fails at one and a half times the budget.

**Done means:** the step's done criteria are met; formatting, lints and all blocking tests pass on both OSes; the blocking checks are still within budget; public items are documented; `docs/` matches the code; and the step's report lists what was built, what was measured and anything left open.

## Working here

| To | Run |
|---|---|
| Check formatting, repo rules, lints and licences | `cargo xtask check` |
| Run all tests | `cargo xtask test` |
| Run tiers 1 and 2 only | `cargo xtask test --profile fast` |
| Run exactly what CI runs | `cargo xtask ci` |
| Format | `cargo xtask fmt` |
| Regenerate the protocol schema and TypeScript declarations | `cargo xtask schema` |
| Regenerate every generated file, the configuration schema and reference included | `cargo xtask gen` |
| Build `cortex` and its alias `cx` | `cargo xtask alias` |

`./x <command>` is the same as `cargo xtask <command>`.

- **Run `cargo xtask ci` before you call a change done**, and read its output: every step runs even when an earlier one fails, so one run shows everything that is wrong.
- **Windows is checked only by CI** unless you have a Windows machine. Write for both from the start: paths, line endings, process handling, file names that differ only by case.
- **Which crate may depend on which** is fixed by [docs/architecture/01-workspace.md](docs/architecture/01-workspace.md) and checked by `cargo xtask rules`. Do not add an edge to make something compile. If the design is wrong, say so and change the page.
- **Tests:** unit tests in `#[cfg(test)]` modules; one integration binary per crate at `tests/it/main.rs`; tests that start a process are named `sys_…`. No sleeping, no network, no doc tests.
- **A pull request that touches only `docs/**` and `*.md`** skips the Rust build and tests in CI and runs only the repo rules. Anything else runs everything, and so does a change to the generated files in `docs/protocol/` or `docs/configuration/`.
- **Configuration keys are declared once,** in `crates/cortex-config/src/schema/sections.rs`: the type, the default, what a project's file may do with the key, and when a change takes effect. After adding or changing one, run `cargo xtask gen` and commit the regenerated files in `docs/configuration/` with it. A section in `[permissions]`, `[sandbox]` or `[limits]` is enforced: there an unknown key is an error, and must stay one.
- **The protocol is a contract.** After changing a type in `cortex-protocol`, run `cargo xtask schema` and commit the regenerated files with it. The versioning rules in [docs/architecture/11-rpc-protocol.md](docs/architecture/11-rpc-protocol.md) say what is an addition and what needs a new protocol version. The golden transcript is not edited to make a test pass. Everything tolerates what it does not know **except the values the harness enforces** (leases, grants, rules, limits): those refuse an unknown field or value, and must stay that way.
- **Lints** are errors in CI. An exception is an `#[expect(…, reason = "…")]` or `#[allow(…, reason = "…")]` with a real reason, at the smallest scope that works.
- **Measure, then write the number down.** Budgets are in [docs/performance-budgets.md](docs/performance-budgets.md). A change that affects one reports its measurement.
- **Commit messages** say what changed and why, in the imperative. No trailers that name a person.
