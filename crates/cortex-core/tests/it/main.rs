//! Integration tests (tier 2): parts of the harness that only meet in this
//! crate, together, on a real file system.
//!
//! The core itself is assembled in its own build step. Until then this
//! binary holds what cannot be tested anywhere else: `cortex-tools` cannot
//! see `cortex-session`, so the editing tools and the session's checkpoint
//! store are run together here.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "an integration test prepares real files and fails by panicking"
)]

mod undo;
