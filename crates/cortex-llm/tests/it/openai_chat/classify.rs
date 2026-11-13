//! The classification table, row by row, with the body each server sends
//! for the failure; the failures recorded from the real servers; whose
//! refusal a refusal at a speed tier is; and the headers.

use std::time::Duration;

use cortex_base::{ErrorKind, UtcTime};
use cortex_llm::error::{ProviderError, Throttle};
use cortex_llm::openai_chat::{OpenAiChat, providers};
use cortex_llm::{EventSink, Headers, HttpFailure, ModelEvent, ModelRequest, Protocol};
use cortex_protocol::Speed;
use cortex_protocol::usage::{LimitMeasure, LimitWindow};

use super::scenario;
use super::{DEEPSEEK, OPENAI_LUNA, VENICE, fixture_names, load};

const NOW: &str = "2026-11-13T10:00:00Z";

fn now() -> UtcTime {
    UtcTime::parse(NOW).unwrap()
}

fn headers(pairs: &[(&str, &str)]) -> Headers {
    let mut headers = Headers::new();
    for (name, value) in pairs {
        headers.insert(name, *value);
    }
    headers
}

fn classify_with(
    status: u16,
    headers: &Headers,
    body: &str,
    request: &ModelRequest,
) -> ProviderError {
    OpenAiChat.classify(
        &HttpFailure {
            status,
            headers,
            body: body.as_bytes(),
            now: now(),
        },
        request,
    )
}

fn classify(status: u16, body: &str) -> ProviderError {
    classify_with(status, &Headers::new(), body, &scenario::text(&DEEPSEEK))
}

