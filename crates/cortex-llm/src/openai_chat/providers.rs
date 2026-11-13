//! The built-in providers that speak `openai-chat`: each one an endpoint,
//! its flags, and where its key is looked for.
//!
//! A provider is data ([`ProviderInfo`]). These are the entries the harness
//! knows without being told; a user adds any other compatible server by
//! writing an entry of the same form in configuration.
//!
//! | Provider | Base URL | What its entry sets | Checked |
//! |---|---|---|---|
//! | [`OPENAI`] | `https://api.openai.com/v1` | `max_completion_tokens`, the `developer` role, a prompt cache key | documentation only |
//! | [`OPENROUTER`] | `https://openrouter.ai/api/v1` | the `reasoning` object, a usage endpoint | against the real API |
//! | [`DEEPSEEK`] | `https://api.deepseek.com` | `reasoning_content` on every assistant message, a usage endpoint | against the real API |
//! | [`VENICE`] | `https://api.venice.ai/api/v1` | `max_completion_tokens`, a prompt cache key, its own system prompt turned off, a usage endpoint | against the real API |
//! | [`OLLAMA`] | `http://localhost:11434/v1` | a context probe, longer time limits | documentation only |
//! | [`LLAMACPP`] | `http://localhost:9931/v1` | a context probe, longer time limits | documentation only |
//! | [`VLLM`] | `http://localhost:8000/v1` | `max_completion_tokens`, a context probe, longer time limits | documentation only |
//! | [`LMSTUDIO`] | `http://localhost:1234/v1` | a context probe, longer time limits | documentation only |
//!
//! # The time limits of a local server
//!
//! A hosted server answers within seconds, or queues a request and says so.
//! A local server does neither. Before it sends anything it may load the
//! model from disk, and it reads the whole prompt, at a few hundred tokens
//! a second on a graphics card and a few dozen on a processor. A request
//! that is given up on is read again from the start when it is retried, so
//! a limit that is too short does not fail fast: it never succeeds.
//!
//! - **The first event may take [`LOCAL_FIRST_EVENT`], ten minutes.** That
//!   is a model load of a minute or two and a prompt of 100,000 tokens read
//!   at 200 tokens a second. It is also the default time limit of the
//!   dialect's own client libraries, so it is what these servers are run
//!   against.
//! - **Silence after that may last [`LOCAL_IDLE`], five minutes.** A server
//!   that parses tool calls may hold back a call's arguments until they are
//!   complete, and a file written by a slow model is minutes of output.
//!
//! Both are the entry's, and a user's own entry can say otherwise.

use std::time::Duration;

use super::{NAME, account, context, flag};
use crate::protocol::{FlagValue, ProviderInfo};

/// OpenAI's own API.
pub const OPENAI: &str = "openai";
/// OpenRouter.
pub const OPENROUTER: &str = "openrouter";
/// DeepSeek's own API.
pub const DEEPSEEK: &str = "deepseek";
/// Venice.ai.
pub const VENICE: &str = "venice";
/// A local Ollama.
pub const OLLAMA: &str = "ollama";
/// A local llama.cpp server.
pub const LLAMACPP: &str = "llamacpp";
/// A local vLLM server.
pub const VLLM: &str = "vllm";
/// A local LM Studio.
pub const LMSTUDIO: &str = "lmstudio";

/// Every built-in provider of this protocol, by name.
pub const ALL: &[&str] = &[
    OPENAI, OPENROUTER, DEEPSEEK, VENICE, OLLAMA, LLAMACPP, VLLM, LMSTUDIO,
];

/// How long a local server may take before the first event of a response.
/// Why is in the [module](self).
pub const LOCAL_FIRST_EVENT: Duration = Duration::from_secs(600);

/// How long a local server's response may be silent after it began.
pub const LOCAL_IDLE: Duration = Duration::from_secs(300);

/// A built-in provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Builtin {
    /// The endpoint and its flags.
    pub info: ProviderInfo,
    /// The environment variable the provider's key is looked for in.
    /// `None` for a server that takes no key by default.
    pub key_variable: Option<&'static str>,
    /// Whether the server runs on the user's own machine.
    pub local: bool,
}

