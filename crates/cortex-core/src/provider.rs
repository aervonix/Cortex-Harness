//! Models, providers and credentials, from configuration to a client the
//! loop can send a request through (`docs/architecture/04-providers.md`).
//!
//! - **The catalog** is the built-in one with the user's entries laid over
//!   it ([`catalog`]). A user entry that the catalog cannot read stops the
//!   core from starting, and the error says which file, which entry and
//!   what is wrong with it: a model that silently fell out of the catalog
//!   would surface much later, as "no such model".
//! - **A provider** is Anthropic's own API, built in, or an entry of
//!   `[providers.<name>]` ([`provider_info`]). A user-defined provider that
//!   speaks `anthropic-messages` is replayed reasoning by the stricter
//!   rule unless its capabilities say `replay_reasoning = "all"`.
//! - **A key** is found by `cortex-config` (a flag, the provider's
//!   environment variable, the credential store) and handed to the
//!   provider layer here ([`Providers::auth`]). Every key that is found is
//!   also given to the secret filter, so that it is masked by value
//!   wherever it turns up.
//! - **A reasoning level** as the user wrote it is mapped onto the model's
//!   own control ([`reasoning`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use cortex_agent::RoleModel;
use cortex_base::{Secret, SharedClock, SharedTimer};
use cortex_config::credentials::Lookup;
use cortex_config::{CredentialStore, Env, KeyPath, Loaded, Settings};
use cortex_llm::anthropic;
use cortex_llm::{
    AnthropicMessages, ApiKeyAuth, AuthMethod, Catalog, FlagValue, ImageSource, KeyPlacement,
    ModelClient, ModelEntry, Protocol, ProviderClient, ProviderInfo, Reasoning,
    ReasoningVisibility, RetryPolicy, SpeedCooldowns, SpeedPolicy, Transport,
};
use cortex_protocol::model::{Modality, ReasoningControlKind};
use cortex_protocol::{ErrorData, ErrorKind, ModelId, ProviderId};
use cortex_tools::ImageSupport;

/// The environment variable Anthropic's key is read from when a provider's
/// entry names no other.
pub const ANTHROPIC_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// The image types a model that takes images is sent, and the largest
/// image: what Anthropic's API accepts.
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// The portable names for a reasoning level, weakest first.
pub const EFFORT_ALIASES: [&str; 5] = ["minimal", "low", "medium", "high", "max"];

fn config_error(code: &str, message: impl Into<String>) -> Box<ErrorData> {
    Box::new(ErrorData::new(ErrorKind::Config, code, message))
}

/// Makes the client of a provider, in place of the built-in way. A test
/// gives the core one that answers from a script; the harness gives none.
pub trait Connector: Send + Sync {
    /// The client for `provider`, or `None` to have the core build the
    /// real one. `images` is the session's stored images, which the client
    /// puts into requests that refer to them.
    fn connect(
        &self,
        provider: &ProviderInfo,
        images: Arc<dyn ImageSource>,
    ) -> Option<Arc<dyn ModelClient>>;
}

/// The built-in catalog with the user's `[models.<name>]` entries over it.
///
/// # Errors
///
/// When the built-in catalog is damaged (a bug), or an entry of the user's
/// cannot be read: the error names the file and line the entry is in, the
/// entry, and what is wrong with it.
pub(crate) fn catalog(loaded: &Loaded, env: &Env) -> Result<Catalog, Box<ErrorData>> {
    let mut catalog = Catalog::builtin()
        .map_err(|error| {
            Box::new(ErrorData::new(
                ErrorKind::Internal,
                "invalid_catalog",
                format!("The built-in model catalog cannot be read: {error}"),
            ))
        })?
        .clone();
    let Some(section) = loaded.section("models") else {
        return Ok(catalog);
    };
    for (name, entry) in section {
        let place = || {
            let origin = loaded.origin(&KeyPath::new(["models", name.as_str()]));
            match (&origin.file, origin.line) {
                (Some(file), Some(line)) => format!("{}:{line}", env.display_path(file)),
                (Some(file), None) => env.display_path(file),
                _ => "the configuration".to_owned(),
            }
        };
        let Some(entry) = entry.as_table() else {
            return Err(config_error(
                "invalid_catalog",
                format!(
                    "`[models.{name}]` in {} is not a table. A model entry is a table with the \
                     keys of a `[[model]]` entry of the built-in catalog.",
                    place()
                ),
            ));
        };
        let mut file = toml::Table::new();
        file.insert(
            "version".to_owned(),
            toml::Value::String(format!("user:{name}")),
        );
        file.insert(
            "model".to_owned(),
            toml::Value::Array(vec![toml::Value::Table(entry.clone())]),
        );
        let one = Catalog::read(toml::Value::Table(file)).map_err(|error| {
            config_error(
                "invalid_catalog",
                format!(
                    "The model entry `[models.{name}]` in {} cannot be used: {}. The session was \
                     not started, so that a model you configured is not silently missing. Fix \
                     the entry or remove it.",
                    place(),
                    error.message
                ),
            )
        })?;
        catalog.overlay(&one);
    }
    Ok(catalog)
}

