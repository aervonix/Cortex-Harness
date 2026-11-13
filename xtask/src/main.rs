//! Developer commands for the Cortex-Harness repository.
//!
//! `cargo xtask <command>`, or `./x <command>` from a shell. The commands are
//! listed in [`USAGE`]. CI runs these same commands, so what passes here
//! passes there.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line tool for developers; its output is its interface"
)]
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "a synchronous tool: it reads files and runs commands, and has no async runtime"
)]

use std::process::ExitCode;

mod alias;
mod checks;
mod junit;
mod mutate;
mod nonblocking;
mod outside;
mod owner_guard;
mod perf;
mod rules;
mod schema;
mod startup;
mod steps;
mod stress;
mod tiers;
mod workspace;

/// Failures are messages for the person at the terminal.
type Result<T> = std::result::Result<T, String>;

const USAGE: &str = "\
Developer commands for the Cortex-Harness repository.

Usage: cargo xtask <command> [arguments]        (or: ./x <command> [arguments])

Checks and tests
  check [STEP...]   Static checks (tier 0): fmt, rules, breaks, generated, clippy,
                    deny. With step names, only those steps.
  test [ARGS...]    All tests, with nextest. Arguments go to `cargo nextest run`,
                    for example: test --profile fast     (tiers 1 and 2 only)
                                 test -p cortex-agent    (one crate, and only it)
  ci [lint|test]    Exactly what the blocking CI jobs run. No argument: both.
  ci --tiers [-p CRATE]... [--top N]
                    Build and run the tests as CI does (third-party crates not
                    optimised) and report each tier's time against its budget
                    and its slowest tests. With --from JUNIT.XML [--os
                    linux|windows], report on a run that was already made:
                    the report a CI job uploads, for example.
  rules             The repo-rule checks alone: no home-directory paths, the
                    crate dependency rules, docs/dependencies.md, workflows,
                    links and the layout of the step records.
  links             The part of `rules` that is about the docs: every link
                    inside the repository leads to a file and a heading that
                    are there, and docs/steps/ and docs/outstanding/ hold
                    what they should. It asks cargo nothing.
  owner-guard --base REV --head REV
                    Fail when the change adds one of the owner's personal
                    details: the terms of OWNER_TERMS, which CI fills from a
                    secret. Without terms it passes with a warning.

Showing that the tests bite
  stress -p CRATE... [--count N] [--first-seed S] [ARGS...]
                    Run the crates' tests N times over (100 when not given),
                    each time under the next seed, and report the tests that
                    fail on some, with the seed that brings each failure back.
  mutate [BREAK...] Break the code on purpose, one break of mutations/*.breaks
                    at a time, on a copy of the tree, and report which tests
                    fail. --check only checks that every break still applies
                    (`check` runs that). `mutate --help` has the rest.

Not blocking (the `main` and `nightly` workflows run these)
  size [--check]    Build the `dist` profile and report the binary's size and
                    the number of crates in it. --check fails over the budget.
  startup BINARY [--runs N] [--budget desktop|linux|windows] [--check]
                    Time `--version` under both names, and a headless run to
                    its request, against the start-up budgets.
  perf [--runs N] [--seconds N] [--json PATH]
                    Measure the terminal layer's demo, optimised, in a
                    pseudo-terminal: start to first frame, key to frame idle
                    and while streaming, frame cost, frame intervals and bytes
                    per frame, against the TUI's budgets.
  unused            Look for dependencies that no code uses (cargo-machete).
  advisories        Check the lockfile against the advisory database.
  fuzz [--seconds N] [TARGET...]
                    Fuzz the stream decoders, each target for N seconds
                    (60 when not given). Needs a nightly toolchain and
                    cargo-fuzz.

By hand only, against a real provider (never in CI)
  live [PROVIDER]   The live smoke test, against anthropic (when no provider
                    is named), openrouter, deepseek or venice. For anthropic
                    and deepseek, then one headless run of the real binary.
  record [--provider PROVIDER] [SCENARIO...]
                    Record provider fixtures, scrubbed. No name: all of them.
                    Both need CORTEX_LIVE_<PROVIDER>_KEY_FILE set to the path
                    of a file that holds that provider's API key. The key is
                    never given on a command line.

Working on the code
  fmt               Format the workspace.
  schema [--check]  Regenerate the protocol's JSON Schema and TypeScript
                    declarations in docs/protocol/. --check only compares.
  gen               Regenerate every checked-in generated file: what `schema`
                    writes, and the configuration schema and reference in
                    docs/configuration/.
  alias [--release] Build `cortex` and put its alias `cx` beside it.

Not built yet
  dist              Package release binaries (step 34).
";

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();
    let rest: Vec<String> = arguments.collect();

    let outcome = match command.as_deref() {
        None | Some("help" | "--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("check") => checks::check(&rest),
        Some("test") => checks::test(&rest),
        Some("ci") => checks::ci(&rest),
        Some("rules") => checks::rules(),
        Some("links") => checks::links(),
        Some("fmt") => checks::fmt(),
        Some("gen") => generate(),
        Some("schema") => schema::run(&rest),
        Some("size") => nonblocking::size(&rest),
        Some("startup") => startup::run(&rest),
        Some("unused") => nonblocking::unused(),
        Some("advisories") => nonblocking::advisories(),
        Some("alias") => alias::run(&rest),
        Some("mutate") => return mutate::main(&rest),
        Some("stress") => stress::run(&rest),
        Some("owner-guard") => owner_guard::run(&rest),
        Some("fuzz") => outside::fuzz(&rest),
        Some("live") => outside::live(&rest),
        Some("record") => outside::record(&rest),
        Some("perf") => perf::run(&rest),
        Some("dist") => not_built("dist", 34),
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

/// `cargo xtask gen`: every checked-in generated file.
///
/// The protocol's files are generated here, from the protocol crate. The
/// configuration's are generated by the test in `cortex-config` that checks
/// them, run in its writing mode, so that this crate does not have to build
/// the configuration crate and its TOML parser before every CI job.
fn generate() -> Result<()> {
    schema::write()?;
    steps::cargo_with(
        &[
            "test",
            "--locked",
            "-p",
            "cortex-config",
            "--test",
            "it",
            "--",
            "generated::",
            "--nocapture",
        ],
        &[("CORTEX_UPDATE_GENERATED", "1")],
    )
}

fn not_built(command: &str, step: u32) -> Result<()> {
    Err(format!(
        "`{command}` is not built yet. It arrives with step {step} of docs/build-plan.md."
    ))
}
