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
| `cargo xtask check` | The static checks (tier 0): `fmt`, `rules`, `clippy`, `deny`. `cargo xtask check clippy` runs one of them. Every step runs even if an earlier one fails, and the timings are printed at the end. |
| `cargo xtask test` | All tests. Arguments go to `cargo nextest run`: `cargo xtask test --profile fast` runs tiers 1 and 2 only, `cargo xtask test -p cortex-cli` one crate. |
| `cargo xtask ci` | Exactly what the blocking CI jobs run: the static checks, then tiers 1 to 3 in the `ci` profile. Run it before opening a pull request. |
| `cargo xtask rules` | The repo-rule checks alone (see below). |
| `cargo xtask fmt` | Formats the workspace. |
| `cargo xtask alias` | Builds `cortex` and puts its alias `cx` beside it in the target directory. |

## Layout

```
crates/cortex-*    the product, one crate per job (docs/architecture/01-workspace.md)
crates/cortex-cli  the `cortex` binary; `cx` is a link to it
xtask/             developer commands; not shipped
docs/              the specification
assets/            the logo
.config/nextest.toml, clippy.toml, deny.toml, rustfmt.toml, rust-toolchain.toml
.github/workflows/ci.yml
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
- **No sleeping.** Time in tests is the fake clock. A process test waits on a condition with a deadline, never on a fixed delay.
- **No retries**, with one exception: a test inside a module named `timing_sensitive` gets one retry, and nextest reports it as flaky when the retry was needed.
- **A test that starts a process** is a system test and is named `sys_…`, wherever it lives. That name puts it in nextest's `process` group (two at a time) and keeps it out of the `fast` profile.
- **A slow test needs a reason.** Over 2 s in tiers 1 and 2, or over 10 s in tier 3.

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

- **`unsafe` is denied**, and allowed per module only in `cortex-sandbox` and `cortex-term`, with a `// SAFETY:` comment on every block.
- **An `allow` or `expect` always carries a `reason`.**
- **Public items are documented.**
- **No `todo!`, `unimplemented!` or `dbg!`** on the main branch, and no `std::process::exit`: return an exit code, so the terminal is always restored.

Clippy treats `#[test]` functions and `#[cfg(test)]` modules as tests. Helper functions at the top level of an integration-test file are not covered, so `tests/it/main.rs` and `tests/system/main.rs` start with an `#![allow(…, reason = "…")]` for what test helpers need. `crates/cortex-cli/tests/system/main.rs` is the model.

## The repo rules

`cargo xtask rules` is the part of the static checks that no off-the-shelf tool does. It fails when:

- **A file contains an absolute home-directory path.** Write `~` or a path relative to the repository. Every file is scanned, binary files included.
- **A crate depends on a workspace crate it must not**, a heavy third-party crate is used outside its owner crate, or a manifest breaks the conventions above.
- **`docs/dependencies.md` and the manifests disagree.**
- **A workflow uses an action that is not pinned by commit hash**, or the blocking workflow uses a secret or a token that can write.

## Dependencies

No new dependency without a row in [docs/dependencies.md](docs/dependencies.md) saying why it is here, what else was considered and what it costs. That page has the steps. The policy is in [docs/architecture/01-workspace.md](docs/architecture/01-workspace.md): prefer the standard library, default features off, nothing that needs build tools beyond the platform's compiler, permissive licences only.

`Cargo.lock` is committed. CI builds with `--locked`.

## Continuous integration

Four jobs block a pull request: `lint-linux`, `lint-windows`, `test-linux` and `test-windows`. Each must finish in under 5 minutes, and a job that takes longer fails by its timeout. The layout and the measured times are in [docs/testing-and-ci.md](docs/testing-and-ci.md).

- A step is not done until its tests pass on both OSes.
- Nothing that blocks uses a secret or a paid API.
- Only the main branch writes the build caches. A pull request reads them.
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
- Commit messages say what changed and why, in the imperative: "Add the session index", not "Added" or "Adds".
