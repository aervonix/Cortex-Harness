//! What the protocol says about a model: the catalog entry a front end needs
//! for its pickers, its status line and its cost figures
//! (`docs/architecture/04-providers.md`, "Model catalog").

use serde::{Deserialize, Serialize};

use crate::ids::{ModelId, ProviderId};
use crate::open::open_enum;
use crate::speed::{Speed, SpeedEffect, SpeedUnavailable};

open_enum! {
    /// The job a model is doing. Configuration names one model per role
    /// (`docs/architecture/14-adaptation.md`, "Model roles and routing").
    pub enum ModelRole {
        /// The agent's own turns.
        Main => "main",
        /// Utility calls: compaction summaries, titles, output summaries.
        Fast => "fast",
        /// Model-graded comparisons in the run engine.
        Judge => "judge",
    }
}

open_enum! {
    /// A kind of input or output a model handles.
    pub enum Modality {
        /// Text.
        Text => "text",
        /// Images.
        Image => "image",
    }
}

open_enum! {
    /// How a model's reasoning depth is controlled.
    pub enum ReasoningControlKind {
        /// The model has no reasoning control.
        None => "none",
        /// An ordered set of named effort levels.
        Levels => "levels",
        /// A token budget within a range.
        Budget => "budget",
    }
}

/// A model's reasoning control. Depth is a per-model set, never a number on a
/// shared scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReasoningControl {
    /// Which kind of control this is.
    pub kind: ReasoningControlKind,
    /// For `levels`: the model's own level names, from least to most.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<String>,
    /// For `levels`: the level used when none is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// For `budget`: the smallest budget, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_tokens: Option<u64>,
    /// For `budget`: the largest budget, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// The price of writing to the prompt cache, for one lifetime class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CacheWritePrice {
    /// The provider's name for the lifetime class, for example `5m` or `1h`.
    pub class: String,
    /// Nano-dollars per million tokens.
    pub price: u64,
}

/// One rate card: what a million tokens of each class cost. Every price is a
/// whole number of nano-dollars (1e-9 US dollars) per million tokens, so
/// `4_000_000_000` is $4.00 per million.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Prices {
    /// Input tokens that were not read from or written to the cache.
    pub input: u64,
    /// Output tokens, reasoning included.
    pub output: u64,
    /// Input tokens read from the cache. Absent when the model has no cache price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    /// Input tokens written to the cache, per lifetime class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_write: Vec<CacheWritePrice>,
    /// Rate cards that replace this one for larger requests, by ascending
    /// threshold. The last tier whose threshold the request passes applies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<PriceTier>,
}

/// The prices of a request whose input is larger than a threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PriceTier {
    /// The tier applies when the request's input tokens exceed this.
    pub above_input_tokens: u64,
    /// Input tokens, nano-dollars per million.
    pub input: u64,
    /// Output tokens, nano-dollars per million.
    pub output: u64,
    /// Cache reads, nano-dollars per million.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    /// Cache writes per lifetime class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_write: Vec<CacheWritePrice>,
}

/// One speed tier a model offers besides `standard`, with everything that
/// makes it different from another provider's tier of the same name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SpeedOffer {
    /// The tier's name: what a setting names and what a usage record carries.
    pub speed: Speed,
    /// What the provider calls it, for display: "Fast mode", "Flex processing".
    pub label: String,
    /// What the tier buys.
    pub effect: SpeedEffect,
    /// The tier's own full rate card. Absent when its price is not known, in
    /// which case its usage shows tokens only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prices: Option<Prices>,
    /// What the provider does when it cannot serve the tier.
    pub when_unavailable: SpeedUnavailable,
    /// Whether the tier has a rate limit of its own, apart from the model's
    /// standard limit.
    #[serde(default)]
    pub separate_limit: bool,
    /// Whether a prompt-cache prefix written at `standard` can be read at
    /// this tier and the other way round. `false` means a change of speed is
    /// a cache miss. Absent when the provider does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shares_cache: Option<bool>,
    /// Whether the provider marks the tier as a preview that may change or
    /// need access to be granted.
    #[serde(default)]
    pub preview: bool,
}

/// Capability flags of a model. A flag that is absent is `false`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelCapabilities {
    /// The provider checks tool arguments against the schema.
    #[serde(default)]
    pub strict_tools: bool,
    /// System messages may appear in the middle of a conversation.
    #[serde(default)]
    pub mid_conversation_system: bool,
    /// The provider can hold the model to an output schema.
    #[serde(default)]
    pub structured_output: bool,
    /// The smallest prefix the provider will cache, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cacheable_tokens: Option<u64>,
}

/// A model, as the catalog describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelInfo {
    /// The model's id at its provider.
    pub id: ModelId,
    /// The provider that serves it.
    pub provider: ProviderId,
    /// The name to show.
    pub display_name: String,
    /// The wire dialect used to reach it, for example `anthropic-messages`.
    pub protocol: String,
    /// The context window, in tokens.
    pub context_window: u64,
    /// The most output tokens one response may have.
    pub max_output: u64,
    /// The rate card at `standard` speed. Absent when no price is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prices: Option<Prices>,
    /// How reasoning depth is controlled.
    pub reasoning: ReasoningControl,
    /// The models whose opaque reasoning this model can read back. Its own id
    /// is not listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads_reasoning_from: Vec<ModelId>,
    /// What it accepts as input.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<Modality>,
    /// Capability flags.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// The speed tiers it offers besides `standard`. Empty for most models.
    /// A front end shows a speed control only when this is not empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speeds: Vec<SpeedOffer>,
}

impl ModelInfo {
    /// The offer for a speed, if the model has one. `standard` has no offer:
    /// its prices are the model's own.
    #[must_use]
    pub fn speed_offer(&self, speed: &Speed) -> Option<&SpeedOffer> {
        self.speeds.iter().find(|offer| offer.speed == *speed)
    }

    /// Whether a request to this model can ask for the speed.
    #[must_use]
    pub fn offers(&self, speed: &Speed) -> bool {
        speed.is_standard() || self.speed_offer(speed).is_some()
    }
}
