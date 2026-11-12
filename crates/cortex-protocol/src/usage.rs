//! Usage and cost: one normalised record per request, the live figures of a
//! session, and the report behind `/usage`
//! (`docs/architecture/04-providers.md`, "Usage and cost", and
//! `docs/architecture/18-usage.md`).

use serde::{Deserialize, Serialize};

use crate::ids::{ModelId, ProjectKey, ProviderId, RawJson, Timestamp};
use crate::model::ModelRole;
use crate::open::open_enum;
use crate::speed::Speed;

/// Input tokens written to the cache, for one lifetime class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CacheWriteTokens {
    /// The provider's name for the lifetime class, for example `5m` or `1h`.
    pub class: String,
    /// The tokens written in that class.
    pub tokens: u64,
}

/// Token counts by class. The classes do not overlap: the tokens sent are
/// `input + cache_read + cache_write`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TokenUsage {
    /// Input tokens billed at the normal rate: not read from the cache and
    /// not written to it.
    #[serde(default)]
    pub input: u64,
    /// Input tokens read from the cache.
    #[serde(default)]
    pub cache_read: u64,
    /// Input tokens written to the cache, all lifetime classes together.
    #[serde(default)]
    pub cache_write: u64,
    /// `cache_write` split by lifetime class, where the provider has classes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_write_classes: Vec<CacheWriteTokens>,
    /// Output tokens, reasoning included.
    #[serde(default)]
    pub output: u64,
    /// The part of `output` that was reasoning, when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
}

open_enum! {
    /// What kind of number a cost is.
    pub enum CostBasis {
        /// Billed per token on an API key: tokens multiplied by the
        /// provider's published price for the speed the request ran at. Our
        /// calculation, not the provider's invoice.
        Metered => "metered",
        /// What the usage would have cost at API prices. It was on a
        /// subscription, or its billing is unknown, so nobody was billed
        /// this way.
        Estimate => "estimate",
        /// A cost that the provider, or another tool's own records, stated.
        Recorded => "recorded",
    }
}

/// An amount of money with its basis. Amounts with different bases are never
/// added together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Cost {
    /// US dollars, as a whole number of nano-dollars (1e-9). Whole numbers
    /// make sums exact: `184_200_000` is $0.1842.
    pub usd_nanos: u64,
    /// What kind of number it is.
    pub basis: CostBasis,
}

open_enum! {
    /// How the usage is paid for.
    pub enum Billing {
        /// An API key billed per token.
        Metered => "metered",
        /// A subscription.
        Subscription => "subscription",
        /// Not known.
        Unspecified => "unknown",
    }
}

/// The usage of one request to a model: what was used, at which speed, and
/// what it cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RequestUsage {
    /// The provider.
    pub provider: ProviderId,
    /// The model.
    pub model: ModelId,
    /// The role the request was made in.
    pub role: ModelRole,
    /// The speed the request **ran at**: the provider's own report where it
    /// gives one, otherwise the speed that was sent. The cost is priced at
    /// this speed's rate.
    #[serde(default, skip_serializing_if = "Speed::is_standard")]
    pub speed: Speed,
    /// The speed that was asked for, when the request ran at another one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_requested: Option<Speed>,
    /// Tokens by class.
    pub tokens: TokenUsage,
    /// The cost, fixed when the request was recorded. Absent when the model
    /// has no known price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    /// The version of the catalog whose prices were used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_version: Option<String>,
    /// How the usage is paid for.
    pub billing: Billing,
    /// Milliseconds from sending the request to the first token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<u64>,
    /// Milliseconds the request took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The provider's own usage object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawJson>,
}

/// Usage added up over some requests: one row of a breakdown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageTotals {
    /// Tokens by class.
    #[serde(default)]
    pub tokens: TokenUsage,
    /// How many requests.
    #[serde(default)]
    pub requests: u64,
    /// The cost, one amount per basis. Empty when no price is known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub costs: Vec<Cost>,
}

