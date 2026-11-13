//! The sections and their settings: every key of a configuration file, declared
//! once (`docs/architecture/08-configuration.md`, "Sections").
//!
//! A section that a later build step owns is `reserved` here: its name is
//! known, its content is accepted unread, and from a project's files it is
//! ignored or held back until that step declares each key and says what a
//! project may set. A reserved section fails closed.

use std::collections::BTreeMap;
use std::time::Duration;

use cortex_protocol::lease::{NetworkAccess, Rule};
use cortex_protocol::{Level, ModelRole, Speed};

use super::types::{Headers, Profiles, ReasoningVisibility, Reserved, StoreChoice};
use super::{CONFIG_VERSION, settings};

settings! {
    /// Everything a configuration file can say, with every default.
    pub struct Settings {
        /// The version of the configuration schema the file was written
        /// for. A file with a newer version than this build understands is
        /// refused.
        [Preference, Restart, 1..=1_000_000]
        pub config_version: u32 = CONFIG_VERSION,
        /// The profile to apply: the name of a `[profiles.<name>]` table.
        /// The `--profile` flag overrides it. A project's file may define
        /// profiles and may not select one: a profile the user wrote is
        /// applied with the user's rights, so a repository that could pick
        /// it could switch on whatever the user's loosest profile allows.
        [UserOnly, NextEpoch]
        pub profile: Option<String>,
        /// Named sets of settings. A profile may hold any key of this file
        /// except `profile`, `profiles`, `managed` and `config_version`.
        [nested]
        pub profiles: Profiles,
        /// For the managed file of a shared machine only.
        [nested]
        pub managed: ManagedSettings,
        /// Models by role, the reasoning level and the speed tier.
        [nested]
        pub model: ModelSettings,
        /// Providers by name. A built-in provider needs no entry; an entry
        /// changes it or adds a new one. `enabled = false` removes one.
        [nested]
        pub providers: BTreeMap<String, ProviderSettings>,
        /// Catalog overrides and additions by model name: limits, prices,
        /// reasoning control, capability flags.
        [reserved UserOnly]
        pub models: Reserved,
        /// The HTTP client: the connect timeout, a proxy, and extra
        /// certificate authorities.
        [reserved UserOnly]
        pub http: Reserved,
        /// How a request that failed is tried again.
        [nested]
        pub retry: RetrySettings,
        /// The permission mode and the permission rules.
        [enforced]
        pub permissions: PermissionSettings,
        /// What a command the harness starts may touch.
        [enforced]
        pub sandbox: SandboxSettings,
        /// The most a turn, a session and a day may spend.
        [enforced]
        pub limits: LimitSettings,
        /// Where stored credentials are kept. Never the credentials
        /// themselves: no key of this file takes a secret.
        [nested]
        pub credentials: CredentialSettings,
        /// Where sessions are stored.
        [nested]
        pub session: SessionSettings,
        /// Which files written for other tools are read.
        [nested]
        pub compat: CompatSettings,
        /// What is logged.
        [nested]
        pub log: LogSettings,
        /// Enabled tools, output limits and per-tool settings.
        [reserved UserOnly]
        pub tools: Reserved,
        /// The shell that runs commands: program, arguments, time limits.
        [reserved UserOnly]
        pub shell: Reserved,
        /// The compaction trigger and settings, and instruction file names.
        [reserved UserOnly]
        pub context: Reserved,
        /// The verification mode, commands and time limits.
        [reserved RunsCode]
        pub verify: Reserved,
        /// Variant overrides per model, and the map and profile budgets.
        [reserved UserOnly]
        pub adapt: Reserved,
        /// Whether local statistics are recorded, and for how long.
        [reserved UserOnly]
        pub stats: Reserved,
        /// The repository map's caps.
        [reserved UserOnly]
        pub index: Reserved,
        /// The run engine: concurrency, workspaces, budgets.
        [reserved UserOnly]
        pub runs: Reserved,
        /// The usage view: week start, ranges, the opt-in refresh and the
        /// other tools whose records may be read.
        [reserved UserOnly]
        pub usage: Reserved,
        /// The terminal UI: presenter, theme, density.
        [reserved UserOnly]
        pub tui: Reserved,
        /// Key bindings by mode.
        [reserved UserOnly]
        pub keys: Reserved,
        /// Terminal capability overrides.
        [reserved UserOnly]
        pub terminal: Reserved,
        /// The RPC server: socket path, token, client timeouts.
        [reserved UserOnly]
        pub rpc: Reserved,
        /// Hooks by name.
        [reserved RunsCode]
        pub hooks: Reserved,
        /// Extensions by name.
        [reserved RunsCode]
        pub extensions: Reserved,
        /// Where skills are looked for.
        [reserved UserOnly]
        pub skills: Reserved,
        /// MCP servers by name, and MCP settings.
        [reserved RunsCode]
        pub mcp: Reserved,
    }
}

