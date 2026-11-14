//! The real provider client, as the core builds it, against a scripted
//! transport (`docs/architecture/04-providers.md`): Anthropic's built-in
//! entry, where the key goes, and a stored image reaching the request.
//!
//! The responses are Anthropic's stream format, written by hand from its
//! documentation for the two shapes these tests need.

use std::sync::Arc;

use cortex_base::Secret;
use cortex_core::ClientInfo;
use cortex_protocol::event::TurnOutcome;
use cortex_protocol::session::SessionSettings;
use cortex_testkit::{Reply, ScriptedTransport};
use serde_json::{Value, json};

use crate::{Front, MODEL, World};

/// The key of these tests. Made up.
const KEY: &str = "sk-ant-cortex-core-test-0f1e2d3c4b5a69788796a5b4";

/// A PNG of one pixel.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn event(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn message_start() -> String {
    event(
        "message_start",
        &json!({"type": "message_start", "message": {
            "id": "msg_test", "type": "message", "role": "assistant", "model": MODEL,
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 12, "output_tokens": 1}
        }}),
    )
}

fn message_end(stop: &str) -> String {
    event(
        "message_delta",
        &json!({"type": "message_delta",
            "delta": {"stop_reason": stop, "stop_sequence": null},
            "usage": {"output_tokens": 9}}),
    ) + &event("message_stop", &json!({"type": "message_stop"}))
}

/// A response that says `text`.
fn text_stream(text: &str) -> String {
    message_start()
        + &event(
            "content_block_start",
            &json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
        )
        + &event(
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": text}}),
        )
        + &event(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": 0}),
        )
        + &message_end("end_turn")
}

/// A response that calls one tool.
fn tool_stream(id: &str, tool: &str, arguments: &Value) -> String {
    message_start()
        + &event(
            "content_block_start",
            &json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": id, "name": tool, "input": {}}}),
        )
        + &event(
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": arguments.to_string()}}),
        )
        + &event(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": 0}),
        )
        + &message_end("tool_use")
}

fn sse(body: String) -> Reply {
    Reply::ok(body).header("content-type", "text/event-stream")
}

#[test]
fn the_built_in_provider_is_asked_with_the_users_key_where_its_protocol_wants_it() {
    let world = World::new("real_client", []);
    let transport = ScriptedTransport::new([sse(text_stream("Hello from the provider."))]);
    let core = world.core_with(|options| {
        options.connector = None;
        options.transport = Some(Arc::new(transport.clone()));
        options
            .api_keys
            .insert("anthropic".to_owned(), Secret::new(KEY));
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        let end = front.turn(&format!("my key is {KEY}")).await;
        assert_eq!(end.outcome, TurnOutcome::Completed, "{:?}", end.error);
        assert_eq!(front.last_text(), "Hello from the provider.");
        front.close().await;
    });
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(request.headers.get("x-api-key"), Some(KEY));
    assert!(request.headers.is_sensitive("x-api-key"));
    assert!(request.headers.get("anthropic-version").is_some());
    assert_eq!(transport.bodies()[0]["model"], MODEL);
    // The key the core found is one its filter masks by value: the prompt
    // that held it is stored without it.
    assert!(!world.log().contains(KEY));
    assert!(world.log().contains("my key is [redacted]"));
}

#[test]
fn an_image_a_tool_stored_reaches_the_request_as_its_bytes() {
    let world = World::new("real_image", []);
    world.put("pixel.png", PIXEL);
    let transport = ScriptedTransport::new([
        sse(tool_stream(
            "toolu_1",
            "read",
            &json!({"path": "pixel.png"}),
        )),
        sse(text_stream("A single pixel.")),
    ]);
    let core = world.core_with(|options| {
        options.connector = None;
        options.transport = Some(Arc::new(transport.clone()));
        options
            .api_keys
            .insert("anthropic".to_owned(), Secret::new(KEY));
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        let end = front.turn("what is in pixel.png?").await;
        assert_eq!(end.outcome, TurnOutcome::Completed, "{:?}", end.error);
        assert_eq!(front.last_text(), "A single pixel.");
        front.close().await;
    });
    // The conversation and the log carry a reference to the stored image…
    let log = world.log();
    assert!(log.contains("\"artifact\":\"a_0001\""), "{log}");
    // …and the request carries the image itself, read back from the
    // session's store.
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 2);
    let second = bodies[1].to_string();
    let encoded = cortex_llm::image::encode_base64(PIXEL);
    assert!(
        second.contains(&encoded),
        "the image's bytes are not in the request"
    );
    assert!(second.contains("image/png"));
    assert!(!second.contains("a_0001"), "a reference was sent as it is");
}

