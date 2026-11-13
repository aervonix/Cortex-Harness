//! Approvals at three scopes, and the project's stored file
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Approvals").

use cortex_base::private;
use cortex_policy::{ApprovalError, ApprovalStore, Engine};
use cortex_protocol::ids::ApprovalId;
use cortex_protocol::lease::{Effect, RuleAction};
use cortex_protocol::methods::{ApprovalChoice, ApprovalScope};

use crate::Said::{Allow, Ask, Deny};
use crate::{Scratch, clock, engine, engine_with, question, rule, said, shell, user};

fn store(scratch: &Scratch) -> ApprovalStore {
    ApprovalStore::new(scratch.dir.join("approvals").join("aaaa1111.toml"), clock())
}

fn with_store(scratch: &Scratch) -> Engine {
    let store = store(scratch);
    let approved = store.load().unwrap();
    engine_with(|parts| {
        parts.approved = approved;
        parts.store = Some(store);
    })
}

#[test]
fn allow_once_adds_nothing() {
    let engine = engine();
    let asked = question(shell(&engine, "cargo build"));
    let recorded = engine.record(&asked, &ApprovalChoice::AllowOnce).unwrap();
    assert!(recorded.allowed);
    assert!(recorded.added.is_empty());
    assert!(engine.approvals().is_empty());
    assert_eq!(said(&shell(&engine, "cargo build")), Ask, "asked again next time");
}

#[test]
fn deny_and_an_answer_nobody_knows_refuse_and_add_nothing() {
    let engine = engine();
    let asked = question(shell(&engine, "cargo build"));
    for choice in [
        ApprovalChoice::Deny,
        ApprovalChoice::Unknown("allow_forever".to_owned()),
    ] {
        let recorded = engine.record(&asked, &choice).unwrap();
        assert!(!recorded.allowed, "{choice:?}");
        assert!(recorded.added.is_empty());
    }
    assert!(engine.approvals().is_empty());
    assert_eq!(said(&shell(&engine, "cargo build")), Ask);
}

#[test]
fn an_approval_for_the_session_covers_the_program_and_its_subcommand() {
    let engine = engine();
    let asked = question(shell(&engine, "cargo build --release"));
    let recorded = engine.record(&asked, &ApprovalChoice::AllowSession).unwrap();
    assert!(recorded.allowed);
    assert_eq!(recorded.added.len(), 1);
    assert_eq!(recorded.added[0].scope, ApprovalScope::Session);
    assert_eq!(recorded.added[0].rule, rule(Effect::Exec, "cargo build *", RuleAction::Allow));

    // The same program and subcommand, with other arguments: allowed.
    assert_eq!(said(&shell(&engine, "cargo build")), Allow);
    assert_eq!(said(&shell(&engine, "cargo build -p cortex-policy")), Allow);
    assert_eq!(said(&shell(&engine, "ls && cargo build")), Allow);
    // Another subcommand, another program: not.
    assert_eq!(said(&shell(&engine, "cargo run")), Ask);
    assert_eq!(said(&shell(&engine, "cargo publish")), Ask);
    assert_eq!(said(&shell(&engine, "./cargo build")), Ask);
    // And nothing the analysis does not model rides on it.
    assert_eq!(said(&shell(&engine, "cargo build $(rm -rf x)")), Ask);
    assert_eq!(said(&shell(&engine, "cargo build; rm -rf x")), Ask);
    assert_eq!(said(&shell(&engine, "cargo build > ~/.bashrc")), Ask);
    assert_eq!(said(&shell(&engine, "FOO=1 cargo build")), Ask);

    assert_eq!(engine.approvals(), recorded.added);
    // A session approval lives in memory: another engine has none of it.
    assert_eq!(said(&shell(&self::engine(), "cargo build")), Ask);
}

