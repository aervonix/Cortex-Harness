//! What the built-in tools share in reading their arguments and in ending a
//! call.

use cortex_base::{AbsPath, PathResolver};
use serde::de::DeserializeOwned;

use crate::blocking::BlockingError;
use crate::contract::{Json, ToolSpec};
use crate::result::{ToolError, ToolResult};
use crate::schema;

/// Checks arguments against a tool's schema, then reads them into the
/// tool's own type. Either failure is an error for the model that names the
/// tool and the argument.
pub(crate) fn parse<T: DeserializeOwned>(spec: &ToolSpec, args: &Json) -> Result<T, ToolError> {
    schema::validate(&spec.parameters, args).map_err(|mismatch| {
        ToolError::invalid(format!(
            "The arguments for `{}` are not valid:\n{mismatch}",
            spec.name
        ))
    })?;
    // The schema and the type are written to agree, and the contract tests
    // call every tool with every argument it has. This is for what a schema
    // cannot say.
    serde_json::from_value(args.clone()).map_err(|error| {
        ToolError::invalid(format!(
            "The arguments for `{}` are not valid: {error}",
            spec.name
        ))
    })
}

/// The result of a call whose blocking work did not return one.
pub(crate) fn unfinished(error: BlockingError, tool: &str) -> ToolResult {
    match error {
        BlockingError::Cancelled => {
            ToolResult::interrupted(format!("`{tool}` was interrupted before it finished."))
        }
        BlockingError::Panicked(message) => ToolError::Internal(message).into_result(),
    }
}

/// A real location as it is shown to the model: relative to the workspace
/// when inside it, absolute otherwise.
pub(crate) fn shown(paths: &PathResolver, path: &AbsPath) -> String {
    match path.strip_prefix(paths.root()) {
        Some(relative) => relative.to_string(),
        None => path.to_string(),
    }
}
