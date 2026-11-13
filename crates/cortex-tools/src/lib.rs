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
//! | [`registry`] | [`ToolRegistry`], where MCP and extension tools register, and the tools [`ToolSearch`] and [`ToolCall`] that reach them. |
//! | [`toolbox`] | [`Toolbox`]: the tools a session declares to the model. |
//!
//! # Running a tool
//!
//! [`Tool::plan`] first, then [`execute`], never [`Tool::run`] directly:
//! `execute` is where the output policy is applied, so that the result of
//! every tool, built in or not, is bounded in the same way.

pub mod blocking;
pub mod context;
pub mod contract;
pub mod files;
pub mod list;
pub mod output;
pub mod read;
pub mod registry;
pub mod result;
pub mod schema;
pub mod search;
pub mod store;
pub mod toolbox;

mod args;
#[cfg(test)]
mod cases;
mod detect;
mod diff;
mod walk;

pub use blocking::{BlockingError, run_blocking};
pub use context::{ImageSupport, OpenGate, Outliner, PlanCx, ReadGate, RunCx};
pub use contract::{
    BoxFuture, Concurrency, ConcurrencyClass, Effect, Json, NetworkTarget, NoOutput, Plan, Tool,
    ToolOutput, ToolSpec,
};
pub use files::{FileSnapshot, FileTable, LineRanges, Shown, Staleness};
pub use list::ListTool;
pub use output::{Budget, Compression, Keep, OutputLimits, Shaped};
pub use read::ReadTool;
pub use registry::{FoundTool, RegistryError, SourceSummary, ToolCall, ToolRegistry, ToolSearch};
pub use result::{ToolError, ToolResult};
pub use search::SearchTool;
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
    if result.is_bounded() {
        return result;
    }
    if cx.limits.fits(&result.text()) && !has_images(&result) {
        // Nothing to cut and nothing to store: no need to leave the thread.
        return output::enforce(
            result,
            &tool.spec().name,
            &cx.limits,
            None,
            cx.store.as_ref(),
        );
    }
    // Shaping a large output and storing its artifact both block. The work
    // is not tied to the call's token: the result of a cancelled call is
    // still recorded, and must be bounded first.
    let name = tool.spec().name.clone();
    let limits = cx.limits;
    let images = cx.images.clone();
    let store = std::sync::Arc::clone(&cx.store);
    let fallback = result.clone();
    let bounded = run_blocking(&cortex_base::CancelToken::new(), move |_| {
        output::enforce(result, &name, &limits, images.as_ref(), store.as_ref())
    })
    .await;
    match bounded {
        Ok(result) => result,
        // The store panicked. The result is cut all the same, without an
        // artifact, by a store that keeps nothing.
        Err(error) => output::enforce(
            fallback,
            &tool.spec().name,
            &cx.limits,
            cx.images.as_ref(),
            &store::Unavailable(error.to_string()),
        ),
    }
}

fn has_images(result: &ToolResult) -> bool {
    result
        .content()
        .iter()
        .any(|part| matches!(part, cortex_protocol::message::Part::Image(_)))
}