settings! {
    /// What only the managed file may say.
    pub struct ManagedSettings {
        /// Keys that no layer above the managed file may change. A table's
        /// name locks every key in it. Read from the managed file only.
        [UserOnly, Now]
        pub locked: Vec<String>,
    }
}

settings! {
    /// Models by role, the reasoning level and the speed tier.
    pub struct ModelSettings {
        /// The model for the agent's own turns. Needed to run a turn.
        [Preference, NextEpoch]
        pub main: Option<String>,
        /// The model for utility calls: summaries, titles. Without one, the
        /// main model is used.
        [Preference, NextEpoch]
        pub fast: Option<String>,
        /// The model for model-graded comparisons. Without one, the main
        /// model is used.
        [Preference, NextEpoch]
        pub judge: Option<String>,
        /// The reasoning level of the main role: one of the model's own
        /// levels, or `minimal`, `low`, `medium`, `high` or `max`.
        [Preference, NextEpoch]
        pub effort: Option<String>,
        /// How much of the model's reasoning is shown.
        [Preference, NextEpoch]
        pub reasoning: ReasoningVisibility = ReasoningVisibility::Summary,
        /// The most output tokens one response may have. Without it, the
        /// model's own limit applies.
        [Preference, NextEpoch, 1..=10_000_000]
        pub max_output: Option<u64>,
        /// The session's speed tier, which the main role runs at:
        /// `standard`, `fast` where the model offers it, or a provider's own
        /// tier name. The utility roles do not follow it: they run at
        /// `standard` unless `model.roles.<role>.speed` says otherwise.
        [Preference, Now]
        pub speed: Speed = Speed::Standard,
        /// Whether a request that a speed tier refuses is sent again at
        /// `standard`. When off, such a refusal is an ordinary rate limit.
        [Preference, Now]
        pub speed_fallback: bool = true,
        /// The longest one request waits in place for a speed tier before it
        /// falls back to `standard`. `"0s"` never waits.
        [Preference, Now]
        pub speed_wait: Duration = Duration::from_secs(20),
        /// How long requests stay at `standard` after a fallback, when the
        /// provider does not say when the tier is available again.
        [Preference, Now]
        pub speed_cooldown: Duration = Duration::from_secs(600),
        /// Settings of one role: `main`, `fast` or `judge`.
        [nested]
        pub roles: BTreeMap<String, RoleSettings>,
    }
}

settings! {
    /// The settings of one model role.
    pub struct RoleSettings {
        /// The provider that serves the role's model. Needed only when two
        /// providers have a model with the same name.
        [Preference, NextEpoch]
        pub provider: Option<String>,
        /// The role's reasoning level. For `main` it overrides
        /// `model.effort`.
        [Preference, NextEpoch]
        pub effort: Option<String>,
        /// The role's speed tier. For `main` it overrides `model.speed`.
        [Preference, Now]
        pub speed: Option<Speed>,
    }
}