/// Where no credential store can be opened, a key from the environment is
/// used all the same: it is looked for before any store is asked. (On
/// Linux no build has a keychain yet, so asking for one is a store that
/// cannot be opened. On Windows there is one.)
#[cfg(unix)]
#[test]
fn a_key_from_the_environment_needs_no_credential_store() {
    let mut world = World::new("no_store", []);
    world.raw_user_config(&format!(
        "[session]\ndurability = \"never\"\n\n[credentials]\nstore = \
         \"keychain\"\n\n[model]\nmain = \"{MODEL}\"\n"
    ));

    // With no key anywhere, the session says why the store is no help.
    let transport = ScriptedTransport::new([]);
    let core = world.core_with(|options| {
        options.connector = None;
        options.transport = Some(Arc::new(transport.clone()));
    });
    world.run(async {
        let refused = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .err()
        .expect("a session started with no key");
        assert_eq!(refused.kind, cortex_protocol::ErrorKind::Auth);
        assert!(refused.message.contains("keychain"), "{}", refused.message);
    });

    // With the key in the environment, the store is never asked.
    world.env = world.env.clone().with("ANTHROPIC_API_KEY", KEY);
    let transport = ScriptedTransport::new([sse(text_stream("Hello."))]);
    let core = world.core_with(|options| {
        options.connector = None;
        options.transport = Some(Arc::new(transport.clone()));
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        let end = front.turn("hello").await;
        assert_eq!(end.outcome, TurnOutcome::Completed, "{:?}", end.error);
        front.close().await;
    });
    assert_eq!(transport.requests()[0].headers.get("x-api-key"), Some(KEY));
}

/// A response in the `openai-chat` protocol's stream format: one recorded
/// for the provider layer's own tests, which is read here and not changed.
const OPENAI_STREAM: &str =
    include_str!("../../../cortex-llm/fixtures/openai-chat/openai-text/response.sse");

/// A built-in provider of the second protocol is asked as its entry says:
/// its endpoint, its key as a bearer token, and what its flags say.
#[test]
fn a_built_in_openai_chat_provider_is_asked_as_its_entry_says() {
    let world = World::new("openai", []);
    world.user_config("");
    let transport = ScriptedTransport::new([sse(OPENAI_STREAM.to_owned())]);
    let core = world.core_with(|options| {
        options.connector = None;
        options.transport = Some(Arc::new(transport.clone()));
        options.overrides.push(cortex_config::Override {
            key: "model.main".to_owned(),
            value: "gpt-6-luna".to_owned(),
            flag: Some("--model".to_owned()),
        });
        options
            .api_keys
            .insert("openai".to_owned(), Secret::new(KEY));
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        let end = front.turn("hello").await;
        assert_eq!(end.outcome, TurnOutcome::Completed, "{:?}", end.error);
        assert!(!front.last_text().is_empty());
        front.close().await;
    });
    let request = &transport.requests()[0];
    assert_eq!(request.url, "https://api.openai.com/v1/chat/completions");
    let bearer = format!("Bearer {KEY}");
    assert_eq!(request.headers.get("authorization"), Some(bearer.as_str()));
    assert!(request.headers.is_sensitive("authorization"));
    let body = &transport.bodies()[0];
    assert_eq!(body["model"], "gpt-6-luna");
    // The entry's flags reached the protocol: OpenAI's name for the
    // system role.
    assert_eq!(body["messages"][0]["role"], "developer");
}

