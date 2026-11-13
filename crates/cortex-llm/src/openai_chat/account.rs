//! A provider's documented usage or balance endpoint: what the opt-in
//! refresh of `/usage` asks (`docs/architecture/18-usage.md`, "The opt-in
//! refresh").
//!
//! This module is the provider's side of the refresh and nothing more:
//! which request to make, and how to read the answer. Whether it is made
//! at all is the user's setting, and making it is the caller's work: with
//! the provider's own credential, over the same client as model calls.
//! [`request`] builds a `GET` with no body, so that nothing of ours can be
//! in it.
//!
//! | Format | Endpoint | What it reports | Read on |
//! |---|---|---|---|
//! | [`OPENROUTER_KEY`] | `GET <base>/key` | What the key has spent, the key's own credit limit and what is left of it, and today's free-model requests. | 2026-11-13 |
//! | [`DEEPSEEK_BALANCE`] | `GET <base>/user/balance` | The account's balance in each currency, and whether it is enough to make calls. | 2026-11-13 |
//! | [`VENICE_RATE_LIMITS`] | `GET <base>/api_keys/rate_limits` | The balance in each of the provider's currencies, whether the key may make calls, and each model's limits by the minute and by the day. | 2026-11-13 |
//!
//! OpenAI documents a usage API, and it takes an admin key, which is not
//! the key a user gives for model calls. So it has no format here, and its
//! provider entry names none. The local servers have nothing to report.
//!
//! # The answer is read and dropped
//!
//! OpenRouter's answer holds the key's label, and the label of a key is a
//! shortened form of the key itself. So what an endpoint answered is never
//! kept: [`read`] takes the figures out of it and returns those alone. An
//! [`AccountUsage`] has no member that holds the provider's own text, and
//! must not gain one.
//!
//! # A balance is not a limit window
//!
//! All three report money: an amount in a currency (US dollars, yuan, or
//! Venice.ai's own unit). The protocol's `LimitWindow` counts requests and
//! tokens and cannot say that. So what is read is returned as an
//! [`AccountUsage`], a type of this crate. How a balance is shown in
//! `/usage`, and whether the protocol's types gain a way to carry one, is
//! for the step that builds the view.

use cortex_base::{ErrorKind, UtcTime};
use cortex_protocol::ModelId;
use cortex_protocol::usage::{LimitMeasure, LimitWindow};
use serde_json::Value;

use super::{classify, flag};
use crate::error::{BuildError, ProviderError};
use crate::http::{Headers, HttpRequest, Method};
use crate::protocol::ProviderInfo;

/// OpenRouter's `GET /api/v1/key`.
pub const OPENROUTER_KEY: &str = "openrouter-key";
/// DeepSeek's `GET /user/balance`.
pub const DEEPSEEK_BALANCE: &str = "deepseek-balance";
/// Venice.ai's `GET /api/v1/api_keys/rate_limits`.
pub const VENICE_RATE_LIMITS: &str = "venice-rate-limits";

/// The code of an answer that was not what the endpoint is documented to
/// send.
pub const UNREADABLE: &str = "unreadable_usage_response";

/// What a provider's usage endpoint reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsage {
    /// Whether the account or key can make calls right now, where the
    /// provider says.
    pub available: Option<bool>,
    /// Money: one entry for each currency or budget the provider reports.
    pub balances: Vec<Balance>,
    /// Limits counted in requests or tokens.
    pub limits: Vec<AccountLimit>,
}

/// An amount of money in one currency, as a provider's usage endpoint
/// reports it. Amounts are whole billionths of the currency's unit, as
/// costs are kept everywhere else, and may be negative: an account can be
/// overdrawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Balance {
    /// The currency, as the provider names it: `USD`, `CNY`, `DIEM`.
    pub currency: String,
    /// What is left. `None` when the provider does not say, which for a
    /// key without a limit of its own is the case.
    pub remaining: Option<i64>,
    /// The size of the budget it is left of, where there is one.
    pub limit: Option<i64>,
    /// What has been spent, where the provider says.
    pub used: Option<i64>,
}

