//! The two things a provider is asked besides model calls: its usage or
//! balance endpoint (the opt-in refresh of `/usage`), and a local server's
//! loaded context size.
//!
//! Both are tested from the answers the servers document, and both are
//! then sent for real to a scripted server on the loopback interface, which
//! answers as the documentation says and keeps what it was sent: one
//! request, to the documented path, with the provider's credential and
//! nothing else.

use cortex_base::{ErrorKind, UtcTime};
use cortex_llm::openai_chat::account::{self, AccountLimit, Balance, Per};
use cortex_llm::openai_chat::{context, flag, providers};
use cortex_llm::{FlagValue, Headers, Method, ProviderInfo};
use cortex_protocol::ModelId;
use cortex_protocol::usage::{LimitMeasure, LimitWindow};

use super::provider;

fn now() -> UtcTime {
    UtcTime::parse("2026-11-13T10:00:00Z").unwrap()
}

fn read(name: &str, body: &str) -> account::AccountUsage {
    account::read(
        &provider(name),
        200,
        &Headers::new(),
        body.as_bytes(),
        now(),
    )
    .unwrap_or_else(|error| panic!("{name}: {} ({})", error.message, error.code))
}

/// OpenRouter's documented answer. The label is how it shows a key: the
/// beginning and the end of the key itself.
const OPENROUTER_KEY: &str = r#"{"data":{"label":"sk-or-v1-0a1...f9e","limit":25,"limit_reset":"monthly","limit_remaining":19.25,"include_byok_in_limit":false,"budgets":[],"effective_budget":null,"usage":5.75,"usage_daily":0.5,"usage_weekly":2.25,"usage_monthly":5.75,"byok_usage":0,"is_free_tier":false,"free_model_daily_requests":{"used":12,"limit":1000,"remaining":988},"rate_limit":{"requests":-1,"interval":"10s"}}}"#;

/// DeepSeek's documented answer.
const DEEPSEEK_BALANCE: &str = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"},{"currency":"USD","total_balance":"4.07","granted_balance":"0.00","topped_up_balance":"4.07"}]}"#;

/// Venice.ai's documented answer.
const VENICE_LIMITS: &str = r#"{"data":{"accessPermitted":true,"apiTier":{"id":"paid","isCharged":true},"balances":{"USD":50.23,"DIEM":100.023,"BUNDLED_CREDITS":25},"keyExpiration":null,"nextEpochBegins":"2026-11-14T00:00:00.000Z","rateLimits":[{"apiModelId":"zai-org-glm-5-1","rateLimits":[{"amount":100,"type":"RPM"},{"amount":2500,"type":"RPD"},{"amount":750000,"type":"TPM"},{"amount":3,"type":"A_KIND_ADDED_LATER"}]}]}}"#;

fn dollars(amount: i64) -> Option<i64> {
    Some(amount * 1_000_000_000)
}

#[test]
fn only_the_providers_that_document_an_endpoint_have_a_refresh() {
    let url = |name: &str| {
        account::request(&provider(name))
            .unwrap()
            .map(|request| request.url)
    };
    assert_eq!(
        url(providers::OPENROUTER).as_deref(),
        Some("https://openrouter.ai/api/v1/key")
    );
    assert_eq!(
        url(providers::DEEPSEEK).as_deref(),
        Some("https://api.deepseek.com/user/balance")
    );
    assert_eq!(
        url(providers::VENICE).as_deref(),
        Some("https://api.venice.ai/api/v1/api_keys/rate_limits")
    );
    // OpenAI's usage API takes an admin key, which is not the key a user
    // gives for model calls. The local servers have nothing to report.
    for none in [
        providers::OPENAI,
        providers::OLLAMA,
        providers::LLAMACPP,
        providers::VLLM,
        providers::LMSTUDIO,
    ] {
        assert_eq!(url(none), None, "{none}");
    }
    // The request carries nothing: no body, no credential yet, and no
    // header but the form of the answer it takes.
    let request = account::request(&provider(providers::OPENROUTER))
        .unwrap()
        .unwrap();
    assert_eq!(request.method, Method::Get);
    assert!(request.body.is_empty());
    let sent: Vec<(&str, &str)> = request.headers.iter().collect();
    assert_eq!(sent, [("accept", "application/json")]);
    // A format this build does not know is refused, not guessed at.
    let mut unknown = provider(providers::DEEPSEEK);
    unknown.flags.set(
        flag::USAGE_ENDPOINT,
        FlagValue::Text("a-new-format".to_owned()),
    );
    assert!(
        account::request(&unknown)
            .unwrap_err()
            .message
            .contains("a-new-format")
    );
}