settings! {
    /// One provider: an endpoint that speaks a protocol.
    pub struct ProviderSettings {
        /// Whether the provider is available. `false` removes an entry that
        /// a lower layer or the built-in table defines.
        [UserOnly, NextEpoch]
        pub enabled: bool = true,
        /// The wire protocol the endpoint speaks.
        [UserOnly, NextEpoch]
        pub protocol: Option<String>,
        /// The endpoint's base URL.
        [UserOnly, NextEpoch]
        pub base_url: Option<String>,
        /// The environment variable that holds the provider's API key. It
        /// is looked at before the credential store.
        [UserOnly, Now]
        pub key_env: Option<String>,
        /// Extra headers for every request. A value is text, or
        /// `{ env = "NAME" }` to read an environment variable when used.
        [UserOnly, Now]
        pub headers: Headers,
        /// How long a response stream may be silent before the request
        /// fails. Overrides `retry.idle_timeout` for this provider.
        [UserOnly, Now]
        pub idle_timeout: Option<Duration>,
        /// How long to wait for the first event of a response. Overrides
        /// `retry.first_event_timeout` for this provider.
        [UserOnly, Now]
        pub first_event_timeout: Option<Duration>,
        /// Capability flags of the endpoint.
        [reserved UserOnly]
        pub capabilities: Reserved,
    }
}

settings! {
    /// How a request that failed is tried again.
    pub struct RetrySettings {
        /// The most attempts for one request, the first included.
        [Preference, Now, 1..=20]
        pub attempts: u32 = 5,
        /// The wait before the second attempt. Each later wait is this
        /// multiplied by `backoff_factor` again, up to `backoff_cap`.
        [Preference, Now]
        pub backoff_base: Duration = Duration::from_secs(1),
        /// What the wait is multiplied by after each attempt.
        [Preference, Now, 1..=10]
        pub backoff_factor: f64 = 2.0,
        /// The longest wait between two attempts.
        [Preference, Now]
        pub backoff_cap: Duration = Duration::from_secs(30),
        /// The longest wait a provider may ask for. A longer one ends the
        /// turn with the error and the provider's hint.
        [Preference, Now]
        pub max_wait: Duration = Duration::from_secs(120),
        /// How long a response stream may be silent before the request
        /// fails.
        [Preference, Now]
        pub idle_timeout: Duration = Duration::from_secs(90),
        /// How long to wait for the first event of a response.
        [Preference, Now]
        pub first_event_timeout: Duration = Duration::from_secs(60),
    }
}

settings! {
    /// The permission mode and the permission rules.
    pub struct PermissionSettings {
        /// The session's level: how much runs without asking.
        [ByLevel, Now]
        pub mode: Level = Level::Supervised,
        /// Permission rules. Every layer adds to the list; `deny` beats
        /// `ask` beats `allow`, whatever the order.
        [ByRuleAction, Now, joined]
        pub rules: Vec<Rule>,
    }
}

settings! {
    /// What a command the harness starts may touch.
    pub struct SandboxSettings {
        /// Whether commands are confined by the OS sandbox where there is
        /// one.
        [UserOnly, Now]
        pub enabled: bool = true,
        /// Paths a command may write besides the workspace. Every layer
        /// adds to the list.
        [WidensAccess, Now, joined]
        pub write: Vec<String>,
        /// Paths a command may not change, besides the built-in ones. Every
        /// layer adds to the list.
        [Preference, Now, joined]
        pub protect: Vec<String>,
        /// Whether a command may use the network.
        [ByNetwork, Now]
        pub network: NetworkAccess = NetworkAccess::Off,
    }
}

settings! {
    /// The most a turn, a session and a day may spend. A limit that is not
    /// set does not apply.
    pub struct LimitSettings {
        /// Limits on one turn.
        [nested]
        pub turn: LimitSet,
        /// Limits on one session.
        [nested]
        pub session: LimitSet,
        /// Limits on one day, across sessions.
        [nested]
        pub day: LimitSet,
    }
}

