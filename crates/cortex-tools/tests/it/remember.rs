//! Contract tests for `remember`: every row of its section in
//! `docs/architecture/05-tools.md`.

use std::sync::Arc;

use cortex_protocol::item::ToolOutcome;
use cortex_protocol::project::{EntryCheck, EntryCheckKind, EntryKind};
use cortex_tools::{
    Concurrency, MemoryProposals, Proposal, ProposalError, ProposalOutcome, Proposals, RememberTool,
};
use serde_json::{Value, json};

use crate::{Workspace, failed, ok};

fn tool(proposals: &MemoryProposals) -> RememberTool {
    RememberTool::new(Arc::new(proposals.clone()))
}

fn convention() -> Value {
    json!({
        "kind": "convention",
        "text": "Tests never sleep: time is the fake clock.",
        "evidence": "docs/testing-and-ci.md, \"No real waiting in tests\"",
    })
}

#[test]
fn a_proposal_is_stored_as_proposed_with_where_it_was_learned() {
    let ws = Workspace::new("remember_stored");
    ws.file("docs/testing-and-ci.md", "No real waiting in tests\n");
    let proposals = MemoryProposals::new();
    let mut args = convention();
    args["check"] = json!({
        "kind": "contains",
        "path": "./docs/testing-and-ci.md",
        "pattern": "No real waiting",
    });
    let result = ws.call_with(&ws.cx().with_turn(4), &tool(&proposals), args);
    assert_eq!(
        ok(&result),
        "Proposed for the project's memory, as a convention. It is not in use: it waits for \
         the user's review, and is used only if they accept it."
    );
    assert_eq!(
        proposals.proposals(),
        [Proposal {
            kind: EntryKind::Convention,
            text: "Tests never sleep: time is the fake clock.".to_owned(),
            evidence: "docs/testing-and-ci.md, \"No real waiting in tests\"".to_owned(),
            check: Some(EntryCheck {
                kind: EntryCheckKind::Contains,
                // Relative to the workspace root, as the resolver has it.
                path: Some("docs/testing-and-ci.md".to_owned()),
                pattern: Some("No real waiting".to_owned()),
                command: None,
            }),
            turn: 4,
        }]
    );
    let payload = result.payload().expect("a payload for front ends");
    assert_eq!(payload["outcome"], "stored");
    assert_eq!(payload["kind"], "convention");
    assert_eq!(payload["check"], true);

    // The same entry again adds nothing, and says so.
    let result = ws.call(&tool(&proposals), convention());
    assert!(ok(&result).contains("Nothing was added."));
    assert_eq!(result.payload().unwrap()["outcome"], "already_known");
    assert_eq!(proposals.proposals().len(), 1);

    // A fact, with no check.
    ok(&ws.call(
        &tool(&proposals),
        json!({"kind": "fact", "text": "  CI runs on two systems.  ", "evidence": "the user"}),
    ));
    let stored = proposals.proposals();
    assert_eq!(stored[1].kind, EntryKind::Fact);
    assert_eq!(stored[1].text, "CI runs on two systems.", "trimmed");
    assert_eq!(stored[1].check, None);
}

#[test]
fn the_call_touches_nothing_outside_the_session_and_says_that_it_changes_the_session() {
    let ws = Workspace::new("remember_plan");
    let proposals = MemoryProposals::new();
    let tool = tool(&proposals);
    let plan = ws.plan(&tool, convention()).unwrap();
    assert!(plan.effects.is_empty(), "nothing for the permission engine");
    assert_eq!(plan.concurrency, Concurrency::ReadOnly);
    assert!(!plan.has_side_effects());
    assert!(plan.changes_session());
    assert_eq!(
        plan.summary,
        "Propose a convention for the project's memory"
    );
    assert!(proposals.proposals().is_empty(), "planning stored nothing");
}