/// The time a limit is counted over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Per {
    /// A minute.
    Minute,
    /// A day.
    Day,
}

/// A limit in requests or tokens that a usage endpoint reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLimit {
    /// The model the limit is for. `None` for a limit on the whole key.
    pub model: Option<ModelId>,
    /// The time it is counted over.
    pub per: Per,
    /// The limit, and what is left of it where the provider says.
    pub window: LimitWindow,
}

/// The request that asks a provider's usage endpoint, without credentials.
/// `None` for a provider whose entry names no endpoint: it has no refresh.
/// An entry that names a format this build does not know is refused.
pub fn request(provider: &ProviderInfo) -> Result<Option<HttpRequest>, BuildError> {
    let Some(format) = provider.flags.text(flag::USAGE_ENDPOINT) else {
        return Ok(None);
    };
    let path = match format {
        OPENROUTER_KEY => "/key",
        DEEPSEEK_BALANCE => "/user/balance",
        VENICE_RATE_LIMITS => "/api_keys/rate_limits",
        other => {
            return Err(BuildError::new(format!(
                "the provider `{}` names the usage endpoint `{other}`, which is not one this \
                 build knows",
                provider.id
            )));
        }
    };
    let mut headers = Headers::new();
    headers.insert("accept", "application/json");
    Ok(Some(HttpRequest {
        method: Method::Get,
        url: format!("{}{path}", provider.base_url),
        headers,
        body: Vec::new(),
    }))
}

/// Reads the answer of a provider's usage endpoint. A status that is not a
/// success is classified like any failed response of this protocol.
pub fn read(
    provider: &ProviderInfo,
    status: u16,
    headers: &Headers,
    body: &[u8],
    now: UtcTime,
) -> Result<AccountUsage, ProviderError> {
    if !(200..300).contains(&status) {
        return Err(classify::answered(Some(status), Some(headers), body, now));
    }
    let unreadable = |what: &str| {
        ProviderError::new(
            ErrorKind::Provider,
            UNREADABLE,
            format!(
                "The usage endpoint of `{}` answered with something else than it documents: {what}.",
                provider.id
            ),
        )
    };
    let Ok(answer @ Value::Object(_)) = serde_json::from_slice::<Value>(body) else {
        return Err(unreadable("not a JSON object"));
    };
    let mut usage = AccountUsage {
        available: None,
        balances: Vec::new(),
        limits: Vec::new(),
    };
    match provider.flags.text(flag::USAGE_ENDPOINT) {
        Some(OPENROUTER_KEY) => {
            let Some(data) = answer.get("data").filter(|data| data.is_object()) else {
                return Err(unreadable("no `data`"));
            };
            usage.balances.push(Balance {
                currency: "USD".to_owned(),
                remaining: data.get("limit_remaining").and_then(amount),
                limit: data.get("limit").and_then(amount),
                used: data.get("usage").and_then(amount),
            });
            if let Some(free) = data.get("free_model_daily_requests") {
                let count = |name: &str| free.get(name).and_then(Value::as_u64);
                if count("limit").is_some() || count("remaining").is_some() {
                    usage.limits.push(AccountLimit {
                        model: None,
                        per: Per::Day,
                        window: LimitWindow {
                            measure: LimitMeasure::Requests,
                            speed: None,
                            limit: count("limit"),
                            remaining: count("remaining"),
                            resets_at: None,
                        },
                    });
                }
            }
        }
        Some(DEEPSEEK_BALANCE) => {
            let Some(Value::Array(infos)) = answer.get("balance_infos") else {
                return Err(unreadable("no `balance_infos`"));
            };
            usage.available = answer.get("is_available").and_then(Value::as_bool);
            for info in infos {
                let Some(Value::String(currency)) = info.get("currency") else {
                    continue;
                };
                usage.balances.push(Balance {
                    currency: currency.clone(),
                    remaining: info.get("total_balance").and_then(amount),
                    limit: None,
                    used: None,
                });
            }
        }
        Some(VENICE_RATE_LIMITS) => {
            let Some(data) = answer.get("data").filter(|data| data.is_object()) else {
                return Err(unreadable("no `data`"));
            };
            usage.available = data.get("accessPermitted").and_then(Value::as_bool);
            if let Some(Value::Object(balances)) = data.get("balances") {
                for (currency, value) in balances {
                    usage.balances.push(Balance {
                        currency: currency.clone(),
                        remaining: amount(value),
                        limit: None,
                        used: None,
                    });
                }
            }
            let next_day = data
                .get("nextEpochBegins")
                .and_then(Value::as_str)
                .and_then(|time| UtcTime::parse(time).ok())
                .map(UtcTime::to_timestamp);
            let models = data.get("rateLimits").and_then(Value::as_array);
            for model in models.into_iter().flatten() {
                let Some(id) = model.get("apiModelId").and_then(Value::as_str) else {
                    continue;
                };
                let limits = model.get("rateLimits").and_then(Value::as_array);
                for limit in limits.into_iter().flatten() {
                    let (measure, per) = match limit.get("type").and_then(Value::as_str) {
                        Some("RPM") => (LimitMeasure::Requests, Per::Minute),
                        Some("RPD") => (LimitMeasure::Requests, Per::Day),
                        Some("TPM") => (LimitMeasure::Tokens, Per::Minute),
                        // A kind of limit added later is not guessed at.
                        _ => continue,
                    };
                    usage.limits.push(AccountLimit {
                        model: Some(ModelId::new(id)),
                        per,
                        window: LimitWindow {
                            measure,
                            speed: None,
                            limit: limit.get("amount").and_then(Value::as_u64),
                            // The endpoint says how large a limit is, not
                            // how much of it is left.
                            remaining: None,
                            resets_at: next_day.clone().filter(|_| per == Per::Day),
                        },
                    });
                }
            }
        }
        _ => return Err(unreadable("the provider's entry names no usage endpoint")),
    }
    Ok(usage)
}

