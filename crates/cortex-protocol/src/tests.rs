//! Tests of the wire form.
//!
//! The golden transcript (`tests/golden/v1-session.jsonl`) is a session
//! written by hand in protocol version 1: every method, every event type and
//! every variant of the unions that travel in them. It is the record of what
//! version 1 looks like on the wire. A change that makes these tests fail has
//! changed the wire form, which is either a mistake or a new protocol
//! version: the transcript is not edited to make a test pass.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::error::{ErrorData, ErrorKind, code};
use crate::event::{Delta, Event, Notice};
use crate::item::{Item, StopReason};
use crate::jsonrpc::{self, ErrorObject, Message, Notification, Request, RequestId, Response};
use crate::lease::{Grant, Lease, Level, Rule, StopCondition};
use crate::message::Part;
use crate::methods::{
    ApprovalDetail, InitializeParams, MethodError, Op, OpResult, ServerNotification, ServerRequest,
    ServerResponse, TurnInterruptParams, negotiate_version,
};
use crate::model::ModelInfo;
use crate::project::PlanEdit;
use crate::speed::Speed;
use crate::usage::RequestUsage;
use crate::{PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, SessionEvent};

const TRANSCRIPT: &str = include_str!("../tests/golden/v1-session.jsonl");

/// Values of the types that do not travel inside a method or an event (a
/// lease, a conversation message, the less common approval details), one per
/// line as `{"type": …, "value": …}`. With the transcript, every type of the
/// protocol is in a golden file; a test in `xtask` checks that none is missing.
const VALUES: &str = include_str!("../tests/golden/v1-values.jsonl");

fn transcript() -> Vec<Value> {
    TRANSCRIPT
        .lines()
        .map(|line| serde_json::from_str(line).expect("a line of the transcript is JSON"))
        .collect()
}

/// Reads every line into its typed form and writes it back as JSON, the way
/// a client and a server together would: requests by method name, responses
/// by the method of the request with the same id.
fn replay(lines: &[Value]) -> Vec<Value> {
    let mut asked_by_client: BTreeMap<RequestId, &'static str> = BTreeMap::new();
    let mut asked_by_server: BTreeMap<RequestId, &'static str> = BTreeMap::new();
    let mut written = Vec::new();

    for line in lines {
        let message: Message = serde_json::from_value(line.clone())
            .unwrap_or_else(|error| panic!("{error}\n  in {line}"));
        let again: Message = match message {
            Message::Request(request) => {
                let Request { id, method, params } = request;
                if Op::METHODS.contains(&method.as_str()) {
                    let op = Op::from_wire(&method, params)
                        .unwrap_or_else(|error| panic!("{error}\n  in {line}"));
                    asked_by_client.insert(id.clone(), op.method());
                    op.to_request(id).unwrap().into()
                } else {
                    let request = ServerRequest::from_wire(&method, params)
                        .unwrap_or_else(|error| panic!("{error}\n  in {line}"));
                    asked_by_server.insert(id.clone(), request.method());
                    request.to_request(id).unwrap().into()
                }
            }
            Message::Response(response) => {
                let id = response
                    .id
                    .clone()
                    .expect("a response in the transcript has an id");
                let result = match response.result {
                    Ok(result) => Ok(if let Some(method) = asked_by_client.remove(&id) {
                        OpResult::from_wire(method, result)
                            .unwrap_or_else(|error| panic!("{error}\n  in {line}"))
                            .result()
                            .unwrap()
                    } else {
                        let method = asked_by_server
                            .remove(&id)
                            .unwrap_or_else(|| panic!("no request has the id {id}"));
                        ServerResponse::from_wire(method, result)
                            .unwrap_or_else(|error| panic!("{error}\n  in {line}"))
                            .result()
                            .unwrap()
                    }),
                    Err(error) => {
                        let data = error.error_data().expect("an application error has data");
                        assert_eq!(error.code, jsonrpc::code::APPLICATION_ERROR);
                        Err(ErrorObject::application(&data))
                    }
                };
                Response {
                    id: Some(id),
                    result,
                }
                .into()
            }
            Message::Notification(notification) => {
                match ServerNotification::from_wire(notification)
                    .unwrap_or_else(|error| panic!("{error}\n  in {line}"))
                {
                    ServerNotification::Event(event) => event.to_notification().unwrap().into(),
                    ServerNotification::Unknown(notification) => notification.into(),
                }
            }
        };
        written.push(serde_json::to_value(&again).unwrap());
    }
    written
}