#[test]
fn openrouters_key_endpoint_gives_the_keys_budget_and_never_its_label() {
    let usage = read(providers::OPENROUTER, OPENROUTER_KEY);
    assert_eq!(usage.available, None, "it does not say");
    assert_eq!(
        usage.balances,
        [Balance {
            currency: "USD".to_owned(),
            remaining: Some(19_250_000_000),
            limit: dollars(25),
            used: Some(5_750_000_000),
        }]
    );
    assert_eq!(
        usage.limits,
        [AccountLimit {
            model: None,
            per: Per::Day,
            window: LimitWindow {
                measure: LimitMeasure::Requests,
                speed: None,
                limit: Some(1000),
                remaining: Some(988),
                resets_at: None,
            },
        }]
    );
    // The label is a shortened form of the key. Nothing of the answer
    // but its figures is kept.
    let kept = format!("{usage:?}");
    assert!(!kept.contains("sk-or"), "{kept}");
    assert!(!kept.contains("label"), "{kept}");

    // A key with no limit of its own: what is left is not known.
    let unlimited = read(
        providers::OPENROUTER,
        r#"{"data":{"label":"x","limit":null,"limit_remaining":null,"usage":0.25}}"#,
    );
    assert_eq!(
        unlimited.balances,
        [Balance {
            currency: "USD".to_owned(),
            remaining: None,
            limit: None,
            used: Some(250_000_000),
        }]
    );
    assert!(unlimited.limits.is_empty());
}

#[test]
fn deepseeks_balance_endpoint_gives_a_balance_in_each_currency() {
    let usage = read(providers::DEEPSEEK, DEEPSEEK_BALANCE);
    assert_eq!(usage.available, Some(true));
    assert_eq!(
        usage.balances,
        [
            Balance {
                currency: "CNY".to_owned(),
                remaining: dollars(110),
                limit: None,
                used: None,
            },
            Balance {
                currency: "USD".to_owned(),
                remaining: Some(4_070_000_000),
                limit: None,
                used: None,
            },
        ]
    );
    assert!(usage.limits.is_empty());
    let empty = read(
        providers::DEEPSEEK,
        r#"{"is_available":false,"balance_infos":[]}"#,
    );
    assert_eq!(empty.available, Some(false));
    assert!(empty.balances.is_empty());
}

#[test]
fn venices_endpoint_gives_balances_and_each_models_limits() {
    let usage = read(providers::VENICE, VENICE_LIMITS);
    assert_eq!(usage.available, Some(true));
    let balance = |currency: &str| {
        usage
            .balances
            .iter()
            .find(|balance| balance.currency == currency)
            .unwrap_or_else(|| panic!("no {currency}"))
            .remaining
    };
    assert_eq!(balance("USD"), Some(50_230_000_000));
    assert_eq!(balance("DIEM"), Some(100_023_000_000));
    assert_eq!(balance("BUNDLED_CREDITS"), dollars(25));
    let model = Some(ModelId::new("zai-org-glm-5-1"));
    let window = |measure: LimitMeasure, limit: u64, resets: bool| LimitWindow {
        measure,
        speed: None,
        limit: Some(limit),
        // The endpoint says how large a limit is, not how much is left.
        remaining: None,
        resets_at: resets.then(|| {
            UtcTime::parse("2026-11-14T00:00:00Z")
                .unwrap()
                .to_timestamp()
        }),
    };
    assert_eq!(
        usage.limits,
        [
            AccountLimit {
                model: model.clone(),
                per: Per::Minute,
                window: window(LimitMeasure::Requests, 100, false),
            },
            AccountLimit {
                model: model.clone(),
                per: Per::Day,
                window: window(LimitMeasure::Requests, 2500, true),
            },
            AccountLimit {
                model,
                per: Per::Minute,
                window: window(LimitMeasure::Tokens, 750_000, false),
            },
        ],
        "a kind of limit added later is left out, not guessed at"
    );
}