/// The built-in provider of a name, or `None` when this protocol has none
/// by that name.
#[must_use]
pub fn builtin(id: &str) -> Option<Builtin> {
    let text = |value: &str| FlagValue::Text(value.to_owned());
    let hosted = |url: &str, key_variable: &'static str| Builtin {
        info: ProviderInfo::new(id, NAME, url),
        key_variable: Some(key_variable),
        local: false,
    };
    let local = |url: &str, probe: &str| {
        let mut info = ProviderInfo::new(id, NAME, url);
        info.first_event_timeout = Some(LOCAL_FIRST_EVENT);
        info.idle_timeout = Some(LOCAL_IDLE);
        info.flags.set(flag::CONTEXT_PROBE, text(probe));
        Builtin {
            info,
            key_variable: None,
            local: true,
        }
    };
    let mut provider = match id {
        OPENAI => hosted("https://api.openai.com/v1", "OPENAI_API_KEY"),
        OPENROUTER => hosted("https://openrouter.ai/api/v1", "OPENROUTER_API_KEY"),
        DEEPSEEK => hosted("https://api.deepseek.com", "DEEPSEEK_API_KEY"),
        VENICE => hosted("https://api.venice.ai/api/v1", "VENICE_API_KEY"),
        OLLAMA => local("http://localhost:11434/v1", context::OLLAMA_PS),
        LLAMACPP => local("http://localhost:9931/v1", context::LLAMACPP_PROPS),
        VLLM => local("http://localhost:8000/v1", context::VLLM_MODELS),
        LMSTUDIO => local("http://localhost:1234/v1", context::LMSTUDIO_MODELS),
        _ => return None,
    };
    let flags = &mut provider.info.flags;
    match id {
        OPENAI => {
            flags.set(flag::MAX_TOKENS_FIELD, text("max_completion_tokens"));
            flags.set(flag::SYSTEM_ROLE, text("developer"));
            flags.set(flag::PROMPT_CACHE_KEY, FlagValue::Bool(true));
        }
        OPENROUTER => {
            flags.set(flag::REASONING_PARAM, text("reasoning"));
            flags.set(flag::USAGE_ENDPOINT, text(account::OPENROUTER_KEY));
        }
        DEEPSEEK => {
            flags.set(flag::REASONING_PASSBACK, text("reasoning_content"));
            flags.set(flag::USAGE_ENDPOINT, text(account::DEEPSEEK_BALANCE));
        }
        VENICE => {
            flags.set(flag::MAX_TOKENS_FIELD, text("max_completion_tokens"));
            flags.set(flag::PROMPT_CACHE_KEY, FlagValue::Bool(true));
            // The server puts a system prompt of its own before the
            // caller's unless it is told not to: a thousand tokens of
            // instructions the harness did not write.
            flags.set(
                flag::EXTRA_BODY,
                text(r#"{"venice_parameters":{"include_venice_system_prompt":false}}"#),
            );
            flags.set(flag::USAGE_ENDPOINT, text(account::VENICE_RATE_LIMITS));
        }
        VLLM => {
            flags.set(flag::MAX_TOKENS_FIELD, text("max_completion_tokens"));
        }
        _ => {}
    }
    Some(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_provider_is_an_entry_of_this_protocol() {
        for id in ALL {
            let provider = builtin(id).unwrap_or_else(|| panic!("no {id}"));
            assert_eq!(provider.info.id.as_str(), *id);
            assert_eq!(provider.info.protocol, NAME);
            assert!(!provider.info.base_url.ends_with('/'), "{id}");
            // A key goes only over TLS. A local server takes none.
            assert_eq!(
                provider.info.base_url.starts_with("https://"),
                !provider.local,
                "{id}"
            );
            assert_eq!(provider.key_variable.is_some(), !provider.local, "{id}");
        }
        // The address each server's documentation gives.
        let urls: Vec<(&str, String)> = ALL
            .iter()
            .map(|id| (*id, builtin(id).unwrap().info.base_url))
            .collect();
        let expected = [
            (OPENAI, "https://api.openai.com/v1"),
            (OPENROUTER, "https://openrouter.ai/api/v1"),
            (DEEPSEEK, "https://api.deepseek.com"),
            (VENICE, "https://api.venice.ai/api/v1"),
            (OLLAMA, "http://localhost:11434/v1"),
            (LLAMACPP, "http://localhost:9931/v1"),
            (VLLM, "http://localhost:8000/v1"),
            (LMSTUDIO, "http://localhost:1234/v1"),
        ];
        for ((id, url), (expected_id, expected_url)) in urls.iter().zip(expected) {
            assert_eq!((*id, url.as_str()), (expected_id, expected_url));
        }
        assert_eq!(builtin("anthropic"), None);
        assert_eq!(builtin("a-server-nobody-told-us-about"), None);
    }

    #[test]
    fn a_local_server_gets_time_to_read_a_long_prompt_and_a_hosted_one_does_not() {
        for id in ALL {
            let provider = builtin(id).unwrap();
            if provider.local {
                assert_eq!(
                    provider.info.first_event_timeout,
                    Some(LOCAL_FIRST_EVENT),
                    "{id}"
                );
                assert_eq!(provider.info.idle_timeout, Some(LOCAL_IDLE), "{id}");
                assert!(
                    provider.info.flags.text(flag::CONTEXT_PROBE).is_some(),
                    "{id}"
                );
                assert_eq!(provider.info.flags.text(flag::USAGE_ENDPOINT), None, "{id}");
            } else {
                // The retry policy's own limits.
                assert_eq!(provider.info.first_event_timeout, None, "{id}");
                assert_eq!(provider.info.idle_timeout, None, "{id}");
                assert_eq!(provider.info.flags.text(flag::CONTEXT_PROBE), None, "{id}");
            }
        }
        assert_eq!(LOCAL_FIRST_EVENT, Duration::from_secs(10 * 60));
    }

    #[test]
    fn each_entry_sets_the_flags_its_server_needs() {
        let flags = |id: &str| builtin(id).unwrap().info.flags;
        let openai = flags(OPENAI);
        assert_eq!(
            openai.text(flag::MAX_TOKENS_FIELD),
            Some("max_completion_tokens")
        );
        assert_eq!(openai.text(flag::SYSTEM_ROLE), Some("developer"));
        assert_eq!(openai.bool(flag::PROMPT_CACHE_KEY), Some(true));
        // Its usage API needs an admin key, which is not the key a user
        // gives for model calls: no refresh.
        assert_eq!(openai.text(flag::USAGE_ENDPOINT), None);

        let openrouter = flags(OPENROUTER);
        assert_eq!(openrouter.text(flag::REASONING_PARAM), Some("reasoning"));
        assert_eq!(
            openrouter.text(flag::USAGE_ENDPOINT),
            Some(account::OPENROUTER_KEY)
        );
        assert_eq!(openrouter.text(flag::MAX_TOKENS_FIELD), None);

        let deepseek = flags(DEEPSEEK);
        assert_eq!(
            deepseek.text(flag::USAGE_ENDPOINT),
            Some(account::DEEPSEEK_BALANCE)
        );
        assert_eq!(deepseek.text(flag::MAX_TOKENS_FIELD), None);

        let venice = flags(VENICE);
        assert_eq!(
            venice.text(flag::MAX_TOKENS_FIELD),
            Some("max_completion_tokens")
        );
        assert_eq!(
            venice.text(flag::USAGE_ENDPOINT),
            Some(account::VENICE_RATE_LIMITS)
        );

        assert_eq!(
            flags(OLLAMA).text(flag::CONTEXT_PROBE),
            Some(context::OLLAMA_PS)
        );
        assert_eq!(
            flags(LLAMACPP).text(flag::CONTEXT_PROBE),
            Some(context::LLAMACPP_PROPS)
        );
        assert_eq!(
            flags(VLLM).text(flag::CONTEXT_PROBE),
            Some(context::VLLM_MODELS)
        );
        assert_eq!(
            flags(LMSTUDIO).text(flag::CONTEXT_PROBE),
            Some(context::LMSTUDIO_MODELS)
        );
        assert_eq!(
            flags(VLLM).text(flag::MAX_TOKENS_FIELD),
            Some("max_completion_tokens")
        );
        assert_eq!(flags(OLLAMA).text(flag::MAX_TOKENS_FIELD), None);
    }
}
