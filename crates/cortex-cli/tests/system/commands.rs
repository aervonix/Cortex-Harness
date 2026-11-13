//! The subcommands, through the real binary: configuration, credentials,
//! models, and a project trusted through `permissions trust` (step 13a's
//! second carried-over point: the pin covers every rule of the project, so
//! the next start reads it as trusted).

use std::io::Write;
use std::process::Stdio;

use serde_json::Value;

use crate::{MODEL, Machine, OTHER_MODEL, Run, jsonl, shown};

/// The model a headless run's session started on, and its standard error.
fn model_of_a_run(machine: &Machine) -> (String, String) {
    machine.reply_text("Done.");
    let run = machine.run(&["-p", "go", "--output", "jsonl"]);
    run.expect_code(0);
    let lines: Vec<Value> = jsonl(&run.stdout);
    (lines[0]["model"].as_str().unwrap().to_owned(), run.stderr)
}

fn trust_line(machine: &Machine) -> String {
    let list = machine.run(&["permissions", "list"]);
    list.expect_code(0);
    list.stdout.lines().next().unwrap().to_owned()
}

#[test]
fn sys_a_project_trusted_with_permissions_trust_is_trusted_on_the_next_start() {
    let machine = Machine::new("sys_trust");
    // A dearer model, which waits for trust, and rules of all three
    // actions: the pin covers each of them.
    let rules = "[[permissions.rules]]\neffect = \"exec\"\nmatch = \"make test\"\naction = \
                 \"allow\"\n\n[[permissions.rules]]\neffect = \"exec\"\nmatch = \"make \
                 deploy\"\naction = \"deny\"\n\n[[permissions.rules]]\neffect = \"read\"\nmatch \
                 = \"secret/**\"\naction = \"ask\"\n";
    machine.project_config(&format!("[model]\nmain = \"{OTHER_MODEL}\"\n\n{rules}"));

    assert!(trust_line(&machine).ends_with(": not trusted"));
    let (model, stderr) = model_of_a_run(&machine);
    assert_eq!(model, MODEL, "an untrusted project's model waits");
    assert!(
        stderr.contains("model.main"),
        "what waits is said: {stderr}"
    );

    // --trust-project trusts it for one run, and pins nothing.
    machine.reply_text("Done.");
    let once = machine.run(&["-p", "go", "--trust-project", "--output", "jsonl"]);
    assert_eq!(jsonl(&once.expect_code(0).stdout)[0]["model"], OTHER_MODEL);
    assert!(trust_line(&machine).ends_with(": not trusted"));

    let trusted = machine.run(&["permissions", "trust"]);
    trusted.expect_code(0);
    for expected in [
        "allow exec make test",
        "deny exec make deploy",
        "ask read secret/**",
        "model.main",
    ] {
        assert!(
            trusted.stdout.contains(expected),
            "{expected}: {}",
            shown(&trusted)
        );
    }
    assert!(trust_line(&machine).ends_with(": trusted"));
    let (model, stderr) = model_of_a_run(&machine);
    assert_eq!(model, OTHER_MODEL, "trusted on the next start: {stderr}");
    assert!(!stderr.contains("rules have changed"), "{stderr}");

    // A rule the user has not seen sets the project's settings aside
    // again, until the change is approved.
    machine.project_config(&format!(
        "[model]\nmain = \"{OTHER_MODEL}\"\n\n{rules}\n[[permissions.rules]]\neffect = \
         \"exec\"\nmatch = \"curl *\"\naction = \"allow\"\n"
    ));
    assert!(trust_line(&machine).contains("rules have changed"));
    assert_eq!(model_of_a_run(&machine).0, MODEL);
    let approved = machine.run(&["permissions", "trust"]);
    assert!(
        approved.expect_code(0).stdout.starts_with("Approved"),
        "{}",
        shown(&approved)
    );
    assert_eq!(model_of_a_run(&machine).0, OTHER_MODEL);

    machine.run(&["permissions", "untrust"]).expect_code(0);
    assert!(trust_line(&machine).ends_with(": not trusted"));
    assert_eq!(model_of_a_run(&machine).0, MODEL);
}

