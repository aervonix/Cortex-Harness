//! Headless runs through the real binary: every exit code of the table in
//! `docs/architecture/10-headless.md`, the three output formats, and
//! standard output carrying the result and nothing else. 130 and 143 are in
//! `signals`.

use std::io::Write;
use std::process::Stdio;

use serde_json::json;

use crate::{Machine, OTHER_MODEL, jsonl, shown};

#[test]
fn sys_exit_0_in_each_format_with_only_the_result_on_standard_output() {
    let machine = Machine::new("sys_exit_0");

    machine.reply_fixture("text");
    let text = machine.run(&["-p", "say hello"]);
    text.expect_code(0);
    assert_eq!(text.stdout, "Hello, test suite!\n", "{}", shown(&text));

    machine.reply_fixture("text");
    let json = machine.run(&["-p", "say hello", "--output", "json"]);
    let record = json.expect_code(0).json();
    assert_eq!(record["type"], "result");
    assert_eq!(record["outcome"], "completed");
    assert_eq!(record["result"], "Hello, test suite!");
    assert_eq!(record["stop_reason"], "end");
    assert_eq!(record["requests"], 1);
    assert_eq!(record["usage"]["input"], 29);
    assert!(record["session_id"].as_str().unwrap().starts_with("s_"));
    assert!(record.get("error").is_none(), "{record}");

    machine.reply_fixture("text");
    let lines = machine.run(&["-p", "say hello", "--output", "jsonl", "--include-deltas"]);
    lines.expect_code(0);
    let lines = jsonl(&lines.stdout);
    assert_eq!(lines[0]["model"], crate::MODEL);
    assert_eq!(lines[0]["session_id"], record_session(&lines));
    let types: Vec<&str> = lines
        .iter()
        .filter_map(|line| line["type"].as_str())
        .collect();
    for wanted in [
        "turn.started",
        "item.delta",
        "item.completed",
        "turn.completed",
    ] {
        assert!(types.contains(&wanted), "{wanted} in {types:?}");
    }
    assert_eq!(lines.last().unwrap()["outcome"], "completed");

    // Without --include-deltas there are none.
    machine.reply_fixture("text");
    let whole = machine.run(&["-p", "say hello", "--output", "jsonl"]);
    let whole = jsonl(&whole.expect_code(0).stdout);
    assert!(
        whole.iter().all(|line| line["type"] != "item.delta"),
        "{whole:?}"
    );
}

fn record_session(lines: &[serde_json::Value]) -> serde_json::Value {
    lines.last().unwrap()["session_id"].clone()
}

#[test]
fn sys_piped_input_comes_before_the_prompt_and_a_tool_call_is_not_on_standard_output() {
    let machine = Machine::new("sys_piped");
    std::fs::write(machine.workspace.join("notes.txt"), "the notes\n").unwrap();
    machine.reply_tool("toolu_1", "read", &json!({"path": "notes.txt"}));
    machine.reply_text("Read it.");
    let mut child = machine
        .command(&[
            "-p",
            "summarise this",
            "--system-append",
            "Answer in haiku.",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"context from a pipe\n")
        .unwrap();
    let run = crate::Run::of(child.wait_with_output().unwrap());
    run.expect_code(0);
    assert_eq!(run.stdout, "Read it.\n", "{}", shown(&run));
    let first = String::from_utf8(machine.server.requests()[0].body.clone()).unwrap();
    assert!(
        first.contains("context from a pipe\\n\\nsummarise this"),
        "{first}"
    );
    assert!(first.contains("Answer in haiku."), "{first}");
}

#[test]
fn sys_exit_2_for_mistakes_and_invalid_configuration_before_anything_is_sent() {
    let machine = Machine::new("sys_exit_2");
    machine.run(&["-p", "x", "--output", "xml"]).expect_code(2);
    machine
        .run(&["-p", "x", "--allow", "not-an-effect:x"])
        .expect_code(2);

    let unknown = machine.run(&["-p", "x", "--model", "no-such-model", "--output", "json"]);
    let record = unknown.expect_code(2).json();
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["error"]["kind"], "config");

    machine.user_config("[limits.turn]\nrequests = \"many\"\n");
    let invalid = machine.run(&["-p", "x", "--output", "json"]);
    let record = invalid.expect_code(2).json();
    assert_eq!(record["error"]["code"], "invalid_configuration");
    assert!(machine.server.requests().is_empty());
}

#[test]
fn sys_exit_3_with_no_key_or_a_rejected_one() {
    let machine = Machine::new("sys_exit_3");
    let no_key = machine
        .command(&["-p", "x", "--output", "json"])
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .unwrap();
    let no_key = crate::Run::of(no_key);
    assert_eq!(no_key.expect_code(3).json()["error"]["kind"], "auth");
    assert!(machine.server.requests().is_empty());

    machine.reply_fixture("error-authentication");
    let rejected = machine.run(&["-p", "x", "--output", "json"]);
    let record = rejected.expect_code(3).json();
    assert_eq!(record["error"]["kind"], "auth");
    // The key never reaches either stream.
    assert!(!rejected.stdout.contains(crate::KEY) && !rejected.stderr.contains(crate::KEY));
}