#[test]
fn an_answer_that_is_not_the_documented_one_is_an_error_and_a_failure_is_classified() {
    let attempt = |name: &str, status: u16, body: &str| {
        account::read(
            &provider(name),
            status,
            &Headers::new(),
            body.as_bytes(),
            now(),
        )
    };
    for (name, body) in [
        (providers::OPENROUTER, "<html>hello</html>"),
        (providers::OPENROUTER, "[]"),
        (providers::OPENROUTER, r#"{"key":{}}"#),
        (providers::DEEPSEEK, r#"{"is_available":true}"#),
        (providers::VENICE, r#"{"data":"none"}"#),
    ] {
        let error = attempt(name, 200, body).unwrap_err();
        assert_eq!(error.code, account::UNREADABLE, "{name} {body}");
        assert_eq!(error.kind, ErrorKind::Provider);
        assert!(!error.message.contains("hello"), "the answer is not quoted");
    }
    // A failed answer is a failure of this protocol, like any other.
    let error = attempt(
        providers::VENICE,
        401,
        r#"{"error":"Authentication failed"}"#,
    )
    .unwrap_err();
    assert_eq!((error.kind, error.status), (ErrorKind::Auth, Some(401)));
    let error = attempt(providers::OPENROUTER, 429, "").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Transient);
    // A provider with no endpoint has nothing to read.
    assert!(attempt(providers::OPENAI, 200, "{}").is_err());
}

/// Each local server's documented answer, the model it is asked about, and
/// the context size it loaded that model with.
const PROBES: &[(&str, &str, &str, &str, u64)] = &[
    (
        providers::OLLAMA,
        "/api/ps",
        "qwen3:8b",
        r#"{"models":[{"name":"gemma4:latest","model":"gemma4:latest","size":6591830464,"digest":"a2af6cc3eb7f","details":{"format":"gguf","family":"gemma4"},"expires_at":"2026-11-13T10:05:00Z","size_vram":5333539264,"context_length":4096},{"name":"qwen3:8b","model":"qwen3:8b","size":5200000000,"digest":"0000","details":{"format":"gguf","family":"qwen3"},"expires_at":"2026-11-13T10:05:00Z","size_vram":5200000000,"context_length":32768}]}"#,
        32_768,
    ),
    (
        providers::LLAMACPP,
        "/props",
        "anything",
        r#"{"default_generation_settings":{"id":0,"id_task":-1,"n_ctx":8192,"speculative":false,"is_processing":false,"params":{"n_predict":-1}},"total_slots":1,"model_path":"qwen3-8b-q4_k_m.gguf","chat_template":"..."}"#,
        8192,
    ),
    (
        providers::VLLM,
        "/v1/models",
        "Qwen/Qwen3-8B",
        r#"{"object":"list","data":[{"id":"Qwen/Qwen3-8B","object":"model","created":1794500000,"owned_by":"vllm","root":"Qwen/Qwen3-8B","parent":null,"max_model_len":40960,"permission":[]}]}"#,
        40_960,
    ),
    (
        providers::LMSTUDIO,
        "/api/v1/models",
        "google/gemma-4-26b-a4b",
        r#"{"models":[{"type":"llm","publisher":"google","key":"google/gemma-4-26b-a4b","display_name":"Gemma 4","loaded_instances":[{"id":"google/gemma-4-26b-a4b","config":{"context_length":4096}}],"max_context_length":262144},{"type":"llm","publisher":"deepseek","key":"deepseek/deepseek-r1","loaded_instances":[],"max_context_length":131072}]}"#,
        4096,
    ),
];

#[test]
fn each_local_server_is_asked_where_it_keeps_its_loaded_context_size() {
    for (name, path, model, answer, size) in PROBES {
        let provider = provider(name);
        let request = context::request(&provider)
            .unwrap()
            .expect("a local server has a probe");
        assert_eq!(request.method, Method::Get);
        assert!(request.body.is_empty());
        // The servers' own endpoints sit beside the dialect's `/v1`, and
        // vLLM's is the dialect's own model list.
        let origin = provider.base_url.trim_end_matches("/v1");
        assert_eq!(request.url, format!("{origin}{path}"), "{name}");
        let read = context::read(&provider, &ModelId::new(*model), 200, answer.as_bytes());
        assert_eq!(read, Ok(Some(*size)), "{name}");
    }
    // A hosted provider is not probed: its catalog entry is the fact.
    for hosted in [
        providers::OPENAI,
        providers::OPENROUTER,
        providers::DEEPSEEK,
        providers::VENICE,
    ] {
        assert_eq!(context::request(&provider(hosted)), Ok(None), "{hosted}");
    }
}

#[test]
fn the_loaded_size_is_not_the_size_the_model_was_trained_for() {
    let size = |name: &str, model: &str, answer: &str| {
        context::read(
            &provider(name),
            &ModelId::new(model),
            200,
            answer.as_bytes(),
        )
    };
    // LM Studio: 262,144 is the most the model supports, and 4,096 is what
    // it was loaded with.
    assert_eq!(
        size(providers::LMSTUDIO, "google/gemma-4-26b-a4b", PROBES[3].3),
        Ok(Some(4096))
    );
    // A model that is not loaded has no size yet: the catalog's stands.
    assert_eq!(
        size(providers::LMSTUDIO, "deepseek/deepseek-r1", PROBES[3].3),
        Ok(None)
    );
    assert_eq!(
        size(providers::LMSTUDIO, "not/there", PROBES[3].3),
        Ok(None)
    );
    assert_eq!(size(providers::OLLAMA, "llama9:1b", PROBES[0].3), Ok(None));
    // Ollama lists a model with its tag, and a name without one means
    // `latest`. Another tag is another model.
    assert_eq!(
        size(providers::OLLAMA, "gemma4", PROBES[0].3),
        Ok(Some(4096))
    );
    assert_eq!(
        size(providers::OLLAMA, "gemma4:latest", PROBES[0].3),
        Ok(Some(4096))
    );
    assert_eq!(size(providers::OLLAMA, "gemma4:27b", PROBES[0].3), Ok(None));
    assert_eq!(size(providers::OLLAMA, "qwen3", PROBES[0].3), Ok(None));
    assert_eq!(
        size(providers::OLLAMA, "qwen3:8b", r#"{"models":[]}"#),
        Ok(None)
    );
    assert_eq!(
        size(providers::VLLM, "another/model", PROBES[2].3),
        Ok(None)
    );
    // llama.cpp's model list has the trained size under another name,
    // which is not read: only `/props` has the loaded one.
    let trained = r#"{"object":"list","data":[{"id":"m","meta":{"n_ctx_train":131072}}]}"#;
    assert_eq!(size(providers::LLAMACPP, "m", trained), Ok(None));
    // Answers that are not the documented ones.
    for (name, answer) in [
        (providers::OLLAMA, r#"{"running":[]}"#),
        (providers::VLLM, "[]"),
        (providers::LMSTUDIO, "<html></html>"),
        (
            providers::LLAMACPP,
            r#"{"default_generation_settings":{"n_ctx":0}}"#,
        ),
    ] {
        let error = size(name, "m", answer).unwrap_err();
        assert!(error.message.contains(name), "{error}");
    }
    let refused = context::read(&provider(providers::OLLAMA), &ModelId::new("m"), 503, b"{}");
    assert!(refused.unwrap_err().message.contains("503"));
    // A probe this build does not know is refused.
    let mut unknown = provider(providers::OLLAMA);
    unknown.flags.set(
        flag::CONTEXT_PROBE,
        FlagValue::Text("a-new-probe".to_owned()),
    );
    assert!(
        context::request(&unknown)
            .unwrap_err()
            .message
            .contains("a-new-probe")
    );
}

/// The same provider entry, pointed at a server on the loopback interface.
#[cfg(feature = "http")]
fn at(server: &cortex_testkit::SseServer, name: &str) -> ProviderInfo {
    let mut local = provider(name);
    // The entry's path under its host is kept: `/v1`, `/api/v1`, or none.
    let path = local
        .base_url
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|at| rest[at..].to_owned()))
        .unwrap_or_default();
    local.base_url = format!("{}{path}", server.base_url());
    local
}

#[cfg(feature = "http")]
mod over_http {
    use cortex_base::{CancelToken, Secret};
    use cortex_llm::openai_chat::{self, account, context};
    use cortex_llm::{AuthMethod, HttpRequest, HttpTransport, NoAuth, Transport};
    use cortex_protocol::ModelId;
    use cortex_testkit::{ReceivedRequest, ServerReply, SseServer};

    use super::{DEEPSEEK_BALANCE, OPENROUTER_KEY, PROBES, VENICE_LIMITS, at, now};
    use crate::openai_chat::provider;

    /// Sends a request for real and reads the whole answer.
    fn fetch(
        mut request: HttpRequest,
        auth: &dyn AuthMethod,
    ) -> (u16, cortex_llm::Headers, Vec<u8>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(async move {
                auth.authorize(&mut request).await.expect("the key is sent");
                let response = HttpTransport::default()
                    .send(request, CancelToken::new())
                    .await
                    .expect("the scripted server answers");
                let (status, headers, mut body) =
                    (response.status, response.headers, response.body);
                let mut bytes = Vec::new();
                while let Some(chunk) = body.next().await {
                    bytes.extend_from_slice(&chunk.expect("the body arrives"));
                }
                (status, headers, bytes)
            })
    }

    fn json(body: &str) -> ServerReply {
        ServerReply::status(200)
            .header("content-type", "application/json")
            .send(body)
    }

    /// The one request the server received.
    fn only(server: &SseServer) -> ReceivedRequest {
        let mut requests = server.requests();
        assert_eq!(requests.len(), 1, "exactly one request was made");
        requests.remove(0)
    }

    #[test]
    fn sys_a_refresh_is_one_request_with_the_providers_key_and_nothing_else() {
        let rows = [
            ("openrouter", "/api/v1/key", OPENROUTER_KEY),
            ("deepseek", "/user/balance", DEEPSEEK_BALANCE),
            ("venice", "/api/v1/api_keys/rate_limits", VENICE_LIMITS),
        ];
        for (name, path, answer) in rows {
            let server = SseServer::start();
            server.push(json(answer));
            let provider = at(&server, name);
            let request = account::request(&provider).unwrap().expect("a refresh");
            let auth = openai_chat::api_key(Secret::new("sk-loopback-test"), "a test");
            let (status, headers, body) = fetch(request, &auth);
            let usage = account::read(&provider, status, &headers, &body, now())
                .unwrap_or_else(|error| panic!("{name}: {}", error.message));
            assert!(!usage.balances.is_empty(), "{name}");

            let sent = only(&server);
            assert_eq!(
                (sent.method.as_str(), sent.path.as_str()),
                ("GET", path),
                "{name}"
            );
            assert_eq!(
                sent.header("authorization"),
                Some("Bearer sk-loopback-test")
            );
            // It carries no usage figures, no project names and nothing
            // that identifies us: there is no body and no query, and no
            // header of ours.
            assert!(sent.body.is_empty(), "{name}");
            assert!(!sent.path.contains('?'), "{name}");
            for (header, _) in &sent.headers {
                let header = header.to_ascii_lowercase();
                assert!(
                    matches!(
                        header.as_str(),
                        "accept" | "authorization" | "host" | "content-length" | "user-agent"
                    ),
                    "{name}: an unexpected header `{header}` was sent"
                );
            }
        }
    }

    #[test]
    fn sys_a_local_servers_loaded_context_size_is_asked_for_and_read() {
        for (name, path, model, answer, size) in PROBES {
            let server = SseServer::start();
            server.push(json(answer));
            let provider = at(&server, name);
            let request = context::request(&provider).unwrap().expect("a probe");
            // A local server takes no key by default.
            let (status, _, body) = fetch(request, &NoAuth);
            assert_eq!(
                context::read(&provider, &ModelId::new(*model), status, &body),
                Ok(Some(*size)),
                "{name}"
            );
            let sent = only(&server);
            assert_eq!(
                (sent.method.as_str(), sent.path.as_str()),
                ("GET", *path),
                "{name}"
            );
            assert_eq!(sent.header("authorization"), None, "{name}");
            assert!(sent.body.is_empty());
        }
        // A server that is still starting says so, and the catalog's
        // figure stands until it can be asked.
        let server = SseServer::start();
        server.push(
            ServerReply::status(503)
                .header("content-type", "application/json")
                .send(r#"{"error":{"code":503,"message":"Loading model","type":"unavailable_error"}}"#),
        );
        let provider = at(&server, "llamacpp");
        let request = context::request(&provider).unwrap().unwrap();
        let (status, _, body) = fetch(request, &NoAuth);
        assert!(context::read(&provider, &ModelId::new("m"), status, &body).is_err());
        // The built-in entries are not what was changed.
        assert!(provider.base_url.starts_with("http://127.0.0.1"));
        assert_ne!(provider.base_url, self::provider("llamacpp").base_url);
    }
}