#[test]
fn every_form_of_check_is_stored_and_a_check_that_is_not_one_is_refused() {
    let ws = Workspace::new("remember_checks");
    ws.file("Cargo.toml", "[workspace]\n");
    let proposals = MemoryProposals::new();
    let with_check = |check: Value| {
        let mut args = convention();
        args["text"] = json!(format!("An entry with the check {check}"));
        args["check"] = check;
        ws.call(&tool(&proposals), args)
    };
    ok(&with_check(json!({"kind": "exists", "path": "Cargo.toml"})));
    ok(&with_check(
        json!({"kind": "command", "command": "cargo metadata --offline"}),
    ));
    // The path of a check need not exist yet. It must be in the workspace.
    ok(&with_check(
        json!({"kind": "exists", "path": "docs/later.md"}),
    ));
    let stored = proposals.proposals();
    assert_eq!(
        stored[0].check,
        Some(EntryCheck {
            kind: EntryCheckKind::Exists,
            path: Some("Cargo.toml".to_owned()),
            pattern: None,
            command: None,
        })
    );
    assert_eq!(
        stored[1].check.as_ref().unwrap().command.as_deref(),
        Some("cargo metadata --offline")
    );

    for (check, wanted) in [
        (
            json!({"kind": "exists"}),
            "A check of kind `exists` needs `path`.",
        ),
        (
            json!({"kind": "contains", "path": "Cargo.toml"}),
            "A check of kind `contains` needs `pattern`.",
        ),
        (
            json!({"kind": "command"}),
            "A check of kind `command` needs `command`.",
        ),
        (
            json!({"kind": "exists", "path": "Cargo.toml", "command": "ls"}),
            "A check of kind `exists` takes `path`, and not `command`.",
        ),
        (
            json!({"kind": "contains", "path": "a", "pattern": "b", "command": "ls"}),
            "A check of kind `contains` takes `path` and `pattern`, and not `command`.",
        ),
        (
            json!({"kind": "command", "command": "ls", "path": "x"}),
            "A check of kind `command` takes `command`, and not `path`.",
        ),
    ] {
        assert_eq!(failed(&with_check(check)), wanted);
    }
    assert!(failed(&with_check(json!({"kind": "runs"}))).contains("kind"));
    let outside = failed(&with_check(
        json!({"kind": "exists", "path": "../elsewhere"}),
    ));
    assert!(outside.contains("is outside the workspace"), "{outside}");
    assert_eq!(proposals.proposals().len(), 3, "none of those was stored");
}

#[test]
fn the_arguments_are_checked_when_the_call_is_planned() {
    let ws = Workspace::new("remember_arguments");
    let proposals = MemoryProposals::new();
    let refused = |args: Value| failed(&ws.call(&tool(&proposals), args));
    assert!(refused(json!({"kind": "fact", "text": "x"})).contains("evidence"));
    assert!(
        refused(json!({"kind": "command", "text": "x", "evidence": "y"})).contains("kind"),
        "only a fact or a convention is proposed"
    );
    assert_eq!(
        refused(json!({"kind": "fact", "text": "   ", "evidence": "y"})),
        "`text` is empty."
    );
    let long = "word ".repeat(200);
    assert_eq!(
        refused(json!({"kind": "fact", "text": long, "evidence": "y"})),
        "`text` is 999 characters long, and may be up to 600. Say it more briefly."
    );
    assert!(proposals.proposals().is_empty());
}

/// A store that cannot store.
struct Full;

impl Proposals for Full {
    fn propose(&self, _proposal: Proposal) -> Result<ProposalOutcome, ProposalError> {
        Err(ProposalError::new("the project's store is read-only"))
    }
}

#[test]
fn a_proposal_that_cannot_be_stored_or_was_cancelled_says_so() {
    let ws = Workspace::new("remember_failed");
    let message = failed(&ws.call(&RememberTool::new(Arc::new(Full)), convention()));
    assert_eq!(
        message,
        "The proposal could not be stored: the project's store is read-only. Nothing was added."
    );

    let proposals = MemoryProposals::new();
    let cancel = cortex_base::CancelToken::new();
    cancel.cancel();
    let result = ws.call_with(
        &ws.cx().with_cancel(cancel),
        &tool(&proposals),
        convention(),
    );
    assert_eq!(result.outcome(), &ToolOutcome::Interrupted);
    assert!(proposals.proposals().is_empty());
}