/// A provider as the provider layer knows it, from the built-in table and
/// the user's `[providers.<name>]` entry.
///
/// # Errors
///
/// When the provider is not built in and has no entry, is turned off, or
/// its entry names no protocol or base URL.
pub(crate) fn provider_info(
    settings: &Settings,
    env: &Env,
    name: &str,
) -> Result<ProviderInfo, Box<ErrorData>> {
    let entry = settings.providers.get(name);
    if entry.is_some_and(|entry| !entry.enabled) {
        return Err(config_error(
            "provider_disabled",
            format!("The provider `{name}` is turned off (`providers.{name}.enabled = false`)."),
        ));
    }
    let mut info = if name == anthropic::PROVIDER {
        anthropic::provider()
    } else {
        let entry = entry.ok_or_else(|| {
            config_error(
                "unknown_provider",
                format!(
                    "There is no provider `{name}`. Add `[providers.{name}]` with its `protocol` \
                     and `base_url`, or use the built-in `{}`.",
                    anthropic::PROVIDER
                ),
            )
        })?;
        let protocol = entry.protocol.as_deref().ok_or_else(|| {
            config_error(
                "invalid_provider",
                format!("`[providers.{name}]` names no `protocol`."),
            )
        })?;
        let base_url = entry.base_url.as_deref().ok_or_else(|| {
            config_error(
                "invalid_provider",
                format!("`[providers.{name}]` names no `base_url`."),
            )
        })?;
        // No flag is set for it: a server that only looks like Anthropic's
        // is replayed reasoning by the stricter rule unless its entry says
        // otherwise, below.
        ProviderInfo::new(name, protocol, base_url)
    };
    let Some(entry) = entry else {
        return Ok(info);
    };
    if let Some(protocol) = &entry.protocol {
        info.protocol.clone_from(protocol);
    }
    if let Some(base_url) = &entry.base_url {
        info.base_url = base_url.trim_end_matches('/').to_owned();
    }
    for (header, value) in &entry.headers.0 {
        if let Some(value) = value.resolve(env) {
            info.headers.insert_secret(header, &value);
        }
    }
    info.idle_timeout = entry.idle_timeout.or(info.idle_timeout);
    info.first_event_timeout = entry.first_event_timeout.or(info.first_event_timeout);
    for (flag, value) in &entry.capabilities.0 {
        let value = match value {
            toml::Value::Boolean(value) => FlagValue::Bool(*value),
            toml::Value::Integer(value) => FlagValue::Number(*value),
            toml::Value::String(value) => FlagValue::Text(value.clone()),
            _ => {
                return Err(config_error(
                    "invalid_provider",
                    format!(
                        "`providers.{name}.capabilities.{flag}` must be text, a whole number or \
                         `true`/`false`."
                    ),
                ));
            }
        };
        info.flags.set(flag, value);
    }
    Ok(info)
}

/// The environment variable a provider's key is read from.
pub(crate) fn key_variable(settings: &Settings, provider: &str) -> Option<String> {
    settings
        .providers
        .get(provider)
        .and_then(|entry| entry.key_env.clone())
        .or_else(|| (provider == anthropic::PROVIDER).then(|| ANTHROPIC_KEY_ENV.to_owned()))
}

/// The key variable of every provider there is: the built-in one, and each
/// configured one's. A shell command is given none of them.
pub(crate) fn key_variables(settings: &Settings) -> Vec<String> {
    let mut variables = vec![ANTHROPIC_KEY_ENV.to_owned()];
    for (name, entry) in &settings.providers {
        if let Some(variable) = key_variable(settings, name)
            && !variables.contains(&variable)
        {
            variables.push(variable);
        }
        // A header that reads a variable holds a key too.
        for value in entry.headers.0.values() {
            if let cortex_config::schema::EnvText::Env(variable) = value
                && !variables.contains(variable)
            {
                variables.push(variable.clone());
            }
        }
    }
    variables
}