settings! {
    /// One set of limits.
    pub struct LimitSet {
        /// Requests to a model.
        [TightenOnly, Now, 1..=1_000_000_000]
        pub requests: Option<u64>,
        /// Tokens, input and output together.
        [TightenOnly, Now, 1..=1_000_000_000_000_000]
        pub tokens: Option<u64>,
        /// Money, in US dollars.
        [TightenOnly, Now, 0..=1_000_000_000]
        pub cost: Option<f64>,
        /// Wall-clock time.
        [TightenOnly, Now]
        pub time: Option<Duration>,
    }
}

settings! {
    /// Where stored credentials are kept.
    pub struct CredentialSettings {
        /// The store for API keys saved with `cortex auth login`.
        [UserOnly, Restart]
        pub store: StoreChoice = StoreChoice::Auto,
    }
}

settings! {
    /// Where sessions are stored.
    pub struct SessionSettings {
        /// The data directory. A leading `~` is the home directory.
        /// Without it the platform's usual place is used.
        [UserOnly, Restart]
        pub data_dir: Option<String>,
    }
}

settings! {
    /// Which files written for other tools are read. They are read and
    /// never changed.
    pub struct CompatSettings {
        /// Instruction files: `AGENTS.md` and `CLAUDE.md`.
        [UserOnly, NextEpoch]
        pub instructions: bool = true,
        /// MCP server definitions of other clients, offered for import.
        [UserOnly, NextEpoch]
        pub mcp: bool = true,
        /// Agent Skills folders: `.agents/skills` and `.claude/skills`.
        [UserOnly, NextEpoch]
        pub skills: bool = true,
    }
}

settings! {
    /// What is logged.
    pub struct LogSettings {
        /// The level (`error`, `warn`, `info`, `debug` or `trace`),
        /// optionally followed by levels for target prefixes:
        /// `"info,cortex_llm=debug"`. The `CORTEX_LOG` variable and
        /// `--log-level` override it.
        [UserOnly, Now]
        pub level: Option<String>,
    }
}

impl LimitSet {
    /// The money limit in nano-dollars, the unit the protocol's budgets
    /// use.
    #[must_use]
    pub fn cost_usd_nanos(&self) -> Option<u64> {
        self.cost.map(|dollars| {
            let nanos = (dollars * 1e9).round();
            if nanos <= 0.0 {
                0
            } else if nanos >= u64::MAX as f64 {
                u64::MAX
            } else {
                nanos as u64
            }
        })
    }

    /// Whether no limit is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests.is_none()
            && self.tokens.is_none()
            && self.cost.is_none()
            && self.time.is_none()
    }
}

impl ModelSettings {
    /// The model configured for a role, without the fallback to the main
    /// model that an unset utility role has.
    #[must_use]
    pub fn model_for(&self, role: &ModelRole) -> Option<&str> {
        match role {
            ModelRole::Main => self.main.as_deref(),
            ModelRole::Fast => self.fast.as_deref(),
            ModelRole::Judge => self.judge.as_deref(),
            ModelRole::Unknown(_) => None,
        }
    }

    /// The speed a role is set to: its own setting; without one, the
    /// session's speed for `main` and `standard` for every other role.
    ///
    /// The utility roles do not inherit a fast session speed (decision
    /// 0027): summaries and titles gain little from speed, and an unset
    /// `fast` role falls back to the main model, so an inherited fast speed
    /// would double the price of every compaction.
    #[must_use]
    pub fn speed_for(&self, role: &ModelRole) -> Speed {
        if let Some(speed) = self
            .roles
            .get(role.as_str())
            .and_then(|settings| settings.speed.clone())
        {
            return speed;
        }
        if *role == ModelRole::Main {
            self.speed.clone()
        } else {
            Speed::Standard
        }
    }

