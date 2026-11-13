//! `execute`: the one way a tool is run, and where the output policy
//! applies to whatever it returned.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cortex_base::AbsPath;
use cortex_protocol::item::ToolOutcome;
use cortex_tools::{
    BoxFuture, ContentStore, Json, ListTool, NoOutput, OutputLimits, Plan, PlanCx, ReadGate,
    ReadTool, RunCx, SearchTool, Tool, ToolError, ToolOutput, ToolResult, ToolSpec, execute,
};
use serde_json::json;

use crate::{Workspace, block_on, ok};

/// A tool that returns whatever text it was made with, however long.
struct Says {
    spec: ToolSpec,
    text: String,
}

impl Says {
    fn new(text: impl Into<String>) -> Self {
        Self {
            spec: ToolSpec::new("says", "Says a thing.", json!({"type": "object"})),
            text: text.into(),
        }
    }
}

impl Tool for Says {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, _args: &Json, _cx: &PlanCx) -> Result<Plan, ToolError> {
        Ok(Plan::new("Say it", ()))
    }

    fn run<'a>(
        &'a self,
        _plan: Plan,
        _cx: &'a RunCx,
        out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            out.output("saying it\n");
            ToolResult::success(self.text.clone())
        })
    }
}

/// A tool that waits until it is cancelled, as a tool waiting on a process
/// or a server would.
struct Waits {
    spec: ToolSpec,
}

impl Tool for Waits {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, _args: &Json, _cx: &PlanCx) -> Result<Plan, ToolError> {
        Ok(Plan::new("Wait", ()))
    }

    fn run<'a>(
        &'a self,
        _plan: Plan,
        cx: &'a RunCx,
        _out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            cx.cancel.cancelled().await;
            ToolResult::interrupted("stopped while waiting\n".repeat(5000))
        })
    }
}

#[test]
fn a_call_can_be_moved_to_another_thread() {
    // The loop spawns each call as a task that may move between threads
    // (`docs/architecture/02-runtime.md`): the future of a call, and what
    // it runs with, must be `Send`. This fails to compile if they are not.
    fn assert_send<T: Send>(_: &T) {}
    let ws = Workspace::new("execute_send");
    let tool = ReadTool::new();
    let cx = ws.cx();
    let plan = ws.plan(&tool, json!({"path": "a.txt"})).unwrap();
    assert_send(&plan);
    assert_send(&cx);
    let mut out = NoOutput;
    let call = execute(&tool, plan, &cx, &mut out);
    assert_send(&call);
    drop(call);
}

#[test]
fn a_result_within_the_limits_passes_as_the_tool_wrote_it() {
    let ws = Workspace::new("execute_fits");
    let tool = Says::new("short and whole");
    let cx = ws.cx();
    let plan = ws.plan(&tool, json!({})).unwrap();
    let mut seen = String::new();
    let result = block_on(execute(&tool, plan, &cx, &mut seen));
    assert!(result.is_bounded());
    assert_eq!(ok(&result), "short and whole");
    assert_eq!(
        seen, "saying it\n",
        "what the tool sent while running reached the caller"
    );
    assert_eq!(ws.store.artifact_count(), 0);
}

#[test]
fn a_result_over_the_limits_is_cut_with_a_notice_and_an_artifact() {
    let ws = Workspace::new("execute_cut");
    let output: String = (1..=4000)
        .map(|n| {
            format!(
                "{} of the output, line {n}\n",
                ["part", "piece", "bit"][n % 3]
            )
        })
        .collect();
    let tool = Says::new(output.clone());
    let cx = ws.cx();
    let plan = ws.plan(&tool, json!({})).unwrap();
    let result = block_on(execute(&tool, plan, &cx, &mut NoOutput));

    let text = ok(&result);
    assert!(cx.limits.fits(&text));
    let lines: Vec<&str> = text.lines().collect();
    // The beginning and the end are kept, and what is between is counted.
    assert_eq!(lines[0], "piece of the output, line 1");
    assert_eq!(lines[lines.len() - 2], "piece of the output, line 4000");
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("[… ")).count(),
        1
    );
    let [artifact] = result.artifacts() else {
        panic!("one artifact, not {:?}", result.artifacts());
    };
    assert_eq!(
        *lines.last().unwrap(),
        format!(
            "[Output cut: 3,202 of 4,000 lines not shown. The whole output (116.1 KiB) is \
             artifact {}: pass that id as `artifact` to read or search.]",
            artifact.id
        )
    );
    assert_eq!(ws.store.artifact(&artifact.id), Some(output.into_bytes()));
}

#[test]
fn control_sequences_alone_do_not_make_a_result_too_long() {
    let ws = Workspace::new("execute_colours");
    // 500 lines, each wrapped in colour codes: over 32 KiB as produced,
    // well under it once the codes are gone.
    let output: String = (1..=500)
        .map(|n| {
            format!(
                "\u{1b}[38;5;{}m{} line {n}\u{1b}[0m{}\n",
                n % 200,
                ["red", "green"][n % 2],
                "\u{1b}[K".repeat(24)
            )
        })
        .collect();
    assert!(output.len() > 32 * 1024);
    let tool = Says::new(output);
    let cx = ws.cx();
    let plan = ws.plan(&tool, json!({})).unwrap();
    let result = block_on(execute(&tool, plan, &cx, &mut NoOutput));
    let text = ok(&result);
    assert_eq!(text.lines().count(), 500, "every line is there");
    assert!(text.starts_with("green line 1\nred line 2\n"));
    assert!(!text.contains('\u{1b}'));
    assert!(
        result.artifacts().is_empty(),
        "nothing was lost, so nothing is stored"
    );
}

