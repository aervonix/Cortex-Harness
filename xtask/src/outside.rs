//! The commands that reach outside the repository: the real provider
//! (`live`, `record`) and the fuzzer (`fuzz`). None of them is part of the
//! blocking checks (`docs/testing-and-ci.md`, tiers 5 and 6).
//!
//! # `live` and `record`
//!
//! Both run ignored tests of `cortex-llm` (`tests/it/anthropic/live.rs`),
//! which is where the HTTP client is. Doing it here would mean building the
//! whole HTTP and TLS stack into this crate, which every CI job builds
//! before it does anything else.
//!
//! The API key is never an argument and never in the environment. The
//! environment names a **file**, in [`KEY_FILE`], and the test reads the
//! key from it. Without the variable these commands say so and stop.
//!
//! # `fuzz`
//!
//! Runs each fuzz target of the `fuzz/` package for a fixed time, with
//! `cargo fuzz`, which needs a nightly toolchain and the `cargo-fuzz`
//! subcommand. The recorded fixtures are the corpus each run starts from.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Result;
use crate::workspace;

/// The environment variable that names the file the API key is in.
const KEY_FILE: &str = "CORTEX_LIVE_ANTHROPIC_KEY_FILE";

/// The environment variable the recorder reads its scenarios from.
const SCENARIOS: &str = "CORTEX_LIVE_SCENARIOS";

/// The fuzz targets, and the fixtures each one starts from: the name of
/// the protocol's fixture directory in `cortex-llm`.
const FUZZ_TARGETS: &[(&str, &str)] = &[
    ("sse_parser", "anthropic-messages"),
    ("anthropic_decoder", "anthropic-messages"),
];

/// How long each fuzz target runs when no time is given, in seconds.
const FUZZ_SECONDS: u64 = 60;

fn require_key_file() -> Result<()> {
    match std::env::var_os(KEY_FILE) {
        Some(path) if !path.is_empty() => Ok(()),
        _ => Err(format!(
            "{KEY_FILE} is not set. Set it to the path of a file that holds the API key and \
             nothing else. The key itself is never given on a command line or in the \
             environment. Nothing was sent."
        )),
    }
}

/// Runs one ignored test of `cortex-llm`'s integration binary, and shows
/// its output.
///
/// A test that was renamed is a test that does not run: cargo then runs
/// nothing and reports success. So the output is read, and the command
/// fails unless exactly one test ran and passed. What the test prints has
/// already been checked for the key by the test itself.
fn ignored_test(name: &str, variables: &[(&str, &str)]) -> Result<()> {
    let arguments = [
        "test",
        "--locked",
        "--package",
        "cortex-llm",
        "--features",
        "http",
        "--test",
        "it",
        "--",
        "--ignored",
        "--exact",
        name,
        "--nocapture",
    ];
    println!("$ cargo {}", arguments.join(" "));
    let output = Command::new(workspace::cargo())
        .args(arguments)
        .envs(variables.iter().copied())
        .current_dir(workspace::root())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("could not run cargo test: {error}"))?;
    let shown = String::from_utf8_lossy(&output.stdout);
    print!("{shown}");
    if !output.status.success() {
        return Err(format!("the test `{name}` failed"));
    }
    if ran_one_test(&shown) {
        Ok(())
    } else {
        Err(format!(
            "the test `{name}` did not run: cargo found no test of that name. Nothing was sent."
        ))
    }
}

/// Whether the output of `cargo test` says that exactly one test ran and
/// passed.
fn ran_one_test(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed;"))
}

/// `cargo xtask live`: the smoke test against the real API.
pub(crate) fn live(arguments: &[String]) -> Result<()> {
    if let Some(argument) = arguments.first() {
        return Err(format!(
            "`live` takes no arguments, and was given `{argument}`"
        ));
    }
    require_key_file()?;
    ignored_test("anthropic::live::live_smoke", &[])
}

/// `cargo xtask record [SCENARIO…]`: record fixtures from the real API.
/// With no scenario named, all of them.
pub(crate) fn record(arguments: &[String]) -> Result<()> {
    if let Some(flag) = arguments.iter().find(|argument| argument.starts_with('-')) {
        return Err(format!(
            "`record` takes scenario names only, and was given `{flag}`"
        ));
    }
    require_key_file()?;
    let scenarios = arguments.join(",");
    ignored_test("anthropic::live::live_record", &[(SCENARIOS, &scenarios)])
}

