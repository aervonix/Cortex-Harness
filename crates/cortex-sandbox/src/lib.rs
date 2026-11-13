//! Process trees, and the sandbox that confines what they run.
//!
//! | Module | What it gives |
//! |---|---|
//! | [`tree`] | [`Tree`](tree::Tree): a command started so that it and everything it starts can be stopped as one, and dies with the harness. [`Guard`](tree::Guard), which makes the second half true on Linux. [`Output`](tree::Output), the one pipe such a command writes to. |
//! | [`policy`] | [`Policy`](policy::Policy): what a sandboxed command may read and write, and whether it may use the network, with the wire form the launcher reads. |
//! | [`support`] | [`detect`](support::detect): what the kernel offers, and whether the sandbox enforces with it. |
//! | [`launch`] | [`Launcher`](launch::Launcher), the harness's side of the launcher, and [`run`](launch::run), the body of its hidden subcommand. |
//! | [`explain`] | One line for the model about what the sandbox blocked. |
//! | [`protect`] | The protected paths of a workspace, looked at before and after a sandboxed command. |
//!
//! The backends that confine, Landlock and seccomp on Linux, are behind the
//! feature `backends`, off by default, so that `cortex-tools`, which needs
//! only process trees, does not link them. `cortex-core`'s `sandbox` feature
//! turns it on (`docs/architecture/01-workspace.md`, rule 8). Without it the
//! launcher refuses to run anything, and [`support::detect`] says so.
//!
//! This crate is one of the places where `unsafe` is allowed, module by
//! module, for the operating-system interfaces nothing safe reaches
//! (`docs/decisions/0030-process-tree-control-in-cortex-sandbox.md`).

#[cfg(all(target_os = "linux", feature = "backends"))]
mod confine;
pub mod explain;
pub mod launch;
pub mod policy;
pub mod protect;
pub mod support;
pub mod tree;
