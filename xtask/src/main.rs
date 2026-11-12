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
mod rules;
mod steps;
mod tiers;
mod workspace;

/// Failures are messages for the person at the terminal.
type Result<T> = std::result::Result<T, String>;

const USAGE: &str = "\
Developer commands for the Cortex-Harness repository.

Usage: cargo xtask <command> [arguments]        (or: ./x <command> [arguments])

Checks and tests
  check [STEP...]   Static checks (tier 0): fmt, rules, clippy, deny.
                    With step names, only those steps.
  test [ARGS...]    All tests, with nextest. Arguments go to `cargo nextest run`,
                    for example: test --profile fast     (tiers 1 and 2 only)
  ci [lint|test]    Exactly what the blocking CI jobs run. No argument: both.
  rules             The repo-rule checks alone: no home-directory paths, the
                    crate dependency rules, docs/dependencies.md, workflows.

Working on the code
  fmt               Format the workspace.
  gen               Regenerate checked-in generated files. (There are none yet.)
  alias [--release] Build `cortex` and put its alias `cx` beside it.

Not built yet
  schema            Protocol schemas and TypeScript declarations (step 3).
  record            Record provider fixtures by hand (step 14).
  perf              Measure the performance budgets (step 20).
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
        Some("fmt") => checks::fmt(),
        Some("gen") => {
            println!(
                "Nothing is generated yet. Generated files arrive with step 3 (the protocol schema)."
            );
            Ok(())
        }
        Some("alias") => alias::run(&rest),
        Some("schema") => not_built("schema", 3),
        Some("record") => not_built("record", 14),
        Some("perf") => not_built("perf", 20),
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

fn not_built(command: &str, step: u32) -> Result<()> {
    Err(format!(
        "`{command}` is not built yet. It arrives with step {step} of docs/build-plan.md."
    ))
}