/// A session's usage for one provider, model, role and speed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelUsage {
    /// The provider.
    pub provider: ProviderId,
    /// The model.
    pub model: ModelId,
    /// The role.
    pub role: ModelRole,
    /// The speed the requests ran at.
    #[serde(default, skip_serializing_if = "Speed::is_standard")]
    pub speed: Speed,
    /// What was used.
    pub totals: UsageTotals,
}

open_enum! {
    /// What a limit counts.
    pub enum LimitMeasure {
        /// Requests.
        Requests => "requests",
        /// Tokens, input and output together.
        Tokens => "tokens",
        /// Input tokens.
        InputTokens => "input_tokens",
        /// Output tokens.
        OutputTokens => "output_tokens",
    }
}

/// One limit a provider reported: how much of it is left and when it resets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LimitWindow {
    /// What the limit counts.
    pub measure: LimitMeasure,
    /// The speed tier the limit belongs to, when the provider keeps a
    /// separate limit for that tier. Absent for the model's ordinary limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
    /// The limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// What is left of it. Absent when not known, which includes "the reset
    /// time has passed and no request has been made since".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    /// When it resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

open_enum! {
    /// Where a limit figure came from.
    pub enum LimitSource {
        /// Rate-limit headers on a response the harness received anyway.
        Headers => "headers",
        /// The provider's documented usage endpoint, asked because the user
        /// turned the refresh on for that provider.
        Refresh => "refresh",
    }
}

/// The limits one provider last reported. A provider that reports none has
/// no entry, and a front end shows "not available".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProviderLimits {
    /// The provider.
    pub provider: ProviderId,
    /// The limits.
    pub windows: Vec<LimitWindow>,
    /// When the figures were seen.
    pub seen_at: Timestamp,
    /// Where they came from.
    pub source: LimitSource,
}

/// The live usage of a session: what `usage/get` returns and `usage.updated`
/// carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionUsage {
    /// Everything the session has used, out-of-band requests included
    /// (compaction, titles).
    #[serde(default)]
    pub totals: UsageTotals,
    /// The same, per provider, model, role and speed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_model: Vec<ModelUsage>,
    /// The most recent request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request: Option<RequestUsage>,
    /// Cache reads as a share of all input tokens sent, from 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_rate: Option<f64>,
    /// Output tokens per second of the most recent request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<f64>,
    /// The tokens in the context of the most recent request to the main model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// The latest limits each provider reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<ProviderLimits>,
}

open_enum! {
    /// The period a usage report covers.
    pub enum UsageRange {
        /// The session named in the request.
        Session => "session",
        /// The local calendar day.
        Today => "today",
        /// The calendar week, or the last 7 days when configured as rolling.
        Week => "week",
        /// The calendar month, or the last 30 days when configured as rolling.
        Month => "month",
        /// Everything on record.
        All => "all",
    }
}

open_enum! {
    /// A way to group the rows of a usage report.
    pub enum UsageGroup {
        /// By provider.
        Provider => "provider",
        /// By model.
        Model => "model",
        /// By project.
        Project => "project",
        /// By the tool that recorded the usage.
        Source => "source",
        /// By the speed the requests ran at.
        Speed => "speed",
    }
}

/// One row of a usage report. The fields that are set are the ones the
/// report is grouped by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageRow {
    /// The provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
    /// The model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelId>,
    /// The project's key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectKey>,
    /// The project's name or shortened path, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_label: Option<String>,
    /// The tool that recorded the usage: `cortex-harness`, or another tool's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The speed the requests ran at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
    /// What was used.
    pub totals: UsageTotals,
}

/// Usage and cost over a range: the result of `usage/report`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageReport {
    /// The range covered.
    pub range: UsageRange,
    /// The start of the range. Absent for all time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Timestamp>,
    /// The end of the range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Timestamp>,
    /// How the rows are grouped, outermost first.
    pub group_by: Vec<UsageGroup>,
    /// The rows.
    pub rows: Vec<UsageRow>,
    /// All rows together.
    pub totals: UsageTotals,
    /// The latest limits each provider reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<ProviderLimits>,
    /// The sources included: `cortex-harness`, and other tools the user turned on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Whether usage history is being recorded. When statistics are off,
    /// only the session range has figures.
    pub recording: bool,
}