#[test]
fn an_approval_never_lifts_a_rule_that_holds_back() {
    let engine = engine_with(|parts| {
        parts.rules = vec![
            user(Effect::Exec, "cargo publish *", RuleAction::Deny),
            user(Effect::Exec, "cargo yank *", RuleAction::Ask),
        ];
    });
    // Approve `cargo *`-shaped things for the session.
    for command in ["cargo build", "cargo test"] {
        let asked = question(shell(&engine, command));
        engine.record(&asked, &ApprovalChoice::AllowSession).unwrap();
    }
    assert_eq!(said(&shell(&engine, "cargo build")), Allow);
    assert_eq!(said(&shell(&engine, "cargo publish")), Deny);
    // A question an `ask` rule raised offers only "once": answering "for
    // the session" allows this call and adds nothing.
    let asked = question(shell(&engine, "cargo yank x"));
    assert!(asked.lifts.is_empty());
    assert!(!asked.choices().contains(&ApprovalChoice::AllowSession));
    let before = engine.approvals().len();
    let recorded = engine.record(&asked, &ApprovalChoice::AllowSession).unwrap();
    assert!(recorded.allowed && recorded.added.is_empty());
    assert_eq!(engine.approvals().len(), before);
    assert_eq!(said(&shell(&engine, "cargo yank x")), Ask);
}

#[test]
fn an_approval_for_the_project_is_stored_owner_only_and_read_back() {
    let scratch = Scratch::new("approvals_project");
    let engine = with_store(&scratch);
    let asked = question(shell(&engine, "npm run build && npm test"));
    let recorded = engine.record(&asked, &ApprovalChoice::AllowProject).unwrap();
    assert!(recorded.allowed);
    let patterns: Vec<&str> = recorded
        .added
        .iter()
        .map(|approval| approval.rule.pattern.as_str())
        .collect();
    assert_eq!(patterns, ["npm run *", "npm test *"]);
    assert_eq!(recorded.added[0].id, ApprovalId::new("ap_1"));
    assert_eq!(recorded.added[0].scope, ApprovalScope::Project);
    assert_eq!(recorded.added[0].created_at.as_str(), "2026-11-13T10:00:00Z");
    // In force at once.
    assert_eq!(said(&shell(&engine, "npm run lint")), Allow);

    // The file: private, and in the documented form.
    let file = store(&scratch);
    assert!(private::is_private(file.path()).unwrap(), "the file is owner-only");
    let text = std::fs::read_to_string(file.path()).unwrap();
    assert!(text.contains("version = 1"), "{text}");
    assert!(text.contains("[[approval]]"), "{text}");
    assert!(text.contains("id = \"ap_1\""), "{text}");
    assert!(text.contains("match = \"npm run *\""), "{text}");
    assert!(text.contains("action = \"allow\""), "{text}");

    // A new session of the same project starts with them, trusted or not:
    // they are the user's own.
    let untrusted = {
        let store = store(&scratch);
        let approved = store.load().unwrap();
        engine_with(|parts| {
            parts.trusted = false;
            parts.approved = approved;
            parts.store = Some(store);
        })
    };
    assert_eq!(said(&shell(&untrusted, "npm test")), Allow);
    assert_eq!(untrusted.approvals().len(), 2);
    assert_eq!(said(&shell(&untrusted, "npm publish")), Ask);

    // The same approval again is the one already stored.
    let again = question(shell(&self::engine(), "npm test"));
    let stored = file.add(&again.lifts).unwrap();
    assert_eq!(stored[0].id, ApprovalId::new("ap_2"));
    assert_eq!(file.load().unwrap().len(), 2);
}