#[test]
fn a_cancelled_call_still_ends_with_one_bounded_result() {
    let ws = Workspace::new("execute_cancelled");
    let tool = Waits {
        spec: ToolSpec::new("waits", "Waits.", json!({"type": "object"})),
    };
    let cx = ws.cx();
    let plan = ws.plan(&tool, json!({})).unwrap();
    // The token fires from another thread while the call is waiting on it.
    let canceller = cx.cancel.clone();
    let thread = std::thread::spawn(move || canceller.cancel());
    let result = block_on(execute(&tool, plan, &cx, &mut NoOutput));
    thread.join().unwrap();
    assert_eq!(result.outcome(), &ToolOutcome::Interrupted);
    assert!(
        result.is_bounded(),
        "what an interrupted call had to show is bounded too"
    );
    assert!(cx.limits.fits(&result.text()));
    assert!(
        result
            .text()
            .starts_with("stopped while waiting\n[the line above repeats 4,999 more times]")
    );
}

#[test]
fn a_plan_run_by_another_tool_is_an_error_result_not_a_panic() {
    let ws = Workspace::new("execute_wrong_plan");
    ws.file("a.txt", "a\n");
    let cx = ws.cx();
    // A plan made by `list`, handed to `read` and to `search`.
    for tool in [&ReadTool::new() as &dyn Tool, &SearchTool::new()] {
        let plan = ws.plan(&ListTool::new(), json!({})).unwrap();
        let result = block_on(execute(tool, plan, &cx, &mut NoOutput));
        assert_eq!(result.outcome(), &ToolOutcome::Error);
        assert_eq!(
            result.text(),
            "internal error: the plan `List .` was made by another tool"
        );
    }
}

#[test]
fn the_limits_in_the_context_are_the_ones_applied() {
    let ws = Workspace::new("execute_limits");
    let tool = Says::new("x\n".repeat(100));
    let tight = ws.cx().with_limits(OutputLimits::new(4096, 10, 256));
    let plan = ws.plan(&tool, json!({})).unwrap();
    let result = block_on(execute(&tool, plan, &tight, &mut NoOutput));
    // A hundred identical lines are a line and a count: nothing is cut.
    assert_eq!(result.text(), "x\n[the line above repeats 99 more times]");
    assert!(result.artifacts().is_empty());

    let varied: String = (1..=100)
        .map(|n| format!("{} {n}\n", ["a", "b"][n % 2]))
        .collect();
    let tool = Says::new(varied);
    let plan = ws.plan(&tool, json!({})).unwrap();
    let result = block_on(execute(&tool, plan, &tight, &mut NoOutput));
    assert_eq!(result.text().lines().count(), 10);
    assert_eq!(result.artifacts().len(), 1);
}

/// A gate that counts what it is asked and fires a token after a number of
/// entries: the walk is cancelled while it is under way.
struct CancelAfter {
    seen: AtomicUsize,
    after: usize,
    cancel: cortex_base::CancelToken,
}

impl ReadGate for CancelAfter {
    fn may_read(&self, _path: &AbsPath) -> bool {
        if self.seen.fetch_add(1, Ordering::SeqCst) + 1 == self.after {
            self.cancel.cancel();
        }
        true
    }
}

#[test]
fn a_walk_that_is_cancelled_part_way_stops_there() {
    let ws = Workspace::new("execute_cancel_walk");
    let total = 1500;
    for n in 0..total {
        ws.file(&format!("d{:02}/f{n:04}.txt", n % 30), "needle\n");
    }
    for tool in [&SearchTool::new() as &dyn Tool, &ListTool::new()] {
        let cx = ws.cx();
        let gate = Arc::new(CancelAfter {
            seen: AtomicUsize::new(0),
            after: 100,
            cancel: cx.cancel.clone(),
        });
        let cx = cx.with_gate(Arc::clone(&gate) as Arc<dyn ReadGate>);
        let result = ws.call_with(
            &cx,
            tool,
            json!({"pattern": "needle", "glob": "**/*"}).pick(tool),
        );
        assert_eq!(
            result.outcome(),
            &ToolOutcome::Interrupted,
            "{}",
            result.text()
        );
        // Every thread stops at its next entry: a few past the hundredth,
        // and nowhere near all of them.
        let seen = gate.seen.load(Ordering::SeqCst);
        assert!(
            (100..400).contains(&seen),
            "{seen} of {} entries were visited",
            total + 30
        );
    }
}

/// The arguments each of the two walking tools takes, from one object.
trait Pick {
    fn pick(self, tool: &dyn Tool) -> Json;
}

impl Pick for Json {
    fn pick(mut self, tool: &dyn Tool) -> Json {
        if let Some(arguments) = self.as_object_mut() {
            if tool.spec().name == "list" {
                arguments.remove("pattern");
            } else {
                arguments.remove("glob");
            }
        }
        self
    }
}

#[test]
fn the_store_a_result_is_cut_into_is_the_one_in_the_context() {
    let ws = Workspace::new("execute_store");
    let other = cortex_tools::MemoryStore::new();
    let cx = RunCx::new(
        Arc::clone(&ws.paths),
        Arc::new(other.clone()),
        Arc::clone(&ws.files),
    );
    let varied: String = (1..=3000)
        .map(|n| format!("{} {n}\n", ["a", "b"][n % 2]))
        .collect();
    let tool = Says::new(varied);
    let plan = ws.plan(&tool, json!({})).unwrap();
    let result = block_on(execute(&tool, plan, &cx, &mut NoOutput));
    assert_eq!(other.artifact_count(), 1);
    assert_eq!(ws.store.artifact_count(), 0);
    let id = &result.artifacts()[0].id;
    assert!(other.open_artifact(id).is_ok());
}
