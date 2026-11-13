//! The real built-in toolbox, as the binary declares it to each provider
//! (`docs/testing-and-ci.md`, "The toolbox fixtures").
//!
//! Step 13b's one live run found three refusals of the real toolbox by
//! Anthropic's strict mode that no recorded fixture could have shown: the
//! fixtures had one small tool. So the tools the real binary sends to each
//! provider's cheapest model are kept, in `fixtures/toolbox/<provider>.json`
//! (written by a system test of `cortex-cli`, which runs the binary against
//! the scripted server), and a request with them and a one-line task is
//! recorded from each provider as the fixture `toolbox`
//! (`<provider>-toolbox` for `openai-chat`).
//!
//! Two tests here hold the builder to what was recorded:
//!
//! - built from the kept tools, a request declares them exactly as the
//!   binary did: name, description, schema and `strict`, through the strict
//!   reduction;
//! - and its bytes are the recorded fixture's `request.json`, which the
//!   provider accepted. A change to the builder or to the reduction that
//!   changes what is sent fails here until the fixture is recorded again
//!   (`cargo xtask record --toolbox`).
//!
//! A change to the toolbox itself shows in the system test of `cortex-cli`,
//! which warns when the binary no longer sends the kept tools.

use std::path::PathBuf;

use cortex_llm::anthropic::AnthropicMessages;
use cortex_llm::openai_chat::OpenAiChat;
use cortex_llm::request::ToolDefinition;
use cortex_llm::{ModelRequest, Protocol as _};
use cortex_testkit::fixture::Fixture;
use serde_json::Value;

use crate::{anthropic, openai_chat};

/// The providers the toolbox is recorded from.
pub(crate) const PROVIDERS: [&str; 4] = ["anthropic", "deepseek", "openrouter", "venice"];

/// What the binary sent one provider: its model, and its tools as the
/// wire format declares them.
pub(crate) struct Kept {
    pub(crate) model: String,
    pub(crate) tools: Vec<Value>,
}

fn kept_file(provider: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("toolbox")
        .join(format!("{provider}.json"))
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test reads the tools kept for it"
)]
pub(crate) fn kept(provider: &str) -> Kept {
    let path = kept_file(provider);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let value: Value = serde_json::from_str(&text).expect("the kept tools are JSON");
    Kept {
        model: value["model"].as_str().expect("a model").to_owned(),
        tools: value["tools"].as_array().expect("a list of tools").clone(),
    }
}

/// A tool as a provider's wire format declares it: its name, description,
/// schema and whether it is strict.
pub(crate) fn declared(provider: &str, tool: &Value) -> (String, String, Value, bool) {
    let declaration = if provider == "anthropic" {
        tool.clone()
    } else {
        tool["function"].clone()
    };
    let schema_key = if provider == "anthropic" {
        "input_schema"
    } else {
        "parameters"
    };
    (
        declaration["name"].as_str().unwrap_or("").to_owned(),
        declaration["description"].as_str().unwrap_or("").to_owned(),
        declaration[schema_key].clone(),
        declaration["strict"] == true,
    )
}

/// The request recorded as the toolbox fixture: the kept tools, a short
/// system prompt and a one-line task that needs no tool.
pub(crate) fn request(provider: &str) -> ModelRequest {
    let kept = kept(provider);
    let mut request = if provider == "anthropic" {
        anthropic::scenario::base(&kept.model, "low")
    } else {
        let server = openai_chat::RECORDED
            .iter()
            .find(|server| server.provider == provider)
            .unwrap_or_else(|| panic!("no recorded server is `{provider}`"));
        openai_chat::scenario::base(
            provider,
            &kept.model,
            Some(server.off.unwrap_or(server.low)),
        )
    };
    request.system = "You are a coding agent working in a repository.".to_owned();
    request.messages = vec![anthropic::user(
        "Reply with the single word ready, and do not call a tool.",
    )];
    request.tools = kept
        .tools
        .iter()
        .map(|tool| {
            let (name, description, schema, _) = declared(provider, tool);
            ToolDefinition {
                name,
                description,
                schema,
            }
        })
        .collect();
    request.max_output = Some(64);
    request
}

/// The body the builder writes for the toolbox request.
pub(crate) fn body(provider: &str) -> Vec<u8> {
    let request = request(provider);
    let built = if provider == "anthropic" {
        AnthropicMessages.build(&request, &anthropic::provider())
    } else {
        OpenAiChat.build(&request, &openai_chat::provider(provider))
    };
    built
        .unwrap_or_else(|error| panic!("{provider}: {error}"))
        .body
}

/// Where the toolbox fixture of a provider is.
pub(crate) fn fixture_directory(provider: &str) -> PathBuf {
    if provider == "anthropic" {
        anthropic::fixtures_directory().join("toolbox")
    } else {
        openai_chat::fixtures_directory().join(format!("{provider}-toolbox"))
    }
}

#[test]
fn the_builder_declares_the_tools_the_binary_declared() {
    for provider in PROVIDERS {
        let kept = kept(provider);
        assert!(
            kept.tools.len() >= 5,
            "{provider}: {} tools",
            kept.tools.len()
        );
        let built: Value = serde_json::from_slice(&body(provider)).unwrap();
        let built_tools = built["tools"].as_array().expect("tools are sent");
        let declared_by = |tools: &[Value]| -> Vec<(String, String, Value, bool)> {
            tools.iter().map(|tool| declared(provider, tool)).collect()
        };
        assert_eq!(
            declared_by(built_tools),
            declared_by(&kept.tools),
            "{provider}: the builder does not declare the tools as the binary did"
        );
    }
}

#[test]
fn a_toolbox_fixture_is_the_request_the_builder_writes_and_the_provider_took() {
    for provider in PROVIDERS {
        let directory = fixture_directory(provider);
        let fixture = Fixture::load(&directory).unwrap_or_else(|error| {
            panic!(
                "{provider}: {error}. Record it with `cargo xtask record --toolbox --provider \
                 {provider}`."
            )
        });
        assert!(
            body(provider) == fixture.request,
            "{provider}: the request the builder writes is not the one recorded in {}, which \
             the provider accepted. The builder or the strict reduction changed what is sent: \
             record it again with `cargo xtask record --toolbox --provider {provider}`.",
            directory.display()
        );
        assert_eq!(
            fixture.status,
            200,
            "{provider}: the provider refused the toolbox: {}",
            String::from_utf8_lossy(&fixture.response)
        );
    }
}
