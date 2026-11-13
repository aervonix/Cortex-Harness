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
mod compare;
mod decide;
mod junit;
mod load;
mod mutate;
mod nonblocking;
mod outside;
mod owner_guard;
mod perf;
mod replace;
mod rules;
mod runs;
mod schema;
mod startup;
mod steps;
mod stress;
mod tiers;
mod try_run;
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
  ci --tiers --compare NEW [--against OLD] [--os linux|windows] [--top N]
                    Two runs' tiers side by side, each above its own start
                    cost: by tier, by crate, and the tests that moved most.
                    NEW and OLD are JUnit reports, run ids of the `ci`
                    workflow (read through gh), or `main`, the main branch's
                    latest successful run (OLD when it is not given).
  ci-times RUN [--against RUN|main]
                    A run's job times and tier times, as tables for a step's
                    ci.md, read through gh.
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
  stress -p CRATE... [--count N] [--first-seed S] [--load N] [--cores K] [ARGS...]
                    Run the crates' tests N times over (100 when not given),
                    each time under the next seed, and report the tests that
                    fail on some, with the seed that brings each failure back.
                    --cores K pins the run to K cores, beside --load N busy
                    loops, to make a race show.
  mutate [BREAK...] Break the code on purpose, one break of mutations/*.breaks
                    at a time, on a copy of the tree, and report which tests
                    fail. --check only checks that every break still applies
                    (`check` runs that). `mutate --help` has the rest.

On the runners, without a pull request (the `try` workflow runs this)
  try --command 'COMMAND [ARGS]' [--profile ci|release|dist] [--count N]
      [--load N] [--cores K]
                    One of test, stress, startup, perf or ci, as the workflow's
                    inputs ask: `try --help` has the rules.

Not blocking (the `main` and `nightly` workflows run these)
  size [--check]    Build the `dist` profile and report the binary's size and
                    the number of crates in it. --check fails over the budget.
  startup BINARY|--profile PROFILE [--runs N] [--budget desktop|linux|windows]
          [--check]
                    Time `--version` under both names, and a headless run to
                    its request, against the start-up budgets. With
                    --profile, build `cortex` in that profile first.
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
  live --spent [--since YYYY-MM-DD]
                    Total the spend ledger: every live call's provider, model,
                    tokens and cost, kept outside the repository.
  record [--provider PROVIDER] [--dry-run] [SCENARIO...]
                    Record provider fixtures, scrubbed. No name: all of them.
                    --dry-run prints the first request and sends nothing.
  record --toolbox [--provider PROVIDER]
                    Keep the tools the binary sends each provider, and record
                    each one's toolbox fixture: the provider named, or every
                    provider whose key file is named.
                    Both need CORTEX_LIVE_<PROVIDER>_KEY_FILE set to the path
                    of a file that holds that provider's API key. The key is
                    never given on a command line.

Working on the code
  fmt               Format the workspace.
  decide [--level L] [--rule RULE]... [--untrusted] [--] COMMAND...
                    What the permission engine decides of a shell command,
                    with its reasons. `decide --help` has the rest.
  replace FILE --from CODE --to CODE [--in ITEM] [--occurrence N] [--dry-run]
                    Replace Rust code found by its tokens, so the match holds
                    however rustfmt laid it out. It must be there once.
                    --from-file and --to-file read the code from files.
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
        // Its exit code says more than pass or fail.
        Some("mutate") => return mutate::main(&rest),
        Some(command) => run(command, &rest),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one command. `try` comes back here with the command it chose.
fn run(command: &str, rest: &[String]) -> Result<()> {
    match command {
        "check" => checks::check(rest),
        "test" => checks::test(rest),
        "ci" => checks::ci(rest),
        "ci-times" => runs::ci_times(rest),
        "replace" => replace::run(rest),
        "rules" => checks::rules(),
        "links" => checks::links(),
        "fmt" => checks::fmt(),
        "decide" => decide::run(rest),
        "gen" => generate(),
        "schema" => schema::run(rest),
        "size" => nonblocking::size(rest),
        "startup" => startup::run(rest),
        "unused" => nonblocking::unused(),
        "advisories" => nonblocking::advisories(),
        "alias" => alias::run(rest),
        "stress" => stress::run(rest),
        "try" => try_run::run(rest),
        "owner-guard" => owner_guard::run(rest),
        "fuzz" => outside::fuzz(rest),
        "live" => outside::live(rest),
        "record" => outside::record(rest),
        "perf" => perf::run(rest),
        "dist" => not_built("dist", 34),
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
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
