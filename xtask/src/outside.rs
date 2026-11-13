//! The commands that reach outside the repository: the real provider
//! (`live`, `record`) and the fuzzer (`fuzz`). None of them is part of the
//! blocking checks (`docs/testing-and-ci.md`, tiers 5 and 6).
//!
//! # `live` and `record`
//!
//! Both run ignored tests of `cortex-llm` (the `live` module of each
//! protocol's tests), which is where the HTTP client is. Doing it here
//! would mean building the whole HTTP and TLS stack into this crate, which
//! every CI job builds before it does anything else. For Anthropic, `live`
//! then runs the one live run of the whole harness: an ignored system test
//! of `cortex-cli` that starts the real binary headless (step 13b).
//!
//! Each takes the provider to talk to ([`PROVIDERS`]); without one it is
//! Anthropic. The API key is never an argument and never in the
//! environment. The environment names a **file**, in the provider's own
//! variable, and the test reads the key from it. Without the variable
//! these commands say so and stop.
//!
//! # `fuzz`
//!
//! Runs each fuzz target of the `fuzz/` package for a fixed time, with
//! `cargo fuzz`, which needs a nightly toolchain and the `cargo-fuzz`
//! subcommand. The recorded fixtures are the corpus each run starts from.
//!
//! The nightly toolchain is pinned, to a date, in `fuzz/rust-toolchain.toml`
//! and nowhere else: the commands here run inside `fuzz/`, where that file
//! decides the toolchain.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Result;
use crate::workspace;

/// A provider the live commands can talk to.
struct LiveProvider {
    /// The name the commands take.
    name: &'static str,
    /// The environment variable that names the file its API key is in.
    key_file: &'static str,
    /// Its smoke test and its recorder: ignored tests of `cortex-llm`.
    smoke: &'static str,
    record: &'static str,
    /// A headless run of the real binary against it: an ignored test of
    /// `cortex-cli`'s system tests, run by `live` after the smoke test.
    headless: Option<&'static str>,
}

/// The providers the live commands can talk to. The first is the one a
/// command talks to when it names none.
const PROVIDERS: &[LiveProvider] = &[
    LiveProvider {
        name: "anthropic",
        key_file: "CORTEX_LIVE_ANTHROPIC_KEY_FILE",
        smoke: "anthropic::live::live_smoke",
        record: "anthropic::live::live_record",
        headless: Some("live::live_headless_run"),
    },
    LiveProvider {
        name: "openrouter",
        key_file: "CORTEX_LIVE_OPENROUTER_KEY_FILE",
        smoke: "openai_chat::live::live_smoke_openrouter",
        record: "openai_chat::live::live_record_openrouter",
        headless: None,
    },
    LiveProvider {
        name: "deepseek",
        key_file: "CORTEX_LIVE_DEEPSEEK_KEY_FILE",
        smoke: "openai_chat::live::live_smoke_deepseek",
        record: "openai_chat::live::live_record_deepseek",
        headless: Some("live::live_headless_run_deepseek"),
    },
    LiveProvider {
        name: "venice",
        key_file: "CORTEX_LIVE_VENICE_KEY_FILE",
        smoke: "openai_chat::live::live_smoke_venice",
        record: "openai_chat::live::live_record_venice",
        headless: None,
    },
];

fn live_provider(name: &str) -> Result<&'static LiveProvider> {
    PROVIDERS
        .iter()
        .find(|provider| provider.name == name)
        .ok_or_else(|| {
            let known: Vec<&str> = PROVIDERS.iter().map(|provider| provider.name).collect();
            format!(
                "there are no live tests for a provider `{name}`. The providers are: {}",
                known.join(", ")
            )
        })
}

/// The environment variable the recorder reads its scenarios from.
const SCENARIOS: &str = "CORTEX_LIVE_SCENARIOS";

/// Set for a dry run: requests are built and printed, and none is sent.
const DRY_RUN: &str = "CORTEX_LIVE_DRY_RUN";

/// The file each live call's spend is added to, as a line of JSON.
const LEDGER: &str = "CORTEX_LIVE_LEDGER";

/// What a dry run prints when it has printed its request.
const DRY_RUN_ENDED: &str = "dry run: nothing was sent.";

