# Contributing

How the repository is laid out, how to build and test it, and the rules the checks enforce. The design itself is in [docs/](docs/README.md): the docs are the specification, and code that must differ from a page changes the page in the same pull request.

## Set-up

You need [rustup](https://rustup.rs) and git. The toolchain is pinned in `rust-toolchain.toml` and rustup installs it on first use. Two more tools are used, both installed as prebuilt binaries in CI (their versions are in [docs/dependencies.md](docs/dependencies.md)):

| Tool | Needed for |
|---|---|
| [cargo-nextest](https://nexte.st) | Running the tests |
| [cargo-deny](https://embarkstudios.github.io/cargo-deny/) | The licence, ban and source checks in `cargo xtask check` |

## The commands

Everything goes through the `xtask` crate: `cargo xtask <command>`, or `./x <command>` from a shell. CI runs these same commands.

| Command | What it does |
|---|---|
| `cargo xtask check` | The static checks (tier 0): `fmt`, `rules`, `breaks`, `generated`, `clippy`, `deny`. `cargo xtask check clippy` runs one of them. Every step runs even if an earlier one fails, and the timings are printed at the end. |
| `cargo xtask test` | All tests. Arguments go to `cargo nextest run`: `cargo xtask test --profile fast` runs tiers 1 and 2 only, `cargo xtask test -p cortex-cli` one crate, and builds only that crate's tests. |
| `cargo xtask ci` | Exactly what the blocking CI jobs run: the static checks, then tiers 1 to 3. Run it before opening a pull request. It builds with cargo's `ci` profile, as CI does, so its first run builds the dependencies once more, unoptimised, into `target/ci`. |
| `cargo xtask ci --tiers` | Builds and runs the tests as CI does, and prints each tier's time against its budget and its slowest tests. `-p <crate>` for some crates, `--top N` for more tests, `--from junit.xml --os windows` to read the report a CI run kept. |
| `cargo xtask stress -p <crate>` | Runs the crate's tests many times over (`--count N`, 100 when not given), each time under the next seed, and reports each test that failed on some, with the seed that brings the failure back. |
| `cargo xtask mutate` | Breaks the code on purpose, one break of `mutations/*.breaks` at a time, on a copy of the tree, and reports which tests failed. It stops after five minutes and carries on from there the next time. `--check` only checks that every break still applies, which `check` also does. |
| `cargo xtask rules` | The repo-rule checks alone (see below). |
| `cargo xtask links` | The part of `rules` that is about the docs, by itself: every link inside the repository leads to a file and a heading that are there, and `docs/steps/` and `docs/outstanding/` hold what they should. It asks cargo nothing, so it answers at once. |
| `cargo xtask fmt` | Formats the workspace. |
| `cargo xtask schema` | Regenerates the protocol's JSON Schema and TypeScript declarations in `docs/protocol/` from the types in `cortex-protocol`. Run it after any change to those types and commit the result. `--check` only compares. |
| `cargo xtask gen` | Regenerates every generated file: what `schema` writes, and the configuration schema and reference in `docs/configuration/`, from the settings in `cortex-config`. Run it after adding or changing a configuration key. |
| `cargo xtask size`, `unused`, `advisories` | What the non-blocking workflows run: the `dist` build with its binary size and crate count, the unused-dependency check (needs [cargo-machete](https://github.com/bnjbvr/cargo-machete)), and the advisory check. |
| `cargo xtask fuzz` | Coverage-guided fuzzing of the stream decoders, each target for a fixed time (`--seconds N`, 60 when not given). What the nightly workflow's `fuzz` job runs. Needs a nightly toolchain and [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz). The targets are in `fuzz/`. |
| `cargo xtask live [PROVIDER]`, `cargo xtask record [--provider PROVIDER] [SCENARIO…]` | By hand only, against a real provider, and never in CI: the live smoke test, and the recorder of provider fixtures. The provider is `anthropic` when none is named, or `openrouter`, `deepseek` or `venice`. Both need `CORTEX_LIVE_<PROVIDER>_KEY_FILE` set to the path of a file that holds that provider's API key. The key is never given on a command line or in the environment, and a recording that still holds it is refused ([docs/testing-and-ci.md](docs/testing-and-ci.md), "Fixtures"). They cost money: a few hundredths of a cent for the smoke test, about a cent and a half to record everything. |
| `cargo xtask alias` | Builds `cortex` and puts its alias `cx` beside it in the target directory. |

## Layout

```
crates/cortex-*    the product, one crate per job (docs/architecture/01-workspace.md)
crates/cortex-cli  the `cortex` binary; `cx` is a link to it
catalog/           the model catalog, embedded in the binary by cortex-llm's build script
xtask/             developer commands; not shipped
fuzz/              fuzz targets for the stream decoders; a package of its own, outside the workspace
mutations/         the break lists `cargo xtask mutate` runs, one file per step
docs/              the specification
docs/protocol/     generated: the protocol's JSON Schema and TypeScript declarations
docs/configuration/  generated: the configuration file's JSON Schema and reference
assets/            the logo
AGENTS.md          the rules for agents; CLAUDE.md is one line that includes it
.config/nextest.toml, clippy.toml, deny.toml, rustfmt.toml, rust-toolchain.toml
.github/workflows/ci.yml             the blocking checks
.github/workflows/main.yml           after each merge: release build, size, crate count
.github/workflows/nightly.yml        nightly: advisories, unused dependencies, every release target
.github/workflows/windows-try.yml    by hand: one crate's tests on Windows, for a branch with no pull request
```

A crate may depend only on the workspace crates that [docs/architecture/01-workspace.md](docs/architecture/01-workspace.md) lists for it. `cargo xtask rules` checks this from `cargo metadata`. If the design has to change, change that page and the table in `xtask/src/rules/crate_graph.rs` in the same pull request.

Every crate's manifest follows the same shape: `version`, `edition`, `license` and `publish` come from the workspace, `[lints] workspace = true`, and `[lib] doctest = false`.

## Tests

The tiers are defined in [docs/testing-and-ci.md](docs/testing-and-ci.md). Where a test lives, and its name, decide its tier:

| Tier | Where | Notes |
|---|---|---|
| 1, unit | `#[cfg(test)]` modules inside the crate | Pure logic. No processes, no network, no real time. |
| 2, integration | `tests/it/main.rs`, one binary per crate, with modules inside it | Several crates together, in-process. |
| 3, system | `crates/cortex-cli/tests/system/`, and test-kit-driven binaries | The real binary as a process. **Test names start with `sys_`.** |

- **One integration-test binary per crate.** Every extra binary is another link step. Add a module to `tests/it/`, not a file to `tests/`.
- **No doc tests.** An example that must compile is an ordinary test.
- **No real waiting.** Timeouts, retries, backoff and cooldowns are tested with paused or simulated time: the async runtime's test clock, or the fake clock. A test never sleeps. A process test waits on a condition with a deadline, never on a fixed delay.
- **Property tests** are written with the small helper described in [docs/testing-and-ci.md](docs/testing-and-ci.md) ("Property tests"), not with a property-testing crate. Every run is the same, a failure prints the seed of the failing case, `CORTEX_PROPERTY_SEED=<seed>` runs that case alone, and `CORTEX_PROPERTY_SCALE=<n>` multiplies the number of cases.
- **A test of the agent loop sees a seeded order of events.** The rig puts the blocking pool's work on the test's thread, at moments a seed decides: 0 in an ordinary run, the next one for each repeat of `cargo xtask stress`. A test must pass under every seed, so it fixes an order it needs with a gate. A failed test prints its seed, and `CORTEX_TEST_SEED=<seed>` runs it again in the same order.
- **A test's directory** is `cortex_testkit::scratch!("name")`: new each time, removed when the test passes, with helpers for files and for a git repository that reads none of the machine's git configuration.
- **No retries**, with one exception: a test inside a module named `timing_sensitive` gets one retry, and nextest reports it as flaky when the retry was needed.
- **A test that starts a process** is a system test and is named `sys_…`, wherever it lives. That name puts it in nextest's `process` group (two at a time), gives it tier 3's time limit, and keeps it out of the `fast` profile.
- **A process test's children are the test binary itself,** run again (`cortex_testkit::children`): a child that stays until it is killed, a grandchild, a child that ignores the polite interrupt. Each reports to the test over a loopback socket and keeps the connection until it dies, so the test sees it start and sees it go. No helper program is built, and no test uses `sleep`.
- **Every test has a time limit.** In tiers 1 and 2 a test is reported as slow at 2 s and stopped, as a failure, at 10 s. In tier 3 the marks are 10 s and 60 s. A test that is slow needs a reason or belongs in a non-blocking tier.
- **Every tier has a budget** for the time its tests take together: 20 s for tier 1, 40 s for tier 2, and 60 s on Linux or 90 s on Windows for tier 3. `cargo xtask ci` prints the totals after the run. A tier over its budget is a warning on the pull request, and a tier more than one and a half times over fails the job.

## Lints

The lint levels are in the root `Cargo.toml` under `[workspace.lints]`, the lists they use are in `clippy.toml`, and CI turns every warning into an error. The project-specific rules:

- **No `unwrap` or `expect` outside tests**, unless an attribute gives the invariant:

  ```rust
  #[expect(clippy::unwrap_used, reason = "the pattern is a literal and is tested")]
  ```

- **No `println!` or `eprintln!`** outside `cortex-cli` and `xtask`, so nothing writes to the user's terminal by accident.
- **No blocking standard-library calls** (`std::fs`, `std::process::Command`, `std::thread::sleep`). The core runs on one async thread, and a blocking call there stalls every session. Code that is meant to block, because it runs on the blocking pool or the whole crate is synchronous, opts out for the module or the crate and says why:

  ```rust
  #![allow(clippy::disallowed_methods, clippy::disallowed_types, reason = "runs on the blocking pool")]
  ```

- **`unsafe` is denied**, and allowed per module only in `cortex-sandbox` (process trees are there for that reason, [decision 0030](docs/decisions/0030-process-tree-control-in-cortex-sandbox.md)) and `cortex-term`, and in one Windows-only module of `cortex-base` (`private::windows`, which sets access lists and reads a file's reparse tag; [decision 0029](docs/decisions/0029-owner-only-files-on-windows.md)). Every block has a `// SAFETY:` comment. A private file is written with `write_atomic` and `owner_only`, or created with `cortex_base::private::create_new`: no other crate needs `unsafe` for it.
- **An `allow` or `expect` always carries a `reason`.**
- **Public items are documented.**
- **No `todo!`, `unimplemented!` or `dbg!`** on the main branch, and no `std::process::exit`: return an exit code, so the terminal is always restored.

Clippy treats `#[test]` functions and `#[cfg(test)]` modules as tests. Helper functions at the top level of an integration-test file are not covered, so `tests/it/main.rs` and `tests/system/main.rs` start with an `#![allow(…, reason = "…")]` for what test helpers need. `crates/cortex-cli/tests/system/main.rs` is the model.

## The repo rules

`cargo xtask rules` is the part of the static checks that no off-the-shelf tool does. It fails when:

- **A file contains an absolute home-directory path.** Write `~` or a path relative to the repository. Every file is scanned, binary files included.
- **A crate depends on a workspace crate it must not**, a heavy third-party crate is used outside its owner crate, or a manifest breaks the conventions above.
- **`docs/dependencies.md` and the manifests disagree.**
- **Anything but a dev-dependency turns on a crate's `test-hooks` feature:** another kind of dependency, a feature of the crate itself (`default` above all), or a feature of another crate that passes it on. The feature holds switches for tests, and must not be in a build of the product.
- **A link inside the repository leads nowhere:** a relative link in a Markdown file to a file that is not there, a `#fragment` that is no heading of the page it names, or a `docs/….md` path in prose, configuration or a code comment that names no file. A heading's name is its text in lower case, without punctuation, with a `-` for each space.
- **A step's record or an open item is not where the others expect it:** anything in a folder of `docs/steps/` but `answered.md` and `ci.md`, a folder name that is not a step's (`07`, `13a`) or a hyphenated name, a first line that is not the heading its kind has, or a file of `docs/outstanding/` that is not the table's header and one row.
- **`cortex-protocol` depends on anything but serde and serde_json**, other than schemars behind its `schema` feature.
- **A workflow uses an action that is not pinned by commit hash**; a workflow that runs on pull requests uses a secret or a token that can write; or any job other than the `report` job of a non-blocking workflow has a write permission, or that job has one other than `issues: write`.

`cargo xtask check` fails in its `breaks` step when a break in `mutations/` no longer finds the code it changes: the message names the list and the line. Change the break to the code as it is now, or delete it if the rule it breaks is gone ([docs/testing-and-ci.md](docs/testing-and-ci.md), "Breaking the code on purpose").

`cargo xtask check` also fails, in its `generated` step, when the files in `docs/protocol/` are not what the protocol types generate. Run `cargo xtask schema`. The files in `docs/configuration/` are checked the same way by a test in `cortex-config`, on both OSes. Run `cargo xtask gen`.

## Dependencies

No new dependency without a row in [docs/dependencies.md](docs/dependencies.md) saying why it is here, what else was considered and what it costs. That page has the steps. The policy is in [docs/architecture/01-workspace.md](docs/architecture/01-workspace.md): prefer the standard library, default features off, nothing that needs build tools beyond the platform's compiler, permissive licences only.

`Cargo.lock` is committed. CI builds with `--locked`.

## Continuous integration

Four jobs block a pull request: `lint-linux`, `lint-windows`, `test-linux` and `test-windows`. Each must finish in under 5 minutes, and a job that takes longer fails by its timeout. The layout and the measured times are in [docs/testing-and-ci.md](docs/testing-and-ci.md).

- A step is not done until its tests pass on both OSes.
- Nothing that blocks uses a secret or a paid API.
- Only the main branch writes the build caches. A pull request reads them.
- A pull request that touches only `docs/**` and `*.md` skips the Rust build and tests. Its four checks still report, and the Linux lint job still runs the repo rules, because those rules are mostly about documentation. The generated files in `docs/protocol/` and `docs/configuration/` are the exception: they count as code.
- Two workflows do not block: `main`, after each merge, and `nightly`. When one fails it opens an issue labelled `ci-failure`, or adds a comment to the open one. Close the issue when the cause is fixed.
- A change that makes the blocking checks slower than their budget moves the slow part to a non-blocking tier, or raises it for a decision. The limit is not loosened quietly.

## Ground rules

These hold for every change. They are the "Rules for every step" of the [build plan](docs/build-plan.md).

1. **Original work.** Other agent harnesses may be studied to understand a problem. Nothing is copied from them: no code, structure, names, prompt text, UI or configuration format. No dependency that is a port or extract of their code.
2. **Speed and efficiency come first**, in the product and in the tooling.
3. **No personal details** in commits, docs, code, configuration or fixtures: no names, no email addresses, no absolute home-directory paths.
4. **Linux and Windows together.**
5. **The docs are the specification.** Keep them true.

## Pull requests

- Keep a pull request to one subject, and say in its description what was built, what was measured, and what is left open.
- Run `cargo xtask ci` first.
- For a change to the loop, or to code that starts processes or crosses threads, run `cargo xtask stress` for the crate first.
- Commit messages say what changed and why, in the imperative: "Add the session index", not "Added" or "Adds".
