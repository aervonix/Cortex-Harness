//! System tests (tier 3): the real `cortex` binary, run as a process.
//!
//! Test names here start with `sys_`. That prefix is what puts them in
//! nextest's process group and keeps them out of the `fast` profile.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a system test starts processes and prepares files, and fails by panicking"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The binary Cargo built for this test run.
const CORTEX: &str = env!("CARGO_BIN_EXE_cortex");

/// Puts the binary under its second name, `cx`, in a directory of this
/// test's own, the way an installation does: a hard link, or a copy where
/// the file system cannot link (`docs/decisions/0013-naming.md`).
fn install_alias(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    fs::create_dir_all(&dir).expect("create the alias directory");
    let alias = dir.join(format!("cx{}", std::env::consts::EXE_SUFFIX));
    if alias.exists() {
        fs::remove_file(&alias).expect("remove the alias left by an earlier run");
    }
    if fs::hard_link(CORTEX, &alias).is_err() {
        fs::copy(CORTEX, &alias).expect("copy the binary where it cannot be linked");
    }
    alias
}

fn run(program: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not run {}: {error}", program.display()))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("the output is UTF-8")
}

/// Asserts that `cortex <args>` and `cx <args>` exit with the same status and
/// print the same text, once the name each was started with is accounted for.
fn assert_same_under_both_names(test: &str, args: &[&str], expected_code: i32) {
    let alias = install_alias(test);
    let as_cortex = run(Path::new(CORTEX), args);
    let as_cx = run(&alias, args);

    assert_eq!(
        as_cortex.status.code(),
        Some(expected_code),
        "cortex {args:?}"
    );
    assert_eq!(as_cx.status.code(), Some(expected_code), "cx {args:?}");

    // The only permitted difference is the name the program echoes back:
    // "cortex" as a whole word in usage and error text. The product name
    // "Cortex-Harness" is capitalised and so is left alone.
    let expected_stdout = text(&as_cortex.stdout).replace("cortex ", "cx ");
    let expected_stderr = text(&as_cortex.stderr)
        .replace("cortex:", "cx:")
        .replace("'cortex ", "'cx ");
    assert_eq!(
        text(&as_cx.stdout),
        expected_stdout,
        "standard output of cx {args:?}"
    );
    assert_eq!(
        text(&as_cx.stderr),
        expected_stderr,
        "standard error of cx {args:?}"
    );
}

#[test]
fn sys_version_is_byte_identical_under_both_names() {
    let alias = install_alias("sys_version");
    let as_cortex = run(Path::new(CORTEX), &["--version"]);
    let as_cx = run(&alias, &["--version"]);

    assert!(as_cortex.status.success());
    assert!(as_cx.status.success());
    assert_eq!(as_cortex.stdout, as_cx.stdout);
    assert_eq!(
        text(&as_cortex.stdout).trim_end(),
        concat!("cortex ", env!("CARGO_PKG_VERSION"))
    );
    assert!(as_cortex.stderr.is_empty() && as_cx.stderr.is_empty());
}

#[test]
fn sys_help_differs_only_in_the_name_used() {
    assert_same_under_both_names("sys_help", &["--help"], 0);

    let alias = install_alias("sys_help_usage_line");
    assert!(text(&run(&alias, &["--help"]).stdout).contains("Usage: cx [OPTIONS]"));
    assert!(text(&run(Path::new(CORTEX), &["--help"]).stdout).contains("Usage: cortex [OPTIONS]"));
}

#[test]
fn sys_no_arguments_behaves_the_same_under_both_names() {
    assert_same_under_both_names("sys_no_arguments", &[], 0);
}

#[test]
fn sys_a_usage_error_is_the_same_under_both_names() {
    assert_same_under_both_names("sys_usage_error", &["--no-such-flag"], 2);

    let alias = install_alias("sys_usage_error_text");
    let output = run(&alias, &["--no-such-flag"]);
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr).starts_with("cx: unexpected argument '--no-such-flag'"));
}