/// Where the spend ledger is: `CORTEX_LIVE_LEDGER` if it is set, or a file
/// in the user's state directory, outside every repository. On the desktop
/// that `rx` runs on, the ledger is the desktop's.
fn ledger() -> Result<std::path::PathBuf> {
    if let Some(path) = std::env::var_os(LEDGER).filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::Path::new(&home).join(".local").join("state"))
            })
    };
    let base = base
        .ok_or("there is no state directory to keep the spend ledger in: set CORTEX_LIVE_LEDGER")?;
    let directory = base.join("cortex-live");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    Ok(directory.join("spend.jsonl"))
}

/// The ledger's variable, for a live test to add its spend to.
fn ledger_variable() -> Result<(&'static str, String)> {
    Ok((LEDGER, ledger()?.to_string_lossy().into_owned()))
}

/// What the ledger says: each provider's and model's calls, tokens and
/// cost, and the total. `since` keeps the lines of that date or later
/// (`YYYY-MM-DD`).
fn spent(text: &str, since: Option<&str>) -> String {
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Sum {
        calls: u64,
        input: u64,
        output: u64,
        nanos: u64,
    }
    let mut sums: BTreeMap<(String, String), Sum> = BTreeMap::new();
    let mut unread = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            unread += 1;
            continue;
        };
        let field = |name: &str| {
            entry
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
        };
        let number = |name: &str| {
            entry
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        if since.is_some_and(|since| field("date") < since) {
            continue;
        }
        let sum = sums
            .entry((field("provider").to_owned(), field("model").to_owned()))
            .or_default();
        sum.calls += 1;
        sum.input += number("input") + number("cache_read") + number("cache_write");
        sum.output += number("output");
        sum.nanos += number("usd_nanos");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a figure for a person to read; the sums are whole nano-dollars"
    )]
    let dollars = |nanos: u64| nanos as f64 / 1e9;
    let mut shown = format!(
        "  {:<12} {:<40} {:>6} {:>12} {:>10} {:>11}\n",
        "provider", "model", "calls", "input", "output", "cost"
    );
    let mut total = Sum::default();
    let mut by_provider: BTreeMap<&str, u64> = BTreeMap::new();
    for ((provider, model), sum) in &sums {
        shown.push_str(&format!(
            "  {provider:<12} {model:<40} {:>6} {:>12} {:>10} {:>10.6}$\n",
            sum.calls,
            sum.input,
            sum.output,
            dollars(sum.nanos)
        ));
        *by_provider.entry(provider).or_default() += sum.nanos;
        total.calls += sum.calls;
        total.nanos += sum.nanos;
    }
    for (provider, nanos) in by_provider {
        shown.push_str(&format!("  {provider}: ${:.6}\n", dollars(nanos)));
    }
    shown.push_str(&format!(
        "  in all: {} call(s), ${:.6}{}\n",
        total.calls,
        dollars(total.nanos),
        since
            .map(|since| format!(" since {since}"))
            .unwrap_or_default()
    ));
    if unread > 0 {
        shown.push_str(&format!(
            "  ({unread} line(s) of the ledger could not be read)\n"
        ));
    }
    shown
}

/// The fuzz targets, and the fixtures each one starts from: the name of
/// the protocol's fixture directory in `cortex-llm`.
const FUZZ_TARGETS: &[(&str, &str)] = &[
    ("sse_parser", "anthropic-messages"),
    ("anthropic_decoder", "anthropic-messages"),
    ("openai_chat_decoder", "openai-chat"),
];

/// How long each fuzz target runs when no time is given, in seconds.
const FUZZ_SECONDS: u64 = 60;

fn require_key_file(provider: &LiveProvider) -> Result<()> {
    let variable = provider.key_file;
    match std::env::var_os(variable) {
        Some(path) if !path.is_empty() => Ok(()),
        _ => Err(format!(
            "{variable} is not set. Set it to the path of a file that holds the API key of \
             `{}` and nothing else. The key itself is never given on a command line or in \
             the environment. Nothing was sent.",
            provider.name
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
    run_ignored(
        &[
            "--package",
            "cortex-llm",
            "--features",
            "http",
            "--test",
            "it",
        ],
        name,
        variables,
    )
}

/// Runs one ignored test of the test binary `selection` names, and shows
/// its output. Fails unless exactly one test ran and passed.
pub(crate) fn run_ignored(selection: &[&str], name: &str, variables: &[(&str, &str)]) -> Result<()> {
    run_ignored_with(selection, name, variables, false)
}

/// Runs one test that is not ignored, and shows its output. Fails unless
/// exactly one test ran and passed: a renamed test is a test that does not
/// run, and cargo calls that a success.
fn run_test(
    selection: &[&str],
    name: &str,
    variables: &[(&str, &str)],
    ignored: bool,
) -> Result<()> {
    let mut arguments = vec!["test", "--locked"];
    arguments.extend_from_slice(selection);
    arguments.push("--");
    if ignored {
        arguments.push("--ignored");
    }
    arguments.extend_from_slice(&["--exact", name, "--nocapture"]);
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
            "the test `{name}` did not run: cargo found no test of that name"
        ))
    }
}