#[test]
fn approvals_are_listed_and_taken_back_by_id() {
    let scratch = Scratch::new("approvals_revoke");
    let engine = with_store(&scratch);
    let project = question(shell(&engine, "make -j4"));
    engine.record(&project, &ApprovalChoice::AllowProject).unwrap();
    let session = question(shell(&engine, "cargo build"));
    engine.record(&session, &ApprovalChoice::AllowSession).unwrap();

    let listed = engine.approvals();
    let ids: Vec<&str> = listed.iter().map(|approval| approval.id.as_str()).collect();
    assert_eq!(ids, ["ap_1", "ap_s1"]);
    assert_eq!(said(&shell(&engine, "make")), Allow);
    assert_eq!(said(&shell(&engine, "cargo build")), Allow);

    assert_eq!(engine.revoke(&[ApprovalId::new("ap_s1")]).unwrap(), 1);
    assert_eq!(said(&shell(&engine, "cargo build")), Ask);
    assert_eq!(said(&shell(&engine, "make")), Allow);

    assert_eq!(engine.revoke(&[ApprovalId::new("ap_1"), ApprovalId::new("ap_9")]).unwrap(), 1);
    assert_eq!(said(&shell(&engine, "make")), Ask);
    assert!(engine.approvals().is_empty());
    assert!(store(&scratch).load().unwrap().is_empty(), "gone from the file too");
    // With none left, the numbering starts again.
    let next = store(&scratch)
        .add(&[rule(Effect::Exec, "make *", RuleAction::Allow)])
        .unwrap();
    assert_eq!(next[0].id, ApprovalId::new("ap_1"));
}

#[test]
fn without_a_store_always_allow_allows_this_call_only() {
    let engine = engine();
    let asked = question(shell(&engine, "make"));
    let recorded = engine.record(&asked, &ApprovalChoice::AllowProject).unwrap();
    assert!(recorded.allowed && recorded.added.is_empty());
    assert_eq!(said(&shell(&engine, "make")), Ask);
}

/// A stored file this build does not understand in full grants nothing,
/// and is not overwritten.
#[test]
fn a_stored_file_that_is_not_understood_grants_nothing() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "unknown key",
            "version = 1\n[[approval]]\nid = \"ap_1\"\ncreated_at = \"2026-11-13T10:00:00Z\"\n\
             until = \"forever\"\nrule = { effect = \"exec\", match = \"make *\", action = \"allow\" }\n",
            "until",
        ),
        (
            "unknown key in the rule",
            "version = 1\n[[approval]]\nid = \"ap_1\"\ncreated_at = \"2026-11-13T10:00:00Z\"\n\
             rule = { effect = \"exec\", match = \"make *\", action = \"allow\", except = \"x\" }\n",
            "except",
        ),
        (
            "unknown effect",
            "version = 1\n[[approval]]\nid = \"ap_1\"\ncreated_at = \"2026-11-13T10:00:00Z\"\n\
             rule = { effect = \"teleport\", match = \"*\", action = \"allow\" }\n",
            "teleport",
        ),
        (
            "unknown action",
            "version = 1\n[[approval]]\nid = \"ap_1\"\ncreated_at = \"2026-11-13T10:00:00Z\"\n\
             rule = { effect = \"exec\", match = \"*\", action = \"permit\" }\n",
            "permit",
        ),
        (
            "an approval that does not allow",
            "version = 1\n[[approval]]\nid = \"ap_1\"\ncreated_at = \"2026-11-13T10:00:00Z\"\n\
             rule = { effect = \"exec\", match = \"*\", action = \"deny\" }\n",
            "does not allow",
        ),
        (
            "newer version",
            "version = 2\n[[approval]]\nid = \"ap_1\"\ncreated_at = \"2026-11-13T10:00:00Z\"\n\
             rule = { effect = \"exec\", match = \"make *\", action = \"allow\" }\n",
            "version is 2",
        ),
        ("unknown top-level key", "version = 1\nscope = \"everything\"\n", "scope"),
        (
            "no version",
            "[[approval]]\nid = \"ap_1\"\ncreated_at = \"2026-11-13T10:00:00Z\"\n\
             rule = { effect = \"exec\", match = \"make *\", action = \"allow\" }\n",
            "version",
        ),
        ("not TOML", "this is not = = toml [[[", ""),
    ];
    for (name, contents, mentions) in cases {
        let scratch = Scratch::new("approvals_strict");
        let file = scratch.file("approvals/aaaa1111.toml", contents);
        let store = store(&scratch);
        let error = store.load().unwrap_err();
        assert!(matches!(error, ApprovalError::NotUnderstood { .. }), "{name}: {error}");
        assert!(error.to_string().contains(mentions), "{name}: {error}");
        assert!(error.to_string().contains("none of them applies"), "{name}: {error}");
        // Adding to it fails too, and leaves it as it was.
        let added = store.add(&[rule(Effect::Exec, "make *", RuleAction::Allow)]);
        assert!(added.is_err(), "{name}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), *contents, "{name}");
    }
    // A file with nothing wrong reads, with or without a byte-order mark.
    let scratch = Scratch::new("approvals_strict_ok");
    scratch.file(
        "approvals/aaaa1111.toml",
        "\u{feff}version = 1\r\n[[approval]]\r\nid = \"ap_7\"\r\ncreated_at = \"2026-11-13T10:00:00Z\"\r\n\
         rule = { effect = \"exec\", match = \"make *\", action = \"allow\" }\r\n",
    );
    let loaded = store(&scratch).load().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, ApprovalId::new("ap_7"));
    // A missing file holds none.
    assert!(store(&Scratch::new("approvals_missing")).load().unwrap().is_empty());
}

