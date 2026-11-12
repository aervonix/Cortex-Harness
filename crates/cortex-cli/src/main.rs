//! The `cortex` binary, also installed under the alias `cx`.
//!
//! This is the repository skeleton: it answers `--version` and `--help` and
//! nothing else. Argument parsing, mode selection and the headless printer
//! arrive in the build step that `docs/build-plan.md` assigns to this crate.
//!
//! The program behaves identically under both names. It reads the name it was
//! started with only to echo it in usage and error text
//! (`docs/decisions/0013-naming.md`).

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the binary is the one place that writes to the user's terminal"
)]

use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;

/// The command's own name. `--version` prints it under either name.
const COMMAND: &str = "cortex";
/// The short alias: the same executable under a second name.
const ALIAS: &str = "cx";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Exit code for bad flags or arguments (`docs/architecture/02-runtime.md`).
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let mut args = env::args_os();
    let name = invoked_name(args.next().as_deref());
    match args.next() {
        None => {
            print!("{}", help(name));
            ExitCode::SUCCESS
        }
        Some(arg) if arg == "--help" || arg == "-h" => {
            print!("{}", help(name));
            ExitCode::SUCCESS
        }
        Some(arg) if arg == "--version" || arg == "-V" => {
            println!("{COMMAND} {VERSION}");
            ExitCode::SUCCESS
        }
        Some(arg) => {
            eprintln!("{name}: unexpected argument '{}'", arg.to_string_lossy());
            eprintln!("Try '{name} --help'.");
            ExitCode::from(EXIT_USAGE)
        }
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

fn help(name: &str) -> String {
    format!(
        "\
Cortex-Harness {VERSION}: a terminal agent harness.

Usage: {name} [OPTIONS]

Options:
  -h, --help     Print this help
  -V, --version  Print the version

This build is the repository skeleton. The harness itself is not built yet.
"
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{help, invoked_name};

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
    fn help_uses_the_name_it_is_given() {
        assert!(help("cx").contains("Usage: cx [OPTIONS]"));
        assert!(help("cortex").contains("Usage: cortex [OPTIONS]"));
    }
}