/// [`run_ignored`]; for a dry run, which ends the test's process when it
/// has printed its request, the dry run's last line is what says it ran.
fn run_ignored_with(
    selection: &[&str],
    name: &str,
    variables: &[(&str, &str)],
    dry_run: bool,
) -> Result<()> {
    let mut arguments = vec!["test", "--locked"];
    arguments.extend_from_slice(selection);
    arguments.extend_from_slice(&["--", "--ignored", "--exact", name, "--nocapture"]);
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
    if dry_run {
        return if shown.contains(DRY_RUN_ENDED) {
            Ok(())
        } else {
            Err(format!(
                "the dry run of `{name}` printed no request: the test did not run, or sent \
                 nothing before it ended"
            ))
        };
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

/// `cargo xtask live [PROVIDER]`: the smoke test against a real API.
/// Anthropic's when no provider is named.
pub(crate) fn live(arguments: &[String]) -> Result<()> {
    if arguments.first().is_some_and(|first| first == "--spent") {
        let since = match &arguments[1..] {
            [] => None,
            [flag, date] if flag == "--since" => Some(date.as_str()),
            _ => return Err("usage: cargo xtask live --spent [--since YYYY-MM-DD]".to_owned()),
        };
        let path = ledger()?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("could not read {}: {error}", path.display())),
        };
        println!("The spend ledger, {}:", path.display());
        print!("{}", spent(&text, since));
        return Ok(());
    }
    let provider = match arguments {
        [] => &PROVIDERS[0],
        [name] => live_provider(name)?,
        [_, extra, ..] => {
            return Err(format!(
                "`live` takes one provider at most, and was also given `{extra}`"
            ));
        }
    };
    require_key_file(provider)?;
    let (name, path) = ledger_variable()?;
    ignored_test(provider.smoke, &[(name, &path)])?;
    match provider.headless {
        Some(name) => run_ignored(&["--package", "cortex-cli", "--test", "system"], name, &[]),
        None => Ok(()),
    }
}

/// `cargo xtask record [--provider PROVIDER] [SCENARIO…]`: record fixtures
/// from a real API, Anthropic's when no provider is named. With no scenario
/// named, all of them.
pub(crate) fn record(arguments: &[String]) -> Result<()> {
    let dry_run = arguments.iter().any(|argument| argument == "--dry-run");
    let toolbox = arguments.iter().any(|argument| argument == "--toolbox");
    let arguments: Vec<String> = arguments
        .iter()
        .filter(|argument| *argument != "--dry-run" && *argument != "--toolbox")
        .cloned()
        .collect();
    if toolbox {
        if dry_run {
            // The toolbox scenario is one request each: `--provider P
            // --dry-run toolbox` prints it. A dry run that kept the tools
            // and then recorded would send what it promised not to.
            return Err(
                "`--toolbox` records, and `--dry-run` sends nothing: to see the toolbox request, \
                 run `cargo xtask record --provider PROVIDER --dry-run toolbox`. Nothing was sent."
                    .to_owned(),
            );
        }
        return record_toolbox(&arguments);
    }
    let (provider, scenarios) = record_arguments(&arguments)?;
    let scenarios = scenarios.join(",");
    if dry_run {
        // No key is read and nothing is sent, so none is needed.
        return run_ignored_with(
            &[
                "--package",
                "cortex-llm",
                "--features",
                "http",
                "--test",
                "it",
            ],
            provider.record,
            &[(SCENARIOS, &scenarios), (DRY_RUN, "1")],
            true,
        );
    }
    require_key_file(provider)?;
    let (name, path) = ledger_variable()?;
    ignored_test(provider.record, &[(SCENARIOS, &scenarios), (name, &path)])
}