/// The retry settings of `provider`, as the provider layer's policy.
pub(crate) fn retry_policy(settings: &Settings, provider: &str) -> RetryPolicy {
    let retry = settings.retry_for(provider);
    RetryPolicy {
        max_attempts: retry.attempts,
        base: retry.backoff_base,
        factor: retry.backoff_factor,
        cap: retry.backoff_cap,
        max_wait: retry.max_wait,
        idle_timeout: retry.idle_timeout,
        first_event_timeout: retry.first_event_timeout,
        speed: SpeedPolicy {
            fallback: settings.model.speed_fallback,
            short_wait: settings.model.speed_wait,
            cooldown: settings.model.speed_cooldown,
            min_cooldown: settings.model.speed_cooldown_min,
        },
    }
}

/// A reasoning level as the user wrote it, mapped onto the model's own
/// control. `None` leaves the model's default.
///
/// - A model with **levels** takes one of its own level names as it is. A
///   portable alias (`minimal`, `low`, `medium`, `high`, `max`) that is not
///   one of its names is mapped to the level at the same place along the
///   model's own scale.
/// - A model with a **token budget** takes a number of tokens, or an alias,
///   which is mapped along its range.
/// - A model with **no control** takes none: the setting is left out.
///
/// # Errors
///
/// When the text is neither a level of the model, an alias, nor (for a
/// budget) a number in the model's range. The error lists what the model
/// takes.
pub(crate) fn reasoning(
    entry: &ModelEntry,
    effort: Option<&str>,
    visibility: ReasoningVisibility,
) -> Result<Reasoning, Box<ErrorData>> {
    let mut reasoning = Reasoning {
        level: None,
        budget_tokens: None,
        visibility,
    };
    let Some(effort) = effort.map(str::trim).filter(|effort| !effort.is_empty()) else {
        return Ok(reasoning);
    };
    let control = &entry.info.reasoning;
    let alias = EFFORT_ALIASES.iter().position(|alias| *alias == effort);
    let id = &entry.info.id;
    match &control.kind {
        ReasoningControlKind::Levels => {
            let levels = &control.levels;
            if levels.iter().any(|level| level == effort) {
                reasoning.level = Some(effort.to_owned());
            } else if let (Some(alias), false) = (alias, levels.is_empty()) {
                // The same place along the model's own scale, rounded to
                // the nearest level.
                let last = levels.len() - 1;
                let place = (alias * last * 2 + (EFFORT_ALIASES.len() - 1))
                    / ((EFFORT_ALIASES.len() - 1) * 2);
                reasoning.level = Some(levels[place.min(last)].clone());
            } else {
                return Err(config_error(
                    "unknown_effort",
                    format!(
                        "The model `{id}` has no reasoning level `{effort}`. Its levels are {}; \
                         the portable names {} work for every model.",
                        levels.join(", "),
                        EFFORT_ALIASES.join(", ")
                    ),
                ));
            }
        }
        ReasoningControlKind::Budget => {
            let min = control.min_tokens.unwrap_or(0);
            let max = control.max_tokens.unwrap_or(min).max(min);
            let tokens = if let Some(alias) = alias {
                let steps = (EFFORT_ALIASES.len() - 1) as u64;
                min + (max - min) * alias as u64 / steps
            } else if let Ok(tokens) = effort.parse::<u64>() {
                if !(min..=max).contains(&tokens) {
                    return Err(config_error(
                        "unknown_effort",
                        format!(
                            "The model `{id}` takes a reasoning budget of {min} to {max} tokens, \
                             not {tokens}."
                        ),
                    ));
                }
                tokens
            } else {
                return Err(config_error(
                    "unknown_effort",
                    format!(
                        "The model `{id}` takes a reasoning budget in tokens ({min} to {max}) or \
                         one of {}, not `{effort}`.",
                        EFFORT_ALIASES.join(", ")
                    ),
                ));
            };
            reasoning.budget_tokens = Some(tokens);
        }
        // A model with no control, or a kind of control this version does
        // not know: it is told nothing.
        _ => {}
    }
    Ok(reasoning)
}

/// The images a model accepts in a tool result.
pub(crate) fn image_support(entry: &ModelEntry) -> Option<ImageSupport> {
    entry
        .info
        .input
        .contains(&Modality::Image)
        .then(|| ImageSupport {
            media_types: IMAGE_TYPES.iter().map(|kind| (*kind).to_owned()).collect(),
            max_bytes: IMAGE_BYTES,
        })
}

