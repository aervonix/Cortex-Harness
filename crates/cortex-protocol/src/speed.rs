//! Speed tiers: a provider running the same model faster, or slower and
//! cheaper, at a different price (`docs/decisions/0027-speed-tiers.md`).
//!
//! A speed is a named tier that a model offers. `standard` is what every
//! model has. `fast` is the well-known name for a tier that produces output
//! faster at a higher price. Any other tier keeps the provider's own name
//! (`flex`, `priority`, `ultrafast`), because providers' tiers differ in what
//! they buy, what they cost and how they fail, and one invented scale would
//! hide that. What a tier is on a given model is described by its
//! [`SpeedOffer`](crate::model::SpeedOffer), never implied by its name.

use serde::{Deserialize, Serialize};

use crate::ids::{ModelId, ProviderId, Timestamp};
use crate::model::ModelRole;
use crate::open::open_enum;

open_enum! {
    /// A speed tier, by name. Which ones a model offers is in its
    /// [`ModelInfo`](crate::model::ModelInfo).
    pub enum Speed, other = Other("A tier with a provider's own name, such as `flex` or `priority`. What it is comes from the model's offer for it, never from the name.") {
        /// The provider's normal processing, at the model's normal prices.
        /// Every model has it, and it is what a request runs at when nothing
        /// else is asked for.
        Standard => "standard",
        /// Faster output at a higher price, where the model offers it.
        Fast => "fast",
    }
}

#[expect(
    clippy::derivable_impls,
    reason = "the enum is defined by a macro, which has no way to mark a default variant"
)]
impl Default for Speed {
    fn default() -> Self {
        Self::Standard
    }
}

impl Speed {
    /// Whether this is the normal tier. Used to leave `"speed": "standard"`
    /// out of places where it is the default.
    #[must_use]
    pub fn is_standard(&self) -> bool {
        *self == Self::Standard
    }
}

open_enum! {
    /// What a speed tier buys, compared with `standard`.
    pub enum SpeedEffect {
        /// The model produces output tokens faster.
        FasterOutput => "faster_output",
        /// The request is served ahead of standard traffic: lower and
        /// steadier latency under load, the same generation speed.
        Priority => "priority",
        /// The request is cheaper and may be slow, queued or turned away
        /// when the provider is busy.
        Deferred => "deferred",
    }
}

open_enum! {
    /// What the provider does when it cannot serve a request at the tier
    /// that was asked for.
    pub enum SpeedUnavailable {
        /// It fails the request with a rate-limit or overload error. The
        /// harness decides whether to wait or to fall back.
        Error => "error",
        /// It serves the request at `standard`, says so in the response, and
        /// bills it at the standard price.
        Downgrade => "downgrade",
    }
}

open_enum! {
    /// Who moved a request to another speed.
    pub enum SpeedFallbackBy {
        /// The harness sent the request at another speed after the provider
        /// refused the one that was asked for.
        Harness => "harness",
        /// The provider served the request at another speed and reported it.
        Provider => "provider",
    }
}

open_enum! {
    /// Why a request did not run at the speed that was asked for.
    pub enum SpeedFallbackReason {
        /// The tier's own rate limit was reached.
        RateLimited => "rate_limited",
        /// The provider had no capacity for the tier.
        Overloaded => "overloaded",
        /// The tier is in a cooldown after an earlier refusal.
        Cooldown => "cooldown",
        /// The model in use does not offer the tier.
        NotOffered => "not_offered",
        /// The provider reported another speed and gave no reason.
        Unspecified => "unspecified",
    }
}

open_enum! {
    /// Whether a cooldown began or ended.
    pub enum CooldownState {
        /// Requests stop asking for the tier until the cooldown ends.
        Started => "started",
        /// Requests ask for the tier again.
        Ended => "ended",
    }
}

/// Notice: a request ran, or will run, at a different speed from the one the
/// session asked for. The usage record of the request says the same thing in
/// numbers: it is priced at the speed it ran at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SpeedFallback {
    /// The notice in words, so that a client that does not know this kind of
    /// notice can still show it.
    pub message: String,
    /// The role whose request it was.
    pub role: ModelRole,
    /// The provider.
    pub provider: ProviderId,
    /// The model.
    pub model: ModelId,
    /// The speed the session asked for.
    pub requested: Speed,
    /// The speed the request ran at, or will run at.
    pub served: Speed,
    /// Who moved it.
    pub by: SpeedFallbackBy,
    /// Why.
    pub reason: SpeedFallbackReason,
    /// When the harness will ask for the requested speed again, if it has
    /// put the tier in a cooldown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<Timestamp>,
}

/// Notice: a speed tier's cooldown began or ended. During a cooldown the
/// harness sends requests at `standard` and does not flip back and forth,
/// because each change of speed can cost a prompt-cache rewrite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SpeedCooldown {
    /// The notice in words.
    pub message: String,
    /// The provider.
    pub provider: ProviderId,
    /// The model.
    pub model: ModelId,
    /// The tier that is cooling down.
    pub speed: Speed,
    /// Whether the cooldown began or ended.
    pub state: CooldownState,
    /// When it ends, for one that began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Timestamp>,
}
