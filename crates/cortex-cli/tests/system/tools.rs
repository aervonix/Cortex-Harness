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
    let (_, tools) = tools_sent(&machine, 0);
    let (_, again) = tools_sent(&machine, 1);
    // The schemas, reduced, are the same bytes every time. (The shell
    // tool's description gains the shell's version once a run has kept it.)
    let schemas = |tools: &[Value]| -> Vec<String> {
        tools
            .iter()
            .map(|tool| tool["input_schema"].to_string())
            .collect()
    };
    assert_eq!(
        schemas(&tools),
        schemas(&again),
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

/// The shell's version is in the shell tool's description. A session does
/// not start the shell to ask it on its way to the first request: the
/// first run on a machine goes without it and has the shell asked on the
/// side, and the runs after it read the answer from the state directory.
#[test]
fn sys_the_shells_version_is_kept_for_the_runs_after_the_first() {
    let shell = if cfg!(windows) { "pwsh" } else { "bash" };
    let machine = Machine::new("sys_shell_version");
    machine.user_config(&format!("shell.program = \"{shell}\"\n"));
    let kept = machine.dirs().state.unwrap().join("shells.json");

    machine.reply_text("Done.");
    machine.run(&["-p", "first"]).expect_code(0);
    cortex_testkit::children::until(
        std::time::Duration::from_secs(30),
        "the shell's version is kept",
        || kept.exists(),
    );
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&kept).unwrap()).unwrap();
    let version = file["shells"][0]["version"].as_str().unwrap().to_owned();

    machine.reply_text("Done.");
    machine.run(&["-p", "second"]).expect_code(0);
    let (_, tools) = tools_sent(&machine, 1);
    let shell_tool = tools.iter().find(|tool| tool["name"] == "shell").unwrap();
    let description = shell_tool["description"].as_str().unwrap();
    assert!(
        description.contains(&version),
        "the second run's description names {version}: {description}"
    );
}

/// The providers the toolbox fixtures of `cortex-llm` are recorded from:
/// each with its cheapest model of the built-in catalog, the variable its
/// key is in, and the fixture the scripted server answers with.
const TOOLBOX: &[(&str, &str, &str, &str, &str)] = &[
    (
        "anthropic",
        "claude-haiku-5-5",
        "ANTHROPIC_API_KEY",
        "anthropic-messages",
        "text",
    ),
    (
        "deepseek",
        "deepseek-flash",
        "DEEPSEEK_API_KEY",
        "openai-chat",
        "deepseek-text",
    ),
    (
        "openrouter",
        "z-ai/glm-5.3-flash",
        "OPENROUTER_API_KEY",
        "openai-chat",
        "openrouter-text",
    ),
    (
        "venice",
        "deepseek-v4-flash-0731",
        "VENICE_API_KEY",
        "openai-chat",
        "venice-text",
    ),
];

/// Where the tools the binary sent each provider are kept.
fn kept_tools(provider: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../cortex-llm/fixtures/toolbox")
        .join(format!("{provider}.json"))
}

/// The tools as they are compared: the shell tool's description names the
/// shell the machine has, and is left out.
fn comparable(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            let name = tool["name"]
                .as_str()
                .or_else(|| tool["function"]["name"].as_str())
                .unwrap_or("")
                .to_owned();
            if name == "shell" {
                if let Some(function) = tool.get_mut("function") {
                    function["description"] = Value::Null;
                } else {
                    tool["description"] = Value::Null;
                }
            }
            tool
        })
        .collect()
}

