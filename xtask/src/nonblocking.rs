//! What the non-blocking workflows run (`docs/testing-and-ci.md`, "Not
//! blocking"): the release build with its size and crate count, the
//! unused-dependency check, the advisory check, and the sandbox's tests
//! with the partial Landlock.
//!
//! They live here, like the blocking checks, so that a workflow is a list of
//! `cargo xtask` commands and the same commands run at a desk.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::Result;
use crate::steps;
use crate::workspace;

/// The package whose binary is shipped.
const BINARY_PACKAGE: &str = "cortex-cli";
/// The profile published binaries are built with (decision 0012). The size
/// budget is defined on it.
const PROFILE: &str = "dist";
const MEBIBYTE: f64 = 1024.0 * 1024.0;

/// The size budget for a target, in bytes, from docs/performance-budgets.md
/// ("Binary size"): 16 MB on Linux, 18 MB on Windows.
fn budget(target: &str) -> u64 {
    let megabytes: u64 = if target.contains("windows") { 18 } else { 16 };
    megabytes * 1024 * 1024
}

/// The target cargo builds for when none is named.
fn host_target() -> Result<String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .current_dir(workspace::root())
        .output()
        .map_err(|error| format!("could not run rustc -vV: {error}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| "rustc -vV did not name a host".to_owned())
}

/// The packages in a build of the binary for a target: every crate cargo
/// compiles for it, ours included, with build dependencies and without
/// development ones.
fn crates_in_build(target: &str) -> Result<BTreeSet<String>> {
    let output = Command::new(workspace::cargo())
        .args([
            "tree",
            "--locked",
            "--package",
            BINARY_PACKAGE,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--target",
            target,
        ])
        .current_dir(workspace::root())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("could not run cargo tree: {error}"))?;
    if !output.status.success() {
        return Err("`cargo tree` failed".to_owned());
    }
    Ok(parse_tree(&String::from_utf8_lossy(&output.stdout)))
}

/// The distinct `name version` pairs in `cargo tree --prefix none` output.
fn parse_tree(tree: &str) -> BTreeSet<String> {
    tree.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            let version = words.next().filter(|word| word.starts_with('v'))?;
            Some(format!("{name} {version}"))
        })
        .collect()
}

/// `cargo xtask size [--check] [--target TRIPLE]`
pub(crate) fn size(arguments: &[String]) -> Result<()> {
    let mut check = false;
    let mut target = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--check" => check = true,
            "--target" => {
                target = Some(
                    arguments
                        .next()
                        .ok_or("--target needs a target triple")?
                        .clone(),
                );
            }
            _ => return Err("usage: cargo xtask size [--check] [--target TRIPLE]".to_owned()),
        }
    }
    let target = match target {
        Some(target) => target,
        None => host_target()?,
    };

    steps::cargo(&[
        "build",
        "--locked",
        "--profile",
        PROFILE,
        "--package",
        BINARY_PACKAGE,
        "--target",
        &target,
    ])?;

    let metadata = workspace::metadata(&workspace::root())?;
    let executable = if target.contains("windows") {
        "cortex.exe"
    } else {
        "cortex"
    };
    let binary: PathBuf = metadata
        .target_directory
        .join(&target)
        .join(PROFILE)
        .join(executable);
    let bytes = fs::metadata(&binary)
        .map_err(|error| format!("could not read {}: {error}", binary.display()))?
        .len();
    let crates = crates_in_build(&target)?.len();
    let limit = budget(&target);

    #[expect(
        clippy::cast_precision_loss,
        reason = "a binary's size in bytes is far below 2^52, where f64 stops being exact"
    )]
    let (megabytes, limit_megabytes) = (bytes as f64 / MEBIBYTE, limit as f64 / MEBIBYTE);
    println!(
        "\n{target}, `{PROFILE}` profile\n  binary   {bytes} bytes ({megabytes:.2} MB; budget {limit_megabytes:.0} MB)\n  crates   {crates}"
    );
    steps::append_job_summary(&format!(
        "### Binary size and crate count\n\n| Target | Profile | Bytes | MB | Budget | Crates |\n|---|---|---:|---:|---:|---:|\n| `{target}` | `{PROFILE}` | {bytes} | {megabytes:.2} | {limit_megabytes:.0} MB | {crates} |\n\n"
    ));

    // The record, for the workflow to keep as an artifact.
    let record = serde_json::json!({
        "target": target,
        "profile": PROFILE,
        "bytes": bytes,
        "budget_bytes": limit,
        "crates": crates,
        "commit": std::env::var("GITHUB_SHA").ok(),
    });
    let directory = metadata.target_directory.join("size");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    let file = directory.join(format!("{target}.json"));
    fs::write(&file, format!("{record:#}\n"))
        .map_err(|error| format!("could not write {}: {error}", file.display()))?;

    if check && bytes > limit {
        return Err(format!(
            "the {target} binary is {megabytes:.2} MB, over its budget of {limit_megabytes:.0} MB \
             (docs/performance-budgets.md, \"Binary size\"). Find what grew, or raise the \
             question: the budget is not loosened quietly."
        ));
    }
    Ok(())
}