/// `cargo xtask fuzz [--seconds N] [TARGET…]`: run the fuzz targets, each
/// for N seconds. With no target named, all of them.
pub(crate) fn fuzz(arguments: &[String]) -> Result<()> {
    let mut seconds = FUZZ_SECONDS;
    let mut wanted: Vec<&str> = Vec::new();
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--seconds" => {
                seconds = rest
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|seconds| *seconds > 0)
                    .ok_or("`--seconds` needs a number of seconds above zero")?;
            }
            flag if flag.starts_with('-') => {
                return Err(format!("`fuzz` does not know the flag `{flag}`"));
            }
            target => wanted.push(target),
        }
    }
    for target in &wanted {
        if !FUZZ_TARGETS.iter().any(|(name, _)| name == target) {
            let known: Vec<&str> = FUZZ_TARGETS.iter().map(|(name, _)| *name).collect();
            return Err(format!(
                "there is no fuzz target `{target}`. The targets are: {}",
                known.join(", ")
            ));
        }
    }

    let root = workspace::root();
    let package = root.join("fuzz");
    let host = nightly_host()?;
    let mut failed = Vec::new();
    for (target, fixtures) in FUZZ_TARGETS {
        if !wanted.is_empty() && !wanted.contains(target) {
            continue;
        }
        let corpus = package.join("corpus").join(target);
        let seeded = seed(
            &root.join("crates/cortex-llm/fixtures").join(fixtures),
            &corpus,
        )?;
        println!("fuzz {target}: {seconds} s, starting from {seeded} recorded streams");
        // `cargo fuzz` builds with a nightly compiler, whatever toolchain
        // this command itself was started with.
        let status = Command::new("cargo")
            // The target is named. `cargo fuzz` otherwise builds for the
            // target it was itself built for, and a prebuilt `cargo-fuzz`
            // is a static binary for a target the fuzzer cannot use: the
            // sanitizer does not work with a statically linked C library.
            .args(["+nightly", "fuzz", "run", "--target", &host, target, "--"])
            .arg(format!("-max_total_time={seconds}"))
            // A stream event is a few hundred bytes. Inputs up to this size
            // hold a whole response of several blocks.
            .arg("-max_len=8192")
            .arg("-timeout=10")
            .arg("-print_final_stats=1")
            .current_dir(&package)
            // The toolchain this command runs under must not be handed on.
            .env_remove("RUSTUP_TOOLCHAIN")
            .env_remove("CARGO")
            .env_remove("RUSTC")
            .env_remove("RUSTDOC")
            .status()
            .map_err(|error| {
                format!(
                    "could not start `cargo +nightly fuzz`: {error}. It needs a nightly \
                     toolchain (`rustup toolchain install nightly`) and cargo-fuzz \
                     (`cargo install cargo-fuzz`)."
                )
            })?;
        if !status.success() {
            failed.push(*target);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the fuzz target(s) {} failed. The input that failed is under fuzz/artifacts/, \
             and `cargo +nightly fuzz run <target> <file>` runs it again.",
            failed.join(", ")
        ))
    }
}

/// The target the nightly compiler builds for when none is named: the
/// machine's own.
fn nightly_host() -> Result<String> {
    let output = Command::new("rustc")
        .args(["+nightly", "-vV"])
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTC")
        .output()
        .map_err(|error| format!("could not run `rustc +nightly -vV`: {error}"))?;
    if !output.status.success() {
        return Err(
            "`rustc +nightly -vV` failed. The fuzzer needs a nightly toolchain: `rustup \
             toolchain install nightly`."
                .to_owned(),
        );
    }
    host_of(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "`rustc +nightly -vV` did not name a host".to_owned())
}

/// The host target in the output of `rustc -vV`.
fn host_of(version: &str) -> Option<String> {
    version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_owned())
        .filter(|host| !host.is_empty())
}

/// Copies the response streams of a fixture directory into a target's
/// corpus, each with two bytes in front for the cuts the targets read
/// there. Returns how many streams the corpus was given.
fn seed(fixtures: &Path, corpus: &Path) -> Result<usize> {
    fs::create_dir_all(corpus)
        .map_err(|error| format!("cannot make {}: {error}", corpus.display()))?;
    let entries = fs::read_dir(fixtures)
        .map_err(|error| format!("cannot read {}: {error}", fixtures.display()))?;
    let mut seeded = 0;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", fixtures.display()))?;
        let stream = entry.path().join("response.sse");
        let Ok(bytes) = fs::read(&stream) else {
            continue;
        };
        let name = entry.file_name();
        // The same stream at two speeds of cutting: whole, and in small
        // pieces.
        for (seed, label) in [([0u8, 0u8], "whole"), ([0x40, 0x07], "cut")] {
            let mut input = seed.to_vec();
            input.extend_from_slice(&bytes);
            let path = corpus.join(format!("fixture-{}-{label}", name.to_string_lossy()));
            fs::write(&path, input)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        }
        seeded += 1;
    }
    Ok(seeded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_command_passes_only_when_its_one_test_ran() {
        let ran = "running 1 test\nsmoke passed.\ntest anthropic::live::live_smoke ... ok\n\n\
                   test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 142 filtered out; finished in 3.25s\n";
        assert!(ran_one_test(ran));
        // A test that was renamed: cargo runs nothing and calls it a success.
        let none = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 143 filtered out; finished in 0.00s\n";
        assert!(!ran_one_test(none));
        let failed = "running 1 test\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n";
        assert!(!ran_one_test(failed));
        assert!(!ran_one_test(""));
    }

    #[test]
    fn the_host_is_read_from_the_compilers_own_report() {
        let report = "rustc 1.102.0-nightly (8eb778d04 2026-11-12)\nbinary: rustc\n\
                      host: x86_64-unknown-linux-gnu\nrelease: 1.102.0-nightly\n";
        assert_eq!(host_of(report).as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(host_of("rustc 1.99.1\n"), None);
        assert_eq!(host_of("host: \n"), None);
    }

    #[test]
    fn the_fuzz_targets_have_fixtures_to_start_from() {
        let root = workspace::root();
        for (target, fixtures) in FUZZ_TARGETS {
            assert!(
                root.join("fuzz/fuzz_targets")
                    .join(format!("{target}.rs"))
                    .is_file(),
                "{target}"
            );
            assert!(
                root.join("crates/cortex-llm/fixtures")
                    .join(fixtures)
                    .is_dir(),
                "{fixtures}"
            );
        }
    }
}
