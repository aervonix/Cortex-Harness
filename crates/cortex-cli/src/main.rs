//! The `cortex` binary, also installed under the alias `cx`.
//!
//! It parses the command line, chooses a mode, drives the core and exits
//! with the code of the table in `docs/architecture/10-headless.md`.
//!
//! | Module | What it does |
//! |---|---|
//! | [`args`] | The command line, read into what to do. |
//! | [`help`] | The help, in the name the program was started with. |
//! | [`headless`] | A prompt run to completion, with standard output for the result. |
//! | [`report`] | The result record and what each output format writes. |
//! | [`exit`] | The exit codes. |
//! | [`signals`] | Ctrl-C, SIGTERM and the console's events. |
//! | [`commands`] | The subcommands: sessions, configuration, permissions, credentials. |
//!
//! The program behaves identically under both names. It reads the name it
//! was started with only to echo it in usage, help and error text
//! (`docs/decisions/0013-naming.md`).

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the binary is the one place that writes to the user's terminal"
)]
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the binary's own threads read standard input, the working directory and the \
              environment, outside the core's runtime"
)]

mod args;
mod commands;
mod exit;
mod headless;
mod help;
mod report;
mod signals;

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use crate::args::{Command, Run};

/// The command's own name. `--version` prints it under either name.
const COMMAND: &str = "cortex";
/// The short alias: the same executable under a second name.
const ALIAS: &str = "cx";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let started = Instant::now();
    let mut arguments = env::args_os();
    let name = invoked_name(arguments.next().as_deref());
    let rest: Vec<OsString> = arguments.collect();

    // The guard of process trees is started by the harness itself, and is
    // started for every command: nothing else is done before it.
    if rest.first().is_some_and(|first| first == args::GUARD) {
        cortex_sandbox::tree::run_guard();
        return ExitCode::SUCCESS;
    }

    let command = match args::parse(rest) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{name}: {}", error.0);
            eprintln!("Try '{name} --help'.");
            return ExitCode::from(exit::USAGE);
        }
    };
    ExitCode::from(dispatch(command, name, started))
}

fn dispatch(command: Command, name: &'static str, started: Instant) -> u8 {
    match command {
        Command::Help(topic) => {
            print!("{}", help::help(topic, name));
            exit::COMPLETED
        }
        Command::Version => {
            println!("{COMMAND} {VERSION}");
            exit::COMPLETED
        }
        Command::Run(run) => match mode(&run, std::io::stdout().is_terminal()) {
            Mode::Headless => headless::run(*run, name, started),
            Mode::Interactive => {
                eprintln!(
                    "{name}: the interactive application is not built yet. Run a prompt \
                     headless with `{name} -p \"…\"`, or see `{name} --help`."
                );
                exit::USAGE
            }
        },
        Command::Sessions(command) => commands::sessions(command, name),
        Command::Config(command) => commands::config(command, name),
        Command::Permissions(command) => commands::permissions(command, name),
        Command::Auth(command) => commands::auth(command, name),
        Command::Models => commands::models(name),
    }
}

/// How a run is carried out.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Headless,
    Interactive,
}

/// Headless with `-p`, or when a prompt is given and standard output is
/// not a terminal (`docs/architecture/10-headless.md`, "Invocation").
fn mode(run: &Run, stdout_is_terminal: bool) -> Mode {
    if run.print || (run.prompt.is_some() && !stdout_is_terminal) {
        Mode::Headless
    } else {
        Mode::Interactive
    }
}

/// The name to show in usage and error text: `cx` when the program was
/// started under its alias, `cortex` otherwise.
///
/// Only the file name counts, without its directory or extension, and without
/// regard to case, so `C:\tools\CX.EXE` is `cx`. An unexpected or missing
/// name is treated as `cortex`.
fn invoked_name(arg0: Option<&OsStr>) -> &'static str {
    let stem = arg0
        .map(Path::new)
        .and_then(Path::file_stem)
        .and_then(OsStr::to_str);
    match stem {
        Some(stem) if stem.eq_ignore_ascii_case(ALIAS) => ALIAS,
        _ => COMMAND,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{Mode, Run, invoked_name, mode};

    fn name_for(arg0: &str) -> &'static str {
        invoked_name(Some(OsStr::new(arg0)))
    }

    #[test]
    fn the_alias_is_recognised_by_file_name() {
        assert_eq!(name_for("cx"), "cx");
        assert_eq!(name_for("./cx"), "cx");
        assert_eq!(name_for("bin/cx"), "cx");
        assert_eq!(name_for("cx.exe"), "cx");
        assert_eq!(name_for("CX.EXE"), "cx");
    }

    #[cfg(windows)]
    #[test]
    fn the_alias_is_recognised_in_a_windows_path() {
        assert_eq!(name_for(r"C:\tools\CX.EXE"), "cx");
        assert_eq!(name_for(r"C:\tools\cortex.exe"), "cortex");
    }

    #[test]
    fn every_other_name_is_cortex() {
        assert_eq!(name_for("cortex"), "cortex");
        assert_eq!(name_for("target/debug/cortex"), "cortex");
        assert_eq!(name_for("cortex.exe"), "cortex");
        assert_eq!(name_for("renamed-by-the-user"), "cortex");
        assert_eq!(name_for("cxx"), "cortex");
        assert_eq!(name_for(""), "cortex");
        assert_eq!(invoked_name(None), "cortex");
    }

    #[test]
    fn a_run_is_headless_with_print_or_a_prompt_and_no_terminal() {
        let with = |print: bool, prompt: Option<&str>| Run {
            print,
            prompt: prompt.map(str::to_owned),
            ..Run::default()
        };
        assert_eq!(mode(&with(true, None), true), Mode::Headless);
        assert_eq!(mode(&with(true, Some("x")), true), Mode::Headless);
        assert_eq!(mode(&with(false, Some("x")), false), Mode::Headless);
        assert_eq!(mode(&with(false, Some("x")), true), Mode::Interactive);
        assert_eq!(mode(&with(false, None), false), Mode::Interactive);
        assert_eq!(mode(&with(false, None), true), Mode::Interactive);
    }
}