#[test]
fn sys_config_shows_and_checks_what_a_session_would_load() {
    let machine = Machine::new("sys_config");
    let show = machine.run(&["config", "show"]);
    assert!(
        show.expect_code(0)
            .stdout
            .contains(&format!("model.main = \"{MODEL}\"")),
        "{}",
        shown(&show)
    );
    let origin = machine.run(&["config", "show", "--origin"]);
    assert!(
        origin.expect_code(0).stdout.contains("# user: "),
        "{}",
        shown(&origin)
    );
    let layer = machine.run(&["config", "show", "--layer", "user"]);
    assert!(layer.expect_code(0).stdout.contains("retry.attempts = 1"));
    machine
        .run(&["config", "show", "--layer", "nowhere"])
        .expect_code(2);
    let defaults = machine.run(&["config", "defaults"]);
    assert!(
        defaults
            .expect_code(0)
            .stdout
            .contains("config_version = 1")
    );
    machine.run(&["config", "check"]).expect_code(0);

    // An unknown key is a warning to a run, and an error to `check`.
    machine.user_config("model.mian = \"x\"\n");
    let shown_anyway = machine.run(&["config", "show"]);
    assert!(shown_anyway.expect_code(0).stderr.contains("mian"));
    let checked = machine.run(&["config", "check"]);
    assert!(
        checked.expect_code(2).stderr.contains("mian"),
        "{}",
        shown(&checked)
    );
}

#[test]
fn sys_a_key_stored_with_auth_login_is_used_and_never_shown() {
    let machine = Machine::new("sys_auth");
    let status = machine.run(&["auth", "status"]);
    assert!(
        status
            .expect_code(0)
            .stdout
            .contains("anthropic: key from the environment variable ANTHROPIC_API_KEY"),
        "{}",
        shown(&status)
    );
    // Nothing on standard input is no key.
    machine.run(&["auth", "login", "anthropic"]).expect_code(2);
    // A provider nobody knows is refused, not stored under a typo.
    let typo = machine.run(&["auth", "logout", "anthropc"]);
    assert!(
        typo.expect_code(2).stderr.contains("anthropic"),
        "{}",
        shown(&typo)
    );

    let stored = "sk-ant-stored-1111";
    let mut login = machine
        .command(&["auth", "login", "anthropic"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    login
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{stored}\n").as_bytes())
        .unwrap();
    let login = Run::of(login.wait_with_output().unwrap());
    login.expect_code(0);

    let without_variable = |arguments: &[&str]| {
        Run::of(
            machine
                .command(arguments)
                .env_remove("ANTHROPIC_API_KEY")
                .output()
                .unwrap(),
        )
    };
    let status = without_variable(&["auth", "status"]);
    assert!(
        status
            .expect_code(0)
            .stdout
            .contains("anthropic: key from the")
    );
    machine.reply_text("Done.");
    let run = without_variable(&["-p", "go"]);
    run.expect_code(0);
    assert_eq!(
        machine.server.requests()[0].header("x-api-key"),
        Some(stored)
    );
    for output in [&login, &status, &run] {
        assert!(!output.stdout.contains(stored) && !output.stderr.contains(stored));
    }

    let logout = machine.run(&["auth", "logout", "anthropic"]);
    assert!(logout.expect_code(0).stdout.starts_with("Removed"));
    let status = without_variable(&["auth", "status"]);
    assert!(status.expect_code(0).stdout.contains("anthropic: no key"));
}

#[test]
fn sys_models_lists_the_catalog() {
    let machine = Machine::new("sys_models");
    let models = machine.run(&["models"]);
    let out = &models.expect_code(0).stdout;
    assert!(out.contains(MODEL) && out.contains(OTHER_MODEL), "{out}");
}