#[test]
fn the_store_refuses_to_store_anything_but_an_allow() {
    let scratch = Scratch::new("approvals_only_allow");
    let store = store(&scratch);
    let error = store
        .add(&[
            rule(Effect::Exec, "make *", RuleAction::Allow),
            rule(Effect::Exec, "rm *", RuleAction::Deny),
        ])
        .unwrap_err();
    assert!(error.to_string().contains("can only allow"), "{error}");
    assert!(!store.path().exists(), "nothing was written");
}

/// A stored approval that somebody edited into something else is not what
/// the user approved: only allows are taken from the list an engine is
/// given.
#[test]
fn an_engine_takes_only_allows_from_the_approvals_it_is_given() {
    use cortex_protocol::ids::Timestamp;
    use cortex_protocol::methods::StoredApproval;
    let approval = |id: &str, action| StoredApproval {
        id: ApprovalId::new(id),
        rule: rule(Effect::Exec, "make *", action),
        scope: ApprovalScope::Project,
        created_at: Timestamp::new("2026-11-13T10:00:00Z"),
    };
    let engine = engine_with(|parts| {
        parts.approved = vec![approval("ap_1", RuleAction::Deny)];
    });
    assert_eq!(said(&shell(&engine, "make")), Ask, "neither allowed nor denied by it");
    let engine = engine_with(|parts| {
        parts.approved = vec![approval("ap_1", RuleAction::Allow)];
    });
    assert_eq!(said(&shell(&engine, "make")), Allow);
}

#[test]
fn approving_a_path_covers_that_path() {
    let engine = engine();
    let asked = question(crate::read(&engine, "/etc/hosts"));
    engine.record(&asked, &ApprovalChoice::AllowSession).unwrap();
    assert_eq!(said(&crate::read(&engine, "/etc/hosts")), Allow);
    assert_eq!(said(&crate::read(&engine, "/etc/passwd")), Ask);
    assert_eq!(said(&crate::write(&engine, "/etc/hosts")), Ask);
    // The command that reads it is now on the safe list.
    assert_eq!(said(&shell(&engine, "cat /etc/hosts")), crate::Said::ReadOnly);
    assert_eq!(said(&shell(&engine, "cat /etc/passwd")), Ask);
    // The harness's own files are not opened by approving one: there is no
    // question to approve, only a denial.
    assert_eq!(
        said(&crate::read(&engine, "/home/dev/.config/cortex-harness/credentials.toml")),
        Deny
    );
}
