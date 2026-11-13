//! Asking a local server for the context size it loaded a model with
//! (`docs/architecture/04-providers.md`, "Model catalog").
//!
//! A local model's context window is not a fact about the model. It is what
//! the server was started with, or what fitted the machine's memory: a
//! model trained for 131,072 tokens is commonly loaded with 4,096 or 8,192.
//! A catalog entry cannot know it, and a request sized for the trained
//! window is cut short or refused. So the server is asked.
//!
//! None of these servers reports it through the dialect itself. Each has an
//! endpoint of its own, which is why the probe is named by its format:
//!
//! | Format | Endpoint | Where the size is | Read on |
//! |---|---|---|---|
//! | [`OLLAMA_PS`] | `GET <origin>/api/ps` | `models[].context_length`, for the running model of that name (a name without a tag is the tag `latest`). A model that is not loaded has none yet. | 2026-11-13, docs.ollama.com/api/ps |
//! | [`LLAMACPP_PROPS`] | `GET <origin>/props` | `default_generation_settings.n_ctx`: the size of one slot, which is what one request gets. (`/v1/models` has `meta.n_ctx_train`, the trained size, which is not it.) | 2026-11-13, the server's README |
//! | [`VLLM_MODELS`] | `GET <base>/models` | `data[].max_model_len`, for the model of that id. | 2026-11-13, the server's source (`models/serving.py`) |
//! | [`LMSTUDIO_MODELS`] | `GET <origin>/api/v1/models` | `models[].loaded_instances[].config.context_length`, for the model of that key. (`max_context_length` is the most the model supports, which is not it.) | 2026-11-13, lmstudio.ai/docs/developer/rest/list |
//!
//! `<base>` is the provider's base URL, and `<origin>` is the same without
//! its final `/v1`: the servers' own endpoints sit beside the dialect's,
//! not under it.
//!
//! This module builds the request and reads the answer. Sending it, when a
//! session connects to a local provider, is the caller's work.

use cortex_protocol::ModelId;
use serde_json::Value;

use super::flag;
use crate::error::BuildError;
use crate::http::{Headers, HttpRequest, Method};
use crate::protocol::ProviderInfo;

/// Ollama's `GET /api/ps`.
pub const OLLAMA_PS: &str = "ollama-ps";
/// llama.cpp's `GET /props`.
pub const LLAMACPP_PROPS: &str = "llamacpp-props";
/// vLLM's `GET /v1/models`.
pub const VLLM_MODELS: &str = "vllm-models";
/// LM Studio's `GET /api/v1/models`.
pub const LMSTUDIO_MODELS: &str = "lmstudio-models";

/// The answer to a probe could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProbeError {
    /// What happened, for a person.
    pub message: String,
}

/// The provider's base URL without a final `/v1`.
fn origin(provider: &ProviderInfo) -> &str {
    provider
        .base_url
        .strip_suffix("/v1")
        .unwrap_or(&provider.base_url)
}

/// The request that asks a local server for its loaded context size,
/// without credentials. `None` for a provider whose entry names no probe.
/// An entry that names a format this build does not know is refused.
pub fn request(provider: &ProviderInfo) -> Result<Option<HttpRequest>, BuildError> {
    let Some(format) = provider.flags.text(flag::CONTEXT_PROBE) else {
        return Ok(None);
    };
    let url = match format {
        OLLAMA_PS => format!("{}/api/ps", origin(provider)),
        LLAMACPP_PROPS => format!("{}/props", origin(provider)),
        VLLM_MODELS => format!("{}/models", provider.base_url),
        LMSTUDIO_MODELS => format!("{}/api/v1/models", origin(provider)),
        other => {
            return Err(BuildError::new(format!(
                "the provider `{}` names the context probe `{other}`, which is not one this \
                 build knows",
                provider.id
            )));
        }
    };
    let mut headers = Headers::new();
    headers.insert("accept", "application/json");
    Ok(Some(HttpRequest {
        method: Method::Get,
        url,
        headers,
        body: Vec::new(),
    }))
}

/// Reads a probe's answer: the context size the server has loaded `model`
/// with, in tokens. `Ok(None)` when the server does not have the model
/// loaded, or does not say: the catalog's figure then stands. An answer
/// that is not what the server documents is an error, and so is a size of
/// zero.
pub fn read(
    provider: &ProviderInfo,
    model: &ModelId,
    status: u16,
    body: &[u8],
) -> Result<Option<u64>, ProbeError> {
    let problem = |what: String| ProbeError {
        message: format!(
            "The context probe of `{}` could not be used: {what}.",
            provider.id
        ),
    };
    if !(200..300).contains(&status) {
        return Err(problem(format!(
            "the server answered with HTTP status {status}"
        )));
    }
    let Ok(answer @ Value::Object(_)) = serde_json::from_slice::<Value>(body) else {
        return Err(problem("the answer is not a JSON object".to_owned()));
    };
    let named = |entry: &Value, keys: &[&str]| {
        keys.iter()
            .any(|key| entry.get(key).and_then(Value::as_str) == Some(model.as_str()))
    };
    // Ollama lists a model under its name and tag, and a name written
    // without a tag means the tag `latest`.
    let tagged = format!("{}:latest", model.as_str());
    let latest = |entry: &Value| {
        !model.as_str().contains(':')
            && ["name", "model"]
                .iter()
                .any(|key| entry.get(key).and_then(Value::as_str) == Some(tagged.as_str()))
    };
    let list = |key: &str| {
        answer
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| problem(format!("it has no `{key}` list")))
    };
    let size = match provider.flags.text(flag::CONTEXT_PROBE) {
        Some(OLLAMA_PS) => list("models")?
            .iter()
            .find(|entry| named(entry, &["name", "model"]) || latest(entry))
            .and_then(|entry| entry.get("context_length"))
            .and_then(Value::as_u64),
        Some(LLAMACPP_PROPS) => answer
            .get("default_generation_settings")
            .and_then(|settings| settings.get("n_ctx"))
            .and_then(Value::as_u64),
        Some(VLLM_MODELS) => list("data")?
            .iter()
            .find(|entry| named(entry, &["id"]))
            .and_then(|entry| entry.get("max_model_len"))
            .and_then(Value::as_u64),
        Some(LMSTUDIO_MODELS) => list("models")?
            .iter()
            .find(|entry| named(entry, &["key"]))
            .and_then(|entry| entry.get("loaded_instances"))
            .and_then(Value::as_array)
            .and_then(|instances| instances.first())
            .and_then(|instance| instance.get("config"))
            .and_then(|config| config.get("context_length"))
            .and_then(Value::as_u64),
        _ => return Err(problem("the provider's entry names no probe".to_owned())),
    };
    match size {
        Some(0) => Err(problem("it reports a context size of zero".to_owned())),
        size => Ok(size),
    }
}
