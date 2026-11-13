//! Sandbox policy types and the per-OS launchers, and process trees.
//!
//! | Module | What it gives |
//! |---|---|
//! | [`tree`] | [`Tree`](tree::Tree): a command started so that it and everything it starts can be stopped as one, and dies with the harness. [`Guard`](tree::Guard), which makes the second half true on Linux. [`Output`](tree::Output), the one pipe such a command writes to. |
//!
//! The sandbox policy and the launchers arrive in the build step that
//! `docs/build-plan.md` assigns to them.
//!
//! This crate is one of the places where `unsafe` is allowed, module by
//! module, for the operating-system interfaces nothing safe reaches
//! (`docs/decisions/0030-process-tree-control-in-cortex-sandbox.md`).

pub mod tree;