#[test]
fn the_golden_transcript_round_trips_exactly() {
    let lines = transcript();
    let written = replay(&lines);
    for (line, again) in lines.iter().zip(&written) {
        assert_eq!(again, line, "\n  read:    {line}\n  written: {again}");
    }
}

#[test]
fn the_golden_values_round_trip_exactly() {
    for line in VALUES.lines() {
        let entry: Value = serde_json::from_str(line).expect("a line of the values file is JSON");
        let value = entry["value"].clone();
        match entry["type"].as_str() {
            Some("Lease") => drop(round_trip::<Lease>(value)),
            Some("Message") => drop(round_trip::<crate::Message>(value)),
            Some("ApprovalDetail") => drop(round_trip::<ApprovalDetail>(value)),
            Some("TurnInterruptParams") => {
                let params = round_trip::<TurnInterruptParams>(value);
                assert!(params.now, "the transcript has the ordinary interrupt");
            }
            other => panic!("the values file names a type this test does not read: {other:?}"),
        }
    }
}

/// Every string found under `key` in objects whose own key in their parent
/// (or whose array's key) is one of `parents`.
fn tags(value: &Value, parents: &[&str], key: &str, parent: &str, found: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if parents.contains(&parent)
                && let Some(Value::String(tag)) = map.get(key)
            {
                found.insert(tag.clone());
            }
            for (name, child) in map {
                tags(child, parents, key, name, found);
            }
        }
        Value::Array(items) => {
            for child in items {
                tags(child, parents, key, parent, found);
            }
        }
        _ => {}
    }
}

fn missing(known: &[&str], found: &BTreeSet<String>) -> Vec<String> {
    known
        .iter()
        .filter(|tag| !found.contains(**tag))
        .map(|tag| (*tag).to_owned())
        .collect()
}