/// What a row of the table expects: the kind, the code, and whether it is
/// a rate limit or an overload.
type Expected = (ErrorKind, &'static str, Option<Throttle>);

const RATE: Option<Throttle> = Some(Throttle::RateLimit);
const OVERLOAD: Option<Throttle> = Some(Throttle::Overload);

fn check(server: &str, rows: &[(u16, &str, Expected)]) {
    for (status, body, (kind, code, throttle)) in rows {
        let error = classify(*status, body);
        assert_eq!(
            (error.kind, error.code.as_str(), error.throttle),
            (*kind, *code, *throttle),
            "{server} {status} {body}"
        );
        assert_eq!(error.status, Some(*status), "{server} {status}");
        assert_eq!(error.tier, None, "{server} {status}: sent at standard");
        // A rate limit or an overload is always something to wait out.
        if throttle.is_some() {
            assert_eq!(error.kind, ErrorKind::Transient, "{server} {status}");
        }
    }
}

#[test]
fn openais_failures() {
    use ErrorKind::{Auth, ContextOverflow, Provider, Transient};
    check(
        "openai",
        &[
            (
                401,
                r#"{"error":{"message":"Incorrect API key provided.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#,
                (Auth, "invalid_api_key", None),
            ),
            (
                403,
                r#"{"error":{"message":"Country, region, or territory not supported","type":"request_forbidden","param":null,"code":"unsupported_country_region_territory"}}"#,
                (Auth, "unsupported_country_region_territory", None),
            ),
            (
                400,
                r#"{"error":{"message":"Invalid `service_tier` argument: The requested service tier is not allowed for this project.","type":"invalid_request_error","param":"service_tier","code":null}}"#,
                (Provider, "invalid_request_error", None),
            ),
            (
                404,
                r#"{"error":{"message":"The model `gpt-9` does not exist","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#,
                (Provider, "model_not_found", None),
            ),
            (
                400,
                r#"{"error":{"message":"This model's maximum context length is 1050000 tokens.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
                (ContextOverflow, "context_length_exceeded", None),
            ),
            // A rate limit, with and without a code.
            (
                429,
                r#"{"error":{"message":"Rate limit reached for requests","type":"rate_limit_error","param":null,"code":"rate_limit_exceeded"}}"#,
                (Transient, "rate_limit_exceeded", RATE),
            ),
            (
                429,
                r#"{"error":{"message":"Rate limit reached for requests","type":"requests","param":null,"code":null}}"#,
                (Transient, "requests", RATE),
            ),
            (
                429,
                r#"{"error":{"message":"Traffic ramped too fast.","type":"rate_limit_error","param":null,"code":"slow_down"}}"#,
                (Transient, "slow_down", RATE),
            ),
            // A 429 that waiting does not lift: the code wins over the
            // status.
            (
                429,
                r#"{"error":{"message":"No prepaid credits remain.","type":"insufficient_quota","param":null,"code":"credit_balance_exhausted"}}"#,
                (Provider, "credit_balance_exhausted", None),
            ),
            (
                429,
                r#"{"error":{"message":"The organization hit its spend limit.","type":"insufficient_quota","param":null,"code":"organization_spend_limit_exceeded"}}"#,
                (Provider, "organization_spend_limit_exceeded", None),
            ),
            (
                429,
                r#"{"error":{"message":"The project hit its spend limit.","type":"insufficient_quota","param":null,"code":"project_spend_limit_exceeded"}}"#,
                (Provider, "project_spend_limit_exceeded", None),
            ),
            (
                429,
                r#"{"error":{"message":"The organization hit its usage limit.","type":"insufficient_quota","param":null,"code":"organization_usage_limit_exceeded"}}"#,
                (Provider, "organization_usage_limit_exceeded", None),
            ),
            (
                429,
                r#"{"error":{"message":"You exceeded your current quota.","type":"insufficient_quota","param":null,"code":null}}"#,
                (Provider, "insufficient_quota", None),
            ),
            (
                500,
                r#"{"error":{"message":"The server had an error.","type":"server_error","param":null,"code":null}}"#,
                (Transient, "server_error", None),
            ),
            (
                503,
                r#"{"error":{"message":"The model is overloaded.","type":"service_unavailable_error","param":null,"code":"server_is_overloaded"}}"#,
                (Transient, "server_is_overloaded", OVERLOAD),
            ),
        ],
    );
}

#[test]
fn openrouters_failures() {
    use ErrorKind::{Auth, Provider, Transient};
    check(
        "openrouter",
        &[
            (
                400,
                r#"{"error":{"message":"not-a-model-0 is not a valid model ID","code":400},"user_id":"user_fixture0003"}"#,
                (Provider, "unknown_error", None),
            ),
            (
                401,
                r#"{"error":{"message":"Invalid API key","code":401}}"#,
                (Auth, "unknown_error", None),
            ),
            (
                402,
                r#"{"error":{"message":"Insufficient credits","code":402}}"#,
                (Provider, "unknown_error", None),
            ),
            // A budget is a quota, though it arrives as a 403.
            (
                403,
                r#"{"error":{"code":403,"message":"Key limit exceeded","metadata":{"limit_source":"openrouter_key_limit","limit_usd":5,"spend_usd":5.01,"resets_at":null}}}"#,
                (Provider, "openrouter_key_limit", None),
            ),
            // A request refused for what it holds is not a credential's
            // failure, though it arrives as a 403 too.
            (
                403,
                r#"{"error":{"code":403,"message":"Input flagged","metadata":{"error_type":"content_policy_violation","reasons":["x"],"provider_name":"Example"}}}"#,
                (Provider, "content_policy_violation", None),
            ),
            (
                403,
                r#"{"error":{"code":403,"message":"Insufficient permissions"}}"#,
                (Auth, "unknown_error", None),
            ),
            (
                408,
                r#"{"error":{"code":408,"message":"The request timed out","metadata":{"error_type":"timeout"}}}"#,
                (Transient, "timeout", None),
            ),
            (
                429,
                r#"{"error":{"code":429,"message":"Rate limited","metadata":{"error_type":"rate_limit_exceeded","provider_code":"429"}}}"#,
                (Transient, "rate_limit_exceeded", RATE),
            ),
            (
                429,
                r#"{"error":{"code":429,"message":"Rate limited"}}"#,
                (Transient, "rate_limited", RATE),
            ),
            (
                502,
                r#"{"error":{"code":502,"message":"The model is down","metadata":{"error_type":"provider_overloaded"}}}"#,
                (Transient, "provider_overloaded", OVERLOAD),
            ),
            (
                502,
                r#"{"error":{"code":502,"message":"Internal error","metadata":{"error_type":"server"}}}"#,
                (Transient, "server", None),
            ),
            (
                503,
                r#"{"error":{"code":503,"message":"No provider meets your routing requirements"}}"#,
                (Transient, "overloaded", OVERLOAD),
            ),
        ],
    );
    // A balance too low for a request in flight is the one 402 that lifts
    // by waiting, and it says how long.
    let error = classify_with(
        402,
        &headers(&[("retry-after", "7")]),
        r#"{"error":{"code":402,"message":"In-flight budget","metadata":{"limit_source":"openrouter_in_flight_budget"}}}"#,
        &scenario::text(&DEEPSEEK),
    );
    assert_eq!((error.kind, error.throttle), (ErrorKind::Transient, RATE));
    assert_eq!(error.retry_after, Some(Duration::from_secs(7)));
}

#[test]
fn deepseeks_failures() {
    use ErrorKind::{Auth, Provider, Transient};
    // Its documentation lists statuses and no codes; the bodies are in the
    // dialect's own form, as the recorded ones show.
    let body = |kind: &str| {
        format!(
            r#"{{"error":{{"message":"x","type":"{kind}","param":null,"code":"invalid_request_error"}}}}"#
        )
    };
    let (invalid, authentication) = (body("invalid_request_error"), body("authentication_error"));
    check(
        "deepseek",
        &[
            (400, &invalid, (Provider, "invalid_request_error", None)),
            (401, &authentication, (Auth, "authentication_error", None)),
            (422, &invalid, (Provider, "invalid_request_error", None)),
            (500, "", (Transient, "unknown_error", None)),
            (503, "", (Transient, "overloaded", OVERLOAD)),
        ],
    );
    // Without a readable body, the status says it.
    let rows: [(u16, ErrorKind, Option<Throttle>); 6] = [
        (400, Provider, None),
        (401, Auth, None),
        (402, Provider, None),
        (422, Provider, None),
        (429, Transient, RATE),
        (503, Transient, OVERLOAD),
    ];
    for (status, kind, throttle) in rows {
        let error = classify(status, "");
        assert_eq!((error.kind, error.throttle), (kind, throttle), "{status}");
        assert!(error.message.contains(&status.to_string()), "{error:?}");
    }
}

#[test]
fn venices_failures() {
    use ErrorKind::{Auth, ContextOverflow, Provider, Transient};
    let body = |code: &str| format!(r#"{{"error":"x","code":"{code}"}}"#);
    let rows: Vec<(u16, String, Expected)> = vec![
        (
            401,
            body("AUTHENTICATION_FAILED"),
            (Auth, "AUTHENTICATION_FAILED", None),
        ),
        (
            401,
            body("AUTHENTICATION_FAILED_INACTIVE_KEY"),
            (Auth, "AUTHENTICATION_FAILED_INACTIVE_KEY", None),
        ),
        (401, body("PRO_ONLY_MODEL"), (Auth, "PRO_ONLY_MODEL", None)),
        (
            402,
            body("INSUFFICIENT_BALANCE"),
            (Provider, "INSUFFICIENT_BALANCE", None),
        ),
        (
            402,
            body("API_KEY_USD_SPEND_LIMIT_EXCEEDED"),
            (Provider, "API_KEY_USD_SPEND_LIMIT_EXCEEDED", None),
        ),
        (
            402,
            body("API_KEY_DIEM_SPEND_LIMIT_EXCEEDED"),
            (Provider, "API_KEY_DIEM_SPEND_LIMIT_EXCEEDED", None),
        ),
        (
            402,
            body("PAYMENT_REQUIRED"),
            (Provider, "PAYMENT_REQUIRED", None),
        ),
        (403, body("UNAUTHORIZED"), (Auth, "UNAUTHORIZED", None)),
        (
            403,
            body("API_ACCESS_DISABLED"),
            (Auth, "API_ACCESS_DISABLED", None),
        ),
        (
            400,
            body("INVALID_REQUEST"),
            (Provider, "INVALID_REQUEST", None),
        ),
        (
            400,
            body("INVALID_MODEL"),
            (Provider, "INVALID_MODEL", None),
        ),
        (
            400,
            body("TOO_MANY_TOKENS"),
            (ContextOverflow, "TOO_MANY_TOKENS", None),
        ),
        (
            404,
            body("MODEL_NOT_FOUND"),
            (Provider, "MODEL_NOT_FOUND", None),
        ),
        (
            413,
            body("PAYLOAD_TOO_LARGE"),
            (ContextOverflow, "PAYLOAD_TOO_LARGE", None),
        ),
        (
            422,
            body("CONTENT_POLICY_VIOLATION"),
            (Provider, "CONTENT_POLICY_VIOLATION", None),
        ),
        (
            429,
            body("RATE_LIMIT_EXCEEDED"),
            (Transient, "RATE_LIMIT_EXCEEDED", RATE),
        ),
        // An overload that arrives as a 429 is an overload.
        (
            429,
            body("MODEL_OVERLOADED"),
            (Transient, "MODEL_OVERLOADED", OVERLOAD),
        ),
        (
            500,
            body("INFERENCE_FAILED"),
            (Transient, "INFERENCE_FAILED", None),
        ),
        (
            500,
            body("UNKNOWN_ERROR"),
            (Transient, "UNKNOWN_ERROR", None),
        ),
        (
            503,
            body("MODEL_OFFLINE"),
            (Transient, "MODEL_OFFLINE", OVERLOAD),
        ),
        (
            503,
            body("MODEL_AT_CAPACITY"),
            (Transient, "MODEL_AT_CAPACITY", OVERLOAD),
        ),
        (
            504,
            body("REQUEST_TIMEOUT"),
            (Transient, "REQUEST_TIMEOUT", None),
        ),
        // The form its real failures have: a sentence and nothing else.
        (
            401,
            r#"{"error":"Authentication failed"}"#.to_owned(),
            (Auth, "unknown_error", None),
        ),
        (
            404,
            r#"{"error":"Specified model not found: not-a-model-0."}"#.to_owned(),
            (Provider, "unknown_error", None),
        ),
    ];
    let rows: Vec<(u16, &str, Expected)> = rows
        .iter()
        .map(|(status, body, expected)| (*status, body.as_str(), *expected))
        .collect();
    check("venice", &rows);
}

#[test]
fn the_local_servers_failures() {
    use ErrorKind::{Auth, ContextOverflow, Provider, Transient};
    check(
        "local",
        &[
            // llama.cpp: the status as the code, and a type.
            (
                503,
                r#"{"error":{"code":503,"message":"Loading model","type":"unavailable_error"}}"#,
                (Transient, "unavailable_error", OVERLOAD),
            ),
            (
                400,
                r#"{"error":{"code":400,"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size_error","n_prompt_tokens":9000,"n_ctx":4096}}"#,
                (ContextOverflow, "exceed_context_size_error", None),
            ),
            (
                401,
                r#"{"error":{"code":401,"message":"Invalid API Key","type":"authentication_error"}}"#,
                (Auth, "authentication_error", None),
            ),
            (
                400,
                r#"{"error":{"code":400,"message":"Failed to parse grammar","type":"invalid_request_error"}}"#,
                (Provider, "invalid_request_error", None),
            ),
            (
                500,
                r#"{"error":{"code":500,"message":"it broke","type":"server_error"}}"#,
                (Transient, "server_error", None),
            ),
            // vLLM: no code for a context that is too long, only the
            // dialect's long-standing sentence, in either of its two forms.
            (
                400,
                r#"{"error":{"message":"This model's maximum context length is 4096 tokens. However, you requested 9000 tokens.","type":"BadRequestError","param":null,"code":400}}"#,
                (ContextOverflow, "BadRequestError", None),
            ),
            (
                400,
                r#"{"object":"error","message":"This model's maximum context length is 4096 tokens.","type":"BadRequestError","param":null,"code":400}"#,
                (ContextOverflow, "BadRequestError", None),
            ),
            (
                400,
                r#"{"object":"error","message":"max_tokens must be at least 1","type":"BadRequestError","param":null,"code":400}"#,
                (Provider, "BadRequestError", None),
            ),
            // Ollama: the dialect's form.
            (
                404,
                r#"{"error":{"message":"model 'qwen9' not found","type":"not_found_error","param":null,"code":null}}"#,
                (Provider, "not_found_error", None),
            ),
            (
                500,
                r#"{"error":{"message":"llama runner process has terminated","type":"api_error","param":null,"code":null}}"#,
                (Transient, "api_error", None),
            ),
        ],
    );
}

#[test]
fn what_no_body_explains_is_classified_by_its_status() {
    use ErrorKind::{Auth, ContextOverflow, Provider, Transient};
    let rows: [(u16, &str, Expected); 12] = [
        (400, "", (Provider, "unknown_error", None)),
        (401, "<html>401</html>", (Auth, "unknown_error", None)),
        (403, "", (Auth, "unknown_error", None)),
        (404, "", (Provider, "unknown_error", None)),
        (408, "", (Transient, "unknown_error", None)),
        (409, "", (Transient, "unknown_error", None)),
        // Too large in bytes: answered by compacting, like too many tokens.
        (
            413,
            "<html>Request Entity Too Large</html>",
            (ContextOverflow, "unknown_error", None),
        ),
        (429, "", (Transient, "rate_limited", RATE)),
        (500, "", (Transient, "unknown_error", None)),
        (
            502,
            "<html><body>Bad Gateway</body></html>",
            (Transient, "unknown_error", None),
        ),
        (503, "", (Transient, "overloaded", OVERLOAD)),
        (529, "", (Transient, "overloaded", OVERLOAD)),
    ];
    check("anyone", &rows);
    // The sentence counts only where a context that is too long can be:
    // not on a failure of the server's own.
    let error = classify(500, r#"{"error":{"message":"maximum context length"}}"#);
    assert_eq!(error.kind, Transient);
    let error = classify(
        422,
        r#"{"error":{"message":"This model's Maximum Context Length is 8 tokens"}}"#,
    );
    assert_eq!(error.kind, ContextOverflow);
    // A page of HTML is not a message.
    let error = classify(502, "<html><body>Bad Gateway</body></html>");
    assert_eq!(error.message, "The provider answered with HTTP status 502.");
    // A long message is cut.
    let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "x".repeat(5000));
    assert!(classify(400, &long).message.len() < 700);
}

#[test]
fn the_recorded_failures_are_what_the_table_says() {
    use ErrorKind::{Auth, Provider};
    let rows = [
        (
            "deepseek-error-not-found",
            400,
            Provider,
            "invalid_request_error",
        ),
        (
            "deepseek-error-invalid-request",
            400,
            Provider,
            "invalid_request_error",
        ),
        (
            "deepseek-error-authentication",
            401,
            Auth,
            "authentication_error",
        ),
        ("openrouter-error-not-found", 400, Provider, "unknown_error"),
        (
            "openrouter-error-authentication",
            401,
            Auth,
            "unknown_error",
        ),
        ("venice-error-not-found", 404, Provider, "unknown_error"),
        ("venice-error-authentication", 401, Auth, "unknown_error"),
        (
            "venice-error-fast-overloaded",
            503,
            ErrorKind::Transient,
            "MODEL_AT_CAPACITY",
        ),
    ];
    for (name, status, kind, code) in rows {
        let fixture = load(name);
        assert_eq!(fixture.status, status, "{name}");
        let error = classify_with(
            fixture.status,
            &fixture.headers,
            &String::from_utf8_lossy(&fixture.response),
            &scenario::text(&DEEPSEEK),
        );
        assert_eq!((error.kind, error.code.as_str()), (kind, code), "{name}");
        assert!(
            !error.message.is_empty() && !error.message.starts_with("The provider answered"),
            "{name}: the server's own message is kept: {error:?}"
        );
    }
    // Every failed response in the fixtures directory is one of these.
    let failed: Vec<String> = fixture_names()
        .into_iter()
        .filter(|name| load(name).status != 200)
        .collect();
    let mut covered: Vec<&str> = rows.iter().map(|row| row.0).collect();
    covered.sort_unstable();
    assert_eq!(failed, covered);
}

/// A request at a tier, on a model whose entry says what the tier does
/// when it cannot be served.
fn at_tier(provider: &str, model: &str, speed: Speed) -> ModelRequest {
    let mut request = scenario::base(provider, model, None);
    request.speed = speed;
    request
}

#[test]
fn a_refusal_at_a_speed_tier_is_the_tiers_only_when_the_tier_refuses() {
    let overloaded = r#"{"error":"The model is at capacity.","code":"MODEL_AT_CAPACITY"}"#;
    let limited = r#"{"error":"Rate limit exceeded","code":"RATE_LIMIT_EXCEEDED"}"#;
    let broke = r#"{"error":"Insufficient balance","code":"INSUFFICIENT_BALANCE"}"#;
    let tier = |request: &ModelRequest, status: u16, body: &str| {
        classify_with(status, &Headers::new(), body, request).tier
    };
    let flex = Speed::Other("flex".to_owned());

    // Venice.ai's fast variant: a model of its own, with capacity and
    // limits of its own, which refuses when it is busy.
    let fast = at_tier(VENICE.provider, VENICE.model, Speed::Fast);
    assert_eq!(tier(&fast, 503, overloaded), Some(Speed::Fast));
    assert_eq!(tier(&fast, 429, limited), Some(Speed::Fast));
    assert_eq!(
        tier(&fast, 503, ""),
        Some(Speed::Fast),
        "a bare 503 is an overload"
    );
    // What is not a rate limit or an overload is never the tier's.
    assert_eq!(tier(&fast, 402, broke), None);
    assert_eq!(tier(&fast, 401, ""), None);
    assert_eq!(tier(&fast, 500, ""), None);
    // At standard speed nothing is a tier's.
    let standard = at_tier(VENICE.provider, VENICE.model, Speed::Standard);
    assert_eq!(tier(&standard, 503, overloaded), None);
    assert_eq!(tier(&standard, 429, limited), None);

    // OpenAI's fast tier is downgraded by the provider, never refused: a
    // refusal at fast is the model's.
    let fast = at_tier(providers::OPENAI, OPENAI_LUNA, Speed::Fast);
    assert_eq!(tier(&fast, 429, ""), None);
    assert_eq!(tier(&fast, 503, ""), None);
    // Its flex tier refuses when there is no capacity, and shares the
    // model's rate limit: an overload is the tier's, a rate limit is not.
    let deferred = at_tier(providers::OPENAI, OPENAI_LUNA, flex.clone());
    assert_eq!(tier(&deferred, 503, ""), Some(flex));
    assert_eq!(tier(&deferred, 429, ""), None);

    // The same inside a successful response.
    let fast = at_tier(VENICE.provider, VENICE.model, Speed::Fast);
    let events = super::decode_events(
        &fast,
        &super::provider(VENICE.provider),
        &[r#"{"error":{"message":"busy","type":"api_error","code":"MODEL_OVERLOADED"}}"#],
    )
    .unwrap();
    let Some(ModelEvent::Error(error)) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!(error.tier, Some(Speed::Fast));
    assert_eq!(error.throttle, OVERLOAD);
}

#[test]
fn the_wait_is_the_servers_or_the_reset_of_a_limit_that_is_used_up() {
    let limited = r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#;
    let wait = |pairs: &[(&str, &str)], status: u16, body: &str| {
        classify_with(status, &headers(pairs), body, &scenario::text(&DEEPSEEK)).retry_after
    };
    let secs = |seconds| Some(Duration::from_secs(seconds));
    // `retry-after`, in seconds or as a date.
    assert_eq!(wait(&[("retry-after", "56")], 429, limited), secs(56));
    assert_eq!(
        wait(
            &[("retry-after", "Fri, 13 Nov 2026 10:00:30 GMT")],
            429,
            limited
        ),
        secs(30)
    );
    assert_eq!(wait(&[("retry-after", "20")], 503, ""), secs(20));
    // Without it, a rate limit waits for the limit that has nothing left:
    // OpenAI writes the reset as a duration, Venice.ai as a time.
    assert_eq!(
        wait(
            &[
                ("x-ratelimit-remaining-requests", "0"),
                ("x-ratelimit-reset-requests", "17s"),
                ("x-ratelimit-remaining-tokens", "5000"),
                ("x-ratelimit-reset-tokens", "6m0s"),
            ],
            429,
            limited
        ),
        secs(17)
    );
    let later = now().checked_add(Duration::from_secs(42)).unwrap();
    assert_eq!(
        wait(
            &[
                ("x-ratelimit-remaining-tokens", "0"),
                ("x-ratelimit-reset-tokens", &later.unix_millis().to_string()),
            ],
            429,
            limited
        ),
        secs(42)
    );
    // `retry-after` wins over the resets.
    assert_eq!(
        wait(
            &[
                ("retry-after", "3"),
                ("x-ratelimit-remaining-requests", "0"),
                ("x-ratelimit-reset-requests", "17s"),
            ],
            429,
            limited
        ),
        secs(3)
    );
    // A limit with something left is not what refused the request, and a
    // failure that is not a rate limit does not wait for a limit.
    assert_eq!(
        wait(
            &[
                ("x-ratelimit-remaining-requests", "4"),
                ("x-ratelimit-reset-requests", "17s"),
            ],
            429,
            limited
        ),
        None
    );
    assert_eq!(
        wait(
            &[
                ("x-ratelimit-remaining-requests", "0"),
                ("x-ratelimit-reset-requests", "17s"),
            ],
            500,
            ""
        ),
        None
    );
    assert_eq!(wait(&[], 429, limited), None);
}

#[test]
fn a_failure_carries_the_servers_id_for_the_request() {
    let error = classify_with(
        500,
        &headers(&[("x-request-id", "req_abc")]),
        "",
        &scenario::text(&DEEPSEEK),
    );
    assert_eq!(error.request_id.as_deref(), Some("req_abc"));
    let error = classify_with(
        500,
        &Headers::new(),
        r#"{"error":{"message":"x"},"request_id":"req_body"}"#,
        &scenario::text(&DEEPSEEK),
    );
    assert_eq!(error.request_id.as_deref(), Some("req_body"));
    assert_eq!(classify(500, "").request_id, None);
}

fn limits_of(headers: &Headers) -> (Option<String>, Vec<LimitWindow>) {
    let mut sink = EventSink::sent_at(Speed::Standard);
    OpenAiChat.read_headers(headers, now(), &mut sink);
    let id = sink.known_request_id().map(str::to_owned);
    let limits = sink
        .take()
        .into_iter()
        .find_map(|event| match event {
            ModelEvent::Limits(limits) => Some(limits),
            _ => None,
        })
        .unwrap_or_default();
    (id, limits)
}

#[test]
fn the_headers_of_a_response_give_its_id_and_the_rate_limits() {
    // OpenAI's form: the resets are durations.
    let fixture = load("openai-text");
    let (id, limits) = limits_of(&fixture.headers);
    assert_eq!(id.as_deref(), Some("req_fixture9001"));
    let reset = |after: u64| {
        Some(
            now()
                .checked_add(Duration::from_secs(after))
                .unwrap()
                .to_timestamp(),
        )
    };
    assert_eq!(
        limits,
        [
            LimitWindow {
                measure: LimitMeasure::Requests,
                speed: None,
                limit: Some(1000),
                remaining: Some(999),
                resets_at: reset(1),
            },
            LimitWindow {
                measure: LimitMeasure::Tokens,
                speed: None,
                limit: Some(1_000_000),
                remaining: Some(999_984),
                resets_at: reset(360),
            },
        ]
    );
    // Venice.ai's form, recorded: the resets are times.
    let fixture = load("venice-text");
    let (id, limits) = limits_of(&fixture.headers);
    assert_eq!(id, None, "this server sends no id for a request");
    assert_eq!(limits.len(), 2);
    let stated: i64 = fixture
        .headers
        .get("x-ratelimit-reset-requests")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        limits[0].resets_at,
        UtcTime::from_unix_millis(stated).map(UtcTime::to_timestamp)
    );
    assert_eq!(limits[0].measure, LimitMeasure::Requests);
    // A server that reports none: nothing is made up.
    assert_eq!(
        limits_of(&load("deepseek-text").headers),
        (None, Vec::new())
    );
    assert_eq!(limits_of(&Headers::new()), (None, Vec::new()));
}

#[test]
fn no_fixture_says_how_large_the_recording_accounts_limits_are() {
    let mut seen = 0;
    for name in fixture_names() {
        for (header, value) in load(&name).headers.iter() {
            if header.starts_with("x-ratelimit-limit-") {
                assert_eq!(
                    value,
                    cortex_testkit::fixture::made_up_limit(header).to_string(),
                    "{name}: {header}"
                );
                seen += 1;
            }
            // Nothing but the headers a fixture may keep.
            assert!(
                cortex_testkit::fixture::keeps_header(header),
                "{name}: {header}"
            );
        }
    }
    assert!(seen > 0, "no recorded fixture has a rate limit");
}

#[test]
fn a_refusal_by_moderation_is_not_a_credentials_failure_with_or_without_a_name() {
    // OpenRouter's documented metadata for a moderation refusal, without a
    // typed name beside it.
    let error = classify(
        403,
        r#"{"error":{"code":403,"message":"Input flagged","metadata":{"reasons":["violence"],"flagged_input":"how to...","provider_name":"Example","model_slug":"example/model"}}}"#,
    );
    assert_eq!(error.kind, ErrorKind::Provider);
    assert_eq!(error.code, "content_policy_violation");
    assert_eq!(error.throttle, None);
    // A 403 that says nothing of the kind is still a credential's.
    assert_eq!(
        classify(
            403,
            r#"{"error":{"code":403,"message":"Forbidden","metadata":{}}}"#
        )
        .kind,
        ErrorKind::Auth
    );
}

#[test]
fn no_part_of_a_key_is_kept_in_a_message() {
    // Recorded: DeepSeek repeats the last characters of the key it was
    // sent. The key the recording was made with was not a real one, and
    // the message is still not kept as it came.
    let fixture = load("deepseek-error-authentication");
    let body = String::from_utf8_lossy(&fixture.response).into_owned();
    assert!(
        body.contains("****"),
        "the fixture no longer shows the case"
    );
    let error = classify_with(401, &fixture.headers, &body, &scenario::text(&DEEPSEEK));
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(!error.message.contains('*'), "{}", error.message);
    assert!(!error.message.contains("0000"), "{}", error.message);
    assert!(
        error.message.contains("Your api key: [key] is invalid"),
        "{}",
        error.message
    );
    // The beginning and the end of a key, as another server masks it.
    let error = classify(
        401,
        r#"{"error":{"message":"Incorrect API key provided: sk-proj-********************abcd. You can find your API key at the dashboard.","type":"invalid_request_error","code":"invalid_api_key"}}"#,
    );
    assert_eq!(
        error.message,
        "Incorrect API key provided: [key] You can find your API key at the dashboard."
    );
    // The same in a failure of a usage endpoint, and inside a response.
    let error = cortex_llm::openai_chat::account::read(
        &super::provider(DEEPSEEK.provider),
        401,
        &Headers::new(),
        body.as_bytes(),
        now(),
    )
    .unwrap_err();
    assert!(!error.message.contains("0000"), "{}", error.message);
    let events = super::decode_events(
        &scenario::text(&DEEPSEEK),
        &super::provider(DEEPSEEK.provider),
        &[&body],
    )
    .unwrap();
    let Some(ModelEvent::Error(error)) = events.last() else {
        panic!("{events:?}");
    };
    assert!(!error.message.contains("0000"), "{}", error.message);
    // Three asterisks are a mask too, and so are bullets, and a word that
    // begins as these providers' keys do, however the rest is hidden.
    for (said, kept) in [
        ("Invalid key ***9f2c given.", "Invalid key [key] given."),
        ("Invalid key •••••9f2c", "Invalid key [key]"),
        ("No such key: sk-or-v1-0a1...f9e", "No such key: [key]"),
        ("Bad key 'SK-abc…xyz'.", "Bad key [key]"),
    ] {
        let body = serde_json::json!({"error": {"message": said}}).to_string();
        assert_eq!(classify(401, &body).message, kept);
    }
    // A message without a masked word is kept as it came.
    assert_eq!(
        classify(400, r#"{"error":{"message":"2 * 3 is not ** valid"}}"#).message,
        "2 * 3 is not ** valid"
    );
}

#[test]
fn a_fast_variant_at_capacity_is_the_tiers_refusal_from_its_fixture() {
    let fixture = load("venice-error-fast-overloaded");
    let body = String::from_utf8_lossy(&fixture.response).into_owned();
    let fast = scenario::fast_model(&VENICE);
    let error = classify_with(fixture.status, &fixture.headers, &body, &fast);
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(error.throttle, OVERLOAD);
    assert_eq!(
        error.tier,
        Some(Speed::Fast),
        "the request can go to the standard model"
    );
    // The same answer to a request at standard speed is the model's own.
    let standard = scenario::text(&VENICE);
    assert_eq!(
        classify_with(fixture.status, &fixture.headers, &body, &standard).tier,
        None
    );
}