/// An amount of money as whole billionths of its unit. The providers write
/// one as a number, or as a decimal number in a string. `None` for a null
/// and for anything that is not an amount.
fn amount(value: &Value) -> Option<i64> {
    let text = match value {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        _ => return None,
    };
    decimal_nanos(&text).or_else(|| {
        // A number written with an exponent.
        let nanos = (text.parse::<f64>().ok()? * 1e9).round();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "checked on the same line: finite and within the range of the type"
        )]
        (nanos.is_finite() && nanos.abs() < 9.0e18).then_some(nanos as i64)
    })
}

/// A plain decimal number (`110.00`, `-0.5`, `3`) as whole billionths,
/// exactly. Digits past the ninth decimal place are cut off.
fn decimal_nanos(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty()) || !all_digits(whole) || !all_digits(fraction) {
        return None;
    }
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut nanos: i64 = 0;
    for place in 0..9 {
        let digit = fraction.as_bytes().get(place).map_or(0, |byte| byte - b'0');
        nanos = nanos * 10 + i64::from(digit);
    }
    let total = whole.checked_mul(1_000_000_000)?.checked_add(nanos)?;
    Some(if negative { -total } else { total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_amount_is_read_exactly_as_billionths() {
        let nanos = |json: &str| amount(&serde_json::from_str(json).unwrap());
        assert_eq!(nanos(r#""110.00""#), Some(110_000_000_000));
        assert_eq!(nanos(r#""0.000000001""#), Some(1));
        assert_eq!(nanos(r#""-2.5""#), Some(-2_500_000_000));
        assert_eq!(nanos("50.23"), Some(50_230_000_000));
        assert_eq!(nanos("100"), Some(100_000_000_000));
        assert_eq!(nanos("0"), Some(0));
        assert_eq!(nanos("-0.75"), Some(-750_000_000));
        // A number small enough to be written with an exponent.
        assert_eq!(nanos("1e-7"), Some(100));
        // Past the ninth place there is nothing to keep.
        assert_eq!(nanos(r#""1.0000000019""#), Some(1_000_000_001));
        for none in [
            "null",
            "true",
            r#""""#,
            r#""soon""#,
            r#""1.2.3""#,
            "[1]",
            r#"".""#,
        ] {
            assert_eq!(nanos(none), None, "{none}");
        }
    }
}