/// A server on the user's own machine takes no key: it is asked without
/// one, and no key is asked of the user.
#[test]
fn a_local_server_is_asked_without_a_key() {
    let world = World::new("ollama", []);
    world.user_config(
        "[models.house]\nprovider = \"ollama\"\nid = \"house-model\"\nname = \"House \
         model\"\nprotocol = \"openai-chat\"\ncontext_window = 32000\nmax_output = \
         4000\ninput = [\"text\"]\n",
    );
    // The server is first asked what it loaded the model with: it has not
    // loaded it yet, so the catalog's figure stands.
    let transport = ScriptedTransport::new([
        Reply::ok(r#"{"models": []}"#),
        sse(OPENAI_STREAM.to_owned()),
    ]);
    let core = world.core_with(|options| {
        options.connector = None;
        options.transport = Some(Arc::new(transport.clone()));
        options.overrides.push(cortex_config::Override {
            key: "model.main".to_owned(),
            value: "house-model".to_owned(),
            flag: Some("--model".to_owned()),
        });
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        let end = front.turn("hello").await;
        assert_eq!(end.outcome, TurnOutcome::Completed, "{:?}", end.error);
        front.close().await;
    });
    let probe = &transport.requests()[0];
    assert_eq!(probe.url, "http://localhost:11434/api/ps");
    let request = &transport.requests()[1];
    assert_eq!(request.url, "http://localhost:11434/v1/chat/completions");
    assert_eq!(request.headers.get("authorization"), None);
}

/// The context size a local server loaded a model with stands in for the
/// catalog's: a model the catalog says takes 32,000 tokens, loaded with
/// 1,024, is too full for the harness's prompt alone, and the loop goes
/// to compact before it sends anything (`docs/decisions/0035-where-the-context-limit-comes-from.md`).
#[test]
fn a_local_servers_loaded_context_size_stands_in_for_the_catalogs() {
    let world = World::new("ollama_probe", []);
    world.user_config(
        "[models.house]\nprovider = \"ollama\"\nid = \"house-model\"\nname = \"House \
         model\"\nprotocol = \"openai-chat\"\ncontext_window = 32000\nmax_output = \
         4000\ninput = [\"text\"]\n",
    );
    let transport = ScriptedTransport::new([
        Reply::ok(
            r#"{"models": [{"name": "house-model:latest", "model": "house-model:latest", "context_length": 1024}]}"#,
        ),
        sse(OPENAI_STREAM.to_owned()),
    ]);
    let core = world.core_with(|options| {
        options.connector = None;
        options.transport = Some(Arc::new(transport.clone()));
        options.overrides.push(cortex_config::Override {
            key: "model.main".to_owned(),
            value: "house-model".to_owned(),
            flag: Some("--model".to_owned()),
        });
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        // The probe is answered before the prompt is sent.
        while transport.requests().is_empty() {
            tokio::task::yield_now().await;
        }
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        front.turn("hello").await;
        front.close().await;
    });
    let requests = transport.requests();
    assert_eq!(requests[0].url, "http://localhost:11434/api/ps");
    let sent = String::from_utf8_lossy(&requests[1].body).into_owned();
    assert!(
        sent.contains("replaced by a summary"),
        "the second request asks for a summary: {sent}"
    );
}

/// A provider that has a key variable and no key anywhere is refused
/// before anything is sent, and told where a key is looked for: a missing
/// key never becomes a request with no key. Its own variable or a
/// built-in one, it is the same.
#[test]
fn a_provider_with_a_key_variable_and_no_key_is_refused_before_anything_is_sent() {
    let world = World::new("no_key", []);
    world.user_config(
        "[providers.gateway]\nprotocol = \"openai-chat\"\nbase_url = \
         \"https://llm.example.invalid/v1\"\nkey_env = \"GATEWAY_KEY\"\n\n[models.gate]\nprovider \
         = \"gateway\"\nid = \"gate-model\"\nname = \"Gate\"\nprotocol = \
         \"openai-chat\"\ncontext_window = 32000\nmax_output = 4000\ninput = [\"text\"]\n",
    );
    for (model, variable) in [
        ("z-ai/glm-5.3-flash", "OPENROUTER_API_KEY"),
        ("gate-model", "GATEWAY_KEY"),
    ] {
        let transport = ScriptedTransport::new([sse(OPENAI_STREAM.to_owned())]);
        let core = world.core_with(|options| {
            options.connector = None;
            options.transport = Some(Arc::new(transport.clone()));
            options.overrides.push(cortex_config::Override {
                key: "model.main".to_owned(),
                value: model.to_owned(),
                flag: Some("--model".to_owned()),
            });
        });
        world.run(async {
            let refused = Front::start_as(
                &world,
                &core,
                ClientInfo::headless(),
                SessionSettings::default(),
            )
            .await
            .err()
            .expect("a session started with no key");
            assert_eq!(refused.kind, cortex_protocol::ErrorKind::Auth, "{model}");
            assert_eq!(refused.code, "no_credentials", "{model}");
            assert!(refused.message.contains(variable), "{}", refused.message);
        });
        assert!(
            transport.requests().is_empty(),
            "{model}: a request was sent"
        );
    }
}