/// The system test of `cortex-cli` that keeps the tools the binary sends
/// each provider (`docs/testing-and-ci.md`, "The toolbox fixtures").
const TOOLBOX_CAPTURE: &str =
    "tools::sys_the_toolbox_the_binary_sends_is_the_one_its_fixtures_were_recorded_with";

/// `cargo xtask record --toolbox [--provider PROVIDER]`: keeps the tools the
/// binary sends now, and records the toolbox fixture of the provider named,
/// or of every provider whose key file is named in the environment.
fn record_toolbox(arguments: &[String]) -> Result<()> {
    let named = match arguments {
        [] => None,
        [flag, name] if flag == "--provider" => Some(live_provider(name)?),
        _ => return Err("usage: cargo xtask record --toolbox [--provider PROVIDER]".to_owned()),
    };
    let chosen: Vec<&LiveProvider> = match named {
        Some(provider) => {
            require_key_file(provider)?;
            vec![provider]
        }
        None => PROVIDERS
            .iter()
            .filter(|provider| {
                std::env::var_os(provider.key_file).is_some_and(|path| !path.is_empty())
            })
            .collect(),
    };
    if chosen.is_empty() {
        return Err(
            "no provider's key file is named in the environment (CORTEX_LIVE_<PROVIDER>_KEY_FILE), \
             so nothing would be recorded. Nothing was sent."
                .to_owned(),
        );
    }
    // Only the chosen providers' kept tools are written: a provider whose
    // tools changed and that is not recorded keeps the tools of its
    // recording, so its fixture still matches and the change stays a
    // warning, and is not a failure of the blocking tests.
    let names: Vec<&str> = chosen.iter().map(|provider| provider.name).collect();
    println!(
        "Keeping the tools the binary sends {} ...",
        names.join(", ")
    );
    run_test(
        &["--package", "cortex-cli", "--test", "system"],
        TOOLBOX_CAPTURE,
        &[("CORTEX_UPDATE_TOOLBOX", &names.join(","))],
        false,
    )?;
    let (name, path) = ledger_variable()?;
    for provider in &chosen {
        println!("Recording the toolbox fixture of `{}` ...", provider.name);
        ignored_test(provider.record, &[(SCENARIOS, "toolbox"), (name, &path)])?;
    }
    let skipped: Vec<&str> = PROVIDERS
        .iter()
        .filter(|provider| !chosen.iter().any(|chosen| chosen.name == provider.name))
        .map(|provider| provider.name)
        .collect();
    if !skipped.is_empty() {
        println!(
            "Not recorded, for want of a key file: {}. Their fixtures no longer match the kept \
             tools if the tools changed.",
            skipped.join(", ")
        );
    }
    Ok(())
}

/// The provider and the scenarios `record` was asked for.
fn record_arguments(arguments: &[String]) -> Result<(&'static LiveProvider, Vec<&str>)> {
    let mut provider = &PROVIDERS[0];
    let mut scenarios = Vec::new();
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        if argument == "--provider" {
            let name = rest
                .next()
                .ok_or("`--provider` needs the provider's name after it")?;
            provider = live_provider(name)?;
        } else if argument.starts_with('-') {
            return Err(format!(
                "`record` takes `--provider NAME` and scenario names, and was given `{argument}`"
            ));
        } else {
            scenarios.push(argument.as_str());
        }
    }
    Ok((provider, scenarios))
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
        // `cargo fuzz` builds with the nightly compiler that
        // `fuzz/rust-toolchain.toml` names, whatever toolchain this command
        // itself was started with.
        let status = Command::new("cargo")
            // The target is named. `cargo fuzz` otherwise builds for the
            // target it was itself built for, and a prebuilt `cargo-fuzz`
            // is a static binary for a target the fuzzer cannot use: the
            // sanitizer does not work with a statically linked C library.
            .args(["fuzz", "run", "--target", &host, target, "--"])
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
                    "could not start `cargo fuzz`: {error}. It needs the nightly \
                     toolchain that fuzz/rust-toolchain.toml names (`rustup toolchain \
                     install`, run inside fuzz/) and cargo-fuzz (`cargo install cargo-fuzz`)."
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
             and `cargo fuzz run <target> <file>`, run inside fuzz/, runs it again.",
            failed.join(", ")
        ))
    }
}

