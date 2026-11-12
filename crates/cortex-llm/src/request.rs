//! One request to a model, in neutral terms
//! (`docs/architecture/04-providers.md`).
//!
//! The agent loop fills in a [`ModelRequest`]. A
//! [`Protocol`](crate::Protocol) turns it into the HTTP request its dialect
//! wants. Nothing here names a provider's field.

use std::sync::Arc;

use cortex_protocol::model::ModelRole;
use cortex_protocol::{Message, Speed};
use serde_json::Value;

use crate::catalog::{ModelEntry, Variants};

/// One call to a model: who is asked, what the conversation is, and how the
/// call is to be made.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    /// The model that is asked, as the catalog describes it.
    pub model: Arc<ModelEntry>,
    /// The job the model is doing in this request. It is recorded with the
    /// usage, and decides nothing here.
    pub role: ModelRole,
    /// The system prompt. Frozen for the context epoch.
    pub system: String,
    /// The conversation, oldest first. Arranging it as a dialect requires
    /// (where tool results go, which messages merge) is the protocol's job.
    pub messages: Vec<Message>,
    /// The tools the model may call, in the order they are sent. The caller
    /// sorts them once per context epoch and never changes the order within
    /// it, because the order is part of the cached prefix.
    pub tools: Vec<ToolDefinition>,
    /// The most output tokens the response may have. `None` leaves it to
    /// the model's own maximum.
    pub max_output: Option<u64>,
    /// How much the model should reason, and what of it to show.
    pub reasoning: Reasoning,
    /// The speed tier the request asks for. The response says which one
    /// served it, and that is what the usage records
    /// (`docs/decisions/0027-speed-tiers.md`).
    pub speed: Speed,
    /// Where the cacheable parts of the request end.
    pub cache: CacheHints,
    /// The per-model variants in effect: how tools are declared and which
    /// edit format and prompt style the conversation was written for.
    pub variants: Variants,
    /// A JSON Schema the output must match, where a fixed shape is needed
    /// (a compaction summary, a title) and the model can be held to one.
    pub output_schema: Option<Value>,
}

impl ModelRequest {
    /// A request to a model with nothing in it yet: no system prompt, no
    /// messages, no tools, the model's default reasoning, standard speed,
    /// and the variants the catalog names for the model.
    #[must_use]
    pub fn new(model: Arc<ModelEntry>, role: ModelRole) -> Self {
        let variants = model.variants.clone();
        Self {
            model,
            role,
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output: None,
            reasoning: Reasoning::default(),
            speed: Speed::Standard,
            cache: CacheHints::default(),
            variants,
            output_schema: None,
        }
    }
}

/// A tool the model may call: a name, a description and the JSON Schema of
/// its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    /// The name. It passes [`is_valid_tool_name`].
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// The JSON Schema of the arguments.
    pub schema: Value,
}

/// Whether a tool name is one every provider accepts: 1 to 64 characters
/// from `a-z`, `A-Z`, `0-9`, `_` and `-`. It is the strictest of the
/// providers' rules, so one name works everywhere.
#[must_use]
pub fn is_valid_tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// How much the model should reason, and what of it to show. Depth and
/// visibility are separate settings because providers keep them separate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reasoning {
    /// The depth, as one of the model's own level names, for a model whose
    /// control is a set of levels. `None` leaves the model's default. The
    /// caller has already mapped a portable alias onto the model's set.
    pub level: Option<String>,
    /// The depth as a token budget, for a model whose control is a budget.
    pub budget_tokens: Option<u64>,
    /// What of the reasoning the response should show.
    pub visibility: ReasoningVisibility,
}

/// What of the model's reasoning a response shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ReasoningVisibility {
    /// A summary, or the visible text, where the provider can give one.
    #[default]
    Summary,
    /// Nothing. The opaque payload still comes back and is still replayed.
    None,
}

/// Where the cacheable parts of a request end. Caching is a property of the
/// request's shape; these hints say where the stable parts stop, and each
/// protocol turns them into its provider's feature, or into nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheHints {
    /// The tools and the system prompt are a prefix that stays the same for
    /// the context epoch: mark its end.
    pub stable_prefix: bool,
    /// The messages are history that later requests will repeat: mark the
    /// end of them.
    pub history: bool,
    /// The lifetime class to ask for, by the provider's name for it (`1h`).
    /// `None` is the provider's default.
    pub lifetime: Option<String>,
    /// A key that stays the same for a session, for providers that route
    /// requests with the same key to the same cache.
    pub key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_follow_the_strictest_provider_rule() {
        for name in ["read", "tool_call", "a", "Read-File_2", &"x".repeat(64)] {
            assert!(is_valid_tool_name(name), "{name:?}");
        }
        for name in ["", "has space", "dot.name", "ünï", "slash/name", &"x".repeat(65)] {
            assert!(!is_valid_tool_name(name), "{name:?}");
        }
    }
}
