//! The one autonomy model: levels and leases, permission rules,
//! shell-command analysis, approvals and project trust
//! (`docs/architecture/06-permissions-and-sandbox.md`).
//!
//! This crate decides what the agent may do without asking. It is not a
//! security boundary: rules reason about what a command says, not what it
//! does, and only the OS sandbox confines a process (`docs/security.md`).
//! What it must never do is grant more than the user allowed, so every part
//! of it fails closed: what is not understood is asked about or refused,
//! never let through.
//!
//! | Module | What it gives |
//! |---|---|
//! | [`level`] | The ladder of levels as a table, and which levels need an enforcing sandbox. |
//! | [`lease`] | [`LeaseExt`]: `narrow` and `is_within` for the protocol's lease, and the standing lease every session holds. |
//! | [`rules`] | [`RuleSet`]: rules compiled and evaluated, `deny` over `ask` over `allow`, with the built-in protection of the harness's own files. |
//! | [`glob`] | The patterns of rules: path globs and name patterns. |
//! | [`secrets`] | The file names that look like secrets, which are asked about by default. |
//! | [`command`] | Command analysis that fails closed, for POSIX shells and PowerShell, and the table of commands that only read. |
//! | [`approvals`] | Approvals for a session and for a project, and the project's stored file. |
//! | [`trust`] | Project trust, pinned to the hash of each definition the user saw. |
//! | [`engine`] | [`Engine`]: one decision for one planned tool call. |
//!
//! # What this crate does not do
//!
//! - **It asks nobody.** It says "ask" and records the answer. Putting the
//!   question to a person, and what happens when nobody can be asked, are
//!   the core's.
//! - **It runs nothing.** Command analysis reads text. It never starts a
//!   shell, an interpreter or a program.
//! - **It reads no configuration.** Rules, the level and the directories
//!   to protect are handed to it, each rule with who wrote it.
//! - **It adds no lenient way to read a lease or a rule.** Both come
//!   through the protocol's strict types, which refuse a field or a value
//!   they do not know
//!   (`docs/decisions/0028-enforced-values-fail-closed.md`).
//!
//! # How the core connects it
//!
//! `cortex-agent` reaches a policy through its own `Policy` trait, and
//! `cortex-tools` and `cortex-session` each have a gate a session fills.
//! This crate depends on none of them, so the core adapts:
//!
//! | Seam | Filled with |
//! |---|---|
//! | `Policy::decide` | [`Engine::decide`] on the plan's effects as [`Access`]es. `Allow { read_only: true }` is the loop's `AllowReadOnly`. |
//! | `Policy::ask` | The question put to a front end with [`Question::choices`], then [`Engine::record`]. With nobody to ask: a denial with [`Question::refusal`], or the loop's `Stop`. |
//! | `Policy::read_gate` | [`Engine::may_read`]. |
//! | `KeepGate` of the base state | [`Engine::may_store`]. |

pub mod approvals;
pub mod command;
pub mod engine;
mod expansion;
pub mod glob;
pub mod lease;
pub mod level;
pub mod rules;
pub mod secrets;
mod store_lock;
pub mod trust;

pub use approvals::{ApprovalError, ApprovalStore, SessionApprovals};
pub use command::{Analysis, ShellSyntax, analyse};
pub use engine::{
    Access, Engine, Lexical, Locate, Outcome, Parts, Question, Recorded, Request, StartError,
};
pub use lease::{LeaseExt, NarrowError, StandingTerms, default_sandbox, standing_lease};
pub use level::{Decider, LevelError, Matter, NoSandbox, SandboxState, check_standing};
pub use rules::{Protected, RuleContext, RuleError, RuleSet, RuleSource, SourcedRule};
pub use trust::{Definition, ProjectTrust, Standing, TrustError, TrustStore};