#[test]
fn the_golden_transcript_covers_every_method_event_and_variant() {
    let lines = transcript();
    let all = Value::Array(lines.clone());

    let methods: BTreeSet<String> = lines
        .iter()
        .filter(|line| line.get("id").is_some())
        .filter_map(|line| line.get("method").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    assert_eq!(missing(Op::METHODS, &methods), Vec::<String>::new());
    assert_eq!(
        missing(ServerRequest::METHODS, &methods),
        Vec::<String>::new()
    );

    let collect = |parents: &[&str], key: &str| {
        let mut found = BTreeSet::new();
        tags(&all, parents, key, "", &mut found);
        found
    };
    let none = Vec::<String>::new();
    assert_eq!(missing(Event::KNOWN, &collect(&["params"], "type")), none);
    assert_eq!(
        missing(Item::KNOWN, &collect(&["item", "items"], "kind")),
        none
    );
    // A `tool_result` part belongs to a message sent to a provider. Front
    // ends see tool results as items, so the transcript has none, and
    // `unions_not_in_the_transcript_round_trip` covers it.
    assert_eq!(
        missing(
            Part::KNOWN,
            &collect(&["parts", "content", "input"], "kind")
        ),
        vec!["tool_result".to_owned()]
    );
    assert_eq!(missing(Delta::KNOWN, &collect(&["delta"], "kind")), none);
    assert_eq!(missing(PlanEdit::KNOWN, &collect(&["edits"], "kind")), none);

    let notices: BTreeSet<String> = lines
        .iter()
        .filter_map(|line| line.get("params"))
        .filter(|params| params.get("type").and_then(Value::as_str) == Some("notice"))
        .filter_map(|params| params.get("kind").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    assert_eq!(missing(Notice::KNOWN, &notices), none);
}

/// Free-form JSON: a field added inside one of these is data, and is kept.
const FREE_FORM: [&str; 4] = ["config", "arguments", "payload", "details"];

/// Enforced values: a field added inside one of these makes the value
/// invalid. `enforced_values_fail_closed` and its neighbours cover them.
const ENFORCED: [&str; 1] = ["rule"];

/// Adds a field no version knows to every object the protocol defines.
fn add_unknown_fields(value: &mut Value, depth: usize) {
    match value {
        Value::Object(map) => {
            for (name, child) in map.iter_mut() {
                if name == "roles" {
                    // A map from role to settings: its keys are data, and
                    // its values are objects the protocol defines.
                    for settings in child
                        .as_object_mut()
                        .into_iter()
                        .flat_map(|roles| roles.values_mut())
                    {
                        add_unknown_fields(settings, depth + 2);
                    }
                } else if !FREE_FORM.contains(&name.as_str()) && !ENFORCED.contains(&name.as_str())
                {
                    add_unknown_fields(child, depth + 1);
                }
            }
            // Not at the top: the JSON-RPC envelope is covered by its own test.
            if depth > 0 {
                map.insert("x_added_later".to_owned(), json!({ "nested": [1, "two"] }));
            }
        }
        Value::Array(items) => {
            for child in items {
                add_unknown_fields(child, depth + 1);
            }
        }
        _ => {}
    }
}

#[test]
fn unknown_fields_are_ignored_in_every_object() {
    let lines = transcript();
    let mut newer = lines.clone();
    for line in &mut newer {
        // An error's `data` is read leniently, so it is checked separately.
        if line.get("error").is_none() {
            add_unknown_fields(line, 0);
        }
    }
    let written = replay(&newer);
    for (line, again) in lines.iter().zip(&written) {
        assert_eq!(again, line);
    }

    // The envelope too.
    let request: Message = serde_json::from_value(
        json!({"jsonrpc": "2.0", "id": 1, "method": "shutdown", "params": {}, "trace": "abc"}),
    )
    .unwrap();
    assert!(matches!(request, Message::Request(_)));
    let error: ErrorData = serde_json::from_value(
        json!({"kind": "auth", "code": "no_key", "message": "No key.", "hint": "added later"}),
    )
    .unwrap();
    assert_eq!(error, ErrorData::new(ErrorKind::Auth, "no_key", "No key."));
}

/// Reads `json` as `T`, checks that it writes back as the same JSON, and
/// returns the value.
fn round_trip<T: DeserializeOwned + Serialize>(json: Value) -> T {
    let value: T =
        serde_json::from_value(json.clone()).unwrap_or_else(|error| panic!("{error}\n  in {json}"));
    assert_eq!(serde_json::to_value(&value).unwrap(), json);
    value
}

/// For each open enumeration: every known value reads to a known variant
/// and writes back as itself, and an unknown value is kept.
macro_rules! check_open_enums {
    ($($name:ty),+ $(,)?) => {$(
        for wire in <$name>::KNOWN {
            let value: $name = round_trip(json!(wire));
            assert!(value.is_known(), "{} {wire}", stringify!($name));
            assert_eq!(value.as_str(), *wire);
            assert_eq!(value.to_string(), *wire);
            assert_eq!(<$name>::from_wire(wire), value);
        }
        let later: $name = round_trip(json!("a_value_added_later"));
        assert!(!later.is_known(), "{}", stringify!($name));
        assert_eq!(later.as_str(), "a_value_added_later");
        assert!(
            serde_json::from_value::<$name>(json!(7)).is_err(),
            "{} must be a string",
            stringify!($name)
        );
    )+};
}

#[test]
fn every_enumeration_is_open_and_keeps_unknown_values() {
    use crate::{event, item, lease, message, methods, model, project, session, speed, usage};
    check_open_enums!(
        ErrorKind,
        event::Actor,
        event::CompactionState,
        event::ItemKind,
        event::LimitKind,
        event::Resolution,
        event::TurnOutcome,
        event::Verification,
        item::Delivery,
        item::NoticeSource,
        item::PlanStatus,
        item::StopReason,
        item::ToolOutcome,
        lease::Effect,
        lease::Level,
        message::Role,
        methods::ApprovalChoice,
        methods::ApprovalScope,
        methods::ClientRole,
        methods::ComponentState,
        methods::Disposition,
        methods::TrustResourceKind,
        model::Modality,
        model::ModelRole,
        model::ReasoningControlKind,
        project::CommandPurpose,
        project::EntryAction,
        project::EntryBasis,
        project::EntryCheckKind,
        project::EntryKind,
        project::EntryScope,
        project::EntrySourceKind,
        project::EntryStatus,
        session::SandboxStatus,
        speed::CooldownState,
        speed::Speed,
        speed::SpeedEffect,
        speed::SpeedFallbackBy,
        speed::SpeedFallbackReason,
        speed::SpeedUnavailable,
        usage::Billing,
        usage::CostBasis,
        usage::LimitMeasure,
        usage::LimitSource,
        usage::UsageGroup,
        usage::UsageRange,
    );
    assert_eq!(Level::ReadOnly.as_str(), "read-only");
}

#[test]
fn an_unknown_enumeration_value_inside_a_message_survives_a_round_trip() {
    let event: SessionEvent = round_trip(json!({
        "session_id": "s_1", "seq": 9, "turn_id": "t_1", "type": "turn.completed",
        "outcome": "handed_over", "stop_reason": "budget_pause",
        "usage": {"tokens": {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0}, "requests": 0},
    }));
    let Event::TurnCompleted(completed) = event.event else {
        panic!("expected turn.completed");
    };
    assert_eq!(completed.outcome.as_str(), "handed_over");
    assert_eq!(
        completed.stop_reason,
        Some(StopReason::Unknown("budget_pause".to_owned()))
    );
}

#[test]
fn unknown_union_variants_are_kept_whole() {
    // An event type added later.
    let event: SessionEvent = round_trip(json!({
        "session_id": "s_1", "seq": 3, "type": "team.updated", "members": [{"id": "e_1"}],
    }));
    let Event::Unknown(unknown) = &event.event else {
        panic!("expected an unknown event");
    };
    assert_eq!(unknown.tag, "team.updated");
    assert_eq!(unknown.fields["members"], json!([{"id": "e_1"}]));
    assert_eq!(event.event.tag(), "team.updated");

    // A notice kind added later can still be shown.
    let event: SessionEvent = round_trip(json!({
        "session_id": "s_1", "seq": 4, "type": "notice", "kind": "lease_expiring",
        "message": "The lease ends in 5 minutes.", "minutes": 5,
    }));
    let Event::Notice(notice) = &event.event else {
        panic!("expected a notice");
    };
    assert!(matches!(notice, Notice::Unknown(_)));
    assert_eq!(notice.message(), Some("The lease ends in 5 minutes."));

    // An item kind and a part kind added later, inside known ones.
    let event: SessionEvent = round_trip(json!({
        "session_id": "s_1", "seq": 5, "type": "item.completed",
        "item": {"kind": "escalation", "id": "i_9", "question": "Merge now?"},
    }));
    let Event::ItemCompleted(completed) = &event.event else {
        panic!("expected item.completed");
    };
    assert_eq!(completed.item.id(), Some("i_9"));
    assert_eq!(completed.item.tag(), "escalation");

    let part: Part =
        round_trip(json!({"kind": "audio", "media_type": "audio/ogg", "artifact": "a_1"}));
    assert!(matches!(part, Part::Unknown(_)));
    let delta: Delta = round_trip(json!({"kind": "image", "bytes": 12}));
    assert_eq!(delta.tag(), "image");
    let _: PlanEdit = round_trip(json!({"kind": "split", "id": "p1", "into": 2}));
    let _: ApprovalDetail = round_trip(json!({"kind": "merge", "branch": "main"}));
}

#[test]
fn a_malformed_known_variant_is_an_error_that_names_it() {
    let missing_field = serde_json::from_value::<SessionEvent>(
        json!({"session_id": "s_1", "seq": 1, "type": "item.delta", "item_id": "i_1"}),
    )
    .unwrap_err()
    .to_string();
    assert!(
        missing_field.contains("Event `item.delta`"),
        "{missing_field}"
    );
    assert!(missing_field.contains("delta"), "{missing_field}");

    let wrong_type = serde_json::from_value::<Part>(json!({"kind": "text", "text": 5}))
        .unwrap_err()
        .to_string();
    assert!(wrong_type.contains("Part `text`"), "{wrong_type}");

    let no_tag = serde_json::from_value::<Part>(json!({"text": "hello"}))
        .unwrap_err()
        .to_string();
    assert!(no_tag.contains("missing field `kind`"), "{no_tag}");

    let tag_not_a_string = serde_json::from_value::<Part>(json!({"kind": 1}))
        .unwrap_err()
        .to_string();
    assert!(
        tag_not_a_string.contains("`kind` must be a string"),
        "{tag_not_a_string}"
    );
}

#[test]
fn unions_not_in_the_transcript_round_trip() {
    let _: ApprovalDetail =
        round_trip(json!({"kind": "diff", "path": "src/lib.rs", "diff": "@@ -1 +1 @@"}));
    let _: ApprovalDetail =
        round_trip(json!({"kind": "target", "effect": "network", "target": "crates.io"}));
    let _: ApprovalDetail = round_trip(json!({"kind": "tool", "arguments": {"query": "x"}}));

    let lease: Lease = round_trip(json!({
        "id": "l_task1",
        "grant": {
            "level": "guided",
            "rules": [
                {"effect": "exec", "match": "cargo test *", "action": "allow"},
                {"effect": "write", "match": "**/.env*", "action": "deny"},
                {"effect": "read", "match": "~/notes/**", "action": "ask"},
            ],
            "sandbox": {
                "read": {"all": true},
                "write": {"all": false, "roots": ["/work/example", "/tmp/cortex-s1"]},
                "protect": {"all": false, "roots": ["/work/example/.git/hooks"]},
                "network": "off",
            },
            "models": ["claude-opus-5-5"],
            "budget": {"usd_nanos": 2_000_000_000_u64, "tokens": 500_000, "requests": 200, "wall_clock_ms": 1_800_000},
            "limits": {"merge_to_main": false, "team_size": 4},
        },
        "stop_when": [
            {"kind": "verification_failed", "rounds": 3},
            {"kind": "out_of_scope"},
            {"kind": "spending", "usd_nanos": 1_500_000_000_u64},
            {"kind": "time", "at": "2026-11-12T11:00:00Z"},
        ],
        "expires": {"kind": "at", "at": "2026-11-12T11:00:00Z"},
        "granted_by": {"kind": "parent", "lease_id": "l_standing"},
    }));
    assert_eq!(lease.grant.level, Level::Guided);
    assert_eq!(lease.grant.rules[0].pattern, "cargo test *");

    // The standing lease of an ordinary session, with nothing optional.
    let _: Lease = round_trip(json!({
        "id": "l_standing",
        "grant": {
            "level": "supervised",
            "sandbox": {"read": {"all": true}, "write": {"all": false}, "protect": {"all": false}, "network": "full"},
            "budget": {},
            "limits": {"merge_to_main": false},
        },
        "expires": {"kind": "end_of_session"},
        "granted_by": {"kind": "user"},
    }));

    let message: crate::Message = round_trip(json!({
        "role": "tool",
        "parts": [{
            "kind": "tool_result", "call_id": "toolu_01", "is_error": true,
            "content": [{"kind": "text", "text": "No such file."}],
        }],
    }));
    assert_eq!(message.role, crate::Role::Tool);
}

/// A lease this version understands in full, to change one thing in.
fn a_lease() -> Value {
    json!({
        "id": "l_task1",
        "grant": {
            "level": "guided",
            "rules": [{"effect": "exec", "match": "cargo test *", "action": "allow"}],
            "sandbox": {
                "read": {"all": true},
                "write": {"all": false, "roots": ["/work/example"]},
                "protect": {"all": false},
                "network": "off",
            },
            "budget": {"usd_nanos": 2_000_000_000_u64},
            "limits": {"merge_to_main": false},
        },
        "stop_when": [{"kind": "verification_failed", "rounds": 3}],
        "expires": {"kind": "end_of_task"},
        "granted_by": {"kind": "user"},
    })
}

/// Sets the value at a JSON pointer, creating the last key if it is new.
fn with(mut lease: Value, pointer: &str, value: Value) -> Value {
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer has a slash");
    let parent = lease
        .pointer_mut(parent)
        .unwrap_or_else(|| panic!("no {parent}"));
    match parent {
        Value::Object(map) => drop(map.insert(key.to_owned(), value)),
        Value::Array(items) => items[key.parse::<usize>().expect("an index")] = value,
        _ => panic!("{pointer} is not inside an object or an array"),
    }
    lease
}

/// The lease is refused whole, with the code a client is told to expect,
/// and the message says what was not understood.
fn assert_refused(lease: Value, expected: &str) {
    let error = Lease::from_wire(lease.clone()).expect_err(&format!("accepted: {lease}"));
    assert_eq!(error.kind, ErrorKind::Permission);
    assert_eq!(error.code, code::LEASE_NOT_UNDERSTOOD);
    assert!(
        error.message.contains(expected),
        "{}\n  for {lease}",
        error.message
    );
    // The plain deserializer refuses it too: there is no lenient way in.
    assert!(serde_json::from_value::<Lease>(lease).is_err());
}

#[test]
fn a_lease_this_version_understands_is_read() {
    let lease = Lease::from_wire(a_lease()).unwrap();
    assert_eq!(lease.grant.level, Level::Guided);
    assert_eq!(serde_json::to_value(&lease).unwrap(), a_lease());
}

#[test]
fn a_lease_with_an_unknown_restriction_field_is_refused() {
    // A newer build added a restriction. Dropping the field would widen the
    // lease, so every place a restriction can sit refuses a field it does
    // not know.
    for pointer in [
        "/max_files_changed",
        "/grant/deny_paths",
        "/grant/rules/0/unless",
        "/grant/sandbox/hosts",
        "/grant/sandbox/write/except",
        "/grant/budget/tool_calls",
        "/grant/limits/no_force_push",
        "/stop_when/0/within_ms",
        "/expires/grace_ms",
        "/granted_by/on_behalf_of",
    ] {
        let name = pointer.rsplit('/').next().unwrap();
        assert_refused(
            with(a_lease(), pointer, json!(1)),
            &format!("unknown field `{name}`"),
        );
    }
}

#[test]
fn a_lease_with_an_unknown_restriction_value_is_refused() {
    for (pointer, value, expected) in [
        // A kind of stop condition added later: never skipped.
        (
            "/stop_when/0",
            json!({"kind": "files_changed", "more_than": 50}),
            "unknown StopCondition `files_changed`",
        ),
        (
            "/expires/kind",
            json!("end_of_day"),
            "unknown ExpiryKind `end_of_day`",
        ),
        (
            "/granted_by/kind",
            json!("manager"),
            "unknown GrantorKind `manager`",
        ),
        (
            "/grant/rules/0/action",
            json!("allow_once"),
            "unknown RuleAction `allow_once`",
        ),
        (
            "/grant/sandbox/network",
            json!("allow_list"),
            "unknown NetworkAccess `allow_list`",
        ),
    ] {
        assert_refused(with(a_lease(), pointer, value), expected);
    }
}

#[test]
fn a_grant_with_an_unknown_scope_is_refused() {
    // What a grant covers: its level, the effects its rules are about, and
    // its sandbox policy. Each is open or tolerant elsewhere in the
    // protocol, and refused here.
    assert_refused(
        with(a_lease(), "/grant/level", json!("delegated")),
        "unknown Level `delegated`",
    );
    assert_refused(
        with(a_lease(), "/grant/rules/0/effect", json!("clipboard")),
        "unknown Effect `clipboard`",
    );

    let grant = with(a_lease(), "/grant/scope", json!("organisation"))["grant"].take();
    let error = Grant::from_wire(grant).unwrap_err();
    assert_eq!(error.code, code::LEASE_NOT_UNDERSTOOD);
    assert!(
        error.message.contains("unknown field `scope`"),
        "{}",
        error.message
    );

    let error =
        Rule::from_wire(json!({"effect": "exec", "match": "*", "action": "allow", "for": "team"}))
            .unwrap_err();
    assert_eq!(error.code, code::LEASE_NOT_UNDERSTOOD);
    assert!(
        error.message.contains("unknown field `for`"),
        "{}",
        error.message
    );

    // The same level and effect stay open where they only inform.
    let state: Value = json!({"kind": "target", "effect": "clipboard", "target": "x"});
    let _: ApprovalDetail = round_trip(state);
    assert_eq!(Level::from_wire("delegated").as_str(), "delegated");
}

#[test]
fn a_rule_in_a_message_is_enforced_too() {
    // `permissions/list` carries rules. A field added to one is not ignored.
    let result = json!({"approvals": [{
        "id": "ap_1",
        "rule": {"effect": "exec", "match": "cargo build *", "action": "allow", "unless": "ci"},
        "scope": "session", "created_at": "2026-11-12T10:15:12Z",
    }]});
    let error = OpResult::from_wire("permissions/list", result)
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown field `unless`"), "{error}");
}

#[test]
fn the_closed_enumerations_have_no_way_to_hold_an_unknown_value() {
    use crate::lease::{ExpiryKind, GrantorKind, NetworkAccess, RuleAction};
    macro_rules! check {
        ($($name:ty),+) => {$(
            for wire in <$name>::KNOWN {
                let value: $name = round_trip(json!(wire));
                assert_eq!(value.as_str(), *wire);
                assert_eq!(<$name>::from_wire(wire), Some(value));
            }
            assert_eq!(<$name>::from_wire("a_value_added_later"), None);
            assert!(serde_json::from_value::<$name>(json!("a_value_added_later")).is_err());
        )+};
    }
    check!(ExpiryKind, GrantorKind, NetworkAccess, RuleAction);
    assert!(serde_json::from_value::<StopCondition>(json!({"kind": "later"})).is_err());
}

#[test]
fn usage_carries_the_speed_a_request_ran_at() {
    // `standard` is the default and is left out.
    let standard: RequestUsage = round_trip(json!({
        "provider": "anthropic", "model": "claude-opus-5-5", "role": "main",
        "tokens": {"input": 10, "cache_read": 0, "cache_write": 0, "output": 5},
        "billing": "metered",
    }));
    assert_eq!(standard.speed, Speed::Standard);
    assert_eq!(standard.speed_requested, None);

    let fast: RequestUsage = round_trip(json!({
        "provider": "anthropic", "model": "claude-opus-5-5", "role": "main", "speed": "fast",
        "tokens": {"input": 10, "cache_read": 0, "cache_write": 0, "output": 5},
        "cost": {"usd_nanos": 280_000, "basis": "metered"},
        "billing": "metered",
    }));
    assert_eq!(fast.speed, Speed::Fast);

    // A tier with a provider's own name is a speed like any other.
    let flex: RequestUsage = round_trip(json!({
        "provider": "example", "model": "example-budget-model", "role": "fast", "speed": "flex",
        "tokens": {"input": 10, "cache_read": 0, "cache_write": 0, "output": 5},
        "billing": "unknown",
    }));
    assert_eq!(flex.speed, Speed::Other("flex".to_owned()));
}

#[test]
fn model_info_says_which_speeds_a_model_offers() {
    let models = transcript()
        .into_iter()
        .find(|line| line.pointer("/result/models").is_some())
        .and_then(|mut line| line.pointer_mut("/result/models").map(Value::take))
        .expect("the transcript has a models/list result");
    let models: Vec<ModelInfo> = serde_json::from_value(models).unwrap();

    let opus = &models[0];
    assert!(opus.offers(&Speed::Standard));
    assert!(opus.offers(&Speed::Fast));
    assert!(!opus.offers(&Speed::from_wire("flex")));
    let offer = opus.speed_offer(&Speed::Fast).unwrap();
    assert_eq!(offer.shares_cache, Some(false));
    // Fast is priced from its own rate card: twice the standard one here.
    let (standard, fast) = (
        opus.prices.as_ref().unwrap(),
        offer.prices.as_ref().unwrap(),
    );
    assert_eq!(fast.input, 2 * standard.input);
    assert_eq!(fast.output, 2 * standard.output);

    let local = &models[1];
    assert!(local.speeds.is_empty());
    assert!(local.offers(&Speed::Standard));
    assert!(!local.offers(&Speed::Fast));
}

#[test]
fn json_rpc_messages_are_told_apart_by_their_members() {
    let request: Message =
        round_trip(json!({"jsonrpc": "2.0", "id": 7, "method": "turn/start", "params": {"a": 1}}));
    assert!(matches!(
        request,
        Message::Request(Request {
            id: RequestId::Number(7),
            ..
        })
    ));

    let string_id: Request =
        round_trip(json!({"jsonrpc": "2.0", "id": "srv-1", "method": "question/ask"}));
    assert_eq!(string_id.id, RequestId::from("srv-1"));
    assert_eq!(string_id.params, None);

    let notification: Notification =
        round_trip(json!({"jsonrpc": "2.0", "method": "session/event", "params": {}}));
    assert_eq!(notification.method, "session/event");

    // `null` is a result, and is not the same as no result.
    let null_result: Response = round_trip(json!({"jsonrpc": "2.0", "id": 7, "result": null}));
    assert_eq!(null_result.result, Ok(Value::Null));

    // A request too malformed to have an id is answered with a null id.
    let no_id: Response = round_trip(json!({
        "jsonrpc": "2.0", "id": null,
        "error": {"code": jsonrpc::code::PARSE_ERROR, "message": "The line was not JSON."},
    }));
    assert_eq!(no_id.id, None);
    assert_eq!(no_id.result.unwrap_err().code, -32700);
}

#[test]
fn malformed_json_rpc_messages_are_refused() {
    for (message, expected) in [
        (
            json!({"id": 1, "method": "shutdown"}),
            "`jsonrpc` must be \"2.0\"",
        ),
        (
            json!({"jsonrpc": "1.0", "id": 1, "method": "shutdown"}),
            "`jsonrpc` must be \"2.0\"",
        ),
        (
            json!({"jsonrpc": "2.0", "id": null, "method": "shutdown"}),
            "must be a number or a string",
        ),
        (
            json!({"jsonrpc": "2.0", "id": 1.5, "method": "shutdown"}),
            "a whole number or a string",
        ),
        (
            json!({"jsonrpc": "2.0", "id": 1, "result": {}, "error": {"code": 1, "message": "x"}}),
            "not both",
        ),
        (
            json!({"jsonrpc": "2.0", "id": 1}),
            "needs `result` or `error`",
        ),
        (json!({"jsonrpc": "2.0"}), "needs `method`"),
    ] {
        let error = serde_json::from_value::<Message>(message.clone())
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{message}: {error}");
    }

    let wrong_kind = serde_json::from_value::<Request>(json!({"jsonrpc": "2.0", "method": "x"}))
        .unwrap_err()
        .to_string();
    assert!(
        wrong_kind.contains("expected a request, found a notification"),
        "{wrong_kind}"
    );
}

#[test]
fn a_method_is_read_by_name_with_errors_a_client_can_act_on() {
    // Absent parameters are an empty object.
    assert_eq!(
        Op::from_wire("shutdown", None),
        Ok(Op::Shutdown(crate::methods::Empty {}))
    );
    assert_eq!(
        Op::from_wire("models/list", Some(Value::Null)),
        Ok(Op::ModelsList(crate::methods::Empty {}))
    );

    let invalid = Op::from_wire("session/open", None).unwrap_err();
    assert_eq!(
        invalid,
        MethodError::Invalid {
            method: "session/open",
            message: "missing field `session_id`".to_owned()
        }
    );
    assert_eq!(
        invalid.to_error_object().code,
        jsonrpc::code::INVALID_PARAMS
    );

    let unknown = Op::from_wire("team/status", Some(json!({}))).unwrap_err();
    assert_eq!(
        unknown,
        MethodError::UnknownMethod("team/status".to_owned())
    );
    assert_eq!(
        unknown.to_error_object().code,
        jsonrpc::code::METHOD_NOT_FOUND
    );
    assert_eq!(unknown.to_string(), "unknown method `team/status`");

    // `null` for an optional field is the same as leaving it out.
    let op = Op::from_wire(
        "turn/start",
        Some(json!({"session_id": "s_1", "input": [], "delivery": null})),
    )
    .unwrap();
    let Op::TurnStart(params) = op else {
        panic!("expected turn/start");
    };
    assert_eq!(params.delivery, None);
}

#[test]
fn an_unknown_notification_method_is_not_an_error() {
    let later = Notification {
        method: "team/event".to_owned(),
        params: Some(json!({"anything": true})),
    };
    assert_eq!(
        ServerNotification::from_wire(later.clone()),
        Ok(ServerNotification::Unknown(later))
    );

    let broken = Notification {
        method: "session/event".to_owned(),
        params: Some(json!({"seq": 1})),
    };
    let error = ServerNotification::from_wire(broken)
        .unwrap_err()
        .to_string();
    assert!(error.contains("`session/event`"), "{error}");
}

#[test]
fn the_version_is_the_newest_one_both_sides_speak() {
    let client = |newest: u32, oldest: Option<u32>| -> InitializeParams {
        serde_json::from_value(json!({
            "protocol_version": newest, "min_protocol_version": oldest,
            "client": {"name": "test", "version": "1"}, "role": "observer",
        }))
        .unwrap()
    };
    assert_eq!(SUPPORTED_PROTOCOL_VERSIONS, &[PROTOCOL_VERSION]);
    assert_eq!(
        negotiate_version(&client(1, None), SUPPORTED_PROTOCOL_VERSIONS),
        Some(1)
    );
    // A newer client that still speaks version 1.
    assert_eq!(negotiate_version(&client(3, Some(1)), &[1, 2]), Some(2));
    assert_eq!(negotiate_version(&client(3, Some(1)), &[1]), Some(1));
    // Nothing in common.
    assert_eq!(negotiate_version(&client(3, Some(2)), &[1]), None);
    assert_eq!(negotiate_version(&client(3, None), &[1, 2]), None);
    // A range written backwards is read as the one version.
    assert_eq!(negotiate_version(&client(1, Some(4)), &[1, 2]), Some(1));

    // No capability is assumed.
    let capabilities = client(1, None).capabilities;
    assert!(!capabilities.approvals && !capabilities.questions && !capabilities.deltas);

    let refused = ErrorObject::application(&ErrorData {
        details: json!({"supported": SUPPORTED_PROTOCOL_VERSIONS})
            .as_object()
            .cloned(),
        ..ErrorData::new(
            ErrorKind::Usage,
            code::UNSUPPORTED_PROTOCOL_VERSION,
            "No version in common.",
        )
    });
    assert_eq!(
        refused.error_data().unwrap().details.unwrap()["supported"],
        json!([1])
    );
}

#[test]
fn raw_provider_json_is_carried_as_text_and_never_reparsed() {
    // Key order, spacing and escapes that a parser would normalise.
    let raw = "{\"type\":\"thinking\", \"z\":1,\"a\":1e2,\"s\":\"caf\\u00e9\"}";
    let part: Part = round_trip(json!({"kind": "reasoning", "text": "", "raw": raw}));
    let Part::Reasoning(reasoning) = &part else {
        panic!("expected reasoning");
    };
    assert_eq!(reasoning.raw.as_ref().unwrap().as_str(), raw);
    assert!(
        serde_json::to_string(&part)
            .unwrap()
            .contains("caf\\\\u00e9")
    );
}