/// `cargo xtask unused`
///
/// The tool is run by its own name, not as `cargo machete`: started through
/// cargo from inside `cargo run`, version 0.9.2 took the word `machete` for
/// the directory to analyse and failed (seen on the first nightly run).
pub(crate) fn unused() -> Result<()> {
    const TOOL: &str = "cargo-machete";
    println!("$ {TOOL}");
    let status = Command::new(TOOL)
        .current_dir(workspace::root())
        .status()
        .map_err(|error| {
            format!(
                "could not run {TOOL}: {error}. Is it installed? The version CI uses is in \
                 docs/dependencies.md."
            )
        })?;
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "`{TOOL}` failed.\n    It reads the sources and reports a dependency that no code names. \
         Remove it from the manifest and from docs/dependencies.md, or, if it is used in a way \
         the tool cannot see, say so in the manifest under [package.metadata.cargo-machete]."
    ))
}

/// `cargo xtask advisories`
pub(crate) fn advisories() -> Result<()> {
    steps::cargo(&["deny", "check", "advisories"])
}

#[cfg(test)]
mod tests {
    use super::{budget, parse_tree};

    #[test]
    fn each_crate_in_the_tree_is_counted_once() {
        let tree = "\
cortex-cli v0.0.0 (/repo/crates/cortex-cli)
cortex-core v0.0.0 (/repo/crates/cortex-core)
cortex-protocol v0.0.0 (/repo/crates/cortex-protocol)
serde v1.0.229
serde_core v1.0.229
serde_derive v1.0.229 (proc-macro)
serde_json v1.0.154
serde_core v1.0.229
cortex-protocol v0.0.0 (/repo/crates/cortex-protocol) (*)

[build-dependencies]
";
        let crates = parse_tree(tree);
        assert_eq!(crates.len(), 7, "{crates:?}");
        assert!(crates.contains("serde_derive v1.0.229"));
    }

    #[test]
    fn the_budgets_are_the_ones_in_the_performance_page() {
        assert_eq!(budget("x86_64-unknown-linux-musl"), 16 * 1024 * 1024);
        assert_eq!(budget("aarch64-unknown-linux-musl"), 16 * 1024 * 1024);
        assert_eq!(budget("x86_64-pc-windows-msvc"), 18 * 1024 * 1024);
    }
}

/// What the sandbox's signal test prints when the kernel's Landlock does not
/// scope signals: the gap of the partial case, named.
const PARTIAL_MARK: &str = "does not scope signals: the gap the status names";

/// `cargo xtask partial-landlock`: `cortex-sandbox`'s tests, as the blocking
/// job runs them, on a kernel whose Landlock is older than 6 (the `nightly`
/// workflow's Ubuntu 22.04 job, ABI 4). The confinement tests fail rather
/// than skip on CI where the sandbox does not enforce; this fails too when
/// the run did not show the partial case, so that a kernel that moves on
/// does not leave the case untested in silence.
pub(crate) fn partial_landlock() -> Result<()> {
    crate::checks::require_nextest()?;
    let mut run = crate::checks::ci_run(&["cortex-sandbox".to_owned()]);
    run.extend(["--success-output".to_owned(), "immediate".to_owned()]);
    println!("$ cargo {}", run.join(" "));
    let output = Command::new(workspace::cargo())
        .args(&run)
        .current_dir(workspace::root())
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run cargo: {error}"))?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{printed}");
    partial_verdict(output.status.success(), &printed)
}

/// Whether a run of the sandbox's tests passed and showed the partial case.
fn partial_verdict(passed: bool, printed: &str) -> Result<()> {
    if !passed {
        return Err("the sandbox's tests failed with the partial Landlock".to_owned());
    }
    if let Some(line) = printed.lines().find(|line| line.contains(PARTIAL_MARK)) {
        println!("\nThe partial case ran: {}", line.trim());
        Ok(())
    } else {
        Err(format!(
            "the tests passed, but none said `{PARTIAL_MARK}`: this kernel's Landlock scopes \
             signals, so the partial case ran nowhere. Move the job to a runner with Landlock 4 \
             or 5, or say in docs/testing-and-ci.md that it is no longer tested."
        ))
    }
}

#[cfg(test)]
mod partial_tests {
    use super::partial_verdict;

    #[test]
    fn the_partial_case_must_be_seen_and_the_tests_must_pass() {
        let shown = "    PASS [ 0.1s] cortex-sandbox::it confined::sys_a_signal_to_a_process_outside_the_sandbox_is_refused\n\
                     Landlock 4 does not scope signals: the gap the status names\n";
        assert_eq!(partial_verdict(true, shown), Ok(()));
        let error =
            partial_verdict(true, "    PASS [ 0.1s] cortex-sandbox::it confined::x\n").unwrap_err();
        assert!(error.contains("the partial case ran nowhere"), "{error}");
        let error = partial_verdict(false, shown).unwrap_err();
        assert!(error.contains("tests failed"), "{error}");
    }
}
