//! The tools as the real binary declares them to a provider in strict mode
//! (`docs/architecture/04-providers.md`, "Tools"), found by the first live
//! run: Anthropic refused the built-in toolbox's `maxItems`.
//!
//! Each test runs the binary against the scripted server and reads the
//! request it sent: the real toolbox, through the real request builder.

use serde_json::Value;

use crate::{Machine, shown};

/// The keywords a strict variant must never be sent.
const ANTHROPIC_REFUSES: &[&str] = &[
    "minimum",
    "exclusiveMinimum",
    "maximum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];
const OPENAI_REFUSES: &[&str] = &[
    "minLength",
    "maxLength",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];

/// Every keyword of `refused` found anywhere in `schema`, with where, and
/// every object that is not closed (`additionalProperties: false`), which
/// both strict modes require.
fn found(schema: &Value, refused: &[&str], at: &str, out: &mut Vec<String>) {
    match schema {
        Value::Object(object) => {
            let is_object = object.get("type") == Some(&Value::from("object"))
                || object.contains_key("properties");
            if is_object && object.get("additionalProperties") != Some(&Value::Bool(false)) {
                out.push(format!("{at}: an object that is not closed"));
            }
            for (key, value) in object {
                // A property may be named like a keyword: the names under
                // `properties` are not keywords.
                if key != "properties" && refused.contains(&key.as_str()) {
                    if key == "minItems" && value.as_u64().is_some_and(|n| n <= 1) {
                        continue;
                    }
                    out.push(format!("{at}.{key}"));
                }
                if key == "properties" {
                    for (name, property) in value.as_object().into_iter().flatten() {
                        found(property, refused, &format!("{at}.properties.{name}"), out);
                    }
                } else {
                    found(value, refused, &format!("{at}.{key}"), out);
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                found(item, refused, &format!("{at}[{index}]"), out);
            }
        }
        _ => {}
    }
}

/// The tools of the first request the server received, as JSON text and
/// as values.
fn tools_sent(machine: &Machine, request: usize) -> (String, Vec<Value>) {
    let body: Value = serde_json::from_slice(&machine.server.requests()[request].body).unwrap();
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    (body["tools"].to_string(), tools)
}

#[test]
fn sys_anthropic_strict_tools_carry_no_keyword_strict_mode_refuses() {
    let machine = Machine::new("sys_strict_anthropic");
    for _ in 0..2 {
        machine.reply_fixture("text");
        let run = machine.run(&["-p", "say hello"]);
        run.expect_code(0);
    }
    let (first, tools) = tools_sent(&machine, 0);
    let (second, _) = tools_sent(&machine, 1);
    assert_eq!(
        first, second,
        "the same tools are the same bytes every time"
    );
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in ["read", "write", "edit", "shell", "plan"] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }
    let mut strict = 0;
    for tool in &tools {
        if tool["strict"] != true {
            continue;
        }
        strict += 1;
        let mut refused = Vec::new();
        found(
            &tool["input_schema"],
            ANTHROPIC_REFUSES,
            tool["name"].as_str().unwrap(),
            &mut refused,
        );
        assert!(refused.is_empty(), "sent as strict with {refused:?}");
    }
    assert!(strict >= 5, "the built-in tools are strict: {tools:?}");
    // Anthropic's limits on a whole request, over its strict tools.
    let optional: usize = tools
        .iter()
        .filter(|tool| tool["strict"] == true)
        .map(|tool| optional_parameters(&tool["input_schema"]))
        .sum();
    assert!(strict <= 20, "{strict} strict tools");
    assert!(
        optional <= 24,
        "{optional} optional parameters over the strict tools"
    );
}

#[test]
fn sys_openai_strict_functions_carry_no_keyword_strict_mode_refuses() {
    let machine = Machine::new("sys_strict_openai");
    machine.user_config(&format!(
        "model.roles.main.provider = \"openai\"\nproviders.openai.base_url = \"{}\"\n",
        machine.server.base_url()
    ));
    machine.reply_fixture_of("openai-chat", "openai-text");
    let run = machine
        .command(&["-p", "say hello", "--model", "gpt-6-luna"])
        .env("OPENAI_API_KEY", crate::KEY)
        .output()
        .unwrap();
    let run = crate::Run::of(run);
    run.expect_code(0);
    let (_, tools) = tools_sent(&machine, 0);
    assert!(!tools.is_empty(), "{}", shown(&run));
    for tool in &tools {
        let function = &tool["function"];
        if function["strict"] != true {
            continue;
        }
        let schema = &function["parameters"];
        let mut refused = Vec::new();
        found(
            schema,
            OPENAI_REFUSES,
            function["name"].as_str().unwrap(),
            &mut refused,
        );
        assert!(refused.is_empty(), "sent as strict with {refused:?}");
        // OpenAI's strict mode requires every property of every object.
        let properties = schema["properties"].as_object().unwrap();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(properties.len(), required.len(), "{function}");
    }
}

/// How many properties of a schema, its nested objects included, are not
/// required.
fn optional_parameters(schema: &Value) -> usize {
    let Some(object) = schema.as_object() else {
        return 0;
    };
    let mut count = 0;
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        count += properties
            .keys()
            .filter(|name| !required.contains(&name.as_str()))
            .count();
        count += properties.values().map(optional_parameters).sum::<usize>();
    }
    if let Some(items) = object.get("items") {
        count += optional_parameters(items);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        for branch in object
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            count += optional_parameters(branch);
        }
    }
    count
}