    /// The reasoning level a role is set to: its own setting; without one,
    /// `model.effort` for `main` and nothing for every other role.
    #[must_use]
    pub fn effort_for(&self, role: &ModelRole) -> Option<&str> {
        let own = self
            .roles
            .get(role.as_str())
            .and_then(|settings| settings.effort.as_deref());
        match own {
            Some(effort) => Some(effort),
            None if *role == ModelRole::Main => self.effort.as_deref(),
            None => None,
        }
    }

    /// The provider a role's model is pinned to, when one is.
    #[must_use]
    pub fn provider_for(&self, role: &ModelRole) -> Option<&str> {
        self.roles
            .get(role.as_str())
            .and_then(|settings| settings.provider.as_deref())
    }
}

impl Settings {
    /// The retry settings for one provider: `[retry]`, with the two
    /// timeouts that provider's entry overrides.
    #[must_use]
    pub fn retry_for(&self, provider: &str) -> RetrySettings {
        let mut retry = self.retry.clone();
        if let Some(entry) = self.providers.get(provider) {
            if let Some(timeout) = entry.idle_timeout {
                retry.idle_timeout = timeout;
            }
            if let Some(timeout) = entry.first_event_timeout {
                retry.first_event_timeout = timeout;
            }
        }
        retry
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Setting, defaults};
    use super::*;

    fn settings(text: &str) -> Settings {
        let table: toml::Table = text.parse().unwrap();
        Settings::read(&toml::Value::Table(table)).unwrap()
    }

    #[test]
    fn the_defaults_are_what_the_docs_say() {
        let settings = Settings::default();
        assert_eq!(settings.config_version, 1);
        assert_eq!(settings.permissions.mode, Level::Supervised);
        assert_eq!(settings.sandbox.network, NetworkAccess::Off);
        assert!(settings.sandbox.enabled);
        assert_eq!(settings.model.speed, Speed::Standard);
        assert!(settings.model.speed_fallback);
        assert_eq!(settings.model.speed_wait, Duration::from_secs(20));
        assert_eq!(settings.model.speed_cooldown, Duration::from_secs(600));
        assert_eq!(settings.retry.attempts, 5);
        assert_eq!(settings.retry.backoff_base, Duration::from_secs(1));
        assert_eq!(settings.retry.backoff_factor, 2.0);
        assert_eq!(settings.retry.backoff_cap, Duration::from_secs(30));
        assert_eq!(settings.retry.max_wait, Duration::from_secs(120));
        assert_eq!(settings.retry.idle_timeout, Duration::from_secs(90));
        assert_eq!(settings.retry.first_event_timeout, Duration::from_secs(60));
        assert_eq!(settings.credentials.store, StoreChoice::Auto);
        assert!(settings.limits.turn.is_empty() && settings.limits.day.is_empty());
        assert!(settings.compat.instructions && settings.compat.mcp && settings.compat.skills);

        // Reading an empty file gives the same thing, and so does reading
        // the defaults written out.
        assert_eq!(self::settings(""), settings);
        assert_eq!(
            Settings::read(&toml::Value::Table(defaults())).unwrap(),
            settings
        );
    }

