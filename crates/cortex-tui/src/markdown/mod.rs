//! Markdown, streamed: what a model writes, laid out as it arrives, with
//! each line committed the moment it can no longer change
//! (`docs/architecture/09-tui.md`, "Rendering approach").

pub(crate) mod inline;
