//! The declared tools: the set a session offers the model by name.
//!
//! The list of their definitions is part of every request's prefix, and is
//! fixed for a context epoch
//! (`docs/architecture/07-sessions-and-context.md`). A [`Toolbox`] is that
//! set. Tools that come and go, from MCP servers and extensions, are not in
//! it: they are in the [`ToolRegistry`], behind the two tools here that
//! reach it.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::contract::{Tool, ToolSpec};
use crate::list::ListTool;
use crate::read::ReadTool;
use crate::registry::{ToolCall, ToolRegistry, ToolSearch};
use crate::search::SearchTool;

/// A set of tools, each known by the name in its spec.
#[derive(Clone, Default)]
pub struct Toolbox {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl Toolbox {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The tools that only read: `read`, `list` and `search`, and
    /// `tool_search` and `tool_call` in front of `registry`. Later build
    /// steps add the tools that write and run things.
    #[must_use]
    pub fn read_only(registry: Arc<ToolRegistry>) -> Self {
        Self::new()
            .with(ReadTool::new())
            .with(ListTool::new())
            .with(SearchTool::new())
            .with(ToolSearch::new(Arc::clone(&registry)))
            .with(ToolCall::new(registry))
    }

    /// Adds a tool. A tool already there under the same name is replaced.
    #[must_use]
    pub fn with(mut self, tool: impl Tool + 'static) -> Self {
        self.insert(Arc::new(tool));
        self
    }

    /// Adds a tool that is already shared. A tool already there under the
    /// same name is replaced, and returned.
    pub fn insert(&mut self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        self.tools.insert(tool.spec().name.clone(), tool)
    }

    /// The tool with this name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// The names of the tools, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// The definitions to declare to the model, sorted by name, so that the
    /// request prefix is the same every time.
    #[must_use]
    pub fn specs(&self) -> Vec<&ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    /// How many tools there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl std::fmt::Debug for Toolbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}