/// What clients are made from, shared by every session of a core.
pub(crate) struct Providers {
    pub(crate) catalog: Catalog,
    pub(crate) cooldowns: SpeedCooldowns,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) clock: SharedClock,
    pub(crate) timer: SharedTimer,
    pub(crate) protocols: BTreeMap<String, Arc<dyn Protocol>>,
    pub(crate) connector: Option<Arc<dyn Connector>>,
    pub(crate) store: Option<Box<dyn CredentialStore>>,
    /// Keys given on the command line, by provider.
    pub(crate) flags: BTreeMap<String, Secret>,
    /// Every key that was found, so that the secret filter masks it.
    pub(crate) found: Mutex<Vec<Secret>>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers")
            .field("protocols", &self.protocols.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The protocols the core knows by itself.
pub(crate) fn builtin_protocols() -> BTreeMap<String, Arc<dyn Protocol>> {
    let mut protocols: BTreeMap<String, Arc<dyn Protocol>> = BTreeMap::new();
    protocols.insert(anthropic::NAME.to_owned(), Arc::new(AnthropicMessages));
    protocols
}

impl Providers {
    /// The key of `provider`, placed the way its protocol wants it.
    ///
    /// # Errors
    ///
    /// An error of kind `auth` when no key is found, saying where one is
    /// looked for, or when the credential store cannot be read.
    pub(crate) fn auth(
        &self,
        settings: &Settings,
        env: &Env,
        provider: &ProviderInfo,
    ) -> Result<Arc<dyn AuthMethod>, Box<ErrorData>> {
        let name = provider.id.as_str();
        let variable = key_variable(settings, name);
        let auth_error =
            |code: &str, message: String| Box::new(ErrorData::new(ErrorKind::Auth, code, message));
        let Some(store) = &self.store else {
            return Err(auth_error(
                "no_credential_store",
                "The credential store could not be opened.".to_owned(),
            ));
        };
        let found = cortex_config::resolve(Lookup {
            provider: name,
            flag: self.flags.get(name),
            variable: variable.as_deref(),
            env,
            store: store.as_ref(),
        })
        .map_err(|error| auth_error("credentials_unreadable", error.to_string()))?;
        let Some(credential) = found else {
            let variable = variable.map_or_else(String::new, |variable| {
                format!("set `{variable}` in the environment, or ")
            });
            return Err(auth_error(
                "no_credentials",
                format!(
                    "No API key for the provider `{name}`. To give one, {variable}store it with \
                     `cortex auth login {name}`."
                ),
            ));
        };
        self.found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(credential.secret.clone());
        let source = credential.source.to_string();
        let auth: ApiKeyAuth = if provider.protocol == anthropic::NAME {
            anthropic::api_key(credential.secret, source)
        } else {
            ApiKeyAuth::new(credential.secret, KeyPlacement::Bearer, source)
        };
        Ok(Arc::new(auth))
    }

    /// The client of `provider` for a session whose stored images are
    /// `images`.
    ///
    /// # Errors
    ///
    /// When the provider's protocol is not one this build speaks, or no
    /// key is found for it.
    pub(crate) fn client(
        &self,
        settings: &Settings,
        env: &Env,
        provider: &ProviderInfo,
        images: Arc<dyn ImageSource>,
    ) -> Result<Arc<dyn ModelClient>, Box<ErrorData>> {
        if let Some(connector) = &self.connector
            && let Some(client) = connector.connect(provider, Arc::clone(&images))
        {
            return Ok(client);
        }
        let protocol = self.protocols.get(&provider.protocol).ok_or_else(|| {
            config_error(
                "unknown_protocol",
                format!(
                    "The provider `{}` speaks `{}`, which this version does not. It speaks {}.",
                    provider.id,
                    provider.protocol,
                    self.protocols
                        .keys()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })?;
        let auth = self.auth(settings, env, provider)?;
        let client = ProviderClient::builder(
            Arc::clone(protocol),
            provider.clone(),
            Arc::clone(&self.transport),
            Arc::clone(&self.clock),
            Arc::clone(&self.timer),
        )
        .auth(auth)
        .policy(retry_policy(settings, provider.id.as_str()))
        .cooldowns(self.cooldowns.clone())
        .images(images)
        .build();
        Ok(Arc::new(client))
    }

    /// The catalog entry of a model named in configuration or in
    /// `session/set`, with an optional provider to tell two models with one
    /// id apart.
    ///
    /// # Errors
    ///
    /// When no model has the id, or several providers have one and none
    /// was named.
    pub(crate) fn entry(
        &self,
        model: &str,
        provider: Option<&str>,
    ) -> Result<Arc<ModelEntry>, Box<ErrorData>> {
        let id = ModelId::new(model);
        if let Some(provider) = provider {
            return self
                .catalog
                .get(&ProviderId::new(provider), &id)
                .cloned()
                .ok_or_else(|| {
                    config_error(
                        "unknown_model",
                        format!("The provider `{provider}` has no model `{model}` in the catalog."),
                    )
                });
        }
        let found: Vec<&Arc<ModelEntry>> = self
            .catalog
            .models()
            .iter()
            .filter(|entry| entry.info.id == id)
            .collect();
        match found.as_slice() {
            [one] => Ok(Arc::clone(one)),
            [] => Err(config_error(
                "unknown_model",
                format!(
                    "There is no model `{model}` in the catalog. `cortex models` lists the ones \
                     there are, and `[models.<name>]` adds one."
                ),
            )),
            several => Err(config_error(
                "ambiguous_model",
                format!(
                    "Several providers have a model `{model}`: {}. Name the one to use with \
                     `model.roles.<role>.provider`.",
                    several
                        .iter()
                        .map(|entry| format!("`{}`", entry.info.provider))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    /// A role's model as the loop runs it: the entry, its client, and the
    /// reasoning level mapped onto the model's control.
    pub(crate) fn role_model(
        &self,
        settings: &Settings,
        env: &Env,
        entry: Arc<ModelEntry>,
        effort: Option<&str>,
        max_output: Option<u64>,
        images: Arc<dyn ImageSource>,
    ) -> Result<RoleModel, Box<ErrorData>> {
        let provider = provider_info(settings, env, entry.info.provider.as_str())?;
        let visibility = match settings.model.reasoning {
            cortex_config::schema::ReasoningVisibility::None => ReasoningVisibility::None,
            _ => ReasoningVisibility::Summary,
        };
        let reasoning = reasoning(&entry, effort, visibility)?;
        let client = self.client(settings, env, &provider, images)?;
        let mut model = RoleModel::new(client, entry);
        model.reasoning = reasoning;
        model.effort = effort.map(str::to_owned);
        model.max_output = max_output;
        Ok(model)
    }
}

#[cfg(test)]
mod tests {
    use cortex_testkit::fixtures;

    use super::*;

    fn levels(effort: &str) -> Result<Option<String>, String> {
        // The fixture model has the levels low, medium and high.
        reasoning(
            &fixtures::model(),
            Some(effort),
            ReasoningVisibility::Summary,
        )
        .map(|reasoning| reasoning.level)
        .map_err(|error| error.message.clone())
    }

    #[test]
    fn a_models_own_level_is_taken_as_it_is_and_an_alias_is_placed_on_its_scale() {
        let own = fixtures::model().info.reasoning.levels.clone();
        assert!(own.len() >= 2, "the fixture model has levels: {own:?}");
        for level in &own {
            assert_eq!(levels(level).unwrap().as_ref(), Some(level));
        }
        // The ends of the portable scale are the ends of the model's.
        assert_eq!(levels("minimal").unwrap().as_ref(), own.first());
        assert_eq!(levels("max").unwrap().as_ref(), own.last());
        // Nothing set leaves the model's default.
        let none = reasoning(&fixtures::model(), None, ReasoningVisibility::None).unwrap();
        assert_eq!(none.level, None);
        assert_eq!(none.visibility, ReasoningVisibility::None);
        let blank = reasoning(&fixtures::model(), Some("  "), ReasoningVisibility::Summary);
        assert_eq!(blank.unwrap().level, None);
    }

    #[test]
    fn a_level_the_model_does_not_have_is_refused_with_the_ones_it_has() {
        let error = levels("ludicrous").unwrap_err();
        assert!(error.contains("ludicrous"), "{error}");
        for level in &fixtures::model().info.reasoning.levels {
            assert!(error.contains(level.as_str()), "{error}");
        }
        assert!(error.contains("minimal"), "{error}");
    }

    #[test]
    fn aliases_spread_evenly_over_a_scale_of_any_length() {
        let place = |levels: usize, alias: usize| {
            let last = levels - 1;
            (alias * last * 2 + 4) / 8
        };
        // Five levels: one each.
        assert_eq!(
            (0..5).map(|a| place(5, a)).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
        // Three: low, low/medium, medium, high-ish, high.
        assert_eq!(
            (0..5).map(|a| place(3, a)).collect::<Vec<_>>(),
            [0, 1, 1, 2, 2]
        );
        // One level: always it.
        assert_eq!(
            (0..5).map(|a| place(1, a)).collect::<Vec<_>>(),
            [0, 0, 0, 0, 0]
        );
        // Two.
        assert_eq!(
            (0..5).map(|a| place(2, a)).collect::<Vec<_>>(),
            [0, 0, 1, 1, 1]
        );
    }
}
