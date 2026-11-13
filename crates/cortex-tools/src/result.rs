//! What a tool call ends with: a [`ToolResult`], or before it has run, a
//! [`ToolError`].
//!
//! A failure is a result, not an unwinding error
//! (`docs/architecture/02-runtime.md`, "Failures are data"). It is written
//! for the model: what was wrong, and what to try.

use cortex_base::{Classified, ErrorKind, PathError};
use cortex_protocol::item::{ArtifactRef, ToolOutcome};
use cortex_protocol::message::Part;

use crate::contract::Json;

/// The result of one tool call.
///
/// It holds what the model is shown (text, and images where a tool returns
/// them), an optional structured payload for front ends, how the call ended,
/// and references to the artifacts that hold what was cut.
///
/// The content of a result is fixed once the output policy has passed it:
/// [`execute`](crate::execute) returns results that are
/// [bounded](Self::is_bounded), and nothing shortens a result afterwards
/// (`docs/architecture/05-tools.md`, "One output policy").
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    outcome: ToolOutcome,
    content: Vec<Part>,
    payload: Option<Json>,
    artifacts: Vec<ArtifactRef>,
    bounded: bool,
}

impl ToolResult {
    fn new(outcome: ToolOutcome, text: String) -> Self {
        Self {
            outcome,
            content: vec![Part::text(text)],
            payload: None,
            artifacts: Vec::new(),
            bounded: false,
        }
    }

    /// A call that succeeded, with the text the model is shown.
    #[must_use]
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(ToolOutcome::Success, text.into())
    }

    /// A call that failed. The text tells the model what was wrong and what
    /// to try.
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(ToolOutcome::Error, text.into())
    }

    /// A call that was cancelled, with whatever it has to show.
    #[must_use]
    pub fn interrupted(text: impl Into<String>) -> Self {
        Self::new(ToolOutcome::Interrupted, text.into())
    }

    /// A call that was not allowed to run. The permission layer makes
    /// these; a tool never does.
    #[must_use]
    pub fn denied(text: impl Into<String>) -> Self {
        Self::new(ToolOutcome::Denied, text.into())
    }

    /// Adds a structured payload for front ends: match counts, a diff, an
    /// exit status. Its shape belongs to the tool.
    #[must_use]
    pub fn with_payload(mut self, payload: Json) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Adds a part after the ones already there: an image, or more text.
    #[must_use]
    pub fn with_part(mut self, part: Part) -> Self {
        self.content.push(part);
        self.bounded = false;
        self
    }

    /// Adds a reference to an artifact the result's content mentions.
    #[must_use]
    pub fn with_artifact(mut self, artifact: ArtifactRef) -> Self {
        self.artifacts.push(artifact);
        self
    }

    /// How the call ended.
    #[must_use]
    pub fn outcome(&self) -> &ToolOutcome {
        &self.outcome
    }

    /// Whether the call did not succeed: it failed, was denied, or was
    /// interrupted. This is the error flag the model's protocol carries.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.outcome != ToolOutcome::Success
    }

    /// What the model is shown.
    #[must_use]
    pub fn content(&self) -> &[Part] {
        &self.content
    }

    /// The text of the content: its text parts, one after another, with a
    /// line break between two.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = String::new();
        for part in &self.content {
            if let Part::Text(part) = part {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&part.text);
            }
        }
        text
    }

    /// The structured payload for front ends.
    #[must_use]
    pub fn payload(&self) -> Option<&Json> {
        self.payload.as_ref()
    }

    /// The artifacts that hold what the content left out.
    #[must_use]
    pub fn artifacts(&self) -> &[ArtifactRef] {
        &self.artifacts
    }

    /// Whether the output policy has passed this result. One that it has
    /// not must not enter the context.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        self.bounded
    }

    /// The result's pieces, for the code that records it: the outcome, the
    /// content, the payload and the artifacts.
    #[must_use]
    pub fn into_parts(self) -> (ToolOutcome, Vec<Part>, Option<Json>, Vec<ArtifactRef>) {
        (self.outcome, self.content, self.payload, self.artifacts)
    }

    /// Replaces the content with what the output policy made of it, and
    /// marks the result as passed.
    pub(crate) fn bounded_with(
        mut self,
        content: Vec<Part>,
        artifact: Option<ArtifactRef>,
    ) -> Self {
        self.content = content;
        self.artifacts.extend(artifact);
        self.bounded = true;
        self
    }
}

/// Why a call could not be planned, or could not be run as planned.
///
/// `Display` is the text the model gets. Each message says what to do next.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The arguments do not fit the tool. The message names the argument
    /// and what it should have been.
    #[error("{0}")]
    InvalidArguments(String),
    /// A path the call names was refused, with the resolver's reason.
    #[error("{0}")]
    Path(#[from] PathError),
    /// There is no tool by that name.
    #[error("{0}")]
    UnknownTool(String),
    /// A bug in the harness: a plan run by the wrong tool, a task that
    /// panicked.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ToolError {
    /// An [`InvalidArguments`](Self::InvalidArguments) error.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArguments(message.into())
    }

    /// The error as the result of the call that caused it.
    #[must_use]
    pub fn into_result(self) -> ToolResult {
        ToolResult::error(self.to_string())
    }
}

impl Classified for ToolError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidArguments(_) | Self::Path(_) | Self::UnknownTool(_) => ErrorKind::Tool,
            Self::Internal(_) => ErrorKind::Internal,
        }
    }

    fn code(&self) -> &str {
        match self {
            Self::InvalidArguments(_) => "invalid_arguments",
            Self::Path(_) => "path_refused",
            Self::UnknownTool(_) => "unknown_tool",
            Self::Internal(_) => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_says_how_the_call_ended() {
        let ok = ToolResult::success("done");
        assert_eq!(ok.outcome(), &ToolOutcome::Success);
        assert!(!ok.is_error());
        assert_eq!(ok.text(), "done");
        assert!(!ok.is_bounded(), "the output policy has not seen it");

        assert!(ToolResult::error("no").is_error());
        assert_eq!(
            ToolResult::interrupted("stopped").outcome(),
            &ToolOutcome::Interrupted
        );
        assert_eq!(ToolResult::denied("no").outcome(), &ToolOutcome::Denied);
    }

    #[test]
    fn a_result_carries_a_payload_parts_and_artifacts() {
        let result = ToolResult::success("first")
            .with_payload(serde_json::json!({"matches": 3}))
            .with_part(Part::text("second"));
        assert_eq!(result.text(), "first\nsecond");
        assert_eq!(result.payload(), Some(&serde_json::json!({"matches": 3})));
        let (outcome, content, payload, artifacts) = result.into_parts();
        assert_eq!(outcome, ToolOutcome::Success);
        assert_eq!(content.len(), 2);
        assert!(payload.is_some());
        assert!(artifacts.is_empty());
    }

    #[test]
    fn an_error_becomes_a_result_the_model_can_read() {
        let error = ToolError::invalid("`offset` must be 1 or more, and is 0.");
        assert_eq!(error.kind(), ErrorKind::Tool);
        assert_eq!(error.code(), "invalid_arguments");
        let result = error.into_result();
        assert!(result.is_error());
        assert_eq!(result.text(), "`offset` must be 1 or more, and is 0.");

        let path = ToolError::from(PathError::Empty);
        assert_eq!(path.code(), "path_refused");
        assert_eq!(path.to_string(), "the path is empty");

        let internal = ToolError::Internal("oops".to_owned());
        assert_eq!(internal.kind(), ErrorKind::Internal);
        assert_eq!(internal.to_error_data().message, "internal error: oops");
    }
}
