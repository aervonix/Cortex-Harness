//! The neutral conversation model: a message is a role and an ordered list
//! of parts (`docs/architecture/04-providers.md`, "The conversation model").
//!
//! Every part of an assistant message can carry `raw`: the provider's own
//! JSON for that block, exactly as it arrived. Replay to the same protocol
//! sends the raw block back unchanged, which is what keeps signatures and
//! encrypted reasoning valid. Anything else gets the neutral fields.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ArtifactId, CallId, ModelId, ProviderId, RawJson};
use crate::open::{open_enum, open_union};

open_enum! {
    /// Who a message is from.
    pub enum Role {
        /// The user, or the harness speaking for the user.
        User => "user",
        /// The model.
        Assistant => "assistant",
        /// The results of the tool calls in the assistant message before it,
        /// in call order.
        Tool => "tool",
    }
}

/// A piece of text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextPart {
    /// The text.
    pub text: String,
    /// The provider's own block, on assistant messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawJson>,
}

/// An image. Input only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ImagePart {
    /// The media type, for example `image/png`.
    pub media_type: String,
    /// The image bytes in base64. Exactly one of `data` and `artifact` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The stored artifact that holds the image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactId>,
}

/// A request by the model to run a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolCallPart {
    /// The provider's id for the call. Its result names it.
    pub id: CallId,
    /// The tool's name.
    pub name: String,
    /// The arguments, as the JSON value the model wrote. When the model's
    /// text was not valid JSON this is that text as a JSON string, and the
    /// call's result is an error that says so.
    pub arguments: Value,
    /// The provider's own block. A protocol with a second id for the call
    /// keeps it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawJson>,
}

/// The result of one tool call, as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolResultPart {
    /// The call this answers.
    pub call_id: CallId,
    /// What the model is shown: text and images.
    pub content: Vec<Part>,
    /// Whether the call failed. A failure is a result, written so the model
    /// can correct itself.
    #[serde(default)]
    pub is_error: bool,
}

/// Reasoning by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReasoningPart {
    /// The visible reasoning or its summary. Often empty.
    #[serde(default)]
    pub text: String,
    /// The provider's own block, which holds the opaque payload (a
    /// signature, encrypted content). It is replayed unchanged to a model
    /// that can read it and dropped for any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawJson>,
}

/// Something the provider produced that the neutral model does not name:
/// server-side tool use and its results, compaction blocks, fallback markers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProviderBlockPart {
    /// The provider's name for the block type, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_type: Option<String>,
    /// The provider's own block.
    pub raw: RawJson,
}

open_union! {
    /// One part of a message.
    pub enum Part [kind] {
        /// Text.
        Text(TextPart) => "text",
        /// An image.
        Image(ImagePart) => "image",
        /// A tool call by the model.
        ToolCall(ToolCallPart) => "tool_call",
        /// The result of a tool call.
        ToolResult(ToolResultPart) => "tool_result",
        /// Reasoning by the model.
        Reasoning(ReasoningPart) => "reasoning",
        /// A provider block with no neutral form.
        ProviderBlock(ProviderBlockPart) => "provider_block",
    }
}

impl Part {
    /// A text part with no raw block.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextPart {
            text: text.into(),
            raw: None,
        })
    }
}

/// What produced an assistant message. Raw blocks are valid only for replay
/// to the same protocol, and to a model that can read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Origin {
    /// The wire dialect, for example `anthropic-messages`.
    pub protocol: String,
    /// The provider.
    pub provider: ProviderId,
    /// The model.
    pub model: ModelId,
    /// The account or organisation the credential belonged to, where the
    /// provider ties opaque reasoning to it. Never the credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_scope: Option<String>,
}

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Message {
    /// Who it is from.
    pub role: Role,
    /// Its content, in order.
    pub parts: Vec<Part>,
    /// For an assistant message: what produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}
