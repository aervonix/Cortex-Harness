//! Runs of the whole harness against a real model (tier 5): the real
//! binary, the provider's real endpoint, a one-line task with one tool
//! call, and `--output json`. They cost money, so they are ignored, and only
//! `cargo xtask live` runs them, by hand and never in CI: Anthropic's on
//! `anthropic-messages`, and DeepSeek's on `openai-chat`, each with the
//! whole built-in toolbox.
//!
//! The API key is read from the file that `CORTEX_LIVE_<PROVIDER>_KEY_FILE`
//! names and handed to the binary's environment, as a user's would be. It
//! is never printed: the test fails if it shows up in either stream.

use std::fs;

use crate::{Machine, Run, shown};

#[test]
#[ignore = "talks to Anthropic's API and costs money: `cargo xtask live` runs it"]
fn live_headless_run() {
    // The cheapest Anthropic model in the built-in catalog.
    headless(
        "CORTEX_LIVE_ANTHROPIC_KEY_FILE",
        "ANTHROPIC_API_KEY",
        "model.main = \"claude-haiku-5-5\"\nmodel.effort = \"low\"\n",
    );
}

#[test]
#[ignore = "talks to DeepSeek's API and costs money: `cargo xtask live deepseek` runs it"]
fn live_headless_run_deepseek() {
    headless(
        "CORTEX_LIVE_DEEPSEEK_KEY_FILE",
        "DEEPSEEK_API_KEY",
        "model.main = \"deepseek-flash\"\n",
    );
}

fn headless(key_file: &str, variable: &str, model: &str) {
    let file = std::env::var_os(key_file)
        .unwrap_or_else(|| panic!("{key_file} names the file the API key is in"));
    let key = fs::read_to_string(file).expect("the key file can be read");
    let key = key.trim();
    assert!(!key.is_empty(), "the key file holds a key");

    let machine = Machine::new("live_headless_run");
    // The real endpoint: no base URL of the test's own.
    machine.raw_user_config(&format!(
        "session.durability = \"never\"\ncredentials.store = \"file\"\n{model}"
    ));
    fs::write(machine.workspace.join("answer.txt"), "forty-two\n").unwrap();

    let output = machine
        .command(&[
            "-p",
            "Read the file answer.txt with the read tool, then reply with its content and \
             nothing else.",
            "--output",
            "json",
            "--max-requests",
            "4",
        ])
        .env(variable, key)
        .output()
        .unwrap();
    let run = Run::of(output);
    assert!(
        !run.stdout.contains(key) && !run.stderr.contains(key),
        "the key was printed"
    );
    eprintln!("cortex -p … --output json wrote:\n{}", run.stdout);
    eprintln!("and on standard error:\n{}", run.stderr);
    let record = run.expect_code(0).json();
    assert_eq!(record["outcome"], "completed", "{}", shown(&run));
    assert_eq!(record["tool_calls"], 1, "{}", shown(&run));
    assert_eq!(
        record["files_changed"],
        serde_json::json!([]),
        "a read changes nothing"
    );
    assert!(
        record["result"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("forty-two"),
        "{}",
        shown(&run)
    );
}