#[test]
fn sys_exit_4_5_and_8_for_the_provider_the_model_and_the_context() {
    let machine = Machine::new("sys_exit_4_5_8");
    machine.reply_fixture("error-invalid-request");
    let record = machine.run(&["-p", "x", "--output", "json"]);
    assert_eq!(record.expect_code(4).json()["error"]["kind"], "provider");

    machine.reply_fixture("stop-refusal");
    let refused = machine.run(&["-p", "x", "--output", "json"]);
    let record = refused.expect_code(5).json();
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["stop_reason"], "refusal");

    machine.server.push(
        cortex_testkit::ServerReply::status(413)
            .header("content-type", "application/json")
            .send(
                json!({"type": "error", "error": {"type": "request_too_large",
                    "message": "Request exceeds the maximum allowed number of bytes."}})
                .to_string(),
            ),
    );
    let large = machine.run(&["-p", "x", "--output", "json"]);
    assert_eq!(
        large.expect_code(8).json()["error"]["kind"],
        "context_overflow"
    );
}

#[test]
fn sys_exit_6_with_fail_on_denial_and_a_denial_listed_without_it() {
    let machine = Machine::new("sys_exit_6");
    // The project's own configuration directory is protected: a write there
    // needs an answer at every level below `unrestricted`.
    let target = ".cortex/notes.md";
    let write = json!({"path": target, "content": "new\n"});

    // Without the flag the call is refused, the model is told, and the run
    // goes on to its end.
    machine.reply_tool("toolu_1", "write", &write);
    machine.reply_text("I could not write it.");
    let refused = machine.run(&["-p", "write a note", "--output", "json"]);
    let record = refused.expect_code(0).json();
    assert_eq!(record["result"], "I could not write it.");
    assert_eq!(record["files_changed"], json!([]));
    assert_eq!(record["permission_denials"][0]["tool"], "write");
    assert_eq!(record["permission_denials"][0]["call_id"], "toolu_1");
    assert!(!machine.workspace.join(target).exists());

    // With it, the first such call ends the run.
    machine.reply_tool("toolu_2", "write", &write);
    let failed = machine.run(&["-p", "write a note", "--output", "json", "--fail-on-denial"]);
    let record = failed.expect_code(6).json();
    assert_eq!(record["outcome"], "denied");
    assert_eq!(record["permission_denials"][0]["call_id"], "toolu_2");
    assert_eq!(
        machine.server.requests().len(),
        3,
        "nothing was sent after the denial"
    );

    // An allow rule on the command line lets it through.
    machine.reply_tool("toolu_3", "write", &write);
    machine.reply_text("Written.");
    let allowed = machine.run(&[
        "-p",
        "write a note",
        "--fail-on-denial",
        "--allow",
        "write .cortex/notes.md",
    ]);
    allowed.expect_code(0);
    assert_eq!(allowed.stdout, "Written.\n");
    assert!(machine.workspace.join(target).exists());
}

#[test]
fn sys_exit_7_at_a_limit() {
    let machine = Machine::new("sys_exit_7");
    std::fs::write(machine.workspace.join("notes.txt"), "the notes\n").unwrap();
    machine.reply_tool("toolu_1", "read", &json!({"path": "notes.txt"}));
    let run = machine.run(&[
        "-p",
        "read the notes",
        "--max-requests",
        "1",
        "--output",
        "jsonl",
    ]);
    run.expect_code(7);
    let lines = jsonl(&run.stdout);
    assert_eq!(lines.last().unwrap()["outcome"], "limit");
    // A read changes nothing.
    assert_eq!(lines.last().unwrap()["files_changed"], json!([]));
    assert_eq!(machine.server.requests().len(), 1);
}

#[test]
fn sys_exit_1_when_the_harness_itself_fails() {
    // The data directory is under a plain file, so no session can be
    // written: nothing the user typed, nothing a provider said.
    let machine = Machine::new("sys_exit_1");
    let blocker = machine.home.join("blocker");
    std::fs::write(&blocker, "a file").unwrap();
    let data = blocker.join("data");
    machine.user_config(&format!(
        "session.data_dir = {}\n",
        toml_string(data.to_str().unwrap())
    ));
    let run = machine.run(&["-p", "x", "--output", "json"]);
    let record = run.expect_code(1).json();
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["error"]["kind"], "internal");
    assert!(machine.server.requests().is_empty());
}

#[test]
fn sys_continue_resumes_the_latest_session_and_no_session_keeps_none() {
    let machine = Machine::new("sys_continue");
    machine.reply_text("First.");
    let first = machine.run(&["-p", "one", "--output", "json"]);
    let session = first.expect_code(0).json()["session_id"].clone();

    machine.reply_text("Second.");
    let second = machine.run(&["-p", "two", "--continue", "--output", "json"]);
    assert_eq!(second.expect_code(0).json()["session_id"], session);
    // The model was sent the first exchange.
    let body = String::from_utf8(machine.server.requests()[1].body.clone()).unwrap();
    assert!(body.contains("First.") && body.contains("two"), "{body}");

    machine.reply_text("Third.");
    let fresh = machine.run(&["-p", "three", "--no-session", "--output", "json"]);
    assert_ne!(fresh.expect_code(0).json()["session_id"], session);
    let listed = machine.run(&["sessions", "list", "--json"]);
    assert_eq!(
        listed.expect_code(0).stdout.lines().count(),
        1,
        "{}",
        shown(&listed)
    );
    assert!(listed.stdout.contains(session.as_str().unwrap()));

    // The flags that choose a model and a speed are the user's own.
    machine.reply_text("Fourth.");
    let chosen = machine.run(&[
        "-p",
        "four",
        "--model",
        OTHER_MODEL,
        "--speed",
        "fast",
        "--output",
        "jsonl",
    ]);
    let lines = jsonl(&chosen.expect_code(0).stdout);
    assert_eq!(lines[0]["model"], OTHER_MODEL);
    let body = String::from_utf8(machine.server.requests()[3].body.clone()).unwrap();
    assert!(body.contains("\"speed\":\"fast\""), "{body}");
}

/// A string as TOML writes it.
pub fn toml_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}
