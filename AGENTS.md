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
3. **No personal details.** The owner's name and email never appear in commits, trailers, docs, code, configuration or fixtures. No absolute home-directory paths: write `~` or a repo-relative path. Commits use the repository's own git identity, never a personal one. CI checks every pull request from this repository, and every push to `main`, for the owner's details, against a secret only the owner holds (`cargo xtask owner-guard`).
4. **Everything fresh.** Nothing is carried over from earlier attempts.
5. **The brand is Cortex-Harness.** Crates use the `cortex-` prefix. The command is `cortex`, with the alias `cx`.
6. **The docs are the specification.** If the code must differ from a doc, change the doc in the same change. If a decision changes, add or amend a decision record.
7. **No new dependency without a row** in [docs/dependencies.md](docs/dependencies.md) saying why, what else was considered and what it costs.
8. **Linux and Windows together.** A step is not done until its tests pass on both in CI.
9. **No paid API in CI.** Tests use the scripted provider and recorded fixtures.
10. **Stay inside the step.** A step's "Must not change" list is binding. A needed change outside the step is reported, not made.
11. **Conflicts are raised.** Where a feature cannot meet a budget, say so in the step report with a recommendation. Do not cut the feature or loosen the budget quietly.
12. **No real waiting in tests.** Timeouts, retries, backoff and cooldowns are tested with paused or simulated time (the async runtime's test clock, or the fake clock), never with real sleeps.
13. **Budgets are enforced.** Each test has a time limit for its tier: slow at 2 s and stopped at 10 s in tiers 1 and 2, slow at 10 s and stopped at 60 s in tier 3. CI warns when a tier's total test time, counted above the start of the tests' processes, goes over its budget (20 s, 40 s, and 60 s on Linux or 90 s on Windows) and fails at one and a half times the budget.
14. **A step's records go in its own files.** The owner's answers go in `docs/steps/<NN>/answered.md` and the CI times in `docs/steps/<NN>/ci.md`, where `<NN>` is the step number in two digits with its letter if it has one (`07`, `12`, `13a`). An item left open is a file of its own in `docs/outstanding/`, named for the item in lowercase words joined by hyphens; settling it deletes the file. An item several steps share is left alone by each step that does its part, which records that in its own `answered.md`; only the step that finishes the item deletes its file. A step adds no section to a shared page for these. The details are in [docs/steps/README.md](docs/steps/README.md).

**Done means:** the step's done criteria are met; formatting, lints and all blocking tests pass on both OSes; the blocking checks are still within budget; public items are documented; `docs/` matches the code; and the step's report lists what was built, what was measured and anything left open.

## Windows facts every session needs

Each cost a session hours to find. The details are in [docs/steps/11/](docs/steps/11/).

- **The system temporary directory is on the hosted runner's slow disk** (`C:`). The Windows test job points `TEMP` and `TMP` at the workspace's disk, which halved `cortex-core`'s tests. A test that needs something only `C:` has (short "8.3" names) must look there itself.
- **Connecting to a loopback port that nothing listens on takes about 2 s** on Windows, which tries three times before it gives up, where Linux refuses at once. A test that connects to a port it may have closed spends that.
- **A Job Object goes on listing a process for a moment after the process has exited.** Ask for its members by process id and look at each one; never trust its count of active processes for "what is still running".
- **Printing can hide a race.** A line written to the terminal takes longer than the window some races have. When a timing bug will not show, keep the trace in memory and print it only when something has gone wrong. Repeat the whole workspace, not one crate: a race that needs other tests' load beside it never shows in a crate run alone.
- **Hosted Windows runners differ in speed by a factor of two** (a test's process starts in 6 to 12 ms). Compare two runs only on runners of the same speed, told by the start cost the tier report prints.

- **A terminal on Windows is a pseudo-console (ConPTY), and it re-renders.** What the program wrote is not what the test reads, ConPTY asks the terminal for modes of its own, it starts with virtual-terminal processing off, and a key takes about 16 ms to come through it. [docs/conpty.md](docs/conpty.md) has the facts step 20 learned one CI round trip at a time.

## Working here

| To | Run |
|---|---|
| Check formatting, repo rules, lints and licences | `cargo xtask check` |
| Run all tests | `cargo xtask test` |
| Run tiers 1 and 2 only | `cargo xtask test --profile fast` |
| Run one crate's tests, and build only it | `cargo xtask test -p <crate>` |
| Check that every link in the docs leads somewhere, and that the step records are in their places | `cargo xtask links` |
| Run exactly what CI runs | `cargo xtask ci` |
| See each tier's time by crate and its slowest tests, built as CI builds | `cargo xtask ci --tiers` |
| Compare a branch's tiers with `main`'s, by tier, crate and test | `cargo xtask ci --tiers --compare <run id or junit.xml> --against main` |
| Tell a change's tier cost from the runner's speed: both refs on one runner, in turn | `gh workflow run try --ref <branch> -f os=windows -f command=test -f against=main`, or `cargo xtask ab --base <other checkout>` |
| Check this branch on top of the pull requests that merge before it | `cargo xtask combine <pr>…` (`--repo URL --head BRANCH` through `rx`) |
| Get a run's job and tier tables for `ci.md` | `cargo xtask ci-times <run id> --against main` |
| Run tests many times over, each under the next seed | `cargo xtask stress -p <crate> --count 300`, and `--cores 2 --load 6` to make a race show |
| Break the code on purpose and see which tests fail | `cargo xtask mutate`, `--jobs 4` for four at a time; `mutate --check` checks every list without building anything |
| Edit Rust code by its tokens, so the match survives `rustfmt` | `cargo xtask replace <file> --from '<code>' --to '<code>'` |
| Ask the permission engine about a command | `cargo xtask decide [--rule '…'] -- <command>` |
| Format | `cargo xtask fmt` |
| Regenerate the protocol schema and TypeScript declarations | `cargo xtask schema` |
| Regenerate every generated file, the configuration schema and reference included | `cargo xtask gen` |
| Build `cortex` and its alias `cx` | `cargo xtask alias` |
| Fuzz the stream decoders (needs a nightly toolchain and cargo-fuzz) | `cargo xtask fuzz` |
| Run the live smoke test, or record provider fixtures: by hand, never in CI, and it costs money | `cargo xtask live [PROVIDER]`, `cargo xtask record [--provider PROVIDER]`, `cargo xtask record --toolbox`; `record --dry-run` sends nothing, `live --spent` totals the spend |

`./x <command>` is the same as `cargo xtask <command>`.

**At the owner's desk, heavy commands run on the desktop through `rx`,** the owner's script, which copies the worktree there without `.git` and runs the command under a test lock shared with the other sessions: `rx check`, `rx test`, `rx -- <command>`. `rx busy` shows what holds the lock and what is queued for it; `rx --time …` also prints how long the run waited for the lock and how long it ran; `rx --exclusive …` is for performance numbers: it refuses while the desktop is throttled, and waits until the machine is idle before it runs. Windows-only code is linted there with `rx -- cargo xwin clippy --target x86_64-pc-windows-msvc --workspace --all-targets --locked -- -D warnings`, which checks and lints and runs nothing.

- **Run `cargo xtask ci` before you call a change done**, and read its output: every step runs even when an earlier one fails, so one run shows everything that is wrong.
- **Windows is checked only by CI** unless you have a Windows machine. Write for both from the start: paths, line endings, process handling, file names that differ only by case. To run an `xtask` command on Windows, Linux or both before there is a pull request, push the branch and start the `try` workflow: `gh workflow run try --ref <branch> -f os=windows -f command="test -p <crate> <filter>"`, or `-f command=startup -f profile=dist` to measure a release build's start-up; `-f filter="EXPR; EXPR"` takes nextest filter expressions, `-f against=main` compares the branch with another ref on one runner, and `-f os=linux-22.04` runs on the one runner with Landlock ABI 4 ([docs/testing-and-ci.md](docs/testing-and-ci.md), "A run on the runners without a pull request").
- **When a test runs out of time,** read its output: a test that uses the test kit's gates, scripted server or children says what each thread was waiting for, and its process tree, before nextest stops it. `CORTEX_TESTKIT_PAUSE_MS=<n>` widens the scripted server's races on purpose ([docs/testing-and-ci.md](docs/testing-and-ci.md), "When a test runs out of time").
- **Stress before you report.** Run `cargo xtask stress -p cortex-agent --count 300` after any change to the loop or its tests, and the same for every crate whose tests start processes, use sockets or cross threads. One green run hides a test that depends on the order things happened to run in. A failure prints its seed, and `CORTEX_TEST_SEED=<seed>` runs it again in the same order.
- **Then show that the tests bite.** A step's rules are broken on purpose, one at a time, from a list in `mutations/step-NN.breaks`, with `cargo xtask mutate step-NN`: run it until it exits 0 or 1, and put its verdicts in the step's report. `cargo xtask check` fails when a break no longer finds its code: update the break to the code as it is now, or delete it if the rule is gone. The format is in [docs/testing-and-ci.md](docs/testing-and-ci.md), "Breaking the code on purpose".
- **Before a push, look at the tier times in CI's own build:** `cargo xtask ci --tiers`. A desk's build optimises third-party crates and CI's does not.
- **In a Claude Code worktree session, the shell guard refuses some commands that name `git`:** a heredoc written `cat <<'EOF' > file` whose text says `.git`, `$?`, `$(git …)`, a loop around git, `--exec`. A refused call in a batch does not stop the calls after it. [docs/shell-guard.md](docs/shell-guard.md) has the forms that are accepted for editing, committing, rebasing and pushing.
- **Which crate may depend on which** is fixed by [docs/architecture/01-workspace.md](docs/architecture/01-workspace.md) and checked by `cargo xtask rules`. Do not add an edge to make something compile. If the design is wrong, say so and change the page.
- **Tests:** unit tests in `#[cfg(test)]` modules; one integration binary per crate at `tests/it/main.rs`; tests that start a process are named `sys_…`. No sleeping, no network, no doc tests.
- **A pull request that touches only `docs/**` and `*.md`** skips the Rust build and tests in CI and runs only the repo rules. Anything else runs everything, and so does a change to the generated files in `docs/protocol/` or `docs/configuration/`.
- **Configuration keys are declared once,** in `crates/cortex-config/src/schema/sections.rs`: the type, the default, what a project's file may do with the key, and when a change takes effect. After adding or changing one, run `cargo xtask gen` and commit the regenerated files in `docs/configuration/` with it. A section in `[permissions]`, `[sandbox]` or `[limits]` is enforced: there an unknown key is an error, and must stay one.
- **The protocol is a contract.** After changing a type in `cortex-protocol`, run `cargo xtask schema` and commit the regenerated files with it. The versioning rules in [docs/architecture/11-rpc-protocol.md](docs/architecture/11-rpc-protocol.md) say what is an addition and what needs a new protocol version. The golden transcript is not edited to make a test pass. Everything tolerates what it does not know **except the values the harness enforces** (leases, grants, rules, limits): those refuse an unknown field or value, and must stay that way.
- **A provider protocol is tested from files.** Its fixtures are in `crates/cortex-llm/fixtures/<protocol>/`: recorded from the real API by `cargo xtask record`, or written by hand from the documented format where a case cannot be provoked. A fixture is not edited to make a test pass. The recorder reads each provider's API key from a file named in `CORTEX_LIVE_<PROVIDER>_KEY_FILE` (`ANTHROPIC`, `OPENROUTER`, `DEEPSEEK`, `VENICE`); do not read, print or copy such a file yourself, and never put a key on a command line.
- **Lints** are errors in CI. An exception is an `#[expect(…, reason = "…")]` or `#[allow(…, reason = "…")]` with a real reason, at the smallest scope that works.
- **Measure, then write the number down.** Budgets are in [docs/performance-budgets.md](docs/performance-budgets.md). A change that affects one reports its measurement.
- **Commit messages** say what changed and why, in the imperative. No trailers that name a person.