/// The tools the binary sends each provider's cheapest model, against
/// those the toolbox fixtures of `cortex-llm` were recorded with
/// (`docs/testing-and-ci.md`, "The toolbox fixtures"). A difference is a
/// warning, not a failure: the provider may well accept the new tools, and
/// only a recording shows it. With `CORTEX_UPDATE_TOOLBOX` set to a list of
/// providers separated by commas, those providers' tools are written there
/// instead, which `cargo xtask record --toolbox` does for the providers it
/// is about to record.
///
/// Not compared on Windows, where the shell tool is PowerShell's: the kept
/// tools are a Linux machine's.
#[test]
fn sys_the_toolbox_the_binary_sends_is_the_one_its_fixtures_were_recorded_with() {
    let update = std::env::var("CORTEX_UPDATE_TOOLBOX").ok();
    let updates = |provider: &str| {
        update
            .as_deref()
            .is_some_and(|list| list.split(',').any(|named| named.trim() == provider))
    };
    let update = update.is_some();
    if cfg!(windows) && !update {
        return;
    }
    let mut changed = Vec::new();
    for (provider, model, variable, protocol, reply) in TOOLBOX {
        let machine = Machine::new(&format!("sys_toolbox_{provider}"));
        if *provider != "anthropic" {
            machine.user_config(&format!(
                "model.roles.main.provider = \"{provider}\"\nproviders.{provider}.base_url = \"{}\"\n",
                machine.server.base_url()
            ));
        }
        machine.reply_fixture_of(protocol, reply);
        let output = machine
            .command(&["-p", "say hello", "--model", model])
            .env(variable, crate::KEY)
            .output()
            .unwrap();
        let run = crate::Run::of(output);
        run.expect_code(0);
        let (_, tools) = tools_sent(&machine, 0);
        assert!(tools.len() >= 5, "{provider}: {}", shown(&run));
        let file = kept_tools(provider);
        if update {
            if !updates(provider) {
                continue;
            }
            let kept = serde_json::json!({ "provider": provider, "model": model, "tools": tools });
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(
                &file,
                format!("{}\n", serde_json::to_string_pretty(&kept).unwrap()),
            )
            .unwrap();
            continue;
        }
        let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let kept_tools = kept["tools"].as_array().cloned().unwrap_or_default();
        if kept["model"] != *model || comparable(&kept_tools) != comparable(&tools) {
            changed.push(*provider);
        }
    }
    if changed.is_empty() {
        return;
    }
    let warning = format!(
        "The tools the binary sends to {} are not the ones the toolbox fixtures were recorded \
         with (crates/cortex-llm/fixtures/toolbox/). Whether the provider still accepts them is \
         known only from a recording: `cargo xtask record --toolbox` (docs/testing-and-ci.md, \
         \"The toolbox fixtures\").",
        changed.join(", ")
    );
    eprintln!("warning: {warning}");
    // A test's output is shown only when it fails; the job's summary page
    // is where a warning that passes can be seen.
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(summary) {
            let _ = writeln!(file, "\n> **Warning:** {warning}\n");
        }
    }
}

#[test]
fn the_shells_description_is_all_that_is_left_out_of_the_comparison() {
    let anthropic = serde_json::json!([
        {"name": "shell", "description": "Runs bash 5.2", "input_schema": {"type": "object"}},
        {"name": "read", "description": "Reads a file", "input_schema": {"type": "object"}}
    ]);
    let other = serde_json::json!([
        {"name": "shell", "description": "Runs pwsh 7.5", "input_schema": {"type": "object"}},
        {"name": "read", "description": "Reads a file", "input_schema": {"type": "object"}}
    ]);
    let tools = |value: &Value| value.as_array().unwrap().clone();
    assert_eq!(comparable(&tools(&anthropic)), comparable(&tools(&other)));
    let changed = serde_json::json!([
        {"name": "shell", "description": "Runs bash 5.2", "input_schema": {"type": "object"}},
        {"name": "read", "description": "Reads a file, now longer", "input_schema": {"type": "object"}}
    ]);
    assert_ne!(comparable(&tools(&anthropic)), comparable(&tools(&changed)));
    let functions = serde_json::json!([
        {"type": "function", "function": {"name": "shell", "description": "bash", "parameters": {}}}
    ]);
    let compared = comparable(&tools(&functions));
    assert_eq!(compared[0]["function"]["description"], Value::Null);
    assert_eq!(compared[0]["function"]["name"], "shell");
}