/// The target the nightly compiler builds for when none is named: the
/// machine's own.
fn nightly_host() -> Result<String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .current_dir(workspace::root().join("fuzz"))
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTC")
        .output()
        .map_err(|error| format!("could not run `rustc -vV` in fuzz/: {error}"))?;
    if !output.status.success() {
        return Err(
            "`rustc -vV` failed in fuzz/. The fuzzer needs the nightly toolchain that \
             fuzz/rust-toolchain.toml names: run `rustup toolchain install` inside fuzz/."
                .to_owned(),
        );
    }
    host_of(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "`rustc -vV` did not name a host".to_owned())
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
    fn a_toolbox_dry_run_is_refused_before_anything_runs() {
        let strings = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        for words in [
            &["--toolbox", "--dry-run"][..],
            &["--dry-run", "--toolbox", "--provider", "deepseek"],
        ] {
            let error = record(&strings(words)).unwrap_err();
            assert!(
                error.contains("`--dry-run` sends nothing"),
                "{words:?}: {error}"
            );
        }
    }

    #[test]
    fn the_ledger_is_totalled_by_provider_and_model() {
        let ledger = r#"{"date":"2026-11-13","provider":"anthropic","model":"claude-haiku","input":100,"cache_read":20,"cache_write":0,"output":30,"usd_nanos":150000,"stated_by_provider":false,"test":"t"}
{"date":"2026-11-14","provider":"anthropic","model":"claude-haiku","input":50,"cache_read":0,"cache_write":0,"output":10,"usd_nanos":50000,"stated_by_provider":false,"test":"t"}
{"date":"2026-11-14","provider":"deepseek","model":"deepseek-chat","input":10,"cache_read":0,"cache_write":0,"output":5,"usd_nanos":7,"stated_by_provider":true,"test":"t"}
not a line of the ledger
"#;
        let all = spent(ledger, None);
        assert!(all.contains("anthropic    claude-haiku"), "{all}");
        assert!(
            all.contains("     2          170         40   0.000200$"),
            "{all}"
        );
        assert!(all.contains("  anthropic: $0.000200\n"), "{all}");
        assert!(all.contains("  deepseek: $0.000000\n"), "{all}");
        assert!(all.contains("  in all: 3 call(s), $0.000200\n"), "{all}");
        assert!(
            all.contains("1 line(s) of the ledger could not be read"),
            "{all}"
        );
        let later = spent(ledger, Some("2026-11-14"));
        assert!(
            later.contains("in all: 2 call(s), $0.000050 since 2026-11-14"),
            "{later}"
        );
        assert!(spent("", None).contains("in all: 0 call(s), $0.000000"));
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
    fn the_live_commands_know_their_providers() {
        let strings = |arguments: &[&str]| -> Vec<String> {
            arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect()
        };
        // No provider named: Anthropic, as before this command took one.
        let arguments = strings(&["text", "fast"]);
        let (provider, scenarios) = record_arguments(&arguments).unwrap();
        assert_eq!(provider.name, "anthropic");
        assert_eq!(scenarios, ["text", "fast"]);
        let arguments = strings(&["text", "--provider", "deepseek", "switch"]);
        let (provider, scenarios) = record_arguments(&arguments).unwrap();
        assert_eq!(provider.name, "deepseek");
        assert_eq!(provider.key_file, "CORTEX_LIVE_DEEPSEEK_KEY_FILE");
        assert_eq!(scenarios, ["text", "switch"]);
        for bad in [&["--provider"][..], &["--provider", "openai"], &["--all"]] {
            assert!(record_arguments(&strings(bad)).is_err(), "{bad:?}");
        }
        // Each provider has a key file of its own, and tests of its own.
        for (at, provider) in PROVIDERS.iter().enumerate() {
            for other in &PROVIDERS[at + 1..] {
                assert_ne!(provider.name, other.name);
                assert_ne!(provider.key_file, other.key_file);
                assert_ne!(provider.smoke, other.smoke);
                assert_ne!(provider.record, other.record);
            }
            assert!(provider.key_file.starts_with("CORTEX_LIVE_"));
            assert!(provider.key_file.ends_with("_KEY_FILE"));
        }
        assert!(live(&strings(&["openrouter", "deepseek"])).is_err());
        assert!(live(&strings(&["nobody"])).is_err());
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
