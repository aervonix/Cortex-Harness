//! The tools a session offers its model, as the loop sees them.

use std::sync::Arc;

use cortex_tools::{Tool, ToolSpec, Toolbox};

/// A set of tools, each known by the name in its spec.
///
/// The loop asks it for two things: the definitions to declare to the
/// model, and the tool a call names. It never names a tool itself.
pub trait ToolSet: Send + Sync {
    /// The tool with this name.
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;

    /// The specs of every tool, sorted by name. The order is part of the
    /// request's cached prefix, so it is the same every time.
    fn specs(&self) -> Vec<&ToolSpec>;
}

impl ToolSet for Toolbox {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        Toolbox::get(self, name).cloned()
    }

    fn specs(&self) -> Vec<&ToolSpec> {
        Toolbox::specs(self)
    }
}