    #[test]
    fn utility_roles_run_at_standard_unless_set() {
        let fast_session = settings("[model]\nspeed = \"fast\"\neffort = \"high\"");
        assert_eq!(fast_session.model.speed_for(&ModelRole::Main), Speed::Fast);
        assert_eq!(
            fast_session.model.speed_for(&ModelRole::Fast),
            Speed::Standard,
            "a utility role does not inherit the session's speed"
        );
        assert_eq!(
            fast_session.model.speed_for(&ModelRole::Judge),
            Speed::Standard
        );
        assert_eq!(
            fast_session.model.effort_for(&ModelRole::Main),
            Some("high")
        );
        assert_eq!(fast_session.model.effort_for(&ModelRole::Fast), None);

        let per_role = settings(
            r#"
            [model]
            main = "big"
            fast = "small"
            speed = "fast"
            [model.roles.fast]
            speed = "fast"
            effort = "low"
            [model.roles.main]
            speed = "standard"
            effort = "max"
            provider = "elsewhere"
            [model.roles.judge]
            speed = "flex"
            "#,
        );
        assert_eq!(per_role.model.speed_for(&ModelRole::Fast), Speed::Fast);
        assert_eq!(
            per_role.model.speed_for(&ModelRole::Main),
            Speed::Standard,
            "a role's own speed wins over the session's"
        );
        assert_eq!(
            per_role.model.speed_for(&ModelRole::Judge),
            Speed::from_wire("flex")
        );
        assert_eq!(per_role.model.effort_for(&ModelRole::Main), Some("max"));
        assert_eq!(per_role.model.effort_for(&ModelRole::Fast), Some("low"));
        assert_eq!(per_role.model.model_for(&ModelRole::Main), Some("big"));
        assert_eq!(per_role.model.model_for(&ModelRole::Fast), Some("small"));
        assert_eq!(per_role.model.model_for(&ModelRole::Judge), None);
        assert_eq!(
            per_role.model.provider_for(&ModelRole::Main),
            Some("elsewhere")
        );
        assert_eq!(per_role.model.provider_for(&ModelRole::Fast), None);

        // A role this build has not heard of gets the safe answers.
        let other = ModelRole::from_wire("planner");
        assert_eq!(per_role.model.speed_for(&other), Speed::Standard);
        assert_eq!(per_role.model.model_for(&other), None);
    }

    #[test]
    fn a_provider_overrides_the_two_stream_timeouts() {
        let settings = settings(
            r#"
            [retry]
            attempts = 3
            idle_timeout = "2m"
            [providers.local]
            first_event_timeout = "5m"
            base_url = "http://localhost:11434"
            key_env = "LOCAL_KEY"
            headers = { X-Team = "a", Authorization = { env = "LOCAL_AUTH" } }
            [providers.off]
            enabled = false
            "#,
        );
        let local = settings.retry_for("local");
        assert_eq!(local.first_event_timeout, Duration::from_secs(300));
        assert_eq!(local.idle_timeout, Duration::from_secs(120));
        assert_eq!(local.attempts, 3);
        assert_eq!(settings.retry_for("anthropic"), settings.retry);
        assert_eq!(settings.providers["local"].headers.0.len(), 2);
        assert!(settings.providers["local"].enabled);
        assert!(!settings.providers["off"].enabled);
    }

    #[test]
    fn a_money_limit_is_whole_nano_dollars() {
        let settings = settings(
            "[limits.session]\ncost = 2.5\n[limits.day]\ncost = 10\ntokens = 2000000\ntime = \"8h\"",
        );
        assert_eq!(
            settings.limits.session.cost_usd_nanos(),
            Some(2_500_000_000)
        );
        assert_eq!(settings.limits.day.cost_usd_nanos(), Some(10_000_000_000));
        assert_eq!(settings.limits.day.tokens, Some(2_000_000));
        assert_eq!(settings.limits.day.time, Some(Duration::from_secs(28_800)));
        assert_eq!(settings.limits.turn.cost_usd_nanos(), None);
        let cents = LimitSet {
            cost: Some(0.01),
            ..LimitSet::default()
        };
        assert_eq!(cents.cost_usd_nanos(), Some(10_000_000));
        assert!(!cents.is_empty());
    }

    #[test]
    fn a_wrong_value_names_the_key_on_the_way() {
        let table: toml::Table = "[retry]\nattempts = \"five\"".parse().unwrap();
        let error = Settings::read(&toml::Value::Table(table)).unwrap_err();
        assert_eq!(
            error,
            "retry: attempts: expected a whole number that is not negative"
        );
    }
}
