//! The tool contract and the built-in tools
//! (`docs/architecture/05-tools.md`).
//!
//! | Module | What it gives |
//! |---|---|
//! | [`contract`] | [`Tool`], [`ToolSpec`], [`Plan`], [`Effect`] and [`Concurrency`]: what a tool is, and what a call declares before it runs. |
//! | [`result`] | [`ToolResult`] and [`ToolError`]. |
//! | [`context`] | [`PlanCx`] and [`RunCx`], and the seams a session fills: [`ReadGate`], [`Outliner`], [`ImageSupport`]. |
//! | [`output`] | The output policy: [`OutputLimits`], structural compression, and the notice and artifact for what was cut. |
//! | [`store`] | [`ContentStore`], the trait artifacts and file versions go through, and [`MemoryStore`]. |
//! | [`files`] | [`FileSnapshot`] and [`FileTable`], the session's table of seen files. |
//! | [`schema`] | Validation of arguments against a tool's JSON Schema. |
//! | [`blocking`] | [`run_blocking`]: blocking work on the blocking pool, with a cancellation token. |
//! | [`read`], [`list`], [`search`] | The tools [`ReadTool`], [`ListTool`] and [`SearchTool`]. |
//! | [`process`] | The process runner: one command from its start to the end of its process tree, with a bounded buffer for what it writes, an artifact for the rest, and staged stopping. |
//! | [`shell`] | [`ShellTool`], and what it needs: which [`Shell`] there is and how each kind is spoken to, the environment a command gets, and which files it changed. |
//! | [`registry`] | [`ToolRegistry`], where MCP and extension tools register, and the tools [`ToolSearch`] and [`ToolCall`] that reach them. |
//! | [`toolbox`] | [`Toolbox`]: the tools a session declares to the model. |
//!
//! # Running a tool
//!
//! [`Tool::plan`] first, then [`execute`], never [`Tool::run`] directly:
//! `execute` is where the output policy is applied, so that the result of
//! every tool, built in or not, is bounded in the same way. A result made
//! without running a tool (the error of a call that could not be planned, a
//! denial) passes the same policy through [`bound`].

pub mod blocking;
pub mod context;
pub mod contract;
pub mod files;
pub mod list;
pub mod output;
pub mod process;
pub mod read;
pub mod registry;
pub mod result;
pub mod schema;
pub mod search;
pub mod shell;
pub mod store;
pub mod toolbox;

mod args;
mod detect;
mod diff;
mod walk;

#[cfg(test)]
use cortex_testkit::cases;

pub use blocking::{BlockingError, run_blocking};
pub use context::{ImageSupport, OpenGate, Outliner, PlanCx, ReadGate, RunCx};
pub use contract::{
    BoxFuture, Concurrency, ConcurrencyClass, Effect, Json, NetworkTarget, NoOutput, Plan, Tool,
    ToolOutput, ToolSpec,
};
pub use files::{FileSnapshot, FileTable, LineRanges, Shown, Staleness};
pub use list::ListTool;
pub use output::{Budget, Compression, Keep, OutputLimits, Shaped, Treatment};
pub use read::ReadTool;
pub use registry::{FoundTool, RegistryError, SourceSummary, ToolCall, ToolRegistry, ToolSearch};
pub use result::{ToolError, ToolResult};
pub use search::SearchTool;
pub use shell::{Shell, ShellConfig, ShellKind, ShellSearch, ShellTool};
pub use store::{ArtifactWriter, ContentStore, MemoryStore, StoreError};
pub use toolbox::Toolbox;

/// Runs a planned call and applies the output policy to its result.
///
/// This is the only way a tool is run. What it returns is
/// [bounded](ToolResult::is_bounded): within the limits in `cx`, with a
/// notice and an artifact if anything had to be cut.
///
/// The policy is applied even when the call was cancelled: a result is
/// recorded for every call, so it is bounded like any other.
pub async fn execute(
    tool: &dyn Tool,
    plan: Plan,
    cx: &RunCx,
    out: &mut dyn ToolOutput,
) -> ToolResult {
    let result = tool.run(plan, cx, out).await;
    bound_as(result, &tool.spec().name, cx, output::Treatment::AsReturned).await
}

/// Applies the output policy to a result that did not come from
/// [`execute`]: the error of a call that could not be planned
/// ([`ToolError::into_result`]), a denial, a result made for a call that
/// never ran. `tool` names the call, for the label of an artifact.
///
/// Every result that enters the context passes this or `execute`. An error
/// can be long too: one for arguments that do not fit quotes the schema of
/// a third-party tool.
pub async fn bound(result: ToolResult, tool: &str, cx: &RunCx) -> ToolResult {
    bound_as(result, tool, cx, output::Treatment::AsReturned).await
}

/// [`execute`] for a tool from outside the harness, whose output is
/// compressed whether or not it fits the limits.
pub(crate) async fn execute_produced(
    tool: &dyn Tool,
    plan: Plan,
    cx: &RunCx,
    out: &mut dyn ToolOutput,
) -> ToolResult {
    let result = tool.run(plan, cx, out).await;
    bound_as(result, &tool.spec().name, cx, output::Treatment::Produced).await
}

async fn bound_as(
    result: ToolResult,
    tool: &str,
    cx: &RunCx,
    treatment: output::Treatment,
) -> ToolResult {
    if result.is_bounded() {
        return result;
    }
    if treatment == output::Treatment::AsReturned
        && cx.limits.fits(&result.text())
        && !has_images(&result)
    {
        // Nothing to cut and nothing to store: no need to leave the thread.
        return output::enforce(result, tool, &cx.limits, None, cx.store.as_ref(), treatment);
    }
    // Shaping a large output and storing its artifact both block. The work
    // is not tied to the call's token: the result of a cancelled call is
    // still recorded, and must be bounded first.
    let name = tool.to_owned();
    let limits = cx.limits;
    let images = cx.images.clone();
    let store = std::sync::Arc::clone(&cx.store);
    let fallback = result.clone();
    let bounded = run_blocking(&cortex_base::CancelToken::new(), move |_| {
        output::enforce(
            result,
            &name,
            &limits,
            images.as_ref(),
            store.as_ref(),
            treatment,
        )
    })
    .await;
    match bounded {
        Ok(result) => result,
        // The store panicked. The result is cut all the same, without an
        // artifact, by a store that keeps nothing.
        Err(error) => output::enforce(
            fallback,
            tool,
            &cx.limits,
            cx.images.as_ref(),
            &store::Unavailable(error.to_string()),
            treatment,
        ),
    }
}

fn has_images(result: &ToolResult) -> bool {
    result
        .content()
        .iter()
        .any(|part| matches!(part, cortex_protocol::message::Part::Image(_)))
}
